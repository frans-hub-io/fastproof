//! CI dispatches of plan launches and the plan check of their results (§7.4).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::reruns::MAX_RERUNS_PER_HOUR;
use crate::tm::{PlanCheck, PlanDispatch, PlanDispatchList, PlanDispatchStatus, TmError, TmResult};
use crate::{Store, no_nul};

/// CI environments of one plan start, at most.
pub const MAX_CI_ENVIRONMENTS: usize = 10;
/// Seconds between two CI starts of the same plan.
pub const PLAN_DISPATCH_COOLDOWN_SECONDS: i64 = 10;
/// Names of the results that are not in the plan listed in a [`PlanCheck`], at most.
pub const MAX_EXTRA_NAMES: i64 = 20;

/// One dispatch to record: the launch it starts and what is sent.
#[derive(Clone, Debug)]
pub struct NewPlanDispatch {
    pub launch_id: Uuid,
    pub environment: Option<String>,
    pub git_ref: String,
    /// The variables the CI is given, without `FASTPROOF_ENVIRONMENT`.
    pub variables: BTreeMap<String, String>,
}

/// Why dispatches were refused before anything was sent.
#[derive(Debug, PartialEq, Eq)]
pub enum DispatchRefusal {
    Cooldown,
    TooManyInHour,
}

const SELECT: &str = "select d.id, d.plan_id, d.launch_id, l.number as launch_number, d.environment, \
    d.provider, d.status, d.error, d.ci_ref, d.git_ref, d.variables, d.requested_by, d.created_at, \
    d.dispatched_at from plan_dispatches d left join launches l on l.id = d.launch_id ";

fn dispatch_row(r: &sqlx::postgres::PgRow) -> Result<PlanDispatch, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let variables: serde_json::Value = r.try_get("variables")?;
    Ok(PlanDispatch {
        id: r.try_get("id")?,
        plan_id: r.try_get("plan_id")?,
        launch_id: r.try_get("launch_id")?,
        launch_number: r.try_get("launch_number")?,
        environment: r.try_get("environment")?,
        provider: r.try_get("provider")?,
        status: PlanDispatchStatus::parse(&status).unwrap_or(PlanDispatchStatus::Failed),
        error: r.try_get("error")?,
        ci_ref: r.try_get("ci_ref")?,
        git_ref: r.try_get("git_ref")?,
        variables: serde_json::from_value(variables).unwrap_or_default(),
        requested_by: r.try_get("requested_by")?,
        created_at: r.try_get("created_at")?,
        dispatched_at: r.try_get("dispatched_at")?,
    })
}

/// CI triggers of the project in the last hour: reruns and plan dispatches, failed ones too.
pub(crate) async fn triggers_last_hour(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        "select (select count(*) from launch_reruns where project_id = $1 \
                 and created_at > now() - interval '1 hour') \
              + (select count(*) from plan_dispatches where project_id = $1 \
                 and created_at > now() - interval '1 hour')",
    )
    .bind(project_id)
    .fetch_one(conn)
    .await?)
}

async fn refusal(
    conn: &mut PgConnection,
    project_id: Uuid,
    plan_id: Uuid,
    n: usize,
) -> anyhow::Result<Option<DispatchRefusal>> {
    let recent: bool = sqlx::query_scalar(
        "select exists (select 1 from plan_dispatches where plan_id = $1 \
         and created_at > now() - make_interval(secs => $2))",
    )
    .bind(plan_id)
    .bind(PLAN_DISPATCH_COOLDOWN_SECONDS as f64)
    .fetch_one(&mut *conn)
    .await?;
    if recent {
        return Ok(Some(DispatchRefusal::Cooldown));
    }
    if triggers_last_hour(conn, project_id).await? + n as i64 > MAX_RERUNS_PER_HOUR {
        return Ok(Some(DispatchRefusal::TooManyInHour));
    }
    Ok(None)
}

const CHECK_CTES: &str = "with l as (select test_plan_id, created_at from launches where id = $1), \
    plan_cases as (select c.id from test_plan_items i join cases c on c.id = i.case_id \
        where i.plan_id = (select test_plan_id from l) and c.archived_at is null and c.automated), \
    ident as (select tc.id, tc.case_id, tc.full_name from test_cases tc where tc.kind = 'code' and tc.id in \
        (select r.test_case_id from results r, l where r.launch_id = $1 and r.launch_created_at = l.created_at)) ";

/// What a finished launch started from a plan ran compared to the plan (`None` for a launch
/// without a plan, and while the launch is not finished).
pub async fn plan_check(
    conn: &mut PgConnection,
    launch_id: Uuid,
) -> anyhow::Result<Option<PlanCheck>> {
    let head: Option<(Option<Uuid>, Option<DateTime<Utc>>)> =
        sqlx::query_as("select test_plan_id, finished_at from launches where id = $1")
            .bind(launch_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((Some(plan_id), Some(_))) = head else {
        return Ok(None);
    };
    let (in_plan, ran, extra): (i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CHECK_CTES} select (select count(*) from plan_cases), \
         (select count(distinct case_id) from ident where case_id in (select id from plan_cases)), \
         (select count(*) from ident where case_id is null \
            or not exists (select 1 from plan_cases p where p.id = ident.case_id))"
    )))
    .bind(launch_id)
    .fetch_one(&mut *conn)
    .await?;
    let extra_names: Vec<String> = if extra > 0 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "{CHECK_CTES} select full_name from ident where case_id is null \
             or not exists (select 1 from plan_cases p where p.id = ident.case_id) \
             order by full_name limit {MAX_EXTRA_NAMES}"
        )))
        .bind(launch_id)
        .fetch_all(&mut *conn)
        .await?
    } else {
        Vec::new()
    };
    Ok(Some(PlanCheck {
        plan_id,
        in_plan,
        ran,
        extra,
        missing: (in_plan - ran).max(0),
        extra_names,
        warning: extra > 0 || (ran == 0 && in_plan > 0),
    }))
}

