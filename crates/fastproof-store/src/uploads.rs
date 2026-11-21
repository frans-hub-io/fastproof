//! Uploads and the ingest queue (`FOR UPDATE SKIP LOCKED`, leases with heartbeat, retries).

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{JOBS_CHANNEL, Store, Upload, UploadError, UploadItemCounts, json_no_nul, no_nul};

const UPLOAD_COLUMNS: &str =
    "id, launch_id, format, status, size_bytes, counts, warnings, error, created_at, finished_at";

pub(crate) fn upload_row(r: &PgRow) -> Result<Upload, sqlx::Error> {
    let counts: serde_json::Value = r.try_get("counts")?;
    let warnings: serde_json::Value = r.try_get("warnings")?;
    let error: Option<serde_json::Value> = r.try_get("error")?;
    Ok(Upload {
        id: r.try_get("id")?,
        launch_id: r.try_get("launch_id")?,
        format: r.try_get("format")?,
        status: r.try_get("status")?,
        size_bytes: r.try_get("size_bytes")?,
        counts: serde_json::from_value::<UploadItemCounts>(counts).unwrap_or_default(),
        warnings: serde_json::from_value(warnings).unwrap_or_default(),
        error: error.and_then(|e| serde_json::from_value(e).ok()),
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
    })
}

/// A new upload whose raw body is already in blob storage.
#[derive(Clone, Debug)]
pub struct NewUpload {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Uuid,
    pub format: String,
    pub file_name: Option<String>,
    pub size_bytes: i64,
    pub sha256: String,
    pub blob_key: String,
    pub created_by: String,
    pub max_attempts: i32,
}

/// A job claimed by a worker.
#[derive(Clone, Debug)]
pub struct ClaimedJob {
    pub job_id: Uuid,
    pub upload_id: Uuid,
    pub attempt: i32,
    pub max_attempts: i32,
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Uuid,
    pub launch_created_at: DateTime<Utc>,
    pub env_key: Option<String>,
    pub format: String,
    pub file_name: Option<String>,
    pub blob_key: Option<String>,
    pub size_bytes: i64,
}

/// Result of a processed upload.
#[derive(Clone, Debug, Default)]
pub struct UploadDone {
    pub format: Option<String>,
    pub counts: UploadItemCounts,
    pub warnings: Vec<String>,
    pub globals: Option<serde_json::Value>,
}

/// Job counts by state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueDepth {
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
}

