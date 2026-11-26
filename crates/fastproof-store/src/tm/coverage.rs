//! Requirement coverage over a scope of results (`74-tm-next-interfaces.md` §7.6).
//!
//! A requirement counts with everything under it: the distinct cases (not archived) actively
//! linked to it or to a requirement below it (not archived). The status of a case in the
//! scope:
//! - default: its last result (`cases.last_*`, the newest result over all its identities);
//! - plan (its newest launch, or the launch given), launches (at most
//!   [`MAX_COVERAGE_LAUNCHES`]) or a window (`days`, `branch`, `environment`): the newest
//!   result per parameter set in those launches, and of several parameter sets the worst.
//!
//! The requirement is `uncovered` without cases, `failed` when a case failed (failed, broken,
//! or blocked, which is stored as broken), `passed` when every case passed, `notRun` when none
//! passed (skipped and unknown count as not run), else `partial`. `changedAfterRun`: its
//! content changed after the newest result of its cases.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::cases::CaseCtx;
use super::lookup::requirement_by_number;
use super::requirements::insert_job;
use super::{
    CoverageCounts, CoverageExportFormat, CoveragePage, CoverageScopeKind, CoverageScopeView,
    CoverageStatus, RequirementCoverage, RequirementStatus, TmError, TmJob, TmJobKind, TmResult,
    lock_project,
};
use crate::Store;

/// Launches a `launchIds` scope names, at most.
pub const MAX_COVERAGE_LAUNCHES: usize = 50;
/// Days of a window scope, at most.
pub const MAX_WINDOW_DAYS: i32 = 365;
/// Days of a window scope given only a branch or an environment.
pub const DEFAULT_WINDOW_DAYS: i32 = 30;
/// Bytes of a branch or environment of a window scope.
pub const MAX_SCOPE_TEXT_BYTES: usize = 200;
/// `statement_timeout` of the coverage statements.
pub const COVERAGE_TIMEOUT: &str = "10s";
/// Requirements computed per statement round of an export or a dashboard widget.
const CHUNK: usize = 500;
/// Top-level requirements a dashboard widget counts, at most.
pub const MAX_WIDGET_REQUIREMENTS: i64 = 5000;
/// Coverage exports a caller has queued or running, at most.
pub const MAX_COVERAGE_EXPORTS_PER_CALLER: i64 = 2;
/// Coverage exports a project has queued or running, at most.
pub const MAX_COVERAGE_EXPORTS_PER_PROJECT: i64 = 4;

/// Which results coverage counts (the query of `GET /coverage`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ids: Option<Vec<Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i32>,
}

/// Parameters of a `coverage_export` job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageExportParams {
    pub format: CoverageExportFormat,
    #[serde(default)]
    pub scope: CoverageScope,
    /// sha256 (hex) of the one-time download token.
    pub token_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default)]
    pub downloaded: bool,
}

/// A scope with its launches looked up.
#[derive(Clone, Debug)]
pub enum Resolved {
    Default,
    Launches(Vec<(Uuid, DateTime<Utc>)>),
    Window {
        since: DateTime<Utc>,
        branch: Option<String>,
        environment: Option<String>,
    },
}

