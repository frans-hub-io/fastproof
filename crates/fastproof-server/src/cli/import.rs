//! `fastproof import allure-history <dir|file>`: sends the history of Allure reports (an Allure 2
//! `history` directory or `history.json`, or an Allure 3 history file) to the server and waits
//! for the import job.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use clap::{Args, Subcommand};
use futures::StreamExt;
use reqwest::Method;
use serde_json::Value;
use uuid::Uuid;

use super::Conn;

#[derive(Subcommand, Debug)]
pub enum ImportCommand {
    /// Import the history of Allure reports as past launches, joined to the tests live results
    /// use: an Allure 2 `history` directory (or its `history.json`), or an Allure 3 history
    /// file (one JSON line per report).
    AllureHistory(AllureHistoryArgs),
}

#[derive(Args, Debug)]
pub struct AllureHistoryArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// A directory with `history.json` (also `history/history.json`, and the trend files next
    /// to it), a `history.json`, or an Allure 3 history file.
    pub path: PathBuf,
    /// `allure2`, `allure3` or `auto` (decided from the files).
    #[arg(long, default_value = "auto")]
    pub source: String,
    /// Branch of the launches made; set it to the branch your live runs report.
    #[arg(long)]
    pub branch: Option<String>,
    /// Environment key of the launches made (as `--environment` of live runs).
    #[arg(long, env = "FASTPROOF_ENVIRONMENT")]
    pub environment: Option<String>,
    /// Send the files and print the import id without waiting for the job.
    #[arg(long)]
    pub no_wait: bool,
    /// Seconds to wait for the job.
    #[arg(long, default_value_t = 3600)]
    pub timeout: u64,
}

const TRENDS: [(&str, &str); 4] = [
    ("historyTrend", "history-trend.json"),
    ("durationTrend", "duration-trend.json"),
    ("categoriesTrend", "categories-trend.json"),
    ("retryTrend", "retry-trend.json"),
];

pub async fn run(c: ImportCommand) -> anyhow::Result<i32> {
    match c {
        ImportCommand::AllureHistory(a) => allure_history(a).await,
    }
}

/// The files to send and the source they are.
pub fn collect(
    path: &Path,
    source: &str,
) -> anyhow::Result<(String, Vec<(&'static str, PathBuf)>)> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let (history, dir) = if meta.is_dir() {
        let nested = path.join("history").join("history.json");
        let direct = path.join("history.json");
        if direct.is_file() {
            (direct, path.to_path_buf())
        } else if nested.is_file() {
            (nested, path.join("history"))
        } else {
            let mut jsonl: Vec<PathBuf> = std::fs::read_dir(path)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.is_file())
                .collect();
            jsonl.sort();
            match jsonl.len() {
                1 => (jsonl.remove(0), path.to_path_buf()),
                0 => bail!(
                    "{} has no history.json, history/history.json or .jsonl file",
                    path.display()
                ),
                _ => bail!(
                    "{} has several .jsonl files: name the history file itself",
                    path.display()
                ),
            }
        }
    } else {
        (
            path.to_path_buf(),
            path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        )
    };
    let source = match source {
        "allure2" | "allure3" => source.to_string(),
        "auto" => detect(&history)?,
        _ => bail!("--source must be auto, allure2 or allure3"),
    };
    let mut files = vec![("history", history)];
    if source == "allure2" {
        for (field, name) in TRENDS {
            let p = dir.join(name);
            if p.is_file() {
                files.push((field, p));
            }
        }
    }
    Ok((source, files))
}

/// Allure 3 files hold one JSON object per line with `testResults`; Allure 2 `history.json` is
/// one object keyed by test ids.
fn detect(file: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(file)
        .with_context(|| format!("reading {}", file.display()))?
        .take(64 * 1024)
        .read_to_end(&mut head)?;
    let text = String::from_utf8_lossy(&head);
    let jsonl = file.extension().is_some_and(|x| x == "jsonl");
    Ok(if jsonl || text.contains("\"testResults\"") {
        "allure3".into()
    } else {
        "allure2".into()
    })
}

fn part_header(boundary: &str, field: &str, file: &str) -> Bytes {
    Bytes::from(format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{file}\"\r\nContent-Type: application/json\r\n\r\n"
    ))
}

