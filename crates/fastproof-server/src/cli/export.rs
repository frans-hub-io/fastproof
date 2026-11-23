//! `fastproof export`: a launch as a self-contained HTML report, an XLSX workbook or a PDF. The
//! PDF is the HTML report printed by a local Chromium (the server image has none).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Args;
use futures::StreamExt;
use reqwest::Method;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::Conn;
use super::client::ServerError;

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[command(flatten)]
    pub conn: Conn,
    /// Launch id, or its external id.
    #[arg(long, env = "FASTPROOF_LAUNCH")]
    pub launch: String,
    /// `html`, `xlsx` or `pdf`.
    #[arg(long, default_value = "html")]
    pub format: String,
    /// The file to write (default: `launch-<project>-<number>.<format>` in the current directory).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Statuses to keep, comma-separated (`failed,broken`).
    #[arg(long)]
    pub status: Option<String>,
    /// More filters of the result list as a query string (`q=login&newFailures=true`).
    #[arg(long)]
    pub filter: Option<String>,
    /// Use a background job even for a small launch.
    #[arg(long)]
    pub background: bool,
    /// Seconds to wait for a background job.
    #[arg(long, default_value_t = 3600)]
    pub timeout: u64,
    /// Chromium or Chrome executable for `--format pdf` (default: FASTPROOF_CHROMIUM, else the
    /// first of chromium, chromium-browser, google-chrome, google-chrome-stable, chrome, microsoft-edge in PATH).
    #[arg(long, env = "FASTPROOF_CHROMIUM")]
    pub chromium: Option<PathBuf>,
    /// Pass `--no-sandbox` to Chromium (needed when running as root in a container).
    #[arg(long)]
    pub no_sandbox: bool,
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
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