fn text(v: &Option<String>, what: &str) -> TmResult<Option<String>> {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.len() > MAX_SCOPE_TEXT_BYTES || s.chars().any(char::is_control) => {
            Err(TmError::bad_request(format!(
                "{what} has at most {MAX_SCOPE_TEXT_BYTES} bytes without control characters"
            )))
        }
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// Checks a scope and looks up its launches: a plan (and one of its launches) and launches
/// must belong to the project (else 404); kinds cannot be mixed (400).
pub async fn resolve(
    conn: &mut PgConnection,
    project_id: Uuid,
    s: &CoverageScope,
) -> TmResult<(Resolved, CoverageScopeView)> {
    let branch = text(&s.branch, "branch")?;
    let environment = text(&s.environment, "environment")?;
    let window = branch.is_some() || environment.is_some() || s.days.is_some();
    let launches = s.launch_ids.as_ref().is_some_and(|l| !l.is_empty());
    let plan = s.plan_id.is_some();
    let kinds = [window, launches, plan || s.launch_id.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if kinds > 1 || (launches && s.launch_id.is_some()) {
        return Err(TmError::bad_request(
            "give one scope: planId (and launchId), launchIds, or days/branch/environment",
        ));
    }
    let mut view = CoverageScopeView {
        kind: CoverageScopeKind::Default,
        plan_id: None,
        launch_ids: None,
        branch: None,
        environment: None,
        days: None,
        launches: 0,
    };
    if plan {
        let plan_id = s.plan_id.unwrap_or_default();
        let found: bool = sqlx::query_scalar(
            "select exists (select 1 from test_plans where project_id = $1 and id = $2)",
        )
        .bind(project_id)
        .bind(plan_id)
        .fetch_one(&mut *conn)
        .await?;
        if !found {
            return Err(TmError::NotFound("test plan"));
        }
        let launch: Option<(Uuid, DateTime<Utc>)> = match s.launch_id {
            Some(l) => Some(
                sqlx::query_as(
                    "select id, created_at from launches where project_id = $1 and id = $2 and test_plan_id = $3",
                )
                .bind(project_id)
                .bind(l)
                .bind(plan_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or(TmError::NotFound("launch"))?,
            ),
            None => {
                sqlx::query_as(
                    "select id, created_at from launches where project_id = $1 and test_plan_id = $2 \
                     order by created_at desc, id desc limit 1",
                )
                .bind(project_id)
                .bind(plan_id)
                .fetch_optional(&mut *conn)
                .await?
            }
        };
        view.kind = CoverageScopeKind::Plan;
        view.plan_id = Some(plan_id);
        view.launch_ids = launch.map(|l| vec![l.0]);
        view.launches = launch.is_some() as i64;
        return Ok((Resolved::Launches(launch.into_iter().collect()), view));
    }
    let ids: Vec<Uuid> = match (&s.launch_ids, s.launch_id) {
        (Some(l), _) if !l.is_empty() => {
            let mut v = l.clone();
            v.sort();
            v.dedup();
            v
        }
        (_, Some(l)) => vec![l],
        _ => Vec::new(),
    };
    if !ids.is_empty() {
        if ids.len() > MAX_COVERAGE_LAUNCHES {
            return Err(TmError::bad_request(format!(
                "launchIds names at most {MAX_COVERAGE_LAUNCHES} launches"
            )));
        }
        let rows: Vec<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "select id, created_at from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        if rows.len() != ids.len() {
            return Err(TmError::NotFound("launch"));
        }
        view.kind = CoverageScopeKind::Launches;
        view.launch_ids = Some(ids);
        view.launches = rows.len() as i64;
        return Ok((Resolved::Launches(rows), view));
    }
    if window {
        let days = s.days.unwrap_or(DEFAULT_WINDOW_DAYS);
        if !(1..=MAX_WINDOW_DAYS).contains(&days) {
            return Err(TmError::bad_request(format!(
                "days must be 1 to {MAX_WINDOW_DAYS}"
            )));
        }
        let since = Utc::now() - chrono::Duration::days(i64::from(days));
        let n: i64 = sqlx::query_scalar(
            "select count(*) from launches where project_id = $1 and not debug and created_at >= $2 \
             and ($3::text is null or branch = $3) and ($4::text is null or env_key = $4)",
        )
        .bind(project_id)
        .bind(since)
        .bind(&branch)
        .bind(&environment)
        .fetch_one(&mut *conn)
        .await?;
        view.kind = CoverageScopeKind::Window;
        view.days = Some(days);
        view.branch = branch.clone();
        view.environment = environment.clone();
        view.launches = n;
        return Ok((
            Resolved::Window {
                since,
                branch,
                environment,
            },
            view,
        ));
    }
    Ok((Resolved::Default, view))
}

/// A requirement whose coverage is computed (with everything under it).
#[derive(Clone, Debug)]
pub struct Root {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    pub req_type: String,
    pub status: RequirementStatus,
    pub depth: i32,
    pub has_children: bool,
    pub content_changed_at: DateTime<Utc>,
    pub parent_number: Option<i64>,
    pub issue_key: Option<String>,
    pub external_state: Option<String>,
    /// Numbers from the top down to this one (tree order of exports).
    pub path_numbers: Vec<i64>,
}

const ROOT_SELECT: &str = "select r.id, r.number, r.title, r.type, r.status, r.depth, r.content_changed_at, \
    r.issue_key, r.external_state, p.number as parent_number, \
    exists (select 1 from requirements ch where ch.parent_id = r.id and ch.archived_at is null) as has_children, \
    array(select a.number from unnest(r.path_ids) with ordinality as x(id, ord) \
      join requirements a on a.id = x.id order by x.ord) as path_numbers \
    from requirements r left join requirements p on p.id = r.parent_id ";

fn root_of(r: &sqlx::postgres::PgRow) -> sqlx::Result<Root> {
    let status: String = r.try_get("status")?;
    Ok(Root {
        id: r.try_get("id")?,
        number: r.try_get("number")?,
        title: r.try_get("title")?,
        req_type: r.try_get("type")?,
        status: RequirementStatus::parse(&status).unwrap_or(RequirementStatus::Active),
        depth: r.try_get::<i16, _>("depth")?.into(),
        has_children: r.try_get("has_children")?,
        content_changed_at: r.try_get("content_changed_at")?,
        parent_number: r.try_get("parent_number")?,
        issue_key: r.try_get("issue_key")?,
        external_state: r.try_get("external_state")?,
        path_numbers: r.try_get("path_numbers")?,
    })
}

/// How a result status counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Passed,
    Skipped,
    Failed,
}

fn outcome(status: &str) -> Outcome {
    match status {
        "passed" => Outcome::Passed,
        "failed" | "broken" | "blocked" => Outcome::Failed,
        _ => Outcome::Skipped,
    }
}

/// The coverage status of counts.
pub fn status_of(c: &CoverageCounts) -> CoverageStatus {
    if c.total == 0 {
        CoverageStatus::Uncovered
    } else if c.failed > 0 {
        CoverageStatus::Failed
    } else if c.passed == c.total {
        CoverageStatus::Passed
    } else if c.passed == 0 {
        CoverageStatus::NotRun
    } else {
        CoverageStatus::Partial
    }
}

async fn set_timeout(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "set local statement_timeout = '{COVERAGE_TIMEOUT}'"
    )))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The statuses (outcome and time) of cases in a scope.
