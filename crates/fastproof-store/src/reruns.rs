//! Reruns of a launch: a request to run some of its tests again in CI. The record links the
//! source launch to the launch the CI starts, which joins it by the external id given here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, no_nul};

/// Reruns of one launch that did not fail, at most (a failed attempt, such as one during a CI
/// outage, does not use them up; the hourly limit counts every attempt).
pub const MAX_RERUNS_PER_LAUNCH: i64 = 20;
/// Reruns per project per hour, at most.
pub const MAX_RERUNS_PER_HOUR: i64 = 60;
/// Seconds between two reruns of the same launch.
pub const RERUN_COOLDOWN_SECONDS: i64 = 10;
/// Result ids of a "rerun selected" request, at most.
pub const MAX_RERUN_RESULTS: usize = 1000;

/// A rerun as listed.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Rerun {
    pub id: Uuid,
    pub source_launch_id: Uuid,
    pub source_launch_number: i64,
    /// The external id the CI is told to start its launch with.
    pub external_id: String,
    pub plan_id: Option<Uuid>,
    /// `github`, `gitlab` or `webhook`.
    pub provider: String,
    /// `failed` or `selected`.
    pub mode: String,
    pub test_count: i32,
    /// `dispatching`, `dispatched` or `failed`.
    pub status: String,
    /// Why the CI was not triggered; never contains URLs or secrets.
    pub error: Option<String>,
    /// A reference the CI returned (a pipeline id or URL on the configured origin).
    pub ci_ref: Option<String>,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    /// The launch the CI started, once it exists.
    pub launch_id: Option<Uuid>,
    pub launch_number: Option<i64>,
}

/// Why a rerun was refused before anything was sent.
#[derive(Debug, PartialEq, Eq)]
pub enum RerunRefusal {
    UnknownLaunch,
    Cooldown,
    TooManyForLaunch,
    TooManyInHour,
}

const SELECT: &str = "select r.id, r.source_launch_id, s.number as source_number, r.external_id, r.plan_id, \
    r.provider, r.mode, r.test_count, r.status, r.error, r.ci_ref, r.requested_by, r.created_at, \
    r.dispatched_at, n.id as new_id, n.number as new_number \
    from launch_reruns r join launches s on s.id = r.source_launch_id \
    left join launches n on n.project_id = r.project_id and n.external_id = r.external_id ";

fn rerun_row(r: &sqlx::postgres::PgRow) -> Result<Rerun, sqlx::Error> {
    Ok(Rerun {
        id: r.try_get("id")?,
        source_launch_id: r.try_get("source_launch_id")?,
        source_launch_number: r.try_get("source_number")?,
        external_id: r.try_get("external_id")?,
        plan_id: r.try_get("plan_id")?,
        provider: r.try_get("provider")?,
        mode: r.try_get("mode")?,
        test_count: r.try_get("test_count")?,
        status: r.try_get("status")?,
        error: r.try_get("error")?,
        ci_ref: r.try_get("ci_ref")?,
        requested_by: r.try_get("requested_by")?,
        created_at: r.try_get("created_at")?,
        dispatched_at: r.try_get("dispatched_at")?,
        launch_id: r.try_get("new_id")?,
        launch_number: r.try_get("new_number")?,
    })
}

impl Store {
    /// Records a rerun about to be sent, after the limits (one per
    /// [`RERUN_COOLDOWN_SECONDS`] per launch, [`MAX_RERUNS_PER_LAUNCH`] per launch,
    /// [`MAX_RERUNS_PER_HOUR`] per project) allow it.
    pub async fn rerun_begin(
        &self,
        project_id: Uuid,
        source_launch_id: Uuid,
        provider: &str,
        mode: &str,
        by: &str,
    ) -> anyhow::Result<Result<(Uuid, String), RerunRefusal>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('rerun:' || $1::text, 0))")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        let known: Option<Uuid> = sqlx::query_scalar(
            "select id from launches where project_id = $1 and id = $2 for update",
        )
        .bind(project_id)
        .bind(source_launch_id)
        .fetch_optional(&mut *tx)
        .await?;
        if known.is_none() {
            return Ok(Err(RerunRefusal::UnknownLaunch));
        }
        let (recent, total): (i64, i64) = sqlx::query_as(
            "select count(*) filter (where created_at > now() - make_interval(secs => $2)), \
             count(*) filter (where status <> 'failed') \
             from launch_reruns where source_launch_id = $1",
        )
        .bind(source_launch_id)
        .bind(RERUN_COOLDOWN_SECONDS as f64)
        .fetch_one(&mut *tx)
        .await?;
        if recent > 0 {
            return Ok(Err(RerunRefusal::Cooldown));
        }
        if total >= MAX_RERUNS_PER_LAUNCH {
            return Ok(Err(RerunRefusal::TooManyForLaunch));
        }
        let hour: i64 = sqlx::query_scalar(
            "select count(*) from launch_reruns where project_id = $1 and created_at > now() - interval '1 hour'",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if hour >= MAX_RERUNS_PER_HOUR {
            return Ok(Err(RerunRefusal::TooManyInHour));
        }
        let id = Uuid::now_v7();
        let external_id = format!("rerun-{id}");
        sqlx::query(
            "insert into launch_reruns (id, project_id, source_launch_id, external_id, provider, mode, requested_by) \
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(project_id)
        .bind(source_launch_id)
        .bind(&external_id)
        .bind(provider)
        .bind(mode)
        .bind(no_nul(by))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok((id, external_id)))
    }

    /// Notes the plan of a rerun.
    pub async fn rerun_plan(&self, id: Uuid, plan_id: Uuid, test_count: i32) -> anyhow::Result<()> {
        sqlx::query("update launch_reruns set plan_id = $2, test_count = $3 where id = $1")
            .bind(id)
            .bind(plan_id)
            .bind(test_count)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Notes how the CI answered.
    pub async fn rerun_done(
        &self,
        id: Uuid,
        error: Option<&str>,
        ci_ref: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update launch_reruns set status = $2, error = $3, ci_ref = $4, \
             dispatched_at = case when $3::text is null then now() end where id = $1",
        )
        .bind(id)
        .bind(if error.is_none() {
            "dispatched"
        } else {
            "failed"
        })
        .bind(error.map(no_nul))
        .bind(ci_ref.map(no_nul))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The reruns of a launch, newest first.
    pub async fn reruns_of(&self, project_id: Uuid, launch_id: Uuid) -> anyhow::Result<Vec<Rerun>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SELECT} where r.project_id = $1 and r.source_launch_id = $2 \
             order by r.created_at desc limit 50"
        )))
        .bind(project_id)
        .bind(launch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(rerun_row).collect::<Result<Vec<_>, _>>()?)
    }

    /// The rerun that started this launch (it carries the rerun's external id).
    pub async fn rerun_that_started(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
    ) -> anyhow::Result<Option<Rerun>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SELECT} where r.project_id = $1 and n.id = $2"
        )))
        .bind(project_id)
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(rerun_row).transpose()?)
    }

    /// The test cases of results of one launch (ids that are not in the launch are skipped).
    pub async fn result_case_ids(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        result_ids: &[Uuid],
    ) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select distinct r.test_case_id from results r join launches l \
             on l.id = r.launch_id and l.created_at = r.launch_created_at \
             where l.project_id = $1 and l.id = $2 and r.id = any($3)",
        )
        .bind(project_id)
        .bind(launch_id)
        .bind(result_ids)
        .fetch_all(&self.pool)
        .await?)
    }
}
