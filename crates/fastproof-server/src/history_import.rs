//! Import of Allure history: a background job reads the uploaded history files, turns every
//! past report into a launch and keeps the progress in `history_imports`. The files are bounded
//! (see `fastproof_ingest::allure_history::HistoryLimits`) and stay in blob storage until the
//! job ends.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use fastproof_blob::BlobKey;
use fastproof_ingest::allure_history::{
    Allure2Files, Allure3Reader, HistoryLimits, HistoryReport, parse_allure2,
};
use fastproof_store::history_import::{ClaimedImport, ImportFile, ImportTarget, PointOutcome};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::state::AppState;

/// How long one import may run in all; a stuck one (a lock wait) is failed then.
const JOB_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3 * 3600);

#[derive(Debug)]
struct JobTimeout;

impl std::fmt::Display for JobTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the import took too long")
    }
}

impl std::error::Error for JobTimeout {}

/// Audits how an import ended (`history.import.done` or `history.import.failed`).
async fn audit_end(s: &AppState, id: Uuid) {
    let Ok(Some((i, project))) = s.store.history_import_with_project(id).await else {
        return;
    };
    let action = match i.status.as_str() {
        "done" => "history.import.done",
        "failed" => "history.import.failed",
        _ => return,
    };
    let mut e = franshub_access::AuditEvent::anonymous(
        &i.created_by,
        Some("system"),
        action,
        serde_json::json!({"importId": id, "pointsDone": i.points_done,
            "launchesCreated": i.launches_created, "resultsImported": i.results_imported,
            "resultsUnmatched": i.results_unmatched, "resultsSkipped": i.results_skipped,
            "error": i.error}),
    );
    e.scope = Some(project);
    s.platform.audit(e).await;
}

/// Imports a replica runs at once.
const WORKERS: usize = 1;
/// Seconds between heartbeats of a running job.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);
/// Warnings kept per import.
const MAX_WARNINGS: usize = 30;

/// The file names an import accepts (multipart field, blob name).
pub const FIELDS: [(&str, &str); 5] = [
    ("history", "history"),
    ("historyTrend", "history-trend"),
    ("durationTrend", "duration-trend"),
    ("categoriesTrend", "categories-trend"),
    ("retryTrend", "retry-trend"),
];

/// Bytes a file of an import may have.
pub fn max_file_bytes(source: &str, blob_name: &str, l: &HistoryLimits) -> u64 {
    match (source, blob_name) {
        ("allure3", _) => l.max_total_bytes,
        (_, "history") => l.max_history_bytes,
        _ => l.max_trend_bytes,
    }
}

fn workers(instance: &str) -> Arc<Semaphore> {
    static ALL: Mutex<Option<std::collections::HashMap<String, Arc<Semaphore>>>> = Mutex::new(None);
    let mut g = ALL.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Default::default)
        .entry(instance.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(WORKERS)))
        .clone()
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Removes what a crashed process left in the import spool.
pub fn sweep_spool(spool_dir: &Path) {
    for d in ["history-imports", "history-imports-incoming"] {
        let _ = std::fs::remove_dir_all(spool_dir.join(d));
    }
}

/// Runs a queued import on this replica when a worker slot frees up.
pub fn spawn_job(s: &AppState, id: Uuid) {
    let s = s.clone();
    tokio::spawn(async move {
        let Ok(_permit) = workers(&s.instance).acquire_owned().await else {
            return;
        };
        let ran = match tokio::time::timeout(JOB_DEADLINE, run_job(&s, id)).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(JobTimeout)),
        };
        if let Err(e) = ran {
            tracing::warn!(job = %id, error = %format!("{e:#}"), "history import failed");
            let msg = match e.downcast_ref::<fastproof_ingest::IngestError>() {
                Some(ie) => ie.to_string(),
                None if e.is::<JobTimeout>() => "the import took too long".to_string(),
                None => "the import failed".to_string(),
            };
            let _ = s.store.fail_history_import(id, &msg).await;
        }
        audit_end(&s, id).await;
    });
}

