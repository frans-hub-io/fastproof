//! Backfill: cases for the code identities that existed before test management. A job per
//! project (`tm_jobs.kind = backfill`) walks the unlinked code identities by `first_seen_at`
//! in batches of 1000 and gives a case to those with a result from a trusted branch (raw
//! `results` with their launch, or `series_daily.branch` once rolled up), through the same
//! path as code sync, so numbers follow `first_seen_at` and an explicit `ALLURE_ID` becomes a
//! key. Launches created before the upgrade count as created with `cases:sync` (the
//! permission did not exist). Idempotent: linked identities are never touched again.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use super::cases::{CaseCtx, case_event, case_sync_settings};
use super::sync::{SYNC_ACTOR, SYNC_BATCH, SyncItem, SyncStats, code_data, item_of, sync_items};
use super::{TmError, TmJob, TmJobKind, TmJobProgress, TmJobStatus, TmLimits, TmResult};
use crate::Store;

/// The migration that brought test management; launches created before it was applied are
/// trusted without `created_by_can_sync`.
pub const UPGRADE_VERSION: i64 = 501;

/// What one call of [`Store::run_case_backfill`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillRun {
    pub job: Option<Uuid>,
    pub done: bool,
    pub scanned: u64,
    pub stats: SyncStats,
}

fn job_row(r: &sqlx::postgres::PgRow) -> sqlx::Result<TmJob> {
    let kind: String = r.try_get("kind")?;
    let status: String = r.try_get("status")?;
    let progress: Value = r.try_get("progress")?;
    let report: Option<Value> = r.try_get("report")?;
    Ok(TmJob {
        id: r.try_get("id")?,
        kind: TmJobKind::parse(&kind).unwrap_or(TmJobKind::Backfill),
        status: TmJobStatus::parse(&status).unwrap_or(TmJobStatus::Queued),
        progress: serde_json::from_value::<TmJobProgress>(progress).unwrap_or_default(),
        report: report.and_then(|r| serde_json::from_value(r).ok()),
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
        download_url: None,
    })
}

const JOB_COLS: &str = "id, kind, status, progress, report, created_by, created_at, finished_at";

impl Store {
    /// When test management was installed (`None` before the migration).
    pub async fn tm_upgrade_at(&self) -> sqlx::Result<Option<DateTime<Utc>>> {
        sqlx::query_scalar(
            "select installed_on from fastproof_schema_migrations where version = $1 and success",
        )
        .bind(UPGRADE_VERSION)
        .fetch_optional(&self.pool)
        .await
    }

