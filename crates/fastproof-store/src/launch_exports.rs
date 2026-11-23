//! Background exports of one launch (HTML or XLSX of its results): the job rows. The file
//! itself lives in blob storage until the job expires.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::Store;

/// Hours a finished export stays downloadable.
pub const LAUNCH_EXPORT_TTL_HOURS: i64 = 24;
/// Exports a caller may have queued or running.
pub const MAX_ACTIVE_PER_CALLER: i64 = 2;
/// Exports a project may have queued or running.
pub const MAX_ACTIVE_PER_PROJECT: i64 = 4;
/// Exports a caller may create per 24 hours (finished ones count too).
pub const MAX_PER_CALLER_DAY: i64 = 20;
/// Exports a project may create per 24 hours.
pub const MAX_PER_PROJECT_DAY: i64 = 100;
/// Exports queued or running on the whole server.
pub const MAX_ACTIVE_TOTAL: i64 = 8;
/// Seconds without progress after which a running export is considered lost.
pub const STALE_AFTER_SECONDS: i64 = 300;

/// A background export of a launch.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchExport {
    pub id: Uuid,
    pub launch_id: Uuid,
    /// `html` or `xlsx`.
    pub format: String,
    /// `queued`, `running`, `done`, `failed` or `expired`.
    pub status: String,
    /// Results to write (the launch's total when the export started).
    pub total: i64,
    /// Results written so far.
    pub done: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Set only in the answer that creates the job and in the status of a done job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
}

/// Why a job was not created.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateDenied {
    /// Too many queued or running exports of the caller, the project or the server.
    Busy,
    /// The rolling 24 hour quota of the caller or the project is used.
    Quota,
}

/// A job with what the worker needs.
#[derive(Clone, Debug)]
pub struct ClaimedLaunchExport {
    pub export: LaunchExport,
    pub project_id: Uuid,
    pub project_key: String,
    pub query: String,
    pub blob_key: Option<String>,
}

macro_rules! columns {
    () => {
        "id, launch_id, format, status, total, done, size_bytes, error, created_by, created_at, finished_at"
    };
}

fn row(r: &PgRow) -> Result<LaunchExport, sqlx::Error> {
    Ok(LaunchExport {
        id: r.try_get("id")?,
        launch_id: r.try_get("launch_id")?,
        format: r.try_get("format")?,
        status: r.try_get("status")?,
        total: r.try_get("total")?,
        done: r.try_get("done")?,
        size_bytes: r.try_get("size_bytes")?,
        error: r.try_get("error")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
        download_url: None,
    })
}

