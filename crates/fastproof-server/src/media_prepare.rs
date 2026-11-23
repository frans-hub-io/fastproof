//! The `media.prepare` job: seekable copies of video attachments (WebM with cues and duration,
//! MP4 with `moov` first, see `fastproof_media`), made after ingest and for existing videos
//! alike. A run claims a few attachments at a time (`fastproof_store::media`), downloads each
//! one to the spool directory, prepares it off the async runtime within the configured time,
//! stores the copy as `projects/<key>/derived/<aa>/<sha256>/seekable.<ext>` and records the
//! result. Content and media routes then serve the copy (`api::attachments::blob_to_serve`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fastproof_blob::BlobKey;
use fastproof_media::{Limits, Outcome};
use fastproof_store::media::{MediaJob, MediaResult};
use uuid::Uuid;

use crate::state::AppState;

/// Attachments claimed per round (one per project).
const BATCH: i64 = 4;
/// Memory one preparation may hold for its index and headers.
const MEMORY: u64 = 64 << 20;
/// First wait before an attachment is tried again after an infrastructure error; it doubles
/// with each attempt.
const RETRY_BASE: Duration = Duration::from_secs(60);

/// What a run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub ready: u64,
    pub native: u64,
    pub skipped: u64,
    pub failed: u64,
    /// Put back after an error of the spool, blob store or database.
    pub retried: u64,
}

/// How one preparation ended.
enum Done {
    /// A verdict on the file (or a copy stored), recorded for good.
    Final(MediaResult),
    /// The infrastructure failed; the file is tried again later.
    Retry(anyhow::Error),
}

/// Prepares claimed videos until none is pending or `budget` is spent (checked before every
/// video). An infrastructure error ends the run: the remaining claims are handed back.
pub async fn run(s: &AppState, budget: Duration) -> anyhow::Result<RunStats> {
    let mut stats = RunStats::default();
    if !s.cfg.media_prepare.enabled {
        return Ok(stats);
    }
    let started = Instant::now();
    let stale = s.cfg.media_prepare.timeout * 3;
    'run: while started.elapsed() < budget {
        let jobs = s.store.claim_media_jobs(BATCH, stale).await?;
        if jobs.is_empty() {
            break;
        }
        for (i, job) in jobs.iter().enumerate() {
            if started.elapsed() >= budget {
                release(s, &jobs[i..]).await?;
                break 'run;
            }
            let (done, stored) = prepare_one(s, job).await;
            let result = match done {
                Done::Retry(e) => {
                    stats.retried += 1;
                    metrics::counter!("media_prepare_total", "outcome" => "retry").increment(1);
                    tracing::warn!(attachment = %job.attachment_id, error = %format!("{e:#}"),
                        "video preparation hit an infrastructure error; it is retried later");
                    s.store
                        .retry_media_job(job.attachment_id, &format!("{e:#}"), RETRY_BASE)
                        .await?;
                    release(s, &jobs[i + 1..]).await?;
                    break 'run;
                }
                Done::Final(r) => r,
            };
            match &result {
                MediaResult::Ready { .. } => stats.ready += 1,
                MediaResult::Native => stats.native += 1,
                MediaResult::Skipped(_) => stats.skipped += 1,
                MediaResult::Failed(e) => {
                    stats.failed += 1;
                    tracing::warn!(attachment = %job.attachment_id, error = %e, "video preparation failed");
                }
            }
            metrics::counter!("media_prepare_total", "outcome" => result.status()).increment(1);
            let present = s.store.finish_media_job(job.attachment_id, &result).await?;
            let keep = present && matches!(result, MediaResult::Ready { .. });
            if !keep && let Some(key) = stored {
                s.blobs.delete(&key).await?;
            }
        }
    }
    Ok(stats)
}

async fn release(s: &AppState, jobs: &[MediaJob]) -> anyhow::Result<()> {
    let ids: Vec<Uuid> = jobs.iter().map(|j| j.attachment_id).collect();
    s.store.release_media_jobs(&ids).await
}