    /// Starts a backfill of the project, or returns the one queued or running.
    pub async fn start_case_backfill(&self, project_id: Uuid, by: Option<&str>) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        super::lock_project(&mut tx, project_id, "backfill").await?;
        let existing = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {JOB_COLS} from tm_jobs where project_id = $1 and kind = 'backfill' \
             and status in ('queued', 'running') order by created_at limit 1"
        )))
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(r) = existing {
            return Ok(job_row(&r)?);
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into tm_jobs (id, project_id, kind, params, status, progress, created_by) \
             values ($1, $2, 'backfill', '{{}}', 'queued', '{{\"done\": 0}}', $3) returning {JOB_COLS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(by.map(crate::no_nul))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(job_row(&r)?)
    }

    /// A test-management job of the project.
    pub async fn tm_job(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<TmJob>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {JOB_COLS} from tm_jobs where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(job_row).transpose()?)
    }

    /// Decides the backfill once per project after the upgrade, `delay` after it was installed
    /// (time for an operator to turn `caseSync` off): projects with launches from before the
    /// upgrade and no backfill job yet get a queued job when `caseSync` is enabled at that
    /// moment, else a `cancelled` one (`params.skipped`) so they are never backfilled on their
    /// own later (`POST /cases/backfill` still starts one). Returns how many were queued.
    pub async fn enqueue_upgrade_backfills(&self, delay: Duration) -> anyhow::Result<u64> {
        let Some(at) = self.tm_upgrade_at().await? else {
            return Ok(0);
        };
        let delay = chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::MAX);
        if at
            .checked_add_signed(delay)
            .is_none_or(|due| due > Utc::now())
        {
            return Ok(0);
        }
        let rows: Vec<bool> = sqlx::query_scalar(
            "insert into tm_jobs (id, project_id, kind, params, status, progress, created_by, finished_at) \
             select gen_random_uuid(), p.id, 'backfill', \
               case when s.enabled then '{\"upgrade\": true}'::jsonb else '{\"upgrade\": true, \"skipped\": true}'::jsonb end, \
               case when s.enabled then 'queued' else 'cancelled' end, '{\"done\": 0}', null, \
               case when s.enabled then null else now() end \
             from projects p cross join lateral (select (p.settings -> 'caseSync' -> 'enabled') \
               is distinct from 'false'::jsonb as enabled) s \
             where p.deleted_at is null \
               and not exists (select 1 from tm_jobs j where j.project_id = p.id and j.kind = 'backfill') \
               and exists (select 1 from launches l where l.project_id = p.id and l.created_at < $1) \
             returning status = 'queued'",
        )
        .bind(at)
        .fetch_all(&self.pool)
        .await?;
        let skipped = rows.iter().filter(|q| !**q).count();
        if skipped > 0 {
            tracing::info!(
                projects = skipped,
                "upgrade backfill skipped: caseSync is off"
            );
        }
        Ok(rows.into_iter().filter(|q| *q).count() as u64)
    }

    /// The `cases.backfill` job: takes one queued backfill (or one whose lease expired) and
    /// works on it until it is done or `budget` is spent; the rest waits for the next run.
    pub async fn run_case_backfill(
        &self,
        owner: &str,
        limits: &TmLimits,
        budget: Duration,
    ) -> anyhow::Result<BackfillRun> {
        let start = Instant::now();
        let claimed: Option<(Uuid, Uuid, String, Value)> = sqlx::query_as(
            "update tm_jobs j set status = 'running', lease_owner = $1, lease_until = now() + interval '5 minutes' \
             from projects p where p.id = j.project_id and j.id = (select id from tm_jobs where kind = 'backfill' \
               and (status = 'queued' or (status = 'running' and lease_until < now())) \
               order by created_at limit 1 for update skip locked) \
             returning j.id, j.project_id, p.key, j.progress",
        )
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        let Some((job, project_id, key, progress)) = claimed else {
            return Ok(BackfillRun::default());
        };
        let mut run = BackfillRun {
            job: Some(job),
            ..Default::default()
        };
        let r = self
            .backfill_batches(
                job, project_id, &key, progress, limits, start, budget, &mut run,
            )
            .await;
        match r {
            Ok(done) => {
                run.done = done;
                let status = if done { "done" } else { "queued" };
                sqlx::query(
                    "update tm_jobs set status = $2, lease_owner = null, lease_until = null, \
                     finished_at = case when $2 = 'done' then now() end where id = $1",
                )
                .bind(job)
                .bind(status)
                .execute(&self.pool)
                .await?;
                if done {
                    let mut conn = self.pool.acquire().await?;
                    case_event(
                        &mut conn,
                        &key,
                        "tmJob.updated",
                        json!({"id": job, "kind": "backfill", "status": "done"}),
                    )
                    .await?;
                }
            }
            Err(e) => {
                tracing::warn!(job = %job, error = %e, "case backfill failed");
                sqlx::query(
                    "update tm_jobs set status = 'failed', lease_owner = null, lease_until = null, \
                     finished_at = now(), report = $2 where id = $1",
                )
                .bind(job)
                .bind(
                    json!({"create": 0, "update": 0, "skip": 0, "folders": 0, "unknownFields": [],
                    "unknownValues": {}, "errors": [{"row": 0, "code": "backfill_failed",
                    "message": format!("{e}").chars().take(500).collect::<String>()}]}),
                )
                .execute(&self.pool)
                .await?;
                let mut conn = self.pool.acquire().await?;
                case_event(
                    &mut conn,
                    &key,
                    "tmJob.updated",
                    json!({"id": job, "kind": "backfill", "status": "failed"}),
                )
                .await?;
            }
        }
        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    async fn backfill_batches(
        &self,
        job: Uuid,
        project_id: Uuid,
        key: &str,
        progress: Value,
        limits: &TmLimits,
        start: Instant,
        budget: Duration,
        run: &mut BackfillRun,
    ) -> TmResult<bool> {
        let mut conn = self.pool.acquire().await?;
        let settings = case_sync_settings(&mut conn, project_id).await?;
        if !settings.enabled {
            return Ok(true);
        }
        let upgrade_at = self
            .tm_upgrade_at()
            .await?
            .ok_or_else(|| TmError::Other(anyhow::anyhow!("test management is not installed")))?;
        let known: Vec<String> = sqlx::query_scalar(
            "select distinct branch from launches where project_id = $1 and branch is not null and branch <> '' \
             union select distinct branch from series_daily where project_id = $1 and branch <> ''",
        )
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
        let branches: Vec<String> = known
            .into_iter()
            .filter(|b| settings.branch_trusted(Some(b), None))
            .collect();
        let mut done: i64 = progress.get("done").and_then(Value::as_i64).unwrap_or(0);
        let mut cursor: Option<(DateTime<Utc>, Uuid)> = progress
            .get("cursorAt")
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_micros)
            .zip(
                progress
                    .get("cursorId")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok()),
            );
        let total: i64 = match progress.get("total").and_then(Value::as_i64) {
            Some(t) => t,
            None => sqlx::query_scalar(
                "select count(*) from test_cases where project_id = $1 and kind = 'code' and case_id is null",
            )
            .bind(project_id)
            .fetch_one(&mut *conn)
            .await?,
        };
        let ctx = CaseCtx {
            project_id,
            project_key: key,
            by: SYNC_ACTOR,
            limits,
        };
        loop {
            if start.elapsed() >= budget {
                return Ok(false);
            }
            let ids: Vec<(Uuid, DateTime<Utc>)> = sqlx::query_as(
                "select id, first_seen_at from test_cases where project_id = $1 and kind = 'code' and case_id is null \
                 and ($2::timestamptz is null or (first_seen_at, id) > ($2, $3)) \
                 order by first_seen_at, id limit $4",
            )
            .bind(project_id)
            .bind(cursor.map(|c| c.0))
            .bind(cursor.map(|c| c.1).unwrap_or_default())
            .bind(SYNC_BATCH as i64)
            .fetch_all(&mut *conn)
            .await?;
            if ids.is_empty() {
                return Ok(true);
            }
            let batch: Vec<Uuid> = ids.iter().map(|r| r.0).collect();
            let rows = sqlx::query(
                "select t.id, t.full_name, t.explicit_id, t.explicit_id_source, r.name, r.suite_path, r.labels, \
                 r.launch_id, (select a.detail from attempts a where a.result_id = r.rid \
                   and a.launch_created_at = r.launch_created_at order by a.seq desc limit 1) as detail \
                 from test_cases t cross join lateral (select r.id as rid, r.name, r.suite_path, r.labels, \
                   r.launch_id, r.launch_created_at from results r join launches l on l.id = r.launch_id \
                   where r.test_case_id = t.id and r.project_id = $1 and r.run_item_id is null \
                   and (l.branch = any($3) or ($4 and l.branch is not null and l.branch = l.vcs ->> 'defaultBranch')) \
                   and ($5 or not l.debug) and (l.created_by_can_sync or l.created_at < $6) and not l.sync_blocked \
                   order by r.launch_created_at desc limit 1) r \
                 where t.project_id = $1 and t.id = any($2)",
            )
            .bind(project_id)
            .bind(&batch)
            .bind(&branches)
            .bind(settings.use_ci_default_branch)
            .bind(settings.include_debug)
            .bind(upgrade_at)
            .fetch_all(&mut *conn)
            .await?;
            let mut found = std::collections::HashMap::new();
            for r in &rows {
                let launch: Uuid = r.try_get("launch_id")?;
                let it = item_of(r, Some(launch))?;
                found.insert(it.test_case_id, it);
            }
            let rest: Vec<Uuid> = batch
                .iter()
                .copied()
                .filter(|id| !found.contains_key(id))
                .collect();
            if !rest.is_empty() && !branches.is_empty() {
                let rolled = sqlx::query(
                    "select t.id, t.full_name, t.display_name, t.explicit_id, t.explicit_id_source from test_cases t \
                     where t.project_id = $1 and t.id = any($2) and exists (select 1 from series s \
                       join series_daily d on d.series_id = s.id and d.project_id = $1 \
                       where s.test_case_id = t.id and d.branch = any($3))",
                )
                .bind(project_id)
                .bind(&rest)
                .bind(&branches)
                .fetch_all(&mut *conn)
                .await?;
                for r in &rolled {
                    let id: Uuid = r.try_get("id")?;
                    let full: String = r.try_get("full_name")?;
                    let display: String = r.try_get("display_name")?;
                    let e: Option<String> = r.try_get("explicit_id")?;
                    let es: Option<String> = r.try_get("explicit_id_source")?;
                    found.insert(
                        id,
                        SyncItem {
                            test_case_id: id,
                            data: code_data(
                                &full,
                                &display,
                                Vec::new(),
                                &Value::Null,
                                None,
                                e.as_deref().zip(es.as_deref()),
                            ),
                            launch_id: None,
                        },
                    );
                }
            }
            let items: Vec<SyncItem> = batch.iter().filter_map(|id| found.remove(id)).collect();
            let stats = sync_items(&self.pool, &ctx, &settings, &items).await?;
            run.stats.add(stats);
            run.scanned += ids.len() as u64;
            done += ids.len() as i64;
            let last = ids.last().map(|(id, at)| (*at, *id));
            cursor = last;
            sqlx::query(
                "update tm_jobs set progress = $2, lease_until = now() + interval '5 minutes' where id = $1",
            )
            .bind(job)
            .bind(json!({"done": done.min(total.max(done)), "total": total.max(done),
                "cursorAt": last.map(|l| l.0.timestamp_micros()),
                "cursorId": last.map(|l| l.1.to_string())}))
            .execute(&mut *conn)
            .await?;
            if ids.len() < SYNC_BATCH {
                return Ok(true);
            }
        }
    }
}
