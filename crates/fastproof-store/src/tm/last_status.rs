//! Denormalized last result of cases (`cases.last_status`, `last_status_at`, `last_source`,
//! `last_launch_id`, `last_result_id`, §4.6): the newest result over all identities of a case,
//! newest by `coalesce(stopped_at, updated_at)`.
//!
//! - [`apply_launch`] moves the columns forward for the cases of a finished launch (from the
//!   `cases.sync` job, for every launch, trusted or not);
//! - [`apply_manual`] recomputes cases after a manual result was projected or removed, and
//!   after identities were linked to or unlinked from cases (code sync, the identity routes);
//! - the `last_status` job ([`Store::run_last_status_job`]) recomputes a whole project in
//!   batches of [`BATCH`] cases, resumable from its cursor;
//! - deleting launches clears the cases that pointed at them ([`CLEAR_SQL`]) and queues a job.
//!
//! Forward moves only write when the stored time is older or equal, so a late sync of an older
//! launch never replaces a newer result.

use std::time::Duration;

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{TmError, TmJobProgress, TmJobStatus, TmResult};
use crate::Store;

/// Cases recomputed per statement of the `last_status` job.
pub const BATCH: i64 = 1000;

/// The newest result of a case `c2.id` over all its identities, as `status, at, src,
/// launch_id, id` (a lateral body).
const NEWEST_RESULT: &str = "select r.status, coalesce(r.stopped_at, r.updated_at) as at, \
    case when r.run_item_id is null then 'automated' else 'manual' end as src, r.launch_id, r.id \
    from test_cases t join results r on r.test_case_id = t.id \
    where t.case_id = c2.id order by coalesce(r.stopped_at, r.updated_at) desc, r.id desc limit 1";

/// Clears the last result of the cases (of the launches' projects) that point at one of the
/// launches `$1`, at most `$2` per statement; returns the cleared case ids' count.
pub(crate) const CLEAR_SQL: &str = "update cases set last_status = null, last_status_at = null, \
    last_source = null, last_launch_id = null, last_result_id = null \
    where id = any(array(select id from cases where project_id in \
      (select project_id from launches where id = any($1)) and last_launch_id = any($1) limit $2))";

/// Moves the last result of every case with a result in the launch forward to the launch's
/// newest result of that case. Returns the cases updated.
pub async fn apply_launch(conn: &mut PgConnection, launch_id: Uuid) -> anyhow::Result<u64> {
    Ok(sqlx::query(
        "with l as (select id, created_at from launches where id = $1), \
         x as (select distinct on (t.case_id) t.case_id, r.status, coalesce(r.stopped_at, r.updated_at) as at, \
             case when r.run_item_id is null then 'automated' else 'manual' end as src, r.launch_id, r.id \
           from l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             join test_cases t on t.id = r.test_case_id \
           where t.case_id is not null \
           order by t.case_id, coalesce(r.stopped_at, r.updated_at) desc, r.id desc) \
         update cases c set last_status = x.status, last_status_at = x.at, last_source = x.src, \
           last_launch_id = x.launch_id, last_result_id = x.id \
         from x where c.id = x.case_id and (c.last_status_at is null or c.last_status_at <= x.at) \
           and (c.last_result_id is distinct from x.id or c.last_status is distinct from x.status \
             or c.last_status_at is distinct from x.at)",
    )
    .bind(launch_id)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// Recomputes the last result of these cases from `results` (a manual result was projected,
/// changed or removed, or identities moved between cases). Returns the cases changed.
pub async fn apply_manual(conn: &mut PgConnection, case_ids: &[Uuid]) -> anyhow::Result<u64> {
    if case_ids.is_empty() {
        return Ok(0);
    }
    recompute(conn, case_ids).await
}

async fn recompute(conn: &mut PgConnection, case_ids: &[Uuid]) -> anyhow::Result<u64> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "update cases c set last_status = x.status, last_status_at = x.at, last_source = x.src, \
           last_launch_id = x.launch_id, last_result_id = x.id \
         from (select c2.id as case_id, lr.status, lr.at, lr.src, lr.launch_id, lr.id \
           from unnest($1::uuid[]) as c2(id) left join lateral ({NEWEST_RESULT}) lr on true) x \
         where c.id = x.case_id and (c.last_result_id is distinct from x.id \
           or c.last_status is distinct from x.status or c.last_status_at is distinct from x.at)"
    )))
    .bind(case_ids)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// Queues a `last_status` job for the project unless one is queued (deleted launches left