/// The Chromium to print with.
pub fn find_chromium(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "microsoft-edge",
    ] {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// What the CLI puts in front of an HTML report it prints: the policy of the server's page,
/// applied again here so that a server that sends something else still cannot make the browser
/// run scripts or load local files (`file:` frames and images are blocked by `default-src`).
pub const PRINT_PREFIX: &str = "<!doctype html>\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\n";

/// A private (0700) directory removed on drop, whatever path leaves the command.
pub struct PrivateDir(PathBuf);

impl PrivateDir {
    pub fn new() -> anyhow::Result<Self> {
        let p = std::env::temp_dir().join(format!("fastproof-export-{}", Uuid::now_v7()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&p)
            .with_context(|| format!("creating {}", p.display()))?;
        Ok(Self(p))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn save(r: reqwest::Response, out: &Path, prefix: &str) -> anyhow::Result<u64> {
    let mut f = tokio::fs::File::create(out)
        .await
        .with_context(|| format!("creating {}", out.display()))?;
    f.write_all(prefix.as_bytes()).await?;
    let mut n = 0u64;
    let mut s = r.bytes_stream();
    while let Some(c) = s.next().await {
        let c = c.context("reading the export")?;
        n += c.len() as u64;
        f.write_all(&c).await?;
    }
    f.flush().await?;
    Ok(n)
}

pub async fn run(a: ExportArgs) -> anyhow::Result<i32> {
    let pdf = match a.format.as_str() {
        "html" | "xlsx" => false,
        "pdf" => true,
        _ => bail!("--format must be html, xlsx or pdf"),
    };
    let chromium = if pdf {
        Some(find_chromium(a.chromium.as_deref()).context(
            "no Chromium found: install chromium or google-chrome, or set --chromium / FASTPROOF_CHROMIUM; \
             alternatively export --format html and print it from a browser",
        )?)
    } else {
        None
    };
    let client = a.conn.client()?;
    let project = &a.conn.project;
    let launch = match a.launch.parse::<Uuid>() {
        Ok(id) => client.launch(project, id).await?,
        Err(_) => client
            .launch_by_external(project, &a.launch)
            .await?
            .with_context(|| format!("no launch with external id {}", a.launch))?,
    };
    let mut filter = String::new();
    if let Some(s) = &a.status {
        filter.push_str(&format!("&status={}", enc(s)));
    }
    if let Some(f) = &a.filter {
        filter.push('&');
        filter.push_str(f.trim_start_matches(['?', '&']));
    }
    let server_format = if pdf { "html" } else { a.format.as_str() };
    let base = format!("api/v1/projects/{}/launches/{}", enc(project), launch.id);
    let out = a.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!("launch-{project}-{}.{}", launch.number, a.format))
    });
    let private = if pdf { Some(PrivateDir::new()?) } else { None };
    let html_path = match &private {
        Some(d) => d.path().join("report.html"),
        None => out.clone(),
    };
    let prefix = if pdf { PRINT_PREFIX } else { "" };
    let big = launch.counts.total > crate::launch_export::MAX_SYNC_RESULTS;
    let bytes = if big || a.background {
        let job: Value = client
            .send(client.request(
                Method::POST,
                &format!("{base}/exports?format={server_format}{filter}"),
            )?)
            .await?;
        let id = job["id"]
            .as_str()
            .context("the server sent no job id")?
            .to_string();
        let deadline = std::time::Instant::now() + Duration::from_secs(a.timeout);
        loop {
            let j: Value = client
                .send(client.request(Method::GET, &format!("{base}/exports/{id}"))?)
                .await?;
            match j["status"].as_str() {
                Some("done") => break,
                Some("failed") | Some("expired") => bail!(
                    "the export {}: {}",
                    j["status"].as_str().unwrap_or("failed"),
                    j["error"].as_str().unwrap_or("")
                ),
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                bail!("the export did not finish within {}s", a.timeout);
            }
            eprintln!(
                "exporting: {} of {} results",
                j["done"].as_i64().unwrap_or(0),
                j["total"].as_i64().unwrap_or(0)
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let r = client
            .request(Method::GET, &format!("{base}/exports/{id}/download"))?
            .send()
            .await
            .context("contacting the FastProof server")?;
        if !r.status().is_success() {
            return Err(error_of(r).await);
        }
        save(r, &html_path, prefix).await?
    } else {
        let r = client
            .request(
                Method::GET,
                &format!("{base}/export?format={server_format}{filter}"),
            )?
            .send()
            .await
            .context("contacting the FastProof server")?;
        if !r.status().is_success() {
            return Err(error_of(r).await);
        }
        save(r, &html_path, prefix).await?
    };
    if let Some(chromium) = chromium {
        let dir = private
            .as_ref()
            .map(PrivateDir::path)
            .unwrap_or(Path::new("."));
        print_pdf(&chromium, &html_path, &out, dir, a.no_sandbox).await?;
        eprintln!("wrote {}", out.display());
    } else {
        eprintln!("wrote {} ({bytes} bytes)", out.display());
    }
    Ok(0)
}

async fn print_pdf(
    chromium: &Path,
    html: &Path,
    out: &Path,
    dir: &Path,
    no_sandbox: bool,
) -> anyhow::Result<()> {
    let profile = dir.join("profile");
    let abs = std::fs::canonicalize(html)?;
    let mut cmd = tokio::process::Command::new(chromium);
    cmd.arg("--headless")
        .arg("--disable-gpu")
        .arg("--disable-extensions")
        .arg("--disable-background-networking")
        .arg("--no-first-run")
        .arg("--blink-settings=scriptEnabled=false")
        .arg("--no-pdf-header-footer")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", out.display()));
    if no_sandbox {
        cmd.arg("--no-sandbox");
    }
    cmd.arg(format!("file://{}", abs.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", chromium.display()))?;
    let o = tokio::time::timeout(Duration::from_secs(180), child.wait_with_output())
        .await
        .context("Chromium did not finish within 180 s")??;
    if !o.status.success() || !out.is_file() {
        bail!(
            "Chromium failed ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
                .chars()
                .take(500)
                .collect::<String>()
        );
    }
    Ok(())
}