impl Store {
    /// Records the upload and queues its ingest job.
    pub async fn enqueue_upload(&self, u: &NewUpload) -> anyhow::Result<Upload> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into uploads (id, project_id, launch_id, format, file_name, size_bytes, sha256, blob_key, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {UPLOAD_COLUMNS}"
        )))
        .bind(u.id)
        .bind(u.project_id)
        .bind(u.launch_id)
        .bind(&u.format)
        .bind(u.file_name.as_deref().map(no_nul))
        .bind(u.size_bytes)
        .bind(&u.sha256)
        .bind(&u.blob_key)
        .bind(no_nul(&u.created_by))
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("insert into ingest_jobs (id, upload_id, max_attempts) values ($1, $2, $3)")
            .bind(Uuid::now_v7())
            .bind(u.id)
            .bind(u.max_attempts.max(1))
            .execute(&mut *tx)
            .await?;
        crate::events::emit(
            &mut tx,
            "upload.queued",
            Some(&u.project_key),
            serde_json::json!({"id": u.id, "launchId": u.launch_id, "sizeBytes": u.size_bytes}),
        )
        .await?;
        crate::events::notify_upload(&mut tx, &u.project_key, u.launch_id, u.id).await?;
        sqlx::query("select pg_notify($1, '')")
            .bind(JOBS_CHANNEL)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(upload_row(&r)?)
    }

    pub async fn upload(&self, launch_id: Uuid, id: Uuid) -> anyhow::Result<Option<Upload>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {UPLOAD_COLUMNS} from uploads where launch_id = $1 and id = $2"
        )))
        .bind(launch_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(upload_row).transpose()?)
    }

    pub async fn uploads(&self, launch_id: Uuid) -> anyhow::Result<Vec<Upload>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {UPLOAD_COLUMNS} from uploads where launch_id = $1 order by created_at, id limit 10000"
        )))
        .bind(launch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(upload_row).collect::<Result<_, _>>()?)
    }

    /// Claims the oldest runnable job: queued and due, or running with an expired lease and
    /// attempts left. Each job is held by one worker at a time.
    pub async fn claim_job(
        &self,
        worker: &str,
        lease: Duration,
    ) -> anyhow::Result<Option<ClaimedJob>> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "with next as (select id from ingest_jobs \
               where (status = 'queued' and run_after <= now()) \
                  or (status = 'running' and locked_until < now() and attempts < max_attempts) \
               order by created_at limit 1 for update skip locked) \
             update ingest_jobs j set status = 'running', attempts = j.attempts + 1, locked_by = $1, \
               locked_until = now() + make_interval(secs => $2), heartbeat_at = now(), updated_at = now() \
             from next where j.id = next.id \
             returning j.id, j.upload_id, j.attempts, j.max_attempts",
        )
        .bind(worker)
        .bind(lease.as_secs_f64())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(r) = r else {
            tx.commit().await?;
            return Ok(None);
        };
        let job_id: Uuid = r.try_get("id")?;
        let upload_id: Uuid = r.try_get("upload_id")?;
        let attempt: i32 = r.try_get("attempts")?;
        let max_attempts: i32 = r.try_get("max_attempts")?;
        let u = sqlx::query(
            "update uploads u set status = 'processing', started_at = coalesce(u.started_at, now()) \
             from launches l, projects p where u.id = $1 and l.id = u.launch_id and p.id = u.project_id \
             returning u.project_id, p.key as project_key, u.launch_id, l.created_at as launch_created_at, \
             l.env_key, u.format, u.file_name, u.blob_key, u.size_bytes",
        )
        .bind(upload_id)
        .fetch_one(&mut *tx)
        .await?;
        let job = ClaimedJob {
            job_id,
            upload_id,
            attempt,
            max_attempts,
            project_id: u.try_get("project_id")?,
            project_key: u.try_get("project_key")?,
            launch_id: u.try_get("launch_id")?,
            launch_created_at: u.try_get("launch_created_at")?,
            env_key: u.try_get("env_key")?,
            format: u.try_get("format")?,
            file_name: u.try_get("file_name")?,
            blob_key: u.try_get("blob_key")?,
            size_bytes: u.try_get("size_bytes")?,
        };
        crate::events::notify_upload(&mut tx, &job.project_key, job.launch_id, upload_id).await?;
        tx.commit().await?;
        Ok(Some(job))
    }

    /// Extends the lease; false when the worker lost it (another worker took the job over).
    pub async fn heartbeat_job(
        &self,
        job_id: Uuid,
        worker: &str,
        lease: Duration,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update ingest_jobs set locked_until = now() + make_interval(secs => $3), heartbeat_at = now(), \
             updated_at = now() where id = $1 and locked_by = $2 and status = 'running'",
        )
        .bind(job_id)
        .bind(worker)
        .bind(lease.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Marks a job and its upload done; false when the lease was lost.
    pub async fn complete_job(
        &self,
        job: &ClaimedJob,
        worker: &str,
        done: &UploadDone,
    ) -> anyhow::Result<bool> {
        let worker = worker.to_string();
        let format = done.format.clone();
        let counts = serde_json::to_value(&done.counts)?;
        let warnings = json_no_nul(serde_json::to_value(&done.warnings)?);
        let globals = done.globals.clone().map(json_no_nul);
        let event = serde_json::json!({"id": job.upload_id, "launchId": job.launch_id, "counts": counts.clone()});
        let (job_id, upload_id, launch_id, key) = (
            job.job_id,
            job.upload_id,
            job.launch_id,
            job.project_key.clone(),
        );
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "update ingest_jobs set status = 'done', locked_until = null, updated_at = now(), last_error = null \
             where id = $1 and locked_by = $2 and status = 'running'",
        )
        .bind(job_id)
        .bind(worker)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "update uploads set status = 'done', format = coalesce($2, format), counts = $3, warnings = $4, \
             globals = $5, error = null, finished_at = now() where id = $1",
        )
        .bind(upload_id)
        .bind(format)
        .bind(counts)
        .bind(warnings)
        .bind(globals)
        .execute(&mut *tx)
        .await?;
        crate::events::emit(&mut tx, "upload.done", Some(&key), event).await?;
        crate::events::notify_upload(&mut tx, &key, launch_id, upload_id).await?;
        crate::events::notify_launch(&mut tx, &key, launch_id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Records a failure: retried later when `retry` and attempts remain, otherwise the upload
    /// fails with `error`.
    pub async fn fail_job(
        &self,
        job: &ClaimedJob,
        worker: &str,
        error: &UploadError,
        retry: bool,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let again = retry && job.attempt < job.max_attempts;
        let r = sqlx::query(
            "update ingest_jobs set status = $3, locked_by = null, locked_until = null, \
             run_after = now() + make_interval(secs => $4), last_error = $5, updated_at = now() \
             where id = $1 and locked_by = $2 and status = 'running'",
        )
        .bind(job.job_id)
        .bind(worker)
        .bind(if again { "queued" } else { "failed" })
        .bind(if again { 5.0 * job.attempt as f64 } else { 0.0 })
        .bind(no_nul(&error.message))
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(());
        }
        if again {
            sqlx::query("update uploads set status = 'queued' where id = $1")
                .bind(job.upload_id)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("update uploads set status = 'failed', error = $2, finished_at = now() where id = $1")
                .bind(job.upload_id)
                .bind(json_no_nul(serde_json::to_value(error)?))
                .execute(&mut *tx)
                .await?;
            crate::events::emit(
                &mut tx,
                "upload.failed",
                Some(&job.project_key),
                serde_json::json!({"id": job.upload_id, "launchId": job.launch_id, "error": error}),
            )
            .await?;
            crate::events::notify_launch(&mut tx, &job.project_key, job.launch_id).await?;
        }
        crate::events::notify_upload(&mut tx, &job.project_key, job.launch_id, job.upload_id)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Fails jobs whose lease expired after their last attempt (a worker crashed each time);
    /// returns how many.
    pub async fn reap_jobs(&self) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        let rows: Vec<(Uuid, Uuid, Uuid, String)> = sqlx::query_as(
            "with dead as (select id from ingest_jobs where status = 'running' and locked_until < now() \
               and attempts >= max_attempts for update skip locked limit 100) \
             update ingest_jobs j set status = 'failed', locked_by = null, locked_until = null, \
               last_error = 'processing was interrupted on every attempt', updated_at = now() \
             from dead, uploads u, projects p where j.id = dead.id and u.id = j.upload_id and p.id = u.project_id \
             returning j.upload_id, u.launch_id, u.project_id, p.key",
        )
        .fetch_all(&mut *tx)
        .await?;
        let error = UploadError {
            code: "interrupted".into(),
            message: "processing was interrupted on every attempt".into(),
            limit: None,
        };
        for (upload_id, launch_id, _, key) in &rows {
            sqlx::query("update uploads set status = 'failed', error = $2, finished_at = now() where id = $1")
                .bind(upload_id)
                .bind(serde_json::to_value(&error)?)
                .execute(&mut *tx)
                .await?;
            crate::events::notify_upload(&mut tx, key, *launch_id, *upload_id).await?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    }

    /// Raw upload blobs that may be deleted: their upload finished (or no longer exists) more
    /// than `older_than` ago. Takes candidate keys and returns the deletable ones.
    pub async fn deletable_upload_blobs(
        &self,
        ids: &[Uuid],
        older_than: Duration,
    ) -> anyhow::Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let alive: Vec<Uuid> = sqlx::query_scalar(
            "select id from uploads where id = any($1) and (status in ('queued', 'processing') \
             or coalesce(finished_at, created_at) > now() - make_interval(secs => $2))",
        )
        .bind(ids)
        .bind(older_than.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        Ok(ids.iter().filter(|i| !alive.contains(i)).copied().collect())
    }

    pub async fn queue_depth(&self) -> anyhow::Result<QueueDepth> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select status, count(*) from ingest_jobs where status <> 'done' group by status",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut d = QueueDepth::default();
        for (s, n) in rows {
            match s.as_str() {
                "queued" => d.queued = n,
                "running" => d.running = n,
                "failed" => d.failed = n,
                _ => {}
            }
        }
        Ok(d)
    }

    /// Attempts and status of the job of an upload (for tests and diagnostics).
    pub async fn job_of_upload(&self, upload_id: Uuid) -> anyhow::Result<Option<(String, i32)>> {
        Ok(
            sqlx::query_as("select status, attempts from ingest_jobs where upload_id = $1")
                .bind(upload_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}