/// cases without a last result).
pub(crate) async fn enqueue(conn: &mut PgConnection, project_ids: &[Uuid]) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "insert into tm_jobs (id, project_id, kind, params, status, progress) \
         select gen_random_uuid(), p.id, 'last_status', '{}', 'queued', '{\"done\": 0}' \
         from projects p where p.id = any($1) and p.deleted_at is null \
           and not exists (select 1 from tm_jobs j where j.project_id = p.id and j.kind = 'last_status' \
             and j.status = 'queued')",
    )
    .bind(project_ids)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

impl Store {
    /// Queues one `last_status` job per project with launches that has none queued, running or
    /// done (the backfill after the replica gate first opens). A fresh install (the gate
    /// migration ran with the first one) needs none: every launch was finished by a build that
    /// keeps the columns. Returns how many were queued.
    pub async fn enqueue_last_status_jobs(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "insert into tm_jobs (id, project_id, kind, params, status, progress) \
             select gen_random_uuid(), p.id, 'last_status', '{\"backfill\": true}', 'queued', '{\"done\": 0}' \
             from projects p where p.deleted_at is null \
               and exists (select 1 from launches l where l.project_id = p.id) \
               and not exists (select 1 from tm_jobs j where j.project_id = p.id and j.kind = 'last_status' \
                 and j.status in ('queued', 'running', 'done')) \
               and (select installed_on from fastproof_schema_migrations where version = $1 and success) \
                 - (select installed_on from fastproof_schema_migrations where success order by version limit 1) \
                 >= make_interval(secs => $2)",
        )
        .bind(crate::replica::GATE_MIGRATION)
        .bind(crate::replica::FRESH_INSTALL_SECONDS as f64)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// The `last_status` job, claimed by `owner`: recomputes the project's cases by id in
    /// batches of [`BATCH`], recording the cursor after each batch (a job taken over after a
    /// crash continues there), and ends `done`.
    pub async fn run_last_status_job(&self, job_id: Uuid, owner: &str) -> TmResult<()> {
        let row: Option<(Uuid, String, Value)> = sqlx::query_as(
            "select j.project_id, p.key, j.params from tm_jobs j join projects p on p.id = j.project_id \
             where j.id = $1 and j.kind = 'last_status' and j.lease_owner = $2",
        )
        .bind(job_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        let Some((project_id, key, params)) = row else {
            return Ok(());
        };
        let mut after: Option<Uuid> = params
            .get("after")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());
        let total: i64 = sqlx::query_scalar("select count(*) from cases where project_id = $1")
            .bind(project_id)
            .fetch_one(&self.pool)
            .await?;
        let mut done: i64 = params.get("done").and_then(Value::as_i64).unwrap_or(0);
        loop {
            let mut conn = self.pool.acquire().await?;
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "select id from cases where project_id = $1 and ($2::uuid is null or id > $2) \
                 order by id limit $3",
            )
            .bind(project_id)
            .bind(after)
            .bind(BATCH)
            .fetch_all(&mut *conn)
            .await?;
            let Some(last) = ids.last().copied() else {
                break;
            };
            recompute(&mut conn, &ids).await?;
            drop(conn);
            done += ids.len() as i64;
            after = Some(last);
            let progress = TmJobProgress {
                done,
                total: Some(total.max(done)),
            };
            let kept = sqlx::query(
                "update tm_jobs set params = params || $3, progress = $4, \
                 lease_until = now() + make_interval(secs => $5) \
                 where id = $1 and lease_owner = $2 and status = 'running'",
            )
            .bind(job_id)
            .bind(owner)
            .bind(json!({"after": last, "done": done}))
            .bind(serde_json::to_value(progress).map_err(anyhow::Error::from)?)
            .bind(super::import::LEASE_SECONDS as f64)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if kept == 0 {
                return Ok(());
            }
            if (ids.len() as i64) < BATCH {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let finished = self
            .finish_tm_job(
                job_id,
                owner,
                &key,
                TmJobStatus::Done,
                TmJobProgress {
                    done,
                    total: Some(total.max(done)),
                },
                None,
                None,
            )
            .await?;
        if !finished {
            return Err(TmError::Other(anyhow::anyhow!(
                "the last_status job was taken over"
            )));
        }
        Ok(())
    }

    /// Recomputes the last result of one project's cases now (tests and the CLI; the job does
    /// the same in batches).
    pub async fn recompute_last_status(&self, project_id: Uuid) -> anyhow::Result<u64> {
        let mut conn = self.pool.acquire().await?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("select id from cases where project_id = $1 order by id")
                .bind(project_id)
                .fetch_all(&mut *conn)
                .await?;
        let mut n = 0;
        for chunk in ids.chunks(BATCH as usize) {
            n += recompute(&mut conn, chunk).await?;
        }
        Ok(n)
    }
}