impl Store {
    /// Creates a queued job, or says why not. `owner` is the stable identity of the caller
    /// (`created_by` is only the name shown).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_launch_export(
        &self,
        id: Uuid,
        project_id: Uuid,
        launch_id: Uuid,
        format: &str,
        query: &str,
        total: i64,
        created_by: &str,
        owner: &str,
    ) -> anyhow::Result<Result<LaunchExport, CreateDenied>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 710))")
            .bind(project_id.to_string())
            .execute(&mut *tx)
            .await?;
        let (mine, all, total_active): (i64, i64, i64) = sqlx::query_as(
            "select count(*) filter (where owner_key = $2), count(*), \
               (select count(*) from launch_exports where status in ('queued', 'running')) \
             from launch_exports where project_id = $1 and status in ('queued', 'running')",
        )
        .bind(project_id)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        if mine >= MAX_ACTIVE_PER_CALLER
            || all >= MAX_ACTIVE_PER_PROJECT
            || total_active >= MAX_ACTIVE_TOTAL
        {
            return Ok(Err(CreateDenied::Busy));
        }
        let (day_mine, day_all): (i64, i64) = sqlx::query_as(
            "select count(*) filter (where owner_key = $2), count(*) from launch_exports \
             where project_id = $1 and created_at > now() - interval '24 hours'",
        )
        .bind(project_id)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        if day_mine >= MAX_PER_CALLER_DAY || day_all >= MAX_PER_PROJECT_DAY {
            return Ok(Err(CreateDenied::Quota));
        }
        let r = sqlx::query(concat!(
            "insert into launch_exports (id, project_id, launch_id, format, query, total, created_by, owner_key) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning ",
            columns!()
        ))
        .bind(id)
        .bind(project_id)
        .bind(launch_id)
        .bind(format)
        .bind(query)
        .bind(total)
        .bind(created_by)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(row(&r)?))
    }

    /// The job with its blob key and owner identity.
    pub async fn launch_export(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<(LaunchExport, Option<String>, String)>> {
        let r = sqlx::query(concat!(
            "select ",
            columns!(),
            ", blob_key, owner_key from launch_exports where project_id = $1 and id = $2"
        ))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(|r| Ok((row(&r)?, r.try_get("blob_key")?, r.try_get("owner_key")?)))
            .transpose()
    }

    /// Takes a queued job of this id for running; `None` when it is not queued any more.
    pub async fn start_launch_export(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Option<ClaimedLaunchExport>> {
        let r = sqlx::query(concat!(
            "update launch_exports set status = 'running', updated_at = now() \
             where id = $1 and status = 'queued' returning ",
            columns!(),
            ", project_id, query, blob_key, \
             (select p.key from projects p where p.id = launch_exports.project_id) as project_key"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(|r| {
            Ok(ClaimedLaunchExport {
                export: row(&r)?,
                project_id: r.try_get("project_id")?,
                project_key: r.try_get("project_key")?,
                query: r.try_get("query")?,
                blob_key: r.try_get("blob_key")?,
            })
        })
        .transpose()
    }

    /// Records progress; `false` when the job is not running any more (stop).
    pub async fn progress_launch_export(&self, id: Uuid, done: i64) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update launch_exports set done = $2, updated_at = now() where id = $1 and status = 'running'",
        )
        .bind(id)
        .bind(done)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Keeps a running job alive without changing its progress (the upload of the file).
    pub async fn touch_launch_export(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "update launch_exports set updated_at = now() where id = $1 and status = 'running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the key the file is about to be uploaded to, so cleanup finds it in any state.
    pub async fn set_launch_export_blob(&self, id: Uuid, blob_key: &str) -> anyhow::Result<()> {
        sqlx::query("update launch_exports set blob_key = $2 where id = $1")
            .bind(id)
            .bind(blob_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Marks the job done; `false` when it is not running any more (the caller deletes the file).
    pub async fn finish_launch_export(
        &self,
        id: Uuid,
        done: i64,
        blob_key: &str,
        size: i64,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update launch_exports set status = 'done', done = $2, blob_key = $3, size_bytes = $4, \
             finished_at = now(), updated_at = now() where id = $1 and status = 'running'",
        )
        .bind(id)
        .bind(done)
        .bind(blob_key)
        .bind(size)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn fail_launch_export(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update launch_exports set status = 'failed', error = $2, finished_at = now(), updated_at = now() \
             where id = $1 and status in ('queued', 'running')",
        )
        .bind(id)
        .bind(error.chars().take(500).collect::<String>())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fails running jobs that made no progress for [`STALE_AFTER_SECONDS`] (their replica is gone)
    /// and jobs queued for an hour.
    pub async fn fail_stale_launch_exports(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "update launch_exports set status = 'failed', error = 'the export was interrupted', \
             finished_at = now(), updated_at = now() \
             where (status = 'running' and updated_at < now() - make_interval(secs => $1)) \
                or (status = 'queued' and updated_at < now() - interval '1 hour')",
        )
        .bind(STALE_AFTER_SECONDS as f64)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Jobs whose file is to be deleted: done ones past the TTL and failed or expired ones that
    /// still name a file.
    pub async fn due_launch_exports(&self, limit: i64) -> anyhow::Result<Vec<(Uuid, String)>> {
        Ok(sqlx::query_as(
            "select id, blob_key from launch_exports where blob_key is not null and ( \
               (status = 'done' and finished_at < now() - make_interval(hours => $1::int)) \
               or (status in ('failed', 'expired') and updated_at < now() - interval '10 minutes')) \
             order by updated_at limit $2",
        )
        .bind(LAUNCH_EXPORT_TTL_HOURS as i32)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// After the file is deleted: a done job becomes `expired`, the key is forgotten.
    pub async fn expire_launch_export(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "update launch_exports set blob_key = null, \
             status = case when status = 'done' then 'expired' else status end where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes the rows of failed and expired jobs after a week.
    pub async fn delete_old_launch_exports(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "delete from launch_exports where status in ('failed', 'expired') \
             and finished_at < now() - interval '7 days'",
        )
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}