async fn case_outcomes(
    conn: &mut PgConnection,
    project_id: Uuid,
    scope: &Resolved,
    cases: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, (Outcome, DateTime<Utc>)>> {
    let mut out: HashMap<Uuid, (Outcome, DateTime<Utc>)> = HashMap::new();
    if cases.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(Uuid, String, DateTime<Utc>)> = match scope {
        Resolved::Default => {
            sqlx::query_as(
                "select id, last_status, last_status_at from cases \
                 where project_id = $1 and id = any($2) and last_status is not null and last_status_at is not null",
            )
            .bind(project_id)
            .bind(cases)
            .fetch_all(&mut *conn)
            .await?
        }
        Resolved::Launches(ls) => {
            if ls.is_empty() {
                return Ok(out);
            }
            let ids: Vec<Uuid> = ls.iter().map(|l| l.0).collect();
            let created: Vec<DateTime<Utc>> = ls.iter().map(|l| l.1).collect();
            sqlx::query_as(
                "select distinct on (t.case_id, s.parameters_hash) t.case_id, r.status, \
                   coalesce(r.stopped_at, r.updated_at) as at \
                 from results r join test_cases t on t.id = r.test_case_id join series s on s.id = r.series_id \
                 where r.project_id = $1 and r.launch_id = any($2) and r.launch_created_at = any($3) \
                   and t.case_id = any($4) \
                 order by t.case_id, s.parameters_hash, coalesce(r.stopped_at, r.updated_at) desc, r.id desc",
            )
            .bind(project_id)
            .bind(&ids)
            .bind(&created)
            .bind(cases)
            .fetch_all(&mut *conn)
            .await?
        }
        Resolved::Window {
            since,
            branch,
            environment,
        } => {
            sqlx::query_as(
                "select distinct on (t.case_id, s.parameters_hash) t.case_id, r.status, \
                   coalesce(r.stopped_at, r.updated_at) as at \
                 from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
                   join test_cases t on t.id = r.test_case_id join series s on s.id = r.series_id \
                 where l.project_id = $1 and not l.debug and l.created_at >= $2 \
                   and ($3::text is null or l.branch = $3) and ($4::text is null or l.env_key = $4) \
                   and r.launch_created_at >= $2 and t.case_id = any($5) \
                 order by t.case_id, s.parameters_hash, coalesce(r.stopped_at, r.updated_at) desc, r.id desc",
            )
            .bind(project_id)
            .bind(since)
            .bind(branch)
            .bind(environment)
            .bind(cases)
            .fetch_all(&mut *conn)
            .await?
        }
    };
    for (case_id, status, at) in rows {
        let o = outcome(&status);
        out.entry(case_id)
            .and_modify(|(w, t)| {
                *w = (*w).max(o);
                *t = (*t).max(at);
            })
            .or_insert((o, at));
    }
    Ok(out)
}

/// Coverage of each root with everything under it.
pub async fn coverage_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    scope: &Resolved,
    roots: &[Root],
) -> sqlx::Result<Vec<RequirementCoverage>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = roots.iter().map(|r| r.id).collect();
    let pairs: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select distinct x.root, l.case_id from unnest($2::uuid[]) as x(root) \
         join requirements d on d.project_id = $1 and d.archived_at is null and d.path_ids @> array[x.root] \
         join requirement_links l on l.requirement_id = d.id and l.status = 'active' \
         join cases c on c.id = l.case_id and c.archived_at is null",
    )
    .bind(project_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_root: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    let mut cases: Vec<Uuid> = Vec::with_capacity(pairs.len());
    for (root, case) in pairs {
        by_root.entry(root).or_default().push(case);
        cases.push(case);
    }
    cases.sort();
    cases.dedup();
    let outcomes = case_outcomes(conn, project_id, scope, &cases).await?;
    Ok(roots
        .iter()
        .map(|r| {
            let mut c = CoverageCounts::default();
            let mut newest: Option<DateTime<Utc>> = None;
            for case in by_root.get(&r.id).map(Vec::as_slice).unwrap_or_default() {
                c.total += 1;
                match outcomes.get(case) {
                    None => c.not_run += 1,
                    Some((o, at)) => {
                        newest = newest.max(Some(*at));
                        match o {
                            Outcome::Passed => c.passed += 1,
                            Outcome::Failed => c.failed += 1,
                            Outcome::Skipped => c.skipped += 1,
                        }
                    }
                }
            }
            RequirementCoverage {
                number: r.number,
                title: r.title.clone(),
                req_type: r.req_type.clone(),
                status: r.status,
                depth: r.depth,
                has_children: r.has_children,
                coverage: status_of(&c),
                cases: c,
                changed_after_run: newest.is_some_and(|n| r.content_changed_at > n),
            }
        })
        .collect())
}

