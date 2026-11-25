//! The analysis queue (contract `76` §4.4): finished launches from the outbox (cursor
//! `analysis` in `integration_cursors`, the gate consumer's settle and `pending` behaviour),
//! recomputes and the first-enable backfill go into `analysis_launches`; workers claim due rows
//! with a lease, wait for uploads still processing, retry failures and record the steps. Also
//! the claim of the `analysis_tasks` kinds of the analysis role and the role's pruning.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::types::{
    AnalysisLaunchRow, AnalysisLaunchState, AnalysisLaunchStatus, BACKTESTS_KEPT, ClaimedTask,
    LaunchSteps, QueueReason,
};
use crate::{Store, json_no_nul, no_nul};

/// Failed runs of a launch retried at most this many times.
pub const MAX_RETRIES: i32 = 3;
/// How long a launch with uploads still queued or processing waits before the next claim.
pub const UPLOAD_WAIT_SECONDS: i64 = 15;
/// Launches the first enable queues per project.
pub const BACKFILL_PER_PROJECT: i64 = 20;
/// Days after which open suggestions expire.
pub const SUGGESTION_DAYS: i64 = 30;
/// Days after which signals never matched to an attempt are deleted.
pub const UNMATCHED_SIGNAL_DAYS: i64 = 2;
/// Rows one prune statement deletes, at most.
const PRUNE_BATCH: i64 = 5000;
/// Candidates one claim looks at, at most (launches waiting for uploads are passed over).
const CLAIM_TRIES: usize = 20;

/// Depth of the analysis queue.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnalysisQueueDepth {
    pub queued: i64,
    pub running: i64,
    pub failed: i64,
    /// Age of the oldest due queued launch.
    pub lag_seconds: f64,
}

