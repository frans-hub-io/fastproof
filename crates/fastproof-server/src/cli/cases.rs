//! `fastproof cases export|import`: test cases as CSV or NDJSON to a file or stdout, and imports
//! from a file (a dry run by default; `--apply` applies it).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use futures::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};

use super::Conn;
use super::client::ServerError;

#[derive(Subcommand, Debug)]
pub enum CasesCommand {
    /// Export the project's test cases (all of them, or those of `--filter`-like options).
    Export(ExportArgs),
    /// Import test cases from a file: a dry run that prints what would change; `--apply`
    /// applies it.
    Import(ImportArgs),
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// `ndjson` (re-importable FastProof format) or `csv`.
    #[arg(long, default_value = "ndjson")]
    pub format: String,
    /// Write to this file instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Only cases of this folder (and its subfolders).
    #[arg(long)]
    pub folder: Option<uuid::Uuid>,
    /// Only cases with this status (`draft`, `active`, `outdated`).
    #[arg(long)]
    pub status: Option<String>,
    /// Only cases with this tag.
    #[arg(long)]
    pub tag: Option<String>,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// The file (CSV, JSON or NDJSON).
    pub file: PathBuf,
    /// `generic-csv`, `testrail-csv`, `qase-csv`, `qase-json`, `testops-csv` or `fastproof-ndjson`.
    #[arg(long)]
    pub format: String,
    /// `createOnly` (default) or `upsert` (update cases with the same external key).
    #[arg(long, default_value = "createOnly")]
    pub mode: String,
    /// Select values that are not options yet: `skip` (default), `create` or `fail`.
    #[arg(long, default_value = "skip")]
    pub unknown_values: String,
    /// Steps layout of a CSV: `rows` or `cells` (detected when absent).
    #[arg(long)]
    pub steps_layout: Option<String>,
    /// A JSON file with the column mapping `{column: target}` (generic CSV).
    #[arg(long)]
    pub mapping: Option<PathBuf>,
    /// A zip with the attachment files cases reference by name.
    #[arg(long)]
    pub attachments: Option<PathBuf>,
    /// The folder the imported folders go under.
    #[arg(long)]
    pub folder: Option<uuid::Uuid>,
    /// Apply the import after the dry run.
    #[arg(long)]
    pub apply: bool,
}

/// Runs a `cases` command.
pub async fn run(c: CasesCommand) -> anyhow::Result<i32> {
    match c {
        CasesCommand::Export(e) => export(e).await,
        CasesCommand::Import(i) => import(i).await,
    }
}