impl Store {
    /// [`plan_check`] on a pooled connection (notifications).
    pub async fn plan_check_of(&self, launch_id: Uuid) -> anyhow::Result<Option<PlanCheck>> {
        let mut conn = self.pool.acquire().await?;
        plan_check(&mut conn, launch_id).await
    }

    /// CI triggers (reruns and plan dispatches) of the project in the last hour.
    pub async fn ci_triggers_last_hour(&self, project_id: Uuid) -> anyhow::Result<i64> {
        let mut conn = self.pool.acquire().await?;
        triggers_last_hour(&mut conn, project_id).await
    }

    /// Whether `n` more dispatches of the plan would be allowed now (the answer is advisory:
    /// [`Store::plan_dispatch_begin`] decides under a lock).
    pub async fn plan_dispatch_check(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        n: usize,
    ) -> anyhow::Result<Option<DispatchRefusal>> {
        let mut conn = self.pool.acquire().await?;
        refusal(&mut conn, project_id, plan_id, n).await
    }

    /// Records the dispatches of one plan start in one transaction after the limits (one start
    /// per plan per [`PLAN_DISPATCH_COOLDOWN_SECONDS`], [`MAX_RERUNS_PER_HOUR`] CI triggers per
    /// project and hour, shared with reruns) allow them. Returns the rows as `dispatching`.
    pub async fn plan_dispatch_begin(
        &self,
        project_key: &str,
        project_id: Uuid,
        plan_id: Uuid,
        provider: &str,
        by: &str,
        rows: &[NewPlanDispatch],
    ) -> anyhow::Result<Result<Vec<PlanDispatch>, DispatchRefusal>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('rerun:' || $1::text, 0))")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        if let Some(r) = refusal(&mut tx, project_id, plan_id, rows.len()).await? {
            return Ok(Err(r));
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into plan_dispatches (id, project_id, plan_id, launch_id, environment, \
                 provider, git_ref, variables, requested_by) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(id)
            .bind(project_id)
            .bind(plan_id)
            .bind(row.launch_id)
            .bind(row.environment.as_deref().map(no_nul))
            .bind(provider)
            .bind(no_nul(&row.git_ref))
            .bind(sqlx::types::Json(&row.variables))
            .bind(no_nul(by))
            .execute(&mut *tx)
            .await?;
            let r = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} where d.id = $1")))
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
            let d = dispatch_row(&r)?;
            dispatch_event(&mut tx, project_key, &d).await?;
            out.push(d);
        }
        tx.commit().await?;
        Ok(Ok(out))
    }

    /// Notes how the CI answered (`error` set: failed) and returns the dispatch.
    pub async fn plan_dispatch_done(
        &self,
        project_key: &str,
        id: Uuid,
        error: Option<&str>,
        ci_ref: Option<&str>,
    ) -> anyhow::Result<PlanDispatch> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "update plan_dispatches set status = $2, error = $3, ci_ref = $4, \
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
        .execute(&mut *tx)
        .await?;
        let r = sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} where d.id = $1")))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let d = dispatch_row(&r)?;
        dispatch_event(&mut tx, project_key, &d).await?;
        tx.commit().await?;
        Ok(d)
    }

    /// The dispatches of a plan, newest first. `before` is the `next` of the previous page.
    pub async fn plan_dispatches(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<PlanDispatchList> {
        let known: bool = sqlx::query_scalar(
            "select exists (select 1 from test_plans where project_id = $1 and id = $2)",
        )
        .bind(project_id)
        .bind(plan_id)
        .fetch_one(&self.pool)
        .await?;
        if !known {
            return Err(TmError::NotFound("test plan"));
        }
        let cursor = match before {
            None => None,
            Some(b) => Some(parse_cursor(b).ok_or_else(|| TmError::bad_request("invalid cursor"))?),
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SELECT} where d.project_id = $1 and d.plan_id = $2 \
             and ($3::timestamptz is null or (d.created_at, d.id) < ($3, $4)) \
             order by d.created_at desc, d.id desc limit $5"
        )))
        .bind(project_id)
        .bind(plan_id)
        .bind(cursor.map(|c| c.0))
        .bind(cursor.map(|c| c.1))
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items = rows
            .iter()
            .map(dispatch_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items
                .last()
                .map(|d| format!("{}_{}", d.created_at.timestamp_micros(), d.id))
        } else {
            None
        };
        Ok(PlanDispatchList { items, next })
    }
}

fn parse_cursor(s: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (micros, id) = s.split_once('_')?;
    Some((
        DateTime::from_timestamp_micros(micros.parse().ok()?)?,
        id.parse().ok()?,
    ))
}

async fn dispatch_event(
    conn: &mut PgConnection,
    project_key: &str,
    d: &PlanDispatch,
) -> anyhow::Result<()> {
    crate::tm::cases::case_event(
        conn,
        project_key,
        "planDispatch.updated",
        serde_json::json!({"id": d.id, "planId": d.plan_id, "launchId": d.launch_id,
            "status": d.status.as_str()}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let id = Uuid::now_v7();
        let at = DateTime::from_timestamp_micros(1_795_249_079_123_456).unwrap();
        let c = format!("{}_{id}", at.timestamp_micros());
        assert_eq!(parse_cursor(&c), Some((at, id)));
        assert_eq!(parse_cursor("nope"), None);
        assert_eq!(parse_cursor("1_x"), None);
    }
}