struct Beat(tokio::task::JoinHandle<()>);

impl Beat {
    fn start(s: &AppState, id: Uuid) -> Self {
        let s = s.clone();
        Self(tokio::spawn(async move {
            let mut t = tokio::time::interval(HEARTBEAT);
            t.tick().await;
            loop {
                t.tick().await;
                let _ = s.store.touch_history_import(id).await;
            }
        }))
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Progress {
    total: i64,
    done: i64,
    launches: i64,
    sums: PointOutcome,
    warnings: Vec<String>,
}

impl Progress {
    fn warn(&mut self, m: String) {
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&m) {
            self.warnings.push(m);
        }
    }

    fn absorb(&mut self, r: &HistoryReport) {
        for w in &r.warnings {
            self.warn(w.clone());
        }
    }

    async fn save(&self, s: &AppState, id: Uuid) -> anyhow::Result<bool> {
        s.store
            .progress_history_import(
                id,
                self.total,
                self.done,
                &self.sums,
                self.launches,
                &self.warnings,
            )
            .await
    }
}

async fn run_job(s: &AppState, id: Uuid) -> anyhow::Result<()> {
    let Some(job) = s.store.start_history_import(id).await? else {
        return Ok(());
    };
    let project = s
        .store
        .project(&job.project_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the project is gone"))?;
    let settings = s.store.project_settings(project.id).await?;
    let target = ImportTarget {
        source: if job.import.source == "allure3" {
            "allure3"
        } else {
            "allure2"
        },
        branch: job.import.branch.clone(),
        environment: job.import.environment.clone(),
        created_by: job.import.created_by.clone(),
        not_before: Utc::now()
            - Duration::days(
                i64::from(settings.retention.raw_days)
                    .min(fastproof_store::history_import::MAX_IMPORT_AGE_DAYS),
            ),
    };
    let dir = TempDir(s.cfg.spool_dir.join("history-imports").join(id.to_string()));
    std::fs::create_dir_all(&dir.0)?;
    let _beat = Beat::start(s, id);
    let limits = HistoryLimits::default();
    let mut progress = Progress {
        total: 0,
        done: 0,
        launches: 0,
        sums: PointOutcome::default(),
        warnings: Vec::new(),
    };
    if target.source == "allure3" {
        run_allure3(s, &job, &dir.0, &target, &limits, &mut progress).await?;
    } else {
        run_allure2(s, &job, &dir.0, &target, &limits, &mut progress).await?;
    }
    if progress.total == 0 {
        anyhow::bail!(fastproof_ingest::IngestError::Unsupported(
            "the files hold no past reports".into()
        ));
    }
    if progress.sums.skipped > 0 {
        let days = settings.retention.raw_days;
        progress.warn(format!(
            "{} results were left out: their report is older than the retention window ({days} days), lies in the future or has no usable data",
            progress.sums.skipped
        ));
    }
    if progress.sums.unmatched > 0 {
        progress.warn(format!(
            "{} results belong to tests this project does not know and the history does not name them; import after the first live run to join them",
            progress.sums.unmatched
        ));
    }
    progress.save(s, id).await?;
    s.store.finish_history_import(id).await?;
    Ok(())
}

async fn fetch(s: &AppState, dir: &Path, f: &ImportFile) -> anyhow::Result<PathBuf> {
    let path = dir.join(&f.name);
    let key = BlobKey::parse(&f.blob_key)?;
    s.blobs
        .get_to_file(&key, &path)
        .await?
        .ok_or_else(|| anyhow::anyhow!("a file of the import is gone"))?;
    Ok(path)
}

fn file<'a>(files: &'a [ImportFile], name: &str) -> Option<&'a ImportFile> {
    files.iter().find(|f| f.name == name)
}

