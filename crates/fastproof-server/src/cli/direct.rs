//! Direct uploads of the CLI: large attachment files of a results directory go straight to the
//! server's S3 store through pre-signed URLs, and the archive carries a short reference in
//! their place (see `crate::direct_uploads::stub`). Anything that cannot go directly (no S3 on
//! the server, the store not reachable from here, a refused or rejected file) stays in the
//! archive, so the upload never depends on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use reqwest::StatusCode;
use uuid::Uuid;

use super::client::{Client, ServerError};
use crate::api::direct_uploads::{DirectUploadStatus, DirectUploadTarget, MAX_ITEMS};

/// `--direct-uploads` / `FASTPROOF_DIRECT_UPLOAD_MODE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Directly when the server offers it, otherwise inside the archive.
    Auto,
    /// Always inside the archive.
    Off,
}

/// Report files the server parses stay in the archive whatever their size.
const REPORT_EXTENSIONS: [&str; 6] = ["json", "xml", "ndjson", "jsonl", "properties", "trx"];

/// Files of `files` that may go directly: at least `min` bytes and not a report file (an
/// Allure `*-attachment*` file always may).
pub fn candidates(files: &[(PathBuf, String)], min: u64) -> Vec<(PathBuf, String, u64)> {
    files
        .iter()
        .filter_map(|(abs, rel)| {
            let size = std::fs::metadata(abs).ok()?.len();
            if size < min.max(1) {
                return None;
            }
            let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
            let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            if !name.contains("-attachment") && REPORT_EXTENSIONS.contains(&ext) {
                return None;
            }
            Some((abs.clone(), rel.clone(), size))
        })
        .collect()
}

/// A content type from the file extension.
pub fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogv" | "ogg" => "video/ogg",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "pdf" => "application/pdf",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

/// What went directly.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Relative path -> attachment id, for the files the archive replaces with a reference.
    pub stubs: HashMap<String, Uuid>,
    pub bytes: u64,
    /// Files that stayed in the archive although they were candidates.
    pub kept: usize,
    /// The server does not offer direct uploads.
    pub unavailable: bool,
    pub warnings: Vec<String>,
}

fn server_status(e: &anyhow::Error) -> Option<StatusCode> {
    e.downcast_ref::<ServerError>().map(|s| s.status)
}

struct Hashed {
    abs: PathBuf,
    rel: String,
    size: u64,
    sha256: String,
}

async fn put(
    http: &reqwest::Client,
    t: &DirectUploadTarget,
    file: &Path,
    size: u64,
) -> Result<(), PutError> {
    let url = t.url.as_deref().ok_or(PutError::Refused(0))?;
    let f = tokio::fs::File::open(file)
        .await
        .map_err(|e| PutError::File(e.to_string()))?;
    let mut rb = http
        .put(url)
        .header(reqwest::header::CONTENT_LENGTH, size)
        .timeout(Duration::from_secs(120 + size / (512 * 1024)))
        .body(reqwest::Body::wrap_stream(
            tokio_util::io::ReaderStream::with_capacity(f, 256 * 1024),
        ));
    for (k, v) in t.headers.iter().flatten() {
        rb = rb.header(k, v);
    }
    match rb.send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(PutError::Refused(r.status().as_u16())),
        Err(_) => Err(PutError::Unreachable),
    }
}

enum PutError {
    /// No answer from the store (network, TLS, timeout): it is not reachable from here.
    Unreachable,
    Refused(u16),
    File(String),
}