/// Coverage of all requirements (each with everything under it), in tree order, for exports.
#[derive(Clone, Debug)]
pub struct CoverageRow {
    pub root: Root,
    pub coverage: RequirementCoverage,
}

/// Counts of top-level requirements by coverage status (dashboard widget).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageStatusCounts {
    pub uncovered: i64,
    pub failed: i64,
    pub not_run: i64,
    pub partial: i64,
    pub passed: i64,
}

impl CoverageStatusCounts {
    pub fn add(&mut self, s: CoverageStatus) {
        match s {
            CoverageStatus::Uncovered => self.uncovered += 1,
            CoverageStatus::Failed => self.failed += 1,
            CoverageStatus::NotRun => self.not_run += 1,
            CoverageStatus::Partial => self.partial += 1,
            CoverageStatus::Passed => self.passed += 1,
        }
    }
}

/// Coverage of the top-level requirements of a project for a dashboard widget.
#[derive(Clone, Debug)]
pub struct CoverageSummary {
    pub scope: CoverageScopeView,
    pub total: i64,
    pub counts: CoverageStatusCounts,
    pub items: Vec<RequirementCoverage>,
}

fn number_cursor(before: Option<&str>) -> TmResult<Option<i64>> {
    match before {
        None => Ok(None),
        Some(b) => Ok(Some(
            b.parse::<i64>()
                .ok()
                .filter(|v| *v >= 0)
                .ok_or_else(|| TmError::bad_request("invalid cursor"))?,
        )),
    }
}