type QueueRow = (
    Uuid,
    Uuid,
    String,
    String,
    DateTime<Utc>,
    i32,
    Option<String>,
    Option<DateTime<Utc>>,
    serde_json::Value,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

const QUEUE_COLUMNS: &str = "launch_id, project_id, state, reason, due_at, attempts, lease_owner, lease_until, steps, \
     error, queued_at, started_at, finished_at";

fn queue_row(r: QueueRow) -> AnalysisLaunchRow {
    AnalysisLaunchRow {
        launch_id: r.0,
        project_id: r.1,
        state: AnalysisLaunchState::parse(&r.2),
        reason: QueueReason::parse(&r.3),
        due_at: r.4,
        attempts: r.5,
        lease_owner: r.6,
        lease_until: r.7,
        steps: serde_json::from_value(r.8).unwrap_or_default(),
        error: r.9,
        queued_at: r.10,
        started_at: r.11,
        finished_at: r.12,
    }
}

impl Store {
    /// Reads `launch.finished` events after the `analysis` cursor (older than `settle`) and
    /// queues the launches (`reason finished`); a launch queued again (finished again, merged,
    /// late results) goes back to `queued` with its reason kept. Launches held by another
    /// transaction wait in the cursor's `pending` list. When the cursor is created (the first
    /// enable), the newest finished launches of every project are queued as `backfill`.
    /// `None` when another replica holds the cursor.
    pub async fn consume_analysis_launches(
        &self,
        consumer: &str,
        settle: Duration,
        batch: i64,
    ) -> anyhow::Result<Option<u64>> {
        let (queued, created) = self
            .consume_finished_into(
                crate::integrations::FinishedTarget::Analysis,
                consumer,
                settle,
                batch,
                crate::integrations::MAX_PENDING_GATES,
            )
            .await?;
        if created {
            let n = self.enqueue_recent_launches(BACKFILL_PER_PROJECT).await?;
            if n > 0 {
                tracing::info!(
                    launches = n,
                    "analysis enabled: recent launches queued for analysis"
                );
            }
        }
        Ok(queued)
    }

    /// Queues a finished launch of the project (`delay` from now, and at least `delay` after its
    /// previous analysis finished); a launch being analysed now is run again after. `false`
    /// when the launch is not finished or not of the project.
    pub async fn enqueue_analysis_launch(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        reason: QueueReason,
        delay: Duration,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "insert into analysis_launches (launch_id, project_id, reason, due_at) \
             select l.id, l.project_id, $3, now() + make_interval(secs => $4) from launches l \
             where l.id = $2 and l.project_id = $1 and l.status <> 'running' and l.merged_into is null \
             on conflict (launch_id) do update set \
               reason = excluded.reason, \
               state = case when analysis_launches.state = 'running' then 'running' else 'queued' end, \
               due_at = case when analysis_launches.state = 'running' then now() \
                 when analysis_launches.state = 'queued' then least(analysis_launches.due_at, \
                   greatest(excluded.due_at, coalesce(analysis_launches.finished_at, '-infinity') + make_interval(secs => $4))) \
                 else greatest(excluded.due_at, coalesce(analysis_launches.finished_at, '-infinity') + make_interval(secs => $4)) end, \
               attempts = case when analysis_launches.state = 'running' then analysis_launches.attempts else 0 end, \
               error = case when analysis_launches.state = 'running' then analysis_launches.error else null end, \
               queued_at = case when analysis_launches.state in ('queued', 'running') then analysis_launches.queued_at else now() end",
        )
        .bind(project_id)
        .bind(launch_id)
        .bind(reason.as_str())
        .bind(delay.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Queues the newest `per_project` finished launches of every project (`reason backfill`);
    /// launches already in the queue are left as they are.
    pub async fn enqueue_recent_launches(&self, per_project: i64) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "insert into analysis_launches (launch_id, project_id, reason) \
             select id, project_id, 'backfill' from (select l.id, l.project_id, \
               row_number() over (partition by l.project_id order by l.created_at desc, l.id desc) as n \
               from launches l join projects p on p.id = l.project_id and p.deleted_at is null \
               where l.status = 'finished' and l.merged_into is null) x \
             where n <= $1 order by id on conflict (launch_id) do nothing",
        )
        .bind(per_project)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Claims the oldest due launch (or one whose worker died) for `lease`. A launch with
    /// uploads still queued or processing is passed over and becomes due again in 15 s.
    pub async fn claim_analysis_launch(
        &self,
        owner: &str,
        lease: Duration,
    ) -> anyhow::Result<Option<AnalysisLaunchRow>> {
        for _ in 0..CLAIM_TRIES {
            let mut tx = self.pool.begin().await?;
            let pick: Option<(Uuid, bool)> = sqlx::query_as(
                "select a.launch_id, exists(select 1 from uploads u where u.launch_id = a.launch_id \
                   and u.status in ('queued', 'processing')) \
                 from analysis_launches a \
                 where (a.state = 'queued' and a.due_at <= now()) or (a.state = 'running' and a.lease_until < now()) \
                 order by a.due_at, a.launch_id limit 1 for update of a skip locked",
            )
            .fetch_optional(&mut *tx)
            .await?;
            let Some((launch, busy)) = pick else {
                return Ok(None);
            };
            if busy {
                sqlx::query(
                    "update analysis_launches set state = 'queued', lease_owner = null, lease_until = null, \
                       due_at = now() + make_interval(secs => $2) where launch_id = $1",
                )
                .bind(launch)
                .bind(UPLOAD_WAIT_SECONDS as f64)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                continue;
            }
            let row: QueueRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "update analysis_launches set state = 'running', lease_owner = $2, \
                   lease_until = now() + make_interval(secs => $3), started_at = now(), attempts = attempts + 1 \
                 where launch_id = $1 returning {QUEUE_COLUMNS}"
            )))
            .bind(launch)
            .bind(no_nul(owner))
            .bind(lease.as_secs_f64())
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Some(queue_row(row)));
        }
        Ok(None)
    }

    /// Extends the lease of a claimed launch; `false` when the lease was lost.
    pub async fn renew_analysis_launch(
        &self,
        launch_id: Uuid,
        owner: &str,
        lease: Duration,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update analysis_launches set lease_until = now() + make_interval(secs => $3) \
             where launch_id = $1 and lease_owner = $2 and state = 'running'",
        )
        .bind(launch_id)
        .bind(owner)
        .bind(lease.as_secs_f64())
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Records the end of a run: `done` (or `queued` again when it was queued meanwhile);
    /// with an error, queued again after `attempts` minutes, or `failed` after
    /// [`MAX_RETRIES`] retries. `false` when the lease was lost.
    pub async fn finish_analysis_launch(
        &self,
        launch_id: Uuid,
        owner: &str,
        steps: &LaunchSteps,
        error: Option<&str>,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update analysis_launches set \
               state = case when $4::text is null then \
                   case when due_at > started_at then 'queued' else 'done' end \
                 when attempts > $5 then 'failed' else 'queued' end, \
               due_at = case when $4::text is not null and attempts <= $5 \
                 then now() + make_interval(mins => attempts) else due_at end, \
               attempts = case when $4::text is null then 0 else attempts end, \
               steps = $3, error = $4, finished_at = now(), lease_owner = null, lease_until = null \
             where launch_id = $1 and lease_owner = $2 and state = 'running'",
        )
        .bind(launch_id)
        .bind(owner)
        .bind(json_no_nul(serde_json::to_value(steps)?))
        .bind(error.map(no_nul))
        .bind(MAX_RETRIES)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// The analysis state of a launch; `None` when it was never queued.
    pub async fn analysis_launch_status(
        &self,
        launch_id: Uuid,
    ) -> anyhow::Result<Option<AnalysisLaunchStatus>> {
        let r: Option<QueueRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {QUEUE_COLUMNS} from analysis_launches where launch_id = $1"
        )))
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(queue_row).map(|r| AnalysisLaunchStatus {
            state: r.state,
            queued_at: Some(r.queued_at),
            finished_at: r.finished_at,
            steps: r.steps,
        }))
    }

    /// A row of the analysis queue (tests and diagnostics).
    pub async fn analysis_launch_row(
        &self,
        launch_id: Uuid,
    ) -> anyhow::Result<Option<AnalysisLaunchRow>> {
        let r: Option<QueueRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {QUEUE_COLUMNS} from analysis_launches where launch_id = $1"
        )))
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(queue_row))
    }

    /// Queued, running and failed launches, and the age of the oldest due queued one.
    pub async fn analysis_queue_depth(&self) -> anyhow::Result<AnalysisQueueDepth> {
        let (queued, running, failed, lag): (i64, i64, i64, Option<f64>) = sqlx::query_as(
            "select count(*) filter (where state = 'queued'), count(*) filter (where state = 'running'), \
               count(*) filter (where state = 'failed'), \
               extract(epoch from now() - min(due_at) filter (where state = 'queued' and due_at <= now()))::float8 \
             from analysis_launches where state in ('queued', 'running', 'failed')",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(AnalysisQueueDepth {
            queued,
            running,
            failed,
            lag_seconds: lag.unwrap_or(0.0).max(0.0),
        })
    }

    /// Claims a queued `analysis_tasks` row of one of `kinds` (or one whose lease ran out).
    pub async fn claim_analysis_task(
        &self,
        owner: &str,
        kinds: &[&str],
        lease: Duration,
    ) -> anyhow::Result<Option<ClaimedTask>> {
        let kinds: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
        let r: Option<(Uuid, Uuid, String, serde_json::Value, Option<String>)> = sqlx::query_as(
            "update analysis_tasks set status = 'running', lease_owner = $1, \
               lease_until = now() + make_interval(secs => $3), started_at = coalesce(started_at, now()) \
             where id = (select id from analysis_tasks where kind = any($2) and (status = 'queued' \
               or (status = 'running' and lease_until < now())) order by created_at limit 1 for update skip locked) \
             returning id, project_id, kind, params, created_by",
        )
        .bind(no_nul(owner))
        .bind(&kinds)
        .bind(lease.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        Ok(
            r.map(|(id, project_id, kind, params, created_by)| ClaimedTask {
                id,
                project_id,
                kind,
                params,
                created_by: created_by.unwrap_or_default(),
                owner: owner.to_string(),
            }),
        )
    }

    /// Records the progress of a claimed task and extends its lease; `false` when it was lost.
    pub async fn task_progress(
        &self,
        task: &ClaimedTask,
        progress: serde_json::Value,
        lease: Duration,
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update analysis_tasks set progress = $3, lease_until = now() + make_interval(secs => $4) \
             where id = $1 and lease_owner = $2 and status = 'running'",
        )
        .bind(task.id)
        .bind(&task.owner)
        .bind(json_no_nul(progress))
        .bind(lease.as_secs_f64())
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// Ends a claimed task: `done` with its result as progress, or `failed` with the error.
    pub async fn finish_analysis_task(
        &self,
        task: &ClaimedTask,
        outcome: Result<serde_json::Value, String>,
    ) -> anyhow::Result<bool> {
        let (status, error, progress) = match outcome {
            Ok(p) => ("done", None, p),
            Err(e) => ("failed", Some(e), serde_json::json!({})),
        };
        Ok(sqlx::query(
            "update analysis_tasks set status = $3, error = $4, \
               progress = case when $3 = 'done' then $5 else progress end, finished_at = now(), \
               lease_owner = null, lease_until = null \
             where id = $1 and lease_owner = $2 and status = 'running'",
        )
        .bind(task.id)
        .bind(&task.owner)
        .bind(status)
        .bind(error.as_deref().map(no_nul))
        .bind(json_no_nul(progress))
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    /// The role's pruning (job `analysis.prune`): open suggestions of launches older than 30
    /// days expire, memory and memory events older than the project's `analysis.memoryDays`
    /// go, backtests beyond the newest 20 per project go, signals never matched to an attempt
    /// go after 2 days. Bounded statements until `budget` is spent; returns the rows changed.
    pub async fn prune_analysis(&self, budget: Duration) -> anyhow::Result<u64> {
        let started = Instant::now();
        let mut n = 0u64;
        let left = |started: &Instant| started.elapsed() < budget;
        loop {
            let r = sqlx::query(
                "update triage_suggestions set state = 'expired', decided_at = now() \
                 where ctid = any(array(select s.ctid from triage_suggestions s join launches l on l.id = s.launch_id \
                   where s.state = 'open' and l.created_at < now() - make_interval(days => $1) limit $2))",
            )
            .bind(SUGGESTION_DAYS as i32)
            .bind(PRUNE_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            n += r;
            if (r as i64) < PRUNE_BATCH || !left(&started) {
                break;
            }
        }
        let projects: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
            "select p.id, p.settings from projects p where exists(select 1 from triage_memory m where m.project_id = p.id) \
               or exists(select 1 from triage_memory_events e where e.project_id = p.id) \
               or exists(select 1 from analysis_backtests b where b.project_id = p.id) order by p.id",
        )
        .fetch_all(&self.pool)
        .await?;
        for (project, settings) in projects {
            if !left(&started) {
                return Ok(n);
            }
            let days = super::settings::ProjectSettings::from_value(settings)
                .analysis
                .memory_days as i32;
            for sql in [
                "delete from triage_memory where ctid = any(array(select ctid from triage_memory \
                   where project_id = $1 and last_decided_at < now() - make_interval(days => $2) limit $3))",
                "delete from triage_memory_events where ctid = any(array(select ctid from triage_memory_events \
                   where project_id = $1 and decided_at < now() - make_interval(days => $2) limit $3))",
            ] {
                loop {
                    let r = sqlx::query(sql)
                        .bind(project)
                        .bind(days)
                        .bind(PRUNE_BATCH)
                        .execute(&self.pool)
                        .await?
                        .rows_affected();
                    n += r;
                    if (r as i64) < PRUNE_BATCH || !left(&started) {
                        break;
                    }
                }
            }
            n += sqlx::query(
                "delete from analysis_backtests where project_id = $1 and id not in \
                   (select id from analysis_backtests where project_id = $1 order by created_at desc, id desc limit $2)",
            )
            .bind(project)
            .bind(BACKTESTS_KEPT)
            .execute(&self.pool)
            .await?
            .rows_affected();
        }
        loop {
            if !left(&started) {
                break;
            }
            let r = sqlx::query(
                "delete from attempt_signals where ctid = any(array(select ctid from attempt_signals \
                   where launch_id is null and created_at < now() - make_interval(days => $1) limit $2))",
            )
            .bind(UNMATCHED_SIGNAL_DAYS as i32)
            .bind(PRUNE_BATCH)
            .execute(&self.pool)
            .await?
            .rows_affected();
            n += r;
            if (r as i64) < PRUNE_BATCH {
                break;
            }
        }
        Ok(n)
    }
}
