//! Rows of the cluster-wide scheduler: a job runs on whichever replica claims it first, at most
//! once per due time, under a lease that the runner extends while it works.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{ScheduledJob, Store, no_nul};

fn job_row(r: &PgRow) -> Result<ScheduledJob, sqlx::Error> {
    Ok(ScheduledJob {
        name: r.try_get("name")?,
        schedule: r.try_get("schedule")?,
        next_run_at: r.try_get("next_run_at")?,
        lease_owner: r.try_get("lease_owner")?,
        lease_until: r.try_get("lease_until")?,
        last_started_at: r.try_get("last_started_at")?,
        last_finished_at: r.try_get("last_finished_at")?,
        last_status: r.try_get("last_status")?,
        last_error: r.try_get("last_error")?,
        last_duration_ms: r.try_get("last_duration_ms")?,
        run_count: r.try_get("run_count")?,
        failure_count: r.try_get("failure_count")?,
    })
}

impl Store {
    /// Registers a job (or updates its schedule); a new job is due at `first_run`.
    pub async fn register_scheduled_job(
        &self,
        name: &str,
        schedule: &str,
        first_run: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "insert into scheduled_jobs (name, schedule, next_run_at) values ($1, $2, $3) \
             on conflict (name) do update set schedule = excluded.schedule, \
               next_run_at = case when scheduled_jobs.schedule = excluded.schedule \
                 then scheduled_jobs.next_run_at else least(scheduled_jobs.next_run_at, excluded.next_run_at) end, \
               updated_at = now()",
        )
        .bind(name)
        .bind(schedule)
        .bind(first_run)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims the due jobs among `names` that nobody holds; returns their names.
    pub async fn claim_scheduled_jobs(
        &self,
        names: &[String],
        owner: &str,
        lease: Duration,
    ) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "update scheduled_jobs set lease_owner = $2, lease_until = now() + make_interval(secs => $3), \
               last_started_at = now(), run_count = run_count + 1, updated_at = now() \
             where name = any($1) and next_run_at <= now() and (lease_until is null or lease_until < now()) \
             returning name",
        )
        .bind(names)
        .bind(owner)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await?)
    }

    /// Extends a lease; false when it was lost.
    pub async fn heartbeat_scheduled_job(
        &self,
        name: &str,
        owner: &str,
        lease: Duration,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update scheduled_jobs set lease_until = now() + make_interval(secs => $3) \
             where name = $1 and lease_owner = $2",
        )
        .bind(name)
        .bind(owner)
        .bind(lease.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Releases a job after a run and sets its next due time.
    pub async fn finish_scheduled_job(
        &self,
        name: &str,
        owner: &str,
        error: Option<&str>,
        duration: Duration,
        next_run_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update scheduled_jobs set lease_owner = null, lease_until = null, last_finished_at = now(), \
               last_status = $3, last_error = $4, last_duration_ms = $5, next_run_at = $6, \
               failure_count = failure_count + case when $3 = 'failed' then 1 else 0 end, updated_at = now() \
             where name = $1 and lease_owner = $2",
        )
        .bind(name)
        .bind(owner)
        .bind(if error.is_some() { "failed" } else { "ok" })
        .bind(error.map(no_nul))
        .bind(duration.as_millis() as i64)
        .bind(next_run_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn scheduled_jobs(&self) -> anyhow::Result<Vec<ScheduledJob>> {
        let rows = sqlx::query(
            "select name, schedule, next_run_at, lease_owner, lease_until, last_started_at, last_finished_at, \
             last_status, last_error, last_duration_ms, run_count, failure_count from scheduled_jobs order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(job_row).collect::<Result<_, _>>()?)
    }
}
