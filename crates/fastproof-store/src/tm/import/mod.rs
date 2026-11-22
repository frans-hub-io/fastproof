//! Import and export jobs (`tm_jobs`, T3). The parsers live in `fastproof-tm-import`; this
//! module keeps the job lifecycle and the database side of an import:
//!
//! `queued` → (a worker claims it) `dry_run` → `ready` with an [`ImportReport`] → `POST
//! …/apply` → `running` → `done` | `failed`; `cancelled` from any state before `done`.
//! Workers hold a lease; at most one import per project is queued, in its dry run or running,
//! and at most [`MAX_RUNNING_IMPORTS`] run at once in the installation. Export jobs (`kind =
//! export`) go `queued` → `running` → `done` with a blob for 24 hours; a caller has at most
//! [`MAX_EXPORTS_PER_CALLER`] and a project [`MAX_EXPORTS_PER_PROJECT`] queued or running
//! (409 `export_busy`), and at most [`MAX_RUNNING_EXPORTS`] run at once in the installation.
//! Imports and exports are claimed by separate workers, so exports never hold imports back.

pub mod plan;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use super::cases::case_event;
use super::{
    CaseFilter, ImportFormat, ImportMode, ImportReport, StepsLayout, TmError, TmJob, TmJobKind,
    TmJobProgress, TmJobStatus, TmResult, UnknownFieldValues,
};
use crate::{Store, no_nul};

pub use plan::*;

/// Imports in their dry run or running at once in the installation.
pub const MAX_RUNNING_IMPORTS: i64 = 4;
/// Exports running at once in the installation.
pub const MAX_RUNNING_EXPORTS: i64 = 4;
/// Queued or running exports of one caller in a project.
pub const MAX_EXPORTS_PER_CALLER: i64 = 2;
/// Queued or running exports of one project.
pub const MAX_EXPORTS_PER_PROJECT: i64 = 4;
/// Lease of a worker on a job; renewed while it works.
pub const LEASE_SECONDS: i64 = 120;
/// Errors kept in a report.
pub const MAX_REPORT_ERRORS: usize = 1000;
/// How long an export download stays available.
pub const EXPORT_TTL_HOURS: i64 = 24;
/// Cases of one export, at most.
pub const MAX_EXPORT_CASES: usize = 50_000;

/// What an import job reads (stored in `tm_jobs.params`; never holds a secret value).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportParams {
    /// `file` or `testops`.
    pub source: String,
    pub format: Option<ImportFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_layout: Option<StepsLayout>,
    pub mode: Option<ImportMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_folder_id: Option<Uuid>,
    pub unknown_field_values: Option<UnknownFieldValues>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// Blob of the uploaded file (or of the cases fetched from TestOps, as NDJSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    /// Blob of the attachment zip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testops: Option<TestOpsParams>,
    /// Keep source numbers (TestOps AllureID, FastProof numbers): set by the dry run when the
    /// project had no cases.
    #[serde(default)]
    pub keep_numbers: bool,
    /// The largest source number kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_number: Option<i64>,
}

/// Where a TestOps import reads from; the token is the project secret `secret`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOpsParams {
    pub base_url: String,
    pub remote_project_id: i64,
    pub secret: String,
    #[serde(default)]
    pub allow_http: bool,
}

/// What an export job writes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportParams {
    #[serde(default)]
    pub filter: CaseFilter,
    pub attachments: bool,
    /// sha256 (hex) of the one-time download token.
    pub token_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default)]
    pub downloaded: bool,
}

/// A job a worker holds.
#[derive(Clone, Debug)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub kind: TmJobKind,
    /// `dry_run` or `running` (import), `running` (export).
    pub status: TmJobStatus,
    pub params: Value,
    pub progress: TmJobProgress,
    pub created_by: Option<String>,
    pub report: Option<ImportReport>,
}

const COLS: &str = "id, kind, status, progress, report, created_by, created_at, finished_at";

fn job_of(r: &sqlx::postgres::PgRow) -> sqlx::Result<TmJob> {
    let kind: String = r.try_get("kind")?;
    let status: String = r.try_get("status")?;
    let progress: Value = r.try_get("progress")?;
    let report: Option<Value> = r.try_get("report")?;
    Ok(TmJob {
        id: r.try_get("id")?,
        kind: TmJobKind::parse(&kind).unwrap_or(TmJobKind::Import),
        status: TmJobStatus::parse(&status).unwrap_or(TmJobStatus::Queued),
        progress: serde_json::from_value(progress).unwrap_or_default(),
        report: report.and_then(|r| serde_json::from_value(r).ok()),
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
        download_url: None,
    })
}