/// A multipart body that streams the files from disk.
fn body(boundary: &str, files: &[(&'static str, PathBuf)]) -> anyhow::Result<(u64, reqwest::Body)> {
    let mut len = 0u64;
    let mut parts: Vec<futures::stream::BoxStream<'static, std::io::Result<Bytes>>> = Vec::new();
    for (field, path) in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .replace(['"', '\r', '\n'], "_");
        let head = part_header(boundary, field, &name);
        let size = std::fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .len();
        len += head.len() as u64 + size + 2;
        let file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let reader = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
        parts.push(futures::stream::once(async move { Ok(head) }).boxed());
        parts.push(reader.boxed());
        parts.push(futures::stream::once(async { Ok(Bytes::from_static(b"\r\n")) }).boxed());
    }
    let tail = Bytes::from(format!("--{boundary}--\r\n"));
    len += tail.len() as u64;
    parts.push(futures::stream::once(async move { Ok(tail) }).boxed());
    Ok((
        len,
        reqwest::Body::wrap_stream(futures::stream::iter(parts).flatten()),
    ))
}

async fn allure_history(a: AllureHistoryArgs) -> anyhow::Result<i32> {
    let client = a.conn.client()?;
    let (source, files) = collect(&a.path, &a.source)?;
    for (_, p) in &files {
        eprintln!("sending {}", p.display());
    }
    let boundary = format!("fastproof-{}", Uuid::now_v7().simple());
    let (len, payload) = body(&boundary, &files)?;
    let mut query = vec![("source", source.as_str())];
    if let Some(b) = &a.branch {
        query.push(("branch", b));
    }
    if let Some(e) = &a.environment {
        query.push(("environment", e));
    }
    let path = format!(
        "api/v1/projects/{}/history-imports",
        super::client::Client::enc(&a.conn.project)
    );
    let rb = client
        .request(Method::POST, &path)?
        .query(&query)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(reqwest::header::CONTENT_LENGTH, len)
        .body(payload);
    let job: Value = client.send(rb).await?;
    let id = Uuid::parse_str(job["id"].as_str().unwrap_or_default())
        .context("the server answered without an import id")?
        .to_string();
    eprintln!("import {id} queued");
    if a.no_wait {
        println!("{id}");
        return Ok(0);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(a.timeout);
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let rb = client.request(
            Method::GET,
            &format!(
                "api/v1/projects/{}/history-imports/{id}",
                super::client::Client::enc(&a.conn.project)
            ),
        )?;
        let j: Value = client.send(rb).await?;
        match j["status"].as_str() {
            Some("done") => {
                print_summary(&j);
                return Ok(0);
            }
            Some("failed") => {
                print_summary(&j);
                eprintln!("failed: {}", j["error"].as_str().unwrap_or("unknown error"));
                return Ok(1);
            }
            _ => {}
        }
        if tokio::time::Instant::now() > deadline {
            eprintln!("still running after {} s; import {id}", a.timeout);
            return Ok(2);
        }
    }
}

fn print_summary(j: &Value) {
    let n = |k: &str| j[k].as_i64().unwrap_or(0);
    println!(
        "reports {}/{}, launches created {}, results stored {}, already there {}, unknown tests {}, left out {}",
        n("pointsDone"),
        n("pointsTotal"),
        n("launchesCreated"),
        n("resultsImported"),
        n("resultsExisting"),
        n("resultsUnmatched"),
        n("resultsSkipped"),
    );
    if let Some(w) = j["warnings"].as_array() {
        for m in w.iter().filter_map(Value::as_str) {
            eprintln!("warning: {m}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_an_allure2_directory_with_trends() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path().join("history");
        std::fs::create_dir(&h).unwrap();
        std::fs::write(h.join("history.json"), "{}").unwrap();
        std::fs::write(h.join("history-trend.json"), "[]").unwrap();
        std::fs::write(h.join("retry-trend.json"), "[]").unwrap();
        let (source, files) = collect(d.path(), "auto").unwrap();
        assert_eq!(source, "allure2");
        let names: Vec<&str> = files.iter().map(|f| f.0).collect();
        assert_eq!(names, ["history", "historyTrend", "retryTrend"]);
    }

    #[test]
    fn detects_allure3_by_content_and_name() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("hist.log");
        std::fs::write(&f, "{\"uuid\":\"a\",\"testResults\":{}}\n").unwrap();
        assert_eq!(collect(&f, "auto").unwrap().0, "allure3");
        let g = d.path().join("history.jsonl");
        std::fs::write(&g, "{}\n").unwrap();
        assert_eq!(collect(&g, "auto").unwrap().0, "allure3");
        assert_eq!(collect(&g, "allure2").unwrap().0, "allure2");
        assert!(collect(&g, "other").is_err());
        let (source, files) = collect(d.path(), "auto").unwrap();
        assert_eq!(source, "allure3");
        assert_eq!(files[0].1, g);
        std::fs::write(d.path().join("other.jsonl"), "{}\n").unwrap();
        assert!(collect(d.path(), "auto").is_err());
        let e = tempfile::tempdir().unwrap();
        assert!(collect(e.path(), "auto").is_err());
    }
}