async fn complete(
    client: &Client,
    project: &str,
    launch: Uuid,
    upload: Uuid,
) -> anyhow::Result<Uuid> {
    let mut delay = Duration::from_millis(500);
    for attempt in 0..5 {
        match client.complete_direct_upload(project, launch, upload).await {
            Ok(c) => return Ok(c.id),
            Err(e) => {
                let retry = matches!(
                    server_status(&e),
                    Some(StatusCode::CONFLICT | StatusCode::SERVICE_UNAVAILABLE) | None
                );
                if !retry || attempt == 4 {
                    return Err(e);
                }
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(8));
    }
    unreachable!("the loop returns")
}

/// One attachment of a batch, with everything it needs owned (the futures run concurrently).
struct Job {
    client: Client,
    http: reqwest::Client,
    project: String,
    launch: Uuid,
    abs: PathBuf,
    rel: String,
    size: u64,
    unreachable: Arc<AtomicBool>,
}

impl Job {
    /// `(relative path, size, attachment id or why it stays in the archive)`; an empty reason
    /// is not worth a warning of its own.
    async fn run(self, t: DirectUploadTarget) -> (String, u64, Result<Uuid, String>) {
        let r = match t.status {
            DirectUploadStatus::Exists => t.id.ok_or_else(|| "no id".to_string()),
            DirectUploadStatus::Refused => Err(format!(
                "refused ({}, limit {})",
                t.code.as_deref().unwrap_or("?"),
                t.limit.unwrap_or_default()
            )),
            DirectUploadStatus::Upload if self.unreachable.load(Ordering::Relaxed) => {
                Err(String::new())
            }
            DirectUploadStatus::Upload => match put(&self.http, &t, &self.abs, self.size).await {
                Ok(()) => match t.upload_id {
                    None => Err("no upload id".into()),
                    Some(upload) => complete(&self.client, &self.project, self.launch, upload)
                        .await
                        .map_err(|e| format!("not verified: {e:#}")),
                },
                Err(PutError::Unreachable) => {
                    self.unreachable.store(true, Ordering::Relaxed);
                    Err(String::new())
                }
                Err(PutError::Refused(code)) => Err(format!("the store answered {code}")),
                Err(PutError::File(e)) => Err(e),
            },
        };
        (self.rel, self.size, r)
    }
}

/// Uploads the candidates directly where possible.
pub async fn upload(
    client: &Client,
    project: &str,
    launch: Uuid,
    files: Vec<(PathBuf, String, u64)>,
) -> anyhow::Result<Outcome> {
    let mut out = Outcome::default();
    if files.is_empty() {
        return Ok(out);
    }
    let hashed: Vec<Hashed> =
        futures::stream::iter(files.into_iter().map(|(abs, rel, size)| async move {
            let p = abs.clone();
            let h = tokio::task::spawn_blocking(move || fastproof_blob::sha256_file(&p)).await;
            match h {
                Ok(Ok((sha256, n))) if n == size => Some(Hashed {
                    abs,
                    rel,
                    size,
                    sha256,
                }),
                _ => None,
            }
        }))
        .buffer_unordered(4)
        .filter_map(|h| async move { h })
        .collect()
        .await;
    let http = reqwest::Client::builder()
        .user_agent(format!("fastproof-cli/{}", crate::VERSION))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    let unreachable = Arc::new(AtomicBool::new(false));
    for batch in hashed.chunks(MAX_ITEMS) {
        if unreachable.load(Ordering::Relaxed) {
            break;
        }
        let items: Vec<serde_json::Value> = batch
            .iter()
            .map(|h| {
                serde_json::json!({"sha256": h.sha256, "size": h.size,
                    "contentType": content_type(&h.rel),
                    "name": h.rel.rsplit('/').next().unwrap_or(&h.rel)})
            })
            .collect();
        let answer = match client.direct_uploads(project, launch, &items).await {
            Ok(a) => a,
            Err(e) => {
                if matches!(
                    server_status(&e),
                    Some(
                        StatusCode::NOT_IMPLEMENTED
                            | StatusCode::NOT_FOUND
                            | StatusCode::METHOD_NOT_ALLOWED
                    )
                ) {
                    out.unavailable = true;
                } else {
                    out.warnings.push(format!("direct uploads not used: {e:#}"));
                }
                break;
            }
        };
        let mut jobs = Vec::with_capacity(batch.len());
        for (h, t) in batch.iter().zip(answer.items) {
            let job = Job {
                client: client.clone(),
                http: http.clone(),
                project: project.to_string(),
                launch,
                abs: h.abs.clone(),
                rel: h.rel.clone(),
                size: h.size,
                unreachable: unreachable.clone(),
            };
            jobs.push(job.run(t));
        }
        let results: Vec<_> = futures::stream::iter(jobs)
            .buffer_unordered(4)
            .collect()
            .await;
        for (rel, size, r) in results {
            match r {
                Ok(id) => {
                    out.bytes += size;
                    out.stubs.insert(rel, id);
                }
                Err(why) => {
                    if !why.is_empty() && out.warnings.len() < 20 {
                        out.warnings
                            .push(format!("{rel}: sent in the archive: {why}"));
                    }
                }
            }
        }
    }
    out.kept = hashed.len() - out.stubs.len();
    if unreachable.load(Ordering::Relaxed) {
        out.warnings.push(
            "the S3 store is not reachable from here: the remaining files were sent in the archive"
                .into(),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_skip_small_and_report_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for (name, size) in [
            ("a-result.json", 2000),
            ("b-attachment.json", 2000),
            ("video.webm", 2000),
            ("small.png", 10),
            ("junit.xml", 5000),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, vec![b'x'; size]).unwrap();
            files.push((p, name.to_string()));
        }
        let c: Vec<String> = candidates(&files, 1000)
            .into_iter()
            .map(|(_, r, _)| r)
            .collect();
        assert_eq!(c, vec!["b-attachment.json", "video.webm"]);
        assert_eq!(content_type("a/B.PNG"), "image/png");
        assert_eq!(content_type("trace.zip"), "application/zip");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