async fn error_of(r: reqwest::Response) -> anyhow::Error {
    let status = r.status();
    let body: Value = r.json().await.unwrap_or_default();
    ServerError {
        status,
        code: body["code"].as_str().unwrap_or("error").to_string(),
        message: body["message"]
            .as_str()
            .unwrap_or("request failed")
            .to_string(),
    }
    .into()
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn export(e: ExportArgs) -> anyhow::Result<i32> {
    if !matches!(e.format.as_str(), "csv" | "ndjson") {
        bail!("--format must be csv or ndjson");
    }
    let client = e.conn.client()?;
    let mut q = format!("format={}", e.format);
    if let Some(f) = e.folder {
        q.push_str(&format!("&folder={f}&recursive=true"));
    }
    if let Some(s) = &e.status {
        q.push_str(&format!("&status={}", enc(s)));
    }
    if let Some(t) = &e.tag {
        q.push_str(&format!("&tag={}", enc(t)));
    }
    let path = format!("api/v1/projects/{}/cases/export?{q}", enc(&e.conn.project));
    let r = client
        .request(Method::GET, &path)?
        .send()
        .await
        .context("contacting the FastProof server")?;
    if !r.status().is_success() {
        return Err(error_of(r).await);
    }
    let mut out: Box<dyn Write + Send> = match &e.out {
        Some(p) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    let mut stream = r.bytes_stream();
    let mut bytes = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading the export")?;
        bytes += chunk.len() as u64;
        out.write_all(&chunk)?;
    }
    out.flush()?;
    eprintln!("exported {bytes} bytes");
    Ok(0)
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = format!("fastproof{}", uuid::Uuid::now_v7().simple());
    let mut body = Vec::new();
    for (name, file, bytes) in parts {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        if let Some(f) = file {
            let f: String = f.chars().filter(|c| *c != '"' && !c.is_control()).collect();
            body.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn wait(
    client: &super::client::Client,
    project: &str,
    id: &str,
    want: &[&str],
) -> anyhow::Result<Value> {
    loop {
        let j: Value = client
            .send(client.request(
                Method::GET,
                &format!("api/v1/projects/{}/imports/{id}", enc(project)),
            )?)
            .await?;
        let st = j["status"].as_str().unwrap_or("");
        if want.contains(&st) || matches!(st, "failed" | "cancelled" | "done") {
            return Ok(j);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn print_report(label: &str, j: &Value) {
    let r = &j["report"];
    eprintln!(
        "{label}: {} to create, {} to update, {} to skip, {} folders, {} errors",
        r["create"],
        r["update"],
        r["skip"],
        r["folders"],
        r["errors"].as_array().map(Vec::len).unwrap_or(0)
    );
    if let Some(f) = r["newFields"].as_array().filter(|f| !f.is_empty()) {
        eprintln!(
            "new fields (created): {}",
            f.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(f) = r["unknownFields"].as_array().filter(|f| !f.is_empty()) {
        eprintln!(
            "unknown fields (left out): {}",
            f.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(v) = r["unknownValues"].as_object().filter(|v| !v.is_empty()) {
        for (k, vals) in v {
            eprintln!("unknown values of {k}: {vals}");
        }
    }
    if let Some(errs) = r["errors"].as_array() {
        for e in errs.iter().take(20) {
            eprintln!(
                "  row {}: {} {}",
                e["row"],
                e["code"].as_str().unwrap_or(""),
                e["message"].as_str().unwrap_or("")
            );
        }
    }
}

async fn import(a: ImportArgs) -> anyhow::Result<i32> {
    let client = a.conn.client()?;
    let file = std::fs::read(&a.file).with_context(|| format!("reading {}", a.file.display()))?;
    let mut options =
        json!({"format": a.format, "mode": a.mode, "unknownFieldValues": a.unknown_values});
    if let Some(l) = &a.steps_layout {
        options["stepsLayout"] = json!(l);
    }
    if let Some(f) = a.folder {
        options["targetFolderId"] = json!(f);
    }
    if let Some(m) = &a.mapping {
        let text =
            std::fs::read_to_string(m).with_context(|| format!("reading {}", m.display()))?;
        options["mapping"] =
            serde_json::from_str(&text).context("the mapping file is not a JSON object")?;
    }
    let zip = match &a.attachments {
        Some(z) => Some(std::fs::read(z).with_context(|| format!("reading {}", z.display()))?),
        None => None,
    };
    let opts = options.to_string();
    let name = a
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cases".into());
    let mut parts: Vec<(&str, Option<&str>, &[u8])> = vec![
        ("options", None, opts.as_bytes()),
        ("file", Some(&name), &file),
    ];
    if let Some(z) = &zip {
        parts.push(("attachments", Some("attachments.zip"), z));
    }
    let (ct, body) = multipart(&parts);
    let project = a.conn.project.clone();
    let job: Value = client
        .send(
            client
                .request(
                    Method::POST,
                    &format!("api/v1/projects/{}/imports", enc(&project)),
                )?
                .header("content-type", ct)
                .body(body),
        )
        .await?;
    let id = job["id"].as_str().unwrap_or("").to_string();
    eprintln!("import {id}: dry run");
    let j = wait(&client, &project, &id, &["ready"]).await?;
    if j["status"] != "ready" {
        print_report("failed", &j);
        return Ok(2);
    }
    print_report("dry run", &j);
    if !a.apply {
        eprintln!(
            "nothing changed; run again with --apply, or apply this dry run: POST /api/v1/projects/{project}/imports/{id}/apply"
        );
        return Ok(0);
    }
    let _: Value = client
        .send(client.request(
            Method::POST,
            &format!("api/v1/projects/{}/imports/{id}/apply", enc(&project)),
        )?)
        .await?;
    let j = wait(&client, &project, &id, &["done"]).await?;
    print_report(j["status"].as_str().unwrap_or("?"), &j);
    Ok(if j["status"] == "done" { 0 } else { 2 })
}