async fn run_allure2(
    s: &AppState,
    job: &ClaimedImport,
    dir: &Path,
    target: &ImportTarget,
    limits: &HistoryLimits,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let mut data: Vec<(&str, Option<Vec<u8>>)> = Vec::new();
    for name in [
        "history",
        "history-trend",
        "duration-trend",
        "categories-trend",
        "retry-trend",
    ] {
        match file(&job.files, name) {
            Some(f) => {
                let p = fetch(s, dir, f).await?;
                let bytes = tokio::fs::read(&p).await?;
                let _ = tokio::fs::remove_file(&p).await;
                data.push((name, Some(bytes)));
            }
            None => data.push((name, None)),
        }
    }
    let l = limits.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        let get = |n: &str| {
            data.iter()
                .find(|(k, _)| *k == n)
                .and_then(|(_, v)| v.as_deref())
        };
        parse_allure2(
            &Allure2Files {
                history: get("history").unwrap_or_default(),
                history_trend: get("history-trend"),
                duration_trend: get("duration-trend"),
                categories_trend: get("categories-trend"),
                retry_trend: get("retry-trend"),
            },
            &l,
        )
    })
    .await??;
    progress.absorb(&parsed.report);
    progress.sums.skipped += parsed.report.skipped_results;
    progress.total = parsed.points.len() as i64;
    let project = s
        .store
        .project(&job.project_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the project is gone"))?;
    for point in &parsed.points {
        let o = s
            .store
            .import_history_point(&project, target, point, Utc::now())
            .await?;
        progress.done += 1;
        progress.launches += i64::from(o.launch_created);
        progress.sums.add(o);
        if !progress.save(s, job.import.id).await? {
            anyhow::bail!("the import was stopped");
        }
    }
    Ok(())
}

async fn run_allure3(
    s: &AppState,
    job: &ClaimedImport,
    dir: &Path,
    target: &ImportTarget,
    limits: &HistoryLimits,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let f = file(&job.files, "history").ok_or_else(|| anyhow::anyhow!("no history file"))?;
    let path = fetch(s, dir, f).await?;
    let counted = path.clone();
    progress.total = tokio::task::spawn_blocking(move || -> std::io::Result<i64> {
        use std::io::BufRead;
        let mut n = 0;
        let mut last = b'\n';
        let mut r = BufReader::new(std::fs::File::open(counted)?);
        loop {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                return Ok(n + i64::from(last != b'\n'));
            }
            n += buf.iter().filter(|b| **b == b'\n').count() as i64;
            last = buf[buf.len() - 1];
            let len = buf.len();
            r.consume(len);
        }
    })
    .await??;
    let project = s
        .store
        .project(&job.project_key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the project is gone"))?;
    let mut reader =
        Allure3Reader::new(BufReader::new(std::fs::File::open(&path)?), limits.clone());
    loop {
        let (r, next) = tokio::task::spawn_blocking(move || {
            let n = reader.next_point();
            (reader, n)
        })
        .await?;
        reader = r;
        let Some(point) = next? else { break };
        let o = s
            .store
            .import_history_point(&project, target, &point, Utc::now())
            .await?;
        progress.done += 1;
        progress.launches += i64::from(o.launch_created);
        progress.sums.add(o);
        progress.absorb(&reader.report);
        if !progress.save(s, job.import.id).await? {
            anyhow::bail!("the import was stopped");
        }
    }
    progress.absorb(&reader.report);
    progress.sums.skipped += reader.report.skipped_results;
    progress.total = progress.done.max(0);
    Ok(())
}

/// The scheduler job: fails lost imports, deletes the files of ended ones and old rows.
pub async fn tick(s: &AppState) -> anyhow::Result<()> {
    s.store.fail_stale_history_imports().await?;
    for (id, files) in s.store.history_import_leftovers(100).await? {
        let mut all = true;
        for f in files {
            if let Ok(k) = BlobKey::parse(&f.blob_key)
                && let Err(e) = s.blobs.delete(&k).await
            {
                tracing::warn!(job = %id, error = %format!("{e:#}"), "deleting a file of a history import failed");
                all = false;
            }
        }
        if all {
            s.store.forget_history_import_files(id).await?;
        }
    }
    s.store.delete_old_history_imports().await?;
    Ok(())
}