/// Removes spool files of preparations older than `age` (left by a process that stopped while
/// it worked; a running preparation is never older than its time limit).
pub async fn clean_spool(spool_dir: &Path, age: Duration) {
    let Ok(mut rd) = tokio::fs::read_dir(spool_dir.join("media")).await else {
        return;
    };
    while let Ok(Some(e)) = rd.next_entry().await {
        let old = e
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|el| el > age);
        if old {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
}

/// Removes the spool files when dropped.
struct Spool(Vec<PathBuf>);

impl Drop for Spool {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// How the preparation ended, and the derived blob key it may have stored (an upload cut off
/// by the time limit may still complete, so the key is reported from the moment it starts).
async fn prepare_one(s: &AppState, job: &MediaJob) -> (Done, Option<BlobKey>) {
    let cfg = &s.cfg.media_prepare;
    if job.size < 0 || job.size as u64 > cfg.max_bytes {
        return (
            Done::Final(MediaResult::Skipped(format!(
                "larger than FASTPROOF_MEDIA_PREPARE_MAX_BYTES ({} bytes)",
                cfg.max_bytes
            ))),
            None,
        );
    }
    let started = Instant::now();
    let deadline = started + cfg.timeout;
    let mut stored = None;
    let done =
        match tokio::time::timeout_at(deadline.into(), work(s, job, deadline, &mut stored)).await {
            Ok(d) => d,
            Err(_) => Done::Final(MediaResult::Failed("time limit reached".into())),
        };
    metrics::histogram!("media_prepare_seconds").record(started.elapsed().as_secs_f64());
    (done, stored)
}

/// An error of the infrastructure: the file is tried again.
fn retry(e: impl Into<anyhow::Error>) -> Done {
    Done::Retry(e.into())
}

async fn work(
    s: &AppState,
    job: &MediaJob,
    deadline: Instant,
    stored: &mut Option<BlobKey>,
) -> Done {
    let original = match BlobKey::parse(&job.blob_key) {
        Ok(k) => k,
        Err(e) => return Done::Final(MediaResult::Failed(format!("{e:#}"))),
    };
    let permits = async {
        let a = s.guards.spool().await?;
        let b = s.guards.spool().await?;
        Ok::<_, franshub_http::ApiError>((a, b))
    };
    let Ok(_permits) = permits.await else {
        return retry(anyhow::anyhow!("no spool slot free"));
    };
    let dir = s.cfg.spool_dir.join("media");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return retry(e);
    }
    let id = Uuid::now_v7();
    let input = dir.join(format!("{id}.in"));
    let output = dir.join(format!("{id}.out"));
    let _spool = Spool(vec![input.clone(), output.clone()]);
    match s.blobs.get_to_file(&original, &input).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Done::Final(MediaResult::Failed("the original blob is missing".into()));
        }
        Err(e) => return retry(e),
    }
    let limits = Limits {
        max_input_bytes: s.cfg.media_prepare.max_bytes,
        max_memory_bytes: MEMORY,
        deadline: Some(deadline),
        ..Limits::default()
    };
    let (i, o) = (input.clone(), output.clone());
    let step = tokio::task::spawn_blocking(move || -> Step {
        match fastproof_media::prepare(&i, &o, &limits) {
            Ok(Outcome::Prepared { format, .. }) if Instant::now() < deadline => {
                match fastproof_blob::sha256_file(&o) {
                    Ok((sha256, size)) => Step::Ready(format, sha256, size),
                    Err(e) => Step::Done(retry(e)),
                }
            }
            Ok(Outcome::Prepared { .. }) | Err(fastproof_media::Error::Timeout) => {
                let _ = std::fs::remove_file(&o);
                Step::Done(Done::Final(MediaResult::Failed(
                    "time limit reached".into(),
                )))
            }
            Ok(Outcome::AlreadySeekable(_)) => Step::Done(Done::Final(MediaResult::Native)),
            Ok(Outcome::Unsupported(m)) => Step::Done(Done::Final(MediaResult::Skipped(m))),
            Err(fastproof_media::Error::Io(e)) => Step::Done(retry(e)),
            Err(e) => Step::Done(Done::Final(MediaResult::Failed(e.to_string()))),
        }
    })
    .await;
    let (format, sha256, size) = match step {
        Ok(Step::Done(d)) => return d,
        Ok(Step::Ready(f, sha, n)) => (f, sha, n),
        Err(e) => {
            return Done::Final(MediaResult::Failed(format!("the preparation stopped: {e}")));
        }
    };
    let key = match BlobKey::derived(
        &job.project_key,
        &job.sha256,
        &format!("seekable.{}", format.extension()),
    ) {
        Ok(k) => k,
        Err(e) => return Done::Final(MediaResult::Failed(format!("{e:#}"))),
    };
    let r = async {
        s.store
            .media_upload_started(job.attachment_id, key.as_str())
            .await?;
        s.store.cancel_blob_deletions(&[key.to_string()]).await?;
        *stored = Some(key.clone());
        s.blobs.put_file(&key, &output).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = r {
        return retry(e);
    }
    metrics::counter!("media_prepare_bytes_total").increment(size);
    Done::Final(MediaResult::Ready {
        format: format.as_str().into(),
        blob_key: key.to_string(),
        sha256,
        size: size as i64,
    })
}

enum Step {
    Done(Done),
    Ready(fastproof_media::Format, String, u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_spool_sweep_removes_old_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("a.in"), b"x").unwrap();
        clean_spool(dir.path(), Duration::from_secs(3600)).await;
        assert!(media.join("a.in").exists());
        tokio::time::sleep(Duration::from_millis(20)).await;
        clean_spool(dir.path(), Duration::from_millis(1)).await;
        assert!(!media.join("a.in").exists());
        clean_spool(&dir.path().join("missing"), Duration::ZERO).await;
    }
}
