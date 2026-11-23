//! The `media.prepare` job: seekable copies of video attachments (WebM with cues and duration,
//! MP4 with `moov` first, see `fastproof_media`), made after ingest and for existing videos
//! alike. A run claims a few attachments at a time (`fastproof_store::media`), downloads each
//! one to the spool directory, prepares it off the async runtime within the configured time,
//! stores the copy as `projects/<key>/derived/<aa>/<sha256>/seekable.<ext>` and records the
//! result. Content and media routes then serve the copy (`api::attachments::blob_to_serve`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastproof_blob::BlobKey;
use fastproof_media::{Limits, Outcome};
use fastproof_store::media::{MediaJob, MediaResult};
use uuid::Uuid;

use crate::state::AppState;

/// Attachments claimed per round.
const BATCH: i64 = 4;
/// Memory one preparation may hold for its index and headers.
const MEMORY: u64 = 64 << 20;

/// What a run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    pub ready: u64,
    pub native: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Prepares claimed videos until none is pending or `budget` is spent.
pub async fn run(s: &AppState, budget: Duration) -> anyhow::Result<RunStats> {
    let mut stats = RunStats::default();
    if !s.cfg.media_prepare.enabled {
        return Ok(stats);
    }
    let started = Instant::now();
    let stale = s.cfg.media_prepare.timeout * 3;
    while started.elapsed() < budget {
        let jobs = s.store.claim_media_jobs(BATCH, stale).await?;
        if jobs.is_empty() {
            break;
        }
        for job in jobs {
            let result = prepare_one(s, &job).await;
            match &result.0 {
                MediaResult::Ready { .. } => stats.ready += 1,
                MediaResult::Native => stats.native += 1,
                MediaResult::Skipped(_) => stats.skipped += 1,
                MediaResult::Failed(e) => {
                    stats.failed += 1;
                    tracing::warn!(attachment = %job.attachment_id, error = %e, "video preparation failed");
                }
            }
            metrics::counter!("media_prepare_total", "outcome" => result.0.status()).increment(1);
            let present = s
                .store
                .finish_media_job(job.attachment_id, &result.0)
                .await?;
            if !present && let Some(key) = result.1 {
                s.blobs.delete(&key).await?;
            }
        }
    }
    Ok(stats)
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

/// The result, and the derived blob it stored.
async fn prepare_one(s: &AppState, job: &MediaJob) -> (MediaResult, Option<BlobKey>) {
    let cfg = &s.cfg.media_prepare;
    if job.size < 0 || job.size as u64 > cfg.max_bytes {
        return (
            MediaResult::Skipped(format!(
                "larger than FASTPROOF_MEDIA_PREPARE_MAX_BYTES ({} bytes)",
                cfg.max_bytes
            )),
            None,
        );
    }
    let started = Instant::now();
    let deadline = started + cfg.timeout;
    let r = match tokio::time::timeout_at(deadline.into(), work(s, job, deadline)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => (MediaResult::Failed(format!("{e:#}")), None),
        Err(_) => (MediaResult::Failed("time limit reached".into()), None),
    };
    metrics::histogram!("media_prepare_seconds").record(started.elapsed().as_secs_f64());
    r
}

async fn work(
    s: &AppState,
    job: &MediaJob,
    deadline: Instant,
) -> anyhow::Result<(MediaResult, Option<BlobKey>)> {
    let original = BlobKey::parse(&job.blob_key)?;
    let _permit = s
        .guards
        .spool()
        .await
        .map_err(|_| anyhow::anyhow!("no spool slot free"))?;
    let dir = s.cfg.spool_dir.join("media");
    tokio::fs::create_dir_all(&dir).await?;
    let id = Uuid::now_v7();
    let input = dir.join(format!("{id}.in"));
    let output = dir.join(format!("{id}.out"));
    let _spool = Spool(vec![input.clone(), output.clone()]);
    if s.blobs.get_to_file(&original, &input).await?.is_none() {
        return Ok((
            MediaResult::Failed("the original blob is missing".into()),
            None,
        ));
    }
    let limits = Limits {
        max_input_bytes: s.cfg.media_prepare.max_bytes,
        max_memory_bytes: MEMORY,
        deadline: Some(deadline),
        ..Limits::default()
    };
    let (i, o) = (input.clone(), output.clone());
    let step = tokio::task::spawn_blocking(move || -> anyhow::Result<Step> {
        Ok(match fastproof_media::prepare(&i, &o, &limits) {
            Ok(Outcome::Prepared { format, .. }) if Instant::now() < deadline => {
                let (sha256, size) = fastproof_blob::sha256_file(&o)?;
                Step::Ready(format, sha256, size)
            }
            Ok(Outcome::Prepared { .. }) | Err(fastproof_media::Error::Timeout) => {
                let _ = std::fs::remove_file(&o);
                Step::Done(MediaResult::Failed("time limit reached".into()))
            }
            Ok(Outcome::AlreadySeekable(_)) => Step::Done(MediaResult::Native),
            Ok(Outcome::Unsupported(m)) => Step::Done(MediaResult::Skipped(m)),
            Err(e) => Step::Done(MediaResult::Failed(e.to_string())),
        })
    })
    .await??;
    let (format, sha256, size) = match step {
        Step::Done(r) => return Ok((r, None)),
        Step::Ready(f, sha, n) => (f, sha, n),
    };
    let key = BlobKey::derived(
        &job.project_key,
        &job.sha256,
        &format!("seekable.{}", format.extension()),
    )?;
    s.store.cancel_blob_deletions(&[key.to_string()]).await?;
    s.blobs.put_file(&key, &output).await?;
    metrics::counter!("media_prepare_bytes_total").increment(size);
    Ok((
        MediaResult::Ready {
            format: format.as_str().into(),
            blob_key: key.to_string(),
            sha256,
            size: size as i64,
        },
        Some(key),
    ))
}

enum Step {
    Done(MediaResult),
    Ready(fastproof_media::Format, String, u64),
}