impl Store {
    /// A page of coverage: the direct children of `parent` (top-level requirements when
    /// `None`), not archived, by number, each with everything under it.
    pub async fn requirement_coverage(
        &self,
        project_id: Uuid,
        scope: &CoverageScope,
        parent: Option<i64>,
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<CoveragePage> {
        let after = number_cursor(before)?;
        let mut tx = self.pool.begin().await?;
        set_timeout(&mut tx).await?;
        let (resolved, view) = resolve(&mut tx, project_id, scope).await?;
        let parent_id = match parent {
            Some(n) => Some(
                requirement_by_number(&mut tx, project_id, n)
                    .await?
                    .ok_or(TmError::NotFound("requirement"))?
                    .id,
            ),
            None => None,
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{ROOT_SELECT} where r.project_id = $1 and r.archived_at is null \
               and r.parent_id is not distinct from $2 and ($3::bigint is null or r.number > $3) \
             order by r.number limit $4"
        )))
        .bind(project_id)
        .bind(parent_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut roots: Vec<Root> = rows.iter().map(root_of).collect::<sqlx::Result<_>>()?;
        let more = roots.len() as i64 > limit;
        roots.truncate(limit.max(0) as usize);
        let next = more
            .then(|| roots.last().map(|r| r.number.to_string()))
            .flatten();
        let items = coverage_of(&mut tx, project_id, &resolved, &roots).await?;
        tx.commit().await?;
        Ok(CoveragePage {
            scope: view,
            items,
            next,
        })
    }