async fn job_event(conn: &mut sqlx::PgConnection, key: &str, job: &TmJob) -> anyhow::Result<()> {
    case_event(
        conn,
        key,
        "tmJob.updated",
        json!({"id": job.id, "kind": job.kind.as_str(), "status": job.status.as_str(),
            "progress": job.progress}),
    )
    .await
}

impl Store {
    /// Creates a queued job. An import fails with 409 `import_busy` while another import of
    /// the project is queued, in its dry run or running; an export fails with 409
    /// `export_busy` when the caller or the project has its most exports queued or running.
    pub async fn create_tm_job(
        &self,
        id: Uuid,
        project_id: Uuid,
        project_key: &str,
        kind: TmJobKind,
        params: &Value,
        by: &str,
    ) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        super::lock_project(&mut tx, project_id, "imports").await?;
        match kind {
            TmJobKind::Import => busy(&mut tx, project_id, None).await?,
            TmJobKind::Export => exports_busy(&mut tx, project_id, by).await?,
            TmJobKind::Backfill => {}
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into tm_jobs (id, project_id, kind, params, status, progress, created_by) \
             values ($1, $2, $3, $4, 'queued', '{{\"done\": 0}}', $5) returning {COLS}"
        )))
        .bind(id)
        .bind(project_id)
        .bind(kind.as_str())
        .bind(crate::json_no_nul(params.clone()))
        .bind(no_nul(by))
        .fetch_one(&mut *tx)
        .await?;
        let job = job_of(&r)?;
        job_event(&mut tx, project_key, &job).await?;
        tx.commit().await?;
        Ok(job)
    }

    /// A job of the project with its parameters.
    pub async fn tm_job_with_params(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<(TmJob, Value)>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLS}, params from tm_jobs where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => Ok(Some((job_of(&r)?, r.try_get("params")?))),
            None => Ok(None),
        }
    }

    /// Asks a ready import to apply its dry run: `ready` → `running` (a worker picks it up).
    pub async fn apply_tm_job(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
    ) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        super::lock_project(&mut tx, project_id, "imports").await?;
        let status: Option<String> = sqlx::query_scalar(
            "select status from tm_jobs where project_id = $1 and id = $2 and kind = 'import' for update",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        match status.as_deref() {
            None => return Err(TmError::NotFound("import")),
            Some("ready") => {}
            Some(s) => {
                return Err(TmError::conflict(
                    "job_not_ready",
                    format!("the import is {s}; only a ready import can be applied"),
                ));
            }
        }
        busy(&mut tx, project_id, Some(id)).await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tm_jobs set status = 'running', lease_owner = null, lease_until = null, \
             progress = jsonb_build_object('done', 0, 'total', progress -> 'total') \
             where id = $1 returning {COLS}"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let job = job_of(&r)?;
        job_event(&mut tx, project_key, &job).await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Cancels a job that is not finished; a finished one answers 409 `job_finished`.
    pub async fn cancel_tm_job(
        &self,
        project_id: Uuid,
        project_key: &str,
        id: Uuid,
        kind: TmJobKind,
    ) -> TmResult<(TmJob, Value)> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLS}, params from tm_jobs where project_id = $1 and id = $2 and kind = $3 for update"
        )))
        .bind(project_id)
        .bind(id)
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(TmError::NotFound("job"))?;
        let job = job_of(&r)?;
        let params: Value = r.try_get("params")?;
        if matches!(
            job.status,
            TmJobStatus::Done | TmJobStatus::Failed | TmJobStatus::Cancelled
        ) {
            return Err(TmError::conflict(
                "job_finished",
                format!("the job is {}", job.status),
            ));
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tm_jobs set status = 'cancelled', finished_at = now(), lease_owner = null, lease_until = null \
             where id = $1 returning {COLS}"
        )))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let job = job_of(&r)?;
        job_event(&mut tx, project_key, &job).await?;
        tx.commit().await?;
        Ok((job, params))
    }

    /// Claims the oldest job of `kind` a worker can run. Imports: a queued import (→
    /// `dry_run`) while fewer than [`MAX_RUNNING_IMPORTS`] imports run, an import asked to
    /// apply (`running` without a lease), or one whose worker's lease expired. Exports: a
    /// queued export (→ `running`) or an expired one while fewer than [`MAX_RUNNING_EXPORTS`]
    /// run. Imports and exports have their own workers.
    pub async fn claim_tm_job(
        &self,
        owner: &str,
        kind: TmJobKind,
    ) -> anyhow::Result<Option<ClaimedJob>> {
        if kind == TmJobKind::Backfill {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('fastproof:tm:jobs', 0))")
            .execute(&mut *tx)
            .await?;
        let running: i64 = sqlx::query_scalar(
            "select count(*) from tm_jobs where kind = $1 and status in ('dry_run', 'running') \
             and lease_until > now()",
        )
        .bind(kind.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let max = match kind {
            TmJobKind::Export => MAX_RUNNING_EXPORTS,
            _ => MAX_RUNNING_IMPORTS,
        };
        if running >= max {
            return Ok(None);
        }
        let r = sqlx::query(
            "select j.id, j.project_id, p.key, j.kind, j.status, j.params, j.progress, j.created_by, j.report \
             from tm_jobs j join projects p on p.id = j.project_id \
             where j.kind = $1 and p.deleted_at is null and ( \
               (j.kind = 'export' and (j.status = 'queued' or (j.status = 'running' and j.lease_until < now()))) \
               or (j.kind = 'import' and (j.status = 'queued' \
                 or (j.status in ('dry_run', 'running') and (j.lease_until is null or j.lease_until < now()))))) \
             order by j.created_at limit 1 for update of j skip locked",
        )
        .bind(kind.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(r) = r else {
            return Ok(None);
        };
        let kind = TmJobKind::parse(&r.try_get::<String, _>("kind")?).unwrap_or(TmJobKind::Import);
        let old: String = r.try_get("status")?;
        let status = match (kind, old.as_str()) {
            (TmJobKind::Import, "queued" | "dry_run") => TmJobStatus::DryRun,
            _ => TmJobStatus::Running,
        };
        let id: Uuid = r.try_get("id")?;
        sqlx::query(
            "update tm_jobs set status = $2, lease_owner = $3, lease_until = now() + make_interval(secs => $4) where id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(owner)
        .bind(LEASE_SECONDS as f64)
        .execute(&mut *tx)
        .await?;
        let key: String = r.try_get("key")?;
        let report: Option<Value> = r.try_get("report")?;
        let claimed = ClaimedJob {
            id,
            project_id: r.try_get("project_id")?,
            project_key: key.clone(),
            kind,
            status,
            params: r.try_get("params")?,
            progress: serde_json::from_value(r.try_get("progress")?).unwrap_or_default(),
            created_by: r.try_get("created_by")?,
            report: report.and_then(|v| serde_json::from_value(v).ok()),
        };
        if old == "queued" {
            case_event(
                &mut tx,
                &key,
                "tmJob.updated",
                json!({"id": id, "kind": kind.as_str(), "status": status.as_str()}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Some(claimed))
    }

    /// Renews the lease and records progress; returns `false` when the job is no longer this
    /// worker's (cancelled, or the lease went to another worker).
    pub async fn heartbeat_tm_job(
        &self,
        id: Uuid,
        owner: &str,
        progress: TmJobProgress,
    ) -> anyhow::Result<bool> {
        let n = sqlx::query(
            "update tm_jobs set lease_until = now() + make_interval(secs => $3), progress = $4 \
             where id = $1 and lease_owner = $2 and status in ('dry_run', 'running')",
        )
        .bind(id)
        .bind(owner)
        .bind(LEASE_SECONDS as f64)
        .bind(serde_json::to_value(progress)?)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Ends a worker's turn on a job: the new status, report and parameters (merged). Returns
    /// `false` when the job was cancelled or taken over meanwhile.
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_tm_job(
        &self,
        id: Uuid,
        owner: &str,
        project_key: &str,
        status: TmJobStatus,
        progress: TmJobProgress,
        report: Option<&ImportReport>,
        params_patch: Option<Value>,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update tm_jobs set status = $3, lease_owner = null, lease_until = null, progress = $4, \
             report = coalesce($5, report), params = params || coalesce($6, '{{}}'::jsonb), \
             finished_at = case when $3 in ('done', 'failed', 'cancelled') then now() end \
             where id = $1 and lease_owner = $2 and status in ('dry_run', 'running') returning {COLS}"
        )))
        .bind(id)
        .bind(owner)
        .bind(status.as_str())
        .bind(serde_json::to_value(progress)?)
        .bind(report.map(serde_json::to_value).transpose()?)
        .bind(params_patch.map(crate::json_no_nul))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(r) = r else {
            return Ok(false);
        };
        let job = job_of(&r)?;
        job_event(&mut tx, project_key, &job).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Marks an export as downloaded once: `true` only for the first call by its creator
    /// with the right token, within [`EXPORT_TTL_HOURS`] of its end.
    pub async fn take_export_download(
        &self,
        project_id: Uuid,
        id: Uuid,
        by: &str,
        token_sha256: &str,
    ) -> anyhow::Result<Option<String>> {
        let blob: Option<String> = sqlx::query_scalar(
            "update tm_jobs set params = params || '{\"downloaded\": true}'::jsonb \
             where project_id = $1 and id = $2 and kind = 'export' and status = 'done' \
               and created_by = $3 and params ->> 'tokenSha256' = $4 \
               and coalesce((params ->> 'downloaded')::boolean, false) = false \
               and finished_at > now() - make_interval(hours => $5) \
             returning params ->> 'blob'",
        )
        .bind(project_id)
        .bind(id)
        .bind(by)
        .bind(token_sha256)
        .bind(EXPORT_TTL_HOURS as i32)
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        Ok(blob)
    }

    /// Blobs of jobs that are over: finished imports, cancelled jobs, exports downloaded or
    /// older than [`EXPORT_TTL_HOURS`], and ready imports nobody applied within 7 days (they
    /// are cancelled). Returns `(job id, blob keys)`; the caller deletes the blobs, then calls
    /// [`Store::forget_tm_job_blobs`].
    pub async fn expired_tm_job_blobs(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, Vec<String>)>> {
        sqlx::query(
            "update tm_jobs set status = 'cancelled', finished_at = now() \
             where kind = 'import' and status = 'ready' and created_at < now() - interval '7 days'",
        )
        .execute(&self.pool)
        .await?;
        let rows: Vec<(Uuid, Value)> = sqlx::query_as(
            "select id, params from tm_jobs where (params ? 'blob' or params ? 'zip') and ( \
               (kind = 'import' and status in ('done', 'failed', 'cancelled')) \
               or (kind = 'export' and (status in ('failed', 'cancelled') \
                 or finished_at < now() - make_interval(hours => $1) \
                 or coalesce((params ->> 'downloaded')::boolean, false)))) \
             order by created_at limit $2",
        )
        .bind(EXPORT_TTL_HOURS as i32)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, p)| {
                let keys = ["blob", "zip"]
                    .iter()
                    .filter_map(|k| p.get(*k).and_then(Value::as_str).map(str::to_string))
                    .collect();
                (id, keys)
            })
            .collect())
    }

    /// Removes the blob references of a job after its blobs were deleted.
    pub async fn forget_tm_job_blobs(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("update tm_jobs set params = params - 'blob' - 'zip' where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Recent import and export jobs of a project (newest first).
    pub async fn tm_jobs(
        &self,
        project_id: Uuid,
        kind: TmJobKind,
        limit: i64,
    ) -> anyhow::Result<Vec<TmJob>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLS} from tm_jobs where project_id = $1 and kind = $2 order by created_at desc limit $3"
        )))
        .bind(project_id)
        .bind(kind.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(job_of).collect::<sqlx::Result<_>>()?)
    }
}

