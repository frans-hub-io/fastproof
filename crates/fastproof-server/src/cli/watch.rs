//! `fastproof watch <dir>`: uploads result files while tests run.
//!
//! A file is sent once it is complete: its size and modification time did not change between
//! two polls (Allure temporary files `.allure-write-*` are ignored). Allure result and container
//! files go together with the attachments they reference; other attachment files wait until a result
//! references them, or until the final flush on exit.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;

use super::archive::{files_under, tar_gz_bytes};
use super::client::Client;
use super::{WatchArgs, print_launch};

/// Files per upload, at most.
pub const BATCH_FILES: usize = 500;
/// Bytes per upload, about.
pub const BATCH_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seen {
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    AllureResult,
    Report,
    Attachment,
}

/// Whether a file is never uploaded.
pub fn ignored(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.starts_with(".allure-write-")
        || (name.starts_with('.') && name.ends_with(".tmp"))
        || name.ends_with(".part")
}

fn kind(rel: &str) -> Kind {
    let lower = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if lower.ends_with("-result.json") || lower.ends_with("-container.json") {
        Kind::AllureResult
    } else if lower.contains("-attachment") {
        Kind::Attachment
    } else if lower.ends_with(".json") || lower.ends_with(".xml") || lower.ends_with(".properties")
    {
        Kind::Report
    } else {
        Kind::Attachment
    }
}

fn collect_sources(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(serde_json::Value::Array(atts)) = m.get("attachments") {
                for a in atts {
                    if let Some(s) = a.get("source").and_then(|s| s.as_str()) {
                        out.push(s.to_string());
                    }
                }
            }
            for (k, val) in m {
                if k != "attachments" {
                    collect_sources(val, out);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_sources(x, out)),
        _ => {}
    }
}

/// Attachment sources referenced by an Allure result file (relative to its directory).
pub fn referenced(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes.len() > 64 * 1024 * 1024 {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_sources(&v, &mut out);
    out
}

/// Polling state of a directory.
pub struct Watcher {
    dir: PathBuf,
    last: HashMap<String, Seen>,
    sent: HashMap<String, Seen>,
}

impl Watcher {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            last: HashMap::new(),
            sent: HashMap::new(),
        }
    }

    /// Files ready to send now, grouped into batches. With `flush`, every file not sent yet is
    /// ready (on exit).
    pub fn poll(&mut self, flush: bool) -> anyhow::Result<Vec<Vec<(PathBuf, String)>>> {
        let files = files_under(&self.dir, 1_000_000)
            .with_context(|| format!("reading {}", self.dir.display()))?;
        let mut now: HashMap<String, (PathBuf, Seen)> = HashMap::new();
        for (abs, rel) in files {
            if ignored(&rel) {
                continue;
            }
            let Ok(m) = std::fs::metadata(&abs) else {
                continue;
            };
            now.insert(
                rel,
                (
                    abs,
                    Seen {
                        size: m.len(),
                        modified: m.modified().ok(),
                    },
                ),
            );
        }
        let stable: HashSet<&String> = now
            .iter()
            .filter(|(rel, (_, seen))| flush || self.last.get(*rel) == Some(seen))
            .filter(|(rel, (_, seen))| self.sent.get(*rel) != Some(seen))
            .map(|(rel, _)| rel)
            .collect();
        let mut ready: Vec<String> = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut sorted: Vec<&String> = stable.iter().copied().collect();
        sorted.sort();
        for rel in &sorted {
            match kind(rel) {
                Kind::Report => {
                    if taken.insert((*rel).clone()) {
                        ready.push((*rel).clone());
                    }
                }
                Kind::AllureResult => {
                    let dir = rel
                        .rsplit_once('/')
                        .map(|(d, _)| format!("{d}/"))
                        .unwrap_or_default();
                    let refs = referenced(&now[*rel].0);
                    let refs: Vec<String> = refs
                        .into_iter()
                        .filter(|s| !s.contains("..") && !s.starts_with('/'))
                        .map(|s| format!("{dir}{s}"))
                        .collect();
                    let unstable = refs.iter().any(|r| {
                        now.contains_key(r)
                            && !stable.contains(r)
                            && self.sent.get(r) != now.get(r).map(|x| &x.1)
                    });
                    if unstable && !flush {
                        continue;
                    }
                    if taken.insert((*rel).clone()) {
                        ready.push((*rel).clone());
                    }
                    for r in refs {
                        if stable.contains(&r) && taken.insert(r.clone()) {
                            ready.push(r);
                        }
                    }
                }
                Kind::Attachment => {
                    if flush && taken.insert((*rel).clone()) {
                        ready.push((*rel).clone());
                    }
                }
            }
        }
        self.last = now.iter().map(|(k, v)| (k.clone(), v.1)).collect();
        let mut batches = Vec::new();
        let mut cur: Vec<(PathBuf, String)> = Vec::new();
        let mut bytes = 0u64;
        for rel in ready {
            let (abs, seen) = &now[&rel];
            if !cur.is_empty()
                && (cur.len() >= BATCH_FILES || bytes + seen.size > BATCH_BYTES)
                && kind(&rel) != Kind::Attachment
            {
                batches.push(std::mem::take(&mut cur));
                bytes = 0;
            }
            bytes += seen.size;
            cur.push((abs.clone(), rel.clone()));
            self.sent.insert(rel, *seen);
        }
        if !cur.is_empty() {
            batches.push(cur);
        }
        Ok(batches)
    }

    /// Files sent so far.
    pub fn sent_count(&self) -> usize {
        self.sent.len()
    }
}

