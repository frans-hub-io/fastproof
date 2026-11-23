//! Seekable copies of video attachments. A new attachment of a video type is pending
//! (`attachments.media_state = 0`, served by the partial index `attachments_media_pending`); the
//! `media.prepare` job claims it into `attachment_media` (`running`) and records the result:
//! `ready` (a derived blob is served instead of the original), `native` (the original already
//! seeks well), `skipped` (not handled, such as too large) or `failed` (the file cannot be
//! prepared; final). An error of the spool, blob store or database is not a verdict on the
//! file: the claim stays `running` and is retried with backoff, like a claim whose runner died,
//! at most [`MAX_ATTEMPTS`] times in all.
//! Deleting an attachment removes its row (cascade) and queues the derived blob in
//! `blob_deletions` together with the original.

use std::time::Duration;

use sqlx::Row;
use uuid::Uuid;

use crate::{Store, no_nul};

/// Content types that are prepared; the same list is the predicate of the partial index.
pub const VIDEO_TYPES: [&str; 4] = [
    "video/webm",
    "video/mp4",
    "video/quicktime",
    "video/x-matroska",
];

/// Claims of one attachment, counting interrupted runs and infrastructure errors.
pub const MAX_ATTEMPTS: i32 = 5;

/// An attachment claimed for preparation.
#[derive(Clone, Debug)]
pub struct MediaJob {
    pub attachment_id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub blob_key: String,
    pub attempt: i32,
}

/// How a preparation ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaResult {
    Ready {
        format: String,
        blob_key: String,
        sha256: String,
        size: i64,
    },
    Native,
    Skipped(String),
    Failed(String),
}

impl MediaResult {
    pub fn status(&self) -> &'static str {
        match self {
            MediaResult::Ready { .. } => "ready",
            MediaResult::Native => "native",
            MediaResult::Skipped(_) => "skipped",
            MediaResult::Failed(_) => "failed",
        }
    }
}

/// A ready seekable copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyMedia {
    pub blob_key: String,
    pub sha256: String,
    pub size: i64,
}

/// The recorded state of one attachment's preparation.
#[derive(Clone, Debug)]
pub struct MediaState {
    pub status: String,
    pub attempts: i32,
    pub format: Option<String>,
    pub blob_key: Option<String>,
    pub sha256: Option<String>,
    pub size: Option<i64>,
    pub error: Option<String>,
}

/// Running claims that are due again: a retry whose time came, or a run that stopped
/// (no retry time, started more than `$1` seconds ago).
const DUE: &str = "status = 'running' and ((retry_at is not null and retry_at <= now()) \
     or (retry_at is null and started_at < now() - make_interval(secs => $1)))";

/// New claims, one per project: the oldest pending video of each project, projects served
/// longest ago first, so one project's backlog does not hold back the others.
const CLAIM_NEW: &str = "with pending as (select x.id from projects p \
       cross join lateral (select a.id, a.created_at from attachments a \
         where a.project_id = p.id and a.media_state = 0 \
           and a.content_type in ('video/webm', 'video/mp4', 'video/quicktime', 'video/x-matroska') \
         order by a.created_at limit 1) x \
     where p.deleted_at is null \
     order by (select max(m.started_at) from attachment_media m where m.project_id = p.id) nulls first, \
       x.created_at \
     limit $1), \
   locked as (select a.id from attachments a join pending on pending.id = a.id \
     where a.media_state = 0 for update of a skip locked), \
   marked as (update attachments a set media_state = 1 from locked where a.id = locked.id \
     returning a.id, a.project_id) \
 insert into attachment_media (attachment_id, project_id, status, attempts, started_at) \
 select id, project_id, 'running', 1, now() from marked \
 on conflict (attachment_id) do nothing \
 returning attachment_id";