async fn busy(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    except: Option<Uuid>,
) -> TmResult<()> {
    let other: Option<Uuid> = sqlx::query_scalar(
        "select id from tm_jobs where project_id = $1 and kind = 'import' \
         and status in ('queued', 'dry_run', 'running') and id is distinct from $2 limit 1",
    )
    .bind(project_id)
    .bind(except)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(o) = other {
        return Err(TmError::Conflict {
            code: "import_busy",
            message: "another import of the project is running".into(),
            details: Some(json!({"jobId": o})),
        });
    }
    Ok(())
}

async fn exports_busy(conn: &mut sqlx::PgConnection, project_id: Uuid, by: &str) -> TmResult<()> {
    let (mine, all): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where created_by = $2), count(*) from tm_jobs \
         where project_id = $1 and kind = 'export' and status in ('queued', 'running')",
    )
    .bind(project_id)
    .bind(no_nul(by))
    .fetch_one(&mut *conn)
    .await?;
    let over = if mine >= MAX_EXPORTS_PER_CALLER {
        Some(("exportsPerCaller", MAX_EXPORTS_PER_CALLER))
    } else if all >= MAX_EXPORTS_PER_PROJECT {
        Some(("exportsPerProject", MAX_EXPORTS_PER_PROJECT))
    } else {
        None
    };
    if let Some((limit, max)) = over {
        return Err(TmError::Conflict {
            code: "export_busy",
            message: format!(
                "{limit} ({max}) exports are queued or running; retry when one is done"
            ),
            details: Some(json!({"limit": limit, "max": max})),
        });
    }
    Ok(())
}

/// When a job finished (for the download expiry shown to clients).
pub fn export_expires(finished_at: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    finished_at.map(|f| f + chrono::Duration::hours(EXPORT_TTL_HOURS))
}