async fn send(
    client: &Client,
    a: &WatchArgs,
    launch: uuid::Uuid,
    batch: Vec<(PathBuf, String)>,
) -> anyhow::Result<uuid::Uuid> {
    let n = batch.len();
    let bytes = tokio::task::spawn_blocking(move || tar_gz_bytes(&batch)).await??;
    let mut delay = Duration::from_millis(500);
    for attempt in 0..5 {
        match client
            .upload(
                &a.launch.conn.project,
                launch,
                &a.format,
                None,
                reqwest::Body::from(bytes.clone()),
            )
            .await
        {
            Ok(u) => {
                eprintln!("uploaded {n} files ({} bytes) as {}", u.size_bytes, u.id);
                return Ok(u.id);
            }
            Err(e)
                if attempt < 4
                    && e.downcast_ref::<super::client::ServerError>()
                        .is_none_or(|s| s.status.is_server_error()) =>
            {
                eprintln!("upload failed ({e:#}); retrying");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns")
}

async fn signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        let t = async {
            match term.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = t => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Runs `watch` until a signal (or `--idle-exit`), then flushes and optionally finishes.
pub async fn run(a: WatchArgs) -> anyhow::Result<i32> {
    let client = a.launch.conn.client()?;
    if !a.dir.is_dir() {
        tokio::fs::create_dir_all(&a.dir)
            .await
            .with_context(|| format!("creating {}", a.dir.display()))?;
    }
    let launch = a.launch.resolve(&client).await?;
    eprintln!(
        "watching {} into launch #{} {} ({})",
        a.dir.display(),
        launch.number,
        launch.name,
        launch.id
    );
    let mut w = Watcher::new(a.dir.clone());
    let interval = Duration::from_millis(a.interval_ms.max(100));
    let mut last_activity = Instant::now();
    let mut uploads = Vec::new();
    let stop = signal();
    tokio::pin!(stop);
    loop {
        for b in w.poll(false)? {
            uploads.push(send(&client, &a, launch.id, b).await?);
            last_activity = Instant::now();
        }
        if a.idle_exit > 0 && last_activity.elapsed() > Duration::from_secs(a.idle_exit) {
            break;
        }
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
    tokio::time::sleep(interval.min(Duration::from_millis(500))).await;
    for b in w.poll(true)? {
        uploads.push(send(&client, &a, launch.id, b).await?);
    }
    eprintln!("sent {} files in {} uploads", w.sent_count(), uploads.len());
    if a.finish {
        if !uploads.is_empty() {
            let done = client
                .wait_uploads(
                    &a.launch.conn.project,
                    launch.id,
                    &uploads,
                    Duration::from_secs(600),
                )
                .await?;
            for d in done.iter().filter(|d| d.error.is_some()) {
                eprintln!("upload {} failed: {:?}", d.id, d.error);
            }
        }
        let l = client
            .finish_launch(&a.launch.conn.project, launch.id, "finished")
            .await?;
        print_launch(&l);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allure_results_go_with_their_attachments() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::fs::write(p.join("a-attachment.txt"), "log").unwrap();
        std::fs::write(p.join("orphan.png"), "png").unwrap();
        std::fs::write(p.join(".allure-write-1.tmp"), "x").unwrap();
        std::fs::write(
            p.join("r1-result.json"),
            r#"{"uuid":"r1","steps":[{"attachments":[{"source":"a-attachment.txt"}]}]}"#,
        )
        .unwrap();
        let mut w = Watcher::new(p.to_path_buf());
        assert!(
            w.poll(false).unwrap().is_empty(),
            "nothing is stable on the first poll"
        );
        let b = w.poll(false).unwrap();
        let names: Vec<&str> = b.iter().flatten().map(|(_, r)| r.as_str()).collect();
        assert_eq!(names, vec!["r1-result.json", "a-attachment.txt"]);
        assert!(
            w.poll(false).unwrap().is_empty(),
            "sent files are not sent again"
        );
        std::fs::write(p.join("c1-container.json"), "{}").unwrap();
        assert!(w.poll(false).unwrap().is_empty());
        let b = w.poll(false).unwrap();
        assert_eq!(b[0][0].1, "c1-container.json");
        let b = w.poll(true).unwrap();
        let names: Vec<&str> = b.iter().flatten().map(|(_, r)| r.as_str()).collect();
        assert_eq!(
            names,
            vec!["orphan.png"],
            "unreferenced files go on the final flush"
        );
        assert!(ignored(".allure-write-abc.tmp"));
    }
}