impl Store {
    /// Claims up to `batch` attachments to prepare: due retries and claims whose runner stopped
    /// more than `stale` ago (again, while under [`MAX_ATTEMPTS`]; else they fail), then new
    /// videos, one per project.
    pub async fn claim_media_jobs(
        &self,
        batch: i64,
        stale: Duration,
    ) -> anyhow::Result<Vec<MediaJob>> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update attachment_media set status = 'failed', finished_at = now(), retry_at = null, \
               error = 'gave up after ' || attempts || ' attempts: ' \
                 || coalesce(error, 'the preparation was interrupted') \
             where {DUE} and attempts >= $2"
        )))
        .bind(stale.as_secs_f64())
        .bind(MAX_ATTEMPTS)
        .execute(&self.pool)
        .await?;
        let mut ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "update attachment_media set attempts = attempts + 1, started_at = now(), retry_at = null \
             where attachment_id in (select attachment_id from attachment_media \
               where {DUE} order by started_at limit $2 for update skip locked) \
             returning attachment_id"
        )))
        .bind(stale.as_secs_f64())
        .bind(batch)
        .fetch_all(&self.pool)
        .await?;
        let left = batch - ids.len() as i64;
        if left > 0 {
            ids.extend(
                sqlx::query_scalar::<_, Uuid>(CLAIM_NEW)
                    .bind(left)
                    .fetch_all(&self.pool)
                    .await?,
            );
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select a.id, a.project_id, p.key, a.sha256, a.size, a.content_type, a.blob_key, m.attempts \
             from attachment_media m join attachments a on a.id = m.attachment_id \
               join projects p on p.id = a.project_id \
             where m.attachment_id = any($1) order by a.created_at",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MediaJob {
                    attachment_id: r.try_get("id")?,
                    project_id: r.try_get("project_id")?,
                    project_key: r.try_get("key")?,
                    sha256: r.try_get("sha256")?,
                    size: r.try_get("size")?,
                    content_type: r.try_get("content_type")?,
                    blob_key: r.try_get("blob_key")?,
                    attempt: r.try_get("attempts")?,
                })
            })
            .collect()
    }

    /// Records how a claimed preparation ended. Returns false when the attachment is gone
    /// (collected meanwhile) or its project is deleted: its derived blob, if one was stored, is
    /// then the caller's to delete. The attachment and project rows are locked first, so neither
    /// the collector nor a project deletion can slip between this check and the update and miss
    /// the derived blob. A key recorded by [`Store::media_upload_started`] stays on the row
    /// whatever the result, so collecting the attachment queues it.
    pub async fn finish_media_job(
        &self,
        attachment_id: Uuid,
        result: &MediaResult,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let present: Option<Uuid> = sqlx::query_scalar(
            "select a.id from attachments a join projects p on p.id = a.project_id \
             where a.id = $1 and p.deleted_at is null for share of a, p",
        )
        .bind(attachment_id)
        .fetch_optional(&mut *tx)
        .await?;
        if present.is_none() {
            tx.rollback().await?;
            return Ok(false);
        }
        let (format, key, sha, size, error) = match result {
            MediaResult::Ready {
                format,
                blob_key,
                sha256,
                size,
            } => (
                Some(format.as_str()),
                Some(blob_key.as_str()),
                Some(sha256.as_str()),
                Some(*size),
                None,
            ),
            MediaResult::Native => (None, None, None, None, None),
            MediaResult::Skipped(m) | MediaResult::Failed(m) => {
                (None, None, None, None, Some(no_nul(&truncate(m, 500))))
            }
        };
        sqlx::query(
            "update attachment_media set status = $2, format = $3, blob_key = coalesce($4, blob_key), \
               sha256 = $5, size = $6, error = $7, finished_at = now(), retry_at = null \
             where attachment_id = $1",
        )
        .bind(attachment_id)
        .bind(result.status())
        .bind(format)
        .bind(key)
        .bind(sha)
        .bind(size)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Records the derived blob key before it is uploaded, so a stored copy is never left without
    /// a row naming it (an upload cut off by the time limit may still complete).
    pub async fn media_upload_started(
        &self,
        attachment_id: Uuid,
        blob_key: &str,
    ) -> anyhow::Result<()> {
        sqlx::query("update attachment_media set blob_key = $2 where attachment_id = $1")
            .bind(attachment_id)
            .bind(blob_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Puts a claim back after an error of the infrastructure (spool, blob store, database):
    /// it stays `running` and is claimed again after `base * 2^(attempts-1)` (at most an hour);
    /// after [`MAX_ATTEMPTS`] it fails with the last error.
    pub async fn retry_media_job(
        &self,
        attachment_id: Uuid,
        error: &str,
        base: Duration,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update attachment_media set error = $2, retry_at = now() + make_interval(secs => \
               least($3 * power(2, greatest(attempts - 1, 0)), 3600)) \
             where attachment_id = $1 and status = 'running'",
        )
        .bind(attachment_id)
        .bind(no_nul(&truncate(error, 500)))
        .bind(base.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Hands back claims that were not worked on: due at once, the attempt not counted.
    pub async fn release_media_jobs(&self, attachment_ids: &[Uuid]) -> anyhow::Result<()> {
        if attachment_ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "update attachment_media set attempts = greatest(attempts - 1, 0), retry_at = now() \
             where attachment_id = any($1) and status = 'running'",
        )
        .bind(attachment_ids)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The ready seekable copy of the project's attachment with this SHA-256.
    pub async fn ready_media(
        &self,
        project_id: Uuid,
        sha256: &str,
    ) -> anyhow::Result<Option<ReadyMedia>> {
        let r = sqlx::query(
            "select m.blob_key, m.sha256, m.size from attachments a \
               join attachment_media m on m.attachment_id = a.id \
             where a.project_id = $1 and a.sha256 = $2 and m.status = 'ready' \
               and m.blob_key is not null and m.sha256 is not null and m.size is not null",
        )
        .bind(project_id)
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(ReadyMedia {
            blob_key: r.try_get("blob_key")?,
            sha256: r.try_get("sha256")?,
            size: r.try_get("size")?,
        }))
    }

    /// The recorded preparation of the project's attachment with this SHA-256.
    pub async fn media_state(
        &self,
        project_id: Uuid,
        sha256: &str,
    ) -> anyhow::Result<Option<MediaState>> {
        let r = sqlx::query(
            "select m.status, m.attempts, m.format, m.blob_key, m.sha256, m.size, m.error \
             from attachments a join attachment_media m on m.attachment_id = a.id \
             where a.project_id = $1 and a.sha256 = $2",
        )
        .bind(project_id)
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(MediaState {
            status: r.try_get("status")?,
            attempts: r.try_get("attempts")?,
            format: r.try_get("format")?,
            blob_key: r.try_get("blob_key")?,
            sha256: r.try_get("sha256")?,
            size: r.try_get("size")?,
            error: r.try_get("error")?,
        }))
    }

    /// Video attachments not looked at yet (all projects).
    pub async fn pending_media_count(&self) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select count(*) from attachments where media_state = 0 \
               and content_type in ('video/webm', 'video/mp4', 'video/quicktime', 'video/x-matroska')",
        )
        .fetch_one(&self.pool)
        .await?)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