    /// The coverage of the top-level requirements (at most [`MAX_WIDGET_REQUIREMENTS`]): counts
    /// by status and the first `limit` by number.
    pub async fn coverage_summary(
        &self,
        project_id: Uuid,
        scope: &CoverageScope,
        limit: usize,
    ) -> TmResult<CoverageSummary> {
        let mut tx = self.pool.begin().await?;
        set_timeout(&mut tx).await?;
        let (resolved, view) = resolve(&mut tx, project_id, scope).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{ROOT_SELECT} where r.project_id = $1 and r.archived_at is null and r.parent_id is null \
             order by r.number limit $2"
        )))
        .bind(project_id)
        .bind(MAX_WIDGET_REQUIREMENTS)
        .fetch_all(&mut *tx)
        .await?;
        let roots: Vec<Root> = rows.iter().map(root_of).collect::<sqlx::Result<_>>()?;
        let mut counts = CoverageStatusCounts::default();
        let mut items = Vec::new();
        for chunk in roots.chunks(CHUNK) {
            for c in coverage_of(&mut tx, project_id, &resolved, chunk).await? {
                counts.add(c.coverage);
                if items.len() < limit {
                    items.push(c);
                }
            }
        }
        tx.commit().await?;
        Ok(CoverageSummary {
            scope: view,
            total: roots.len() as i64,
            counts,
            items,
        })
    }

    /// Coverage of every requirement that is not archived (at most `max`), each with everything
    /// under it, in tree order (by the numbers along its path).
    pub async fn coverage_rows(
        &self,
        project_id: Uuid,
        scope: &CoverageScope,
        max: usize,
    ) -> TmResult<(CoverageScopeView, Vec<CoverageRow>)> {
        let mut tx = self.pool.begin().await?;
        set_timeout(&mut tx).await?;
        let (resolved, view) = resolve(&mut tx, project_id, scope).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{ROOT_SELECT} where r.project_id = $1 and r.archived_at is null order by r.number limit $2"
        )))
        .bind(project_id)
        .bind(max as i64)
        .fetch_all(&mut *tx)
        .await?;
        let mut roots: Vec<Root> = rows.iter().map(root_of).collect::<sqlx::Result<_>>()?;
        roots.sort_by(|a, b| a.path_numbers.cmp(&b.path_numbers));
        let mut out = Vec::with_capacity(roots.len());
        for chunk in roots.chunks(CHUNK) {
            let cov = coverage_of(&mut tx, project_id, &resolved, chunk).await?;
            out.extend(
                chunk
                    .iter()
                    .cloned()
                    .zip(cov)
                    .map(|(root, coverage)| CoverageRow { root, coverage }),
            );
        }
        tx.commit().await?;
        Ok((view, out))
    }

    /// Queues a coverage export after checking its scope. A caller has at most
    /// [`MAX_COVERAGE_EXPORTS_PER_CALLER`] and a project [`MAX_COVERAGE_EXPORTS_PER_PROJECT`]
    /// queued or running (409 `export_busy`).
    pub async fn start_coverage_export(
        &self,
        ctx: &CaseCtx<'_>,
        owner: &str,
        id: Uuid,
        params: &CoverageExportParams,
    ) -> TmResult<TmJob> {
        let mut tx = self.pool.begin().await?;
        resolve(&mut tx, ctx.project_id, &params.scope).await?;
        lock_project(&mut tx, ctx.project_id, "coverage-exports").await?;
        let (mine, all): (i64, i64) = sqlx::query_as(
            "select count(*) filter (where coalesce(owner_key, created_by) = $2), count(*) from tm_jobs \
             where project_id = $1 and kind = 'coverage_export' and status in ('queued', 'running')",
        )
        .bind(ctx.project_id)
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
        let over = if mine >= MAX_COVERAGE_EXPORTS_PER_CALLER {
            Some(("exportsPerCaller", MAX_COVERAGE_EXPORTS_PER_CALLER))
        } else if all >= MAX_COVERAGE_EXPORTS_PER_PROJECT {
            Some(("exportsPerProject", MAX_COVERAGE_EXPORTS_PER_PROJECT))
        } else {
            None
        };
        if let Some((limit, max)) = over {
            return Err(TmError::Conflict {
                code: "export_busy",
                message: format!(
                    "{limit} ({max}) coverage exports are queued or running; retry when one is done"
                ),
                details: Some(json!({"limit": limit, "max": max})),
            });
        }
        let job = insert_job(
            &mut tx,
            ctx,
            id,
            TmJobKind::CoverageExport,
            &serde_json::to_value(params)?,
            owner,
        )
        .await?;
        tx.commit().await?;
        Ok(job)
    }

    /// Marks a finished coverage export as downloaded once: its blob key only for the first
    /// call by its creator with the right token, within a day of its end.
    pub async fn take_coverage_download(
        &self,
        project_id: Uuid,
        id: Uuid,
        owner: &str,
        token_sha256: &str,
    ) -> anyhow::Result<Option<(String, CoverageExportFormat)>> {
        let r: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "update tm_jobs set params = params || '{\"downloaded\": true}'::jsonb \
             where project_id = $1 and id = $2 and kind = 'coverage_export' and status = 'done' \
               and coalesce(owner_key, created_by) = $3 and params ->> 'tokenSha256' = $4 \
               and coalesce((params ->> 'downloaded')::boolean, false) = false \
               and finished_at > now() - make_interval(hours => $5) \
             returning params ->> 'blob', params ->> 'format'",
        )
        .bind(project_id)
        .bind(id)
        .bind(owner)
        .bind(token_sha256)
        .bind(super::import::EXPORT_TTL_HOURS as i32)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.and_then(|(blob, format)| Some((blob?, CoverageExportFormat::parse(&format?)?))))
    }

    /// Blobs of coverage exports that are over (failed, cancelled, downloaded or older than a
    /// day). The caller deletes them, then calls [`Store::forget_tm_job_blobs`].
    pub async fn expired_coverage_export_blobs(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, Vec<String>)>> {
        let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "select id, params ->> 'blob' from tm_jobs where kind = 'coverage_export' and params ? 'blob' \
             and (status in ('failed', 'cancelled') or finished_at < now() - make_interval(hours => $1) \
               or coalesce((params ->> 'downloaded')::boolean, false)) \
             order by created_at limit $2",
        )
        .bind(super::import::EXPORT_TTL_HOURS as i32)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, b)| (id, b.into_iter().collect()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(total: i64, passed: i64, failed: i64, skipped: i64) -> CoverageCounts {
        CoverageCounts {
            total,
            passed,
            failed,
            not_run: total - passed - failed - skipped,
            skipped,
        }
    }

    #[test]
    fn status_rules() {
        assert_eq!(status_of(&counts(0, 0, 0, 0)), CoverageStatus::Uncovered);
        assert_eq!(status_of(&counts(3, 2, 1, 0)), CoverageStatus::Failed);
        assert_eq!(status_of(&counts(3, 3, 0, 0)), CoverageStatus::Passed);
        assert_eq!(status_of(&counts(3, 0, 0, 0)), CoverageStatus::NotRun);
        assert_eq!(status_of(&counts(3, 0, 0, 3)), CoverageStatus::NotRun);
        assert_eq!(status_of(&counts(3, 1, 0, 1)), CoverageStatus::Partial);
        assert_eq!(status_of(&counts(3, 2, 0, 0)), CoverageStatus::Partial);
    }

    #[test]
    fn worst_outcome_wins() {
        assert!(outcome("failed") > outcome("skipped"));
        assert!(outcome("broken") > outcome("passed"));
        assert!(outcome("unknown") > outcome("passed"));
        assert_eq!(outcome("blocked"), Outcome::Failed);
    }
}
