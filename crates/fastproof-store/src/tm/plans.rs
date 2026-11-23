//! Managed test plans: explicit items plus items added by a stored [`CaseFilter`], sync of the
//! filter with a diff, `testplan.json` computed live from the plan's automated cases, and
//! launches started from a plan with run items for its manual cases.
//!
//! Adhoc plans (stage 1, `analysis/plans.rs`) keep their stored `tests` and expire; managed
//! plans never expire and are archived instead.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    CaseFilter, FolderCounts, PlanAssign, StartPlanLaunch, TestPlan, TestPlanItem,
    TestPlanItemsChange, TestPlanKind, TestPlanWrite, TmError, TmLimits, TmResult, check_name,
    double_option, lock_project,
};
use crate::analysis::plans::{TestPlanJson, TestPlanTest};
use crate::launches::{LaunchStart, like_escape};
use crate::{Launch, Page, Project, Store, json_no_nul, no_nul};

/// Bytes of an assignee, at most.
pub const MAX_ASSIGNEE_BYTES: usize = 200;
/// Code identities read for one `testplan.json`, at most.
pub const MAX_TESTPLAN_IDENTITIES: i64 = 100_000;
/// Case numbers listed in a "not found" error, at most.
const MAX_LISTED: usize = 100;

/// Body of `PATCH /test-plans/{id}` (with `If-Match`): every field optional, `filter: null`
/// removes the filter (the items it added stay until the next sync).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPlanPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<CaseFilter>)]
    pub filter: Option<Option<CaseFilter>>,
}

/// Filters of the item list of a plan.
#[derive(Clone, Debug, Default)]
pub struct PlanItemFilter {
    /// Cases in this folder or its subfolders.
    pub folder: Option<Uuid>,
    /// `Some(None)`: unassigned items.
    pub assignee: Option<Option<String>>,
    pub before: Option<String>,
    pub limit: i64,
}

/// Filters of the managed plan list.
#[derive(Clone, Debug, Default)]
pub struct PlanListFilter {
    pub q: Option<String>,
    /// `true`: archived plans only; otherwise plans that are not archived.
    pub archived: bool,
    pub before: Option<String>,
    pub limit: i64,
}

/// A launch started from a plan.
#[derive(Clone, Debug)]
pub struct StartedPlanLaunch {
    pub launch: Launch,
    /// `false` when the external id named a launch of this plan that already existed.
    pub created: bool,
    pub run_items: i64,
    /// The plan has automated cases (CI runs them from `testplan.json`).
    pub automated: i64,
}

fn bad(message: impl Into<String>) -> TmError {
    TmError::bad_request(message)
}

/// An assignee: 1 to [`MAX_ASSIGNEE_BYTES`] bytes, no control characters.
pub fn check_assignee(a: &str) -> TmResult<String> {
    check_name("assignee", a, MAX_ASSIGNEE_BYTES)
}

/// Ids of the cases (not archived) matching a filter, at most `cap`, by number: the case
/// list's filter (`cases::push_filter`), so plans and the case list match the same cases.
/// The caller checks the filter with `cases::check_filter` and `cases::check_filter_folders`.
pub async fn filter_case_ids(
    conn: &mut PgConnection,
    project_id: Uuid,
    f: &CaseFilter,
    cap: usize,
) -> sqlx::Result<Vec<Uuid>> {
    let f = plan_filter(f.clone());
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "select c.id from cases c where c.archived_at is null and c.project_id = ",
    );
    qb.push_bind(project_id);
    super::cases::push_filter(&mut qb, project_id, &f);
    qb.push(" order by c.number limit ").push_bind(cap as i64);
    qb.build_query_scalar().fetch_all(&mut *conn).await
}

/// A plan filter with its defaults: folders include their subfolders unless `recursive: false`
/// (the case list's filter defaults to `false`; a plan's folder means "this folder and below").
pub fn plan_filter(mut f: CaseFilter) -> CaseFilter {
    if f.folder_ids.as_ref().is_some_and(|v| !v.is_empty()) && f.recursive.is_none() {
        f.recursive = Some(true);
    }
    f
}

/// Resolves case numbers of the project (not archived); unknown or archived numbers are a 400
/// `bad_request` with `details {unknownCases}` (the first [`MAX_LISTED`]).
pub(crate) async fn resolve_numbers(
    conn: &mut PgConnection,
    project_id: Uuid,
    numbers: &[i64],
) -> TmResult<Vec<Uuid>> {
    let mut wanted: Vec<i64> = numbers.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "select id, number from cases where project_id = $1 and number = any($2) \
         and archived_at is null order by number",
    )
    .bind(project_id)
    .bind(&wanted)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() != wanted.len() {
        let found: BTreeSet<i64> = rows.iter().map(|r| r.1).collect();
        let missing: Vec<i64> = wanted
            .iter()
            .filter(|n| !found.contains(n))
            .take(MAX_LISTED)
            .copied()
            .collect();
        return Err(TmError::Invalid {
            code: "bad_request",
            message: format!(
                "unknown or archived cases: {}",
                missing
                    .iter()
                    .map(|n| format!("TC-{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            details: Some(serde_json::json!({"unknownCases": missing})),
        });
    }
    Ok(rows.into_iter().map(|(id, _)| id).collect())
}

const PLAN_COLS: &str = "p.id, p.kind, p.name, p.description, p.filter, p.revision, p.created_by, \
    p.created_at, p.updated_at, p.updated_by, p.archived_at, p.test_count, \
    coalesce(k.total, 0) as total, coalesce(k.manual, 0) as manual, coalesce(k.automated, 0) as automated";

const PLAN_FROM: &str = " from test_plans p left join lateral (select count(*) as total, \
    count(*) filter (where not cs.automated) as manual, count(*) filter (where cs.automated) as automated \
    from test_plan_items i join cases cs on cs.id = i.case_id \
    where i.plan_id = p.id and cs.archived_at is null) k on p.kind = 'managed' ";

/// `number, name, folder_id, automated, assignee, via_filter, position` of a plan item.
type PlanItemRow = (i64, String, Option<Uuid>, bool, Option<String>, bool, i32);

/// A plan row and who changed it last.
pub(crate) struct PlanRow {
    pub plan: TestPlan,
    pub updated_by: Option<String>,
}

fn plan_row(r: &PgRow) -> sqlx::Result<PlanRow> {
    let kind: String = r.try_get("kind")?;
    let kind = TestPlanKind::parse(&kind).unwrap_or(TestPlanKind::Adhoc);
    let filter: Option<serde_json::Value> = r.try_get("filter")?;
    let archived_at: Option<DateTime<Utc>> = r.try_get("archived_at")?;
    let counts = match kind {
        TestPlanKind::Managed => FolderCounts {
            total: r.try_get("total")?,
            manual: r.try_get("manual")?,
            automated: r.try_get("automated")?,
        },
        TestPlanKind::Adhoc => {
            let n = r.try_get::<i32, _>("test_count")? as i64;
            FolderCounts {
                total: n,
                manual: 0,
                automated: n,
            }
        }
    };
    Ok(PlanRow {
        plan: TestPlan {
            id: r.try_get("id")?,
            kind,
            name: r.try_get("name")?,
            description: r.try_get("description")?,
            filter: filter.and_then(|f| serde_json::from_value(f).ok()),
            counts,
            revision: r.try_get("revision")?,
            created_by: r.try_get("created_by")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            archived: archived_at.is_some(),
        },
        updated_by: r.try_get("updated_by")?,
    })
}

/// A plan of the project (adhoc or managed).
pub(crate) async fn plan_in(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
    lock: bool,
) -> sqlx::Result<Option<PlanRow>> {
    if lock {
        let found: Option<Uuid> = sqlx::query_scalar(
            "select id from test_plans where project_id = $1 and id = $2 for update",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        if found.is_none() {
            return Ok(None);
        }
    }
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {PLAN_COLS}{PLAN_FROM} where p.project_id = $1 and p.id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(plan_row).transpose()
}

/// A managed plan of the project, locked; archived plans are a 409 `plan_archived` when
/// `allow_archived` is false.
async fn managed_for_change(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
    allow_archived: bool,
) -> TmResult<PlanRow> {
    let row = plan_in(conn, project_id, id, true)
        .await?
        .ok_or(TmError::NotFound("test plan"))?;
    if row.plan.kind != TestPlanKind::Managed {
        return Err(TmError::conflict(
            "plan_not_managed",
            "only managed plans can be changed",
        ));
    }
    if row.plan.archived && !allow_archived {
        return Err(TmError::conflict("plan_archived", "the plan is archived"));
    }
    Ok(row)
}

async fn plan_size(conn: &mut PgConnection, plan_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("select count(*) from test_plan_items where plan_id = $1")
        .bind(plan_id)
        .fetch_one(&mut *conn)
        .await
}

/// Adds cases to a plan (explicit unless `via_filter`); an explicit add of a case the filter
/// added makes it explicit. Returns the cases added.
async fn add_items(
    conn: &mut PgConnection,
    plan_id: Uuid,
    case_ids: &[Uuid],
    via_filter: bool,
    max_plan_cases: usize,
) -> TmResult<Vec<Uuid>> {
    if case_ids.is_empty() {
        return Ok(Vec::new());
    }
    let added: Vec<Uuid> = sqlx::query_scalar(
        "with base as (select coalesce(max(position), 0) as p from test_plan_items where plan_id = $1) \
         insert into test_plan_items (plan_id, case_id, position, via_filter) \
         select $1, t.c, base.p + t.o::int, $3 from unnest($2::uuid[]) with ordinality as t(c, o), base \
         on conflict (plan_id, case_id) do update set via_filter = false \
           where test_plan_items.via_filter and not excluded.via_filter \
         returning case_id",
    )
    .bind(plan_id)
    .bind(case_ids)
    .bind(via_filter)
    .fetch_all(&mut *conn)
    .await?;
    if plan_size(conn, plan_id).await? as usize > max_plan_cases {
        return Err(TmError::limit("maxPlanCases", max_plan_cases as u64));
    }
    Ok(added)
}

async fn touch_plan(conn: &mut PgConnection, plan_id: Uuid, by: &str) -> sqlx::Result<()> {
    sqlx::query(
        "update test_plans set revision = revision + 1, updated_at = now(), updated_by = $2 where id = $1",
    )
    .bind(plan_id)
    .bind(no_nul(by))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn encode_cursor(parts: &[String]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(parts.join("\n"))
}

fn decode_cursor(c: &str, n: usize) -> TmResult<Vec<String>> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .map_err(|_| bad("invalid cursor"))?;
    let s = String::from_utf8(raw).map_err(|_| bad("invalid cursor"))?;
    let parts: Vec<String> = s.split('\n').map(str::to_string).collect();
    if parts.len() != n {
        return Err(bad("invalid cursor"));
    }
    Ok(parts)
}

/// The managed testplan of a plan: every automated case of the plan → its code identities →
/// `{id: ALLURE_ID when the code has one, selector: full name}`; muted ones also in
/// `mutedTests`. A plan without automated cases is a 409 `plan_has_no_automated_cases`.
pub async fn managed_testplan(conn: &mut PgConnection, plan_id: Uuid) -> TmResult<TestPlanJson> {
    let rows: Vec<(String, Option<String>, Option<String>, bool)> = sqlx::query_as(
        "select tc.full_name, tc.explicit_id, tc.explicit_id_source, \
           exists (select 1 from series s join series_mutes m on m.series_id = s.id \
             where s.test_case_id = tc.id and (m.until is null or m.until > now())) as muted \
         from test_plan_items i join cases c on c.id = i.case_id \
           join test_cases tc on tc.case_id = c.id and tc.kind = 'code' \
         where i.plan_id = $1 and c.archived_at is null and c.automated \
         limit $2",
    )
    .bind(plan_id)
    .bind(MAX_TESTPLAN_IDENTITIES)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Err(TmError::conflict(
            "plan_has_no_automated_cases",
            "the plan has no automated cases (an empty testplan.json would run every test)",
        ));
    }
    let mut tests: BTreeMap<(String, Option<String>), bool> = BTreeMap::new();
    for (full_name, explicit, source, muted) in rows {
        let id = explicit.filter(|_| source.as_deref() == Some("allure_id"));
        let e = tests.entry((full_name, id)).or_insert(false);
        *e |= muted;
    }
    let mut out = Vec::with_capacity(tests.len());
    let mut muted = Vec::new();
    for ((selector, id), m) in tests {
        let t = TestPlanTest { id, selector };
        if m {
            muted.push(t.clone());
        }
        out.push(t);
    }
    Ok(TestPlanJson {
        version: "1.0".into(),
        tests: out,
        muted_tests: (!muted.is_empty()).then_some(muted),
    })
}

/// A shell word: kept as is when it is made of safe characters, otherwise single-quoted.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':' | b'@')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `maxOpenPlanLaunches` for `creator` in a project, inside the transaction that opens one more
/// plan launch: an advisory lock on (project, creator) until the commit makes concurrent starts
/// and reopens of the same creator count one after another.
pub(crate) async fn check_open_plan_launches(
    tx: &mut PgConnection,
    project_id: Uuid,
    creator: &str,
    limits: &TmLimits,
) -> TmResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 7902))")
        .bind(project_id.to_string())
        .bind(no_nul(creator))
        .execute(&mut *tx)
        .await?;
    let open: i64 = sqlx::query_scalar(
        "select count(*) from launches where project_id = $1 and test_plan_id is not null \
         and manual_state = 'open' and created_by = $2",
    )
    .bind(project_id)
    .bind(no_nul(creator))
    .fetch_one(&mut *tx)
    .await?;
    if open as usize >= limits.max_open_plan_launches {
        return Err(TmError::limit(
            "maxOpenPlanLaunches",
            limits.max_open_plan_launches as u64,
        ));
    }
    Ok(())
}

impl Store {
    /// Launches started from a plan of the project, newest first: numbers below `before`, at
    /// most `limit`. 404 when the plan is not in the project.
    pub async fn plan_launches(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        before: Option<i64>,
        limit: i64,
    ) -> TmResult<crate::Page<crate::Launch>> {
        let mut conn = self.pool.acquire().await?;
        plan_in(&mut conn, project_id, plan_id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{} where l.project_id = $1 and l.test_plan_id = $2 and ($3::bigint is null or l.number < $3) \
             order by l.number desc limit $4",
            crate::launches::LAUNCH_SELECT
        )))
        .bind(project_id)
        .bind(plan_id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let mut items: Vec<crate::Launch> = rows
            .iter()
            .map(crate::launches::launch_row)
            .collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|l| l.number.to_string())
        } else {
            None
        };
        Ok(crate::Page { items, next })
    }

    /// Creates a managed plan: explicit cases (`caseNumbers`), then the cases of its filter.
    pub async fn create_managed_plan(
        &self,
        project_id: Uuid,
        w: &TestPlanWrite,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<TestPlan> {
        let name = check_name("name", &w.name, limits.max_name_bytes)?;
        let description = w.description.clone().unwrap_or_default();
        if description.len() > limits.max_markdown_bytes {
            return Err(TmError::Invalid {
                code: "invalid_markdown",
                message: format!(
                    "description has at most {} bytes",
                    limits.max_markdown_bytes
                ),
                details: Some(
                    serde_json::json!({"path": "description", "max": limits.max_markdown_bytes}),
                ),
            });
        }
        let filter = w.filter.clone().flatten().map(plan_filter);
        if let Some(f) = &filter {
            super::cases::check_filter(f)?;
        }
        let numbers = w.case_numbers.clone().unwrap_or_default();
        if numbers.len() > limits.max_bulk {
            return Err(TmError::limit("maxBulk", limits.max_bulk as u64));
        }
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "plans").await?;
        if let Some(f) = &filter {
            super::cases::check_filter_folders(&mut tx, project_id, f).await?;
        }
        let explicit = resolve_numbers(&mut tx, project_id, &numbers).await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into test_plans (id, project_id, name, kind, description, filter, created_by, updated_at, updated_by) \
             values ($1, $2, $3, 'managed', $4, $5, $6, now(), $6)",
        )
        .bind(id)
        .bind(project_id)
        .bind(no_nul(&name))
        .bind(no_nul(&description))
        .bind(
            filter
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?
                .map(json_no_nul),
        )
        .bind(no_nul(by))
        .execute(&mut *tx)
        .await?;
        add_items(&mut tx, id, &explicit, false, limits.max_plan_cases).await?;
        if let Some(f) = &filter {
            let matched =
                filter_case_ids(&mut tx, project_id, f, limits.max_plan_cases + 1).await?;
            add_items(&mut tx, id, &matched, true, limits.max_plan_cases).await?;
        }
        let plan = plan_in(&mut tx, project_id, id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        tx.commit().await?;
        Ok(plan.plan)
    }

    /// A plan of the project (adhoc or managed).
    pub async fn test_plan(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<TestPlan>> {
        let mut conn = self.pool.acquire().await?;
        Ok(plan_in(&mut conn, project_id, id, false)
            .await?
            .map(|r| r.plan))
    }

    /// Managed plans, newest first.
    pub async fn managed_plans(
        &self,
        project_id: Uuid,
        f: &PlanListFilter,
    ) -> TmResult<Page<TestPlan>> {
        let mut qb: QueryBuilder<Postgres> =
            QueryBuilder::new(format!("select {PLAN_COLS}{PLAN_FROM}"));
        qb.push("where p.project_id = ")
            .push_bind(project_id)
            .push(" and p.kind = 'managed' and p.archived_at is ")
            .push(if f.archived { "not null" } else { "null" });
        if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            qb.push(" and p.name ilike ")
                .push_bind(format!("%{}%", like_escape(q)));
        }
        if let Some(b) = &f.before {
            let parts = decode_cursor(b, 1)?;
            let id: Uuid = parts[0].parse().map_err(|_| bad("invalid cursor"))?;
            qb.push(" and p.id < ").push_bind(id);
        }
        qb.push(" order by p.id desc limit ").push_bind(f.limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut items: Vec<TestPlan> = rows
            .iter()
            .map(|r| plan_row(r).map(|p| p.plan))
            .collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > f.limit {
            items.truncate(f.limit as usize);
            items.last().map(|p| encode_cursor(&[p.id.to_string()]))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Changes the name, description or filter of a managed plan at `revision`.
    pub async fn update_managed_plan(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        p: &TestPlanPatch,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<TestPlan> {
        let name = p
            .name
            .as_deref()
            .map(|n| check_name("name", n, limits.max_name_bytes))
            .transpose()?;
        if let Some(d) = &p.description
            && d.len() > limits.max_markdown_bytes
        {
            return Err(TmError::Invalid {
                code: "invalid_markdown",
                message: format!(
                    "description has at most {} bytes",
                    limits.max_markdown_bytes
                ),
                details: Some(
                    serde_json::json!({"path": "description", "max": limits.max_markdown_bytes}),
                ),
            });
        }
        if let Some(Some(f)) = &p.filter {
            super::cases::check_filter(f)?;
        }
        let mut tx = self.pool.begin().await?;
        let row = managed_for_change(&mut tx, project_id, id, false).await?;
        if row.plan.revision != revision {
            return Err(TmError::Stale {
                current_revision: row.plan.revision,
                changed_by: row.updated_by,
                source: Some("ui".into()),
            });
        }
        if let Some(Some(f)) = &p.filter {
            super::cases::check_filter_folders(&mut tx, project_id, f).await?;
        }
        let filter_json = match &p.filter {
            Some(Some(f)) => Some(json_no_nul(serde_json::to_value(plan_filter(f.clone()))?)),
            _ => None,
        };
        sqlx::query(
            "update test_plans set name = coalesce($3, name), description = coalesce($4, description), \
             filter = case when $5 then $6 else filter end, \
             revision = revision + 1, updated_at = now(), updated_by = $7 \
             where id = $1 and project_id = $2 and revision = $8",
        )
        .bind(id)
        .bind(project_id)
        .bind(name.as_deref().map(no_nul))
        .bind(p.description.as_deref().map(no_nul))
        .bind(p.filter.is_some())
        .bind(filter_json)
        .bind(no_nul(by))
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        let plan = plan_in(&mut tx, project_id, id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        tx.commit().await?;
        Ok(plan.plan)
    }

    /// Archives a managed plan (idempotent).
    pub async fn archive_managed_plan(
        &self,
        project_id: Uuid,
        id: Uuid,
        by: &str,
    ) -> TmResult<TestPlan> {
        let mut tx = self.pool.begin().await?;
        let row = managed_for_change(&mut tx, project_id, id, true).await?;
        if !row.plan.archived {
            sqlx::query(
                "update test_plans set archived_at = now(), revision = revision + 1, updated_at = now(), \
                 updated_by = $2 where id = $1",
            )
            .bind(id)
            .bind(no_nul(by))
            .execute(&mut *tx)
            .await?;
        }
        let plan = plan_in(&mut tx, project_id, id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        tx.commit().await?;
        Ok(plan.plan)
    }

    /// The cases of a plan by position.
    pub async fn plan_items(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        f: &PlanItemFilter,
    ) -> TmResult<Page<TestPlanItem>> {
        let mut conn = self.pool.acquire().await?;
        let row = plan_in(&mut conn, project_id, plan_id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        if row.plan.kind != TestPlanKind::Managed {
            return Ok(Page {
                items: Vec::new(),
                next: None,
            });
        }
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select c.number, c.name, c.folder_id, c.automated, i.assignee, i.via_filter, i.position \
             from test_plan_items i join cases c on c.id = i.case_id where i.plan_id = ",
        );
        qb.push_bind(plan_id).push(" and c.archived_at is null");
        if let Some(folder) = f.folder {
            qb.push(
                " and c.folder_id in (select fo.id from case_folders fo where fo.project_id = ",
            )
            .push_bind(project_id)
            .push(" and fo.path_ids @> array[")
            .push_bind(folder)
            .push("]::uuid[])");
        }
        match &f.assignee {
            Some(Some(a)) => {
                qb.push(" and i.assignee = ").push_bind(no_nul(a));
            }
            Some(None) => {
                qb.push(" and i.assignee is null");
            }
            None => {}
        }
        if let Some(b) = &f.before {
            let parts = decode_cursor(b, 2)?;
            let pos: i32 = parts[0].parse().map_err(|_| bad("invalid cursor"))?;
            let n: i64 = parts[1].parse().map_err(|_| bad("invalid cursor"))?;
            qb.push(" and (i.position, c.number) > (")
                .push_bind(pos)
                .push(", ")
                .push_bind(n)
                .push(")");
        }
        qb.push(" order by i.position, c.number limit ")
            .push_bind(f.limit + 1);
        let rows: Vec<PlanItemRow> = qb.build_query_as().fetch_all(&mut *conn).await?;
        let mut items: Vec<TestPlanItem> = rows
            .into_iter()
            .map(
                |(case_number, name, folder_id, automated, assignee, via_filter, position)| {
                    TestPlanItem {
                        case_number,
                        name,
                        folder_id,
                        automated,
                        assignee,
                        via_filter,
                        position,
                    }
                },
            )
            .collect();
        let next = if items.len() as i64 > f.limit {
            items.truncate(f.limit as usize);
            items
                .last()
                .map(|i| encode_cursor(&[i.position.to_string(), i.case_number.to_string()]))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Adds, removes and assigns plan cases (at most `max_bulk` numbers in all).
    pub async fn change_plan_items(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        change: &TestPlanItemsChange,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<TestPlan> {
        let add = change.add.clone().unwrap_or_default();
        let remove = change.remove.clone().unwrap_or_default();
        let assign = change.assign.clone().unwrap_or_default();
        if add.len() + remove.len() + assign.len() > limits.max_bulk {
            return Err(TmError::limit("maxBulk", limits.max_bulk as u64));
        }
        if add.is_empty() && remove.is_empty() && assign.is_empty() {
            return Err(bad("nothing to change: give add, remove or assign"));
        }
        let mut assignees: Vec<(i64, Option<String>)> = Vec::with_capacity(assign.len());
        for a in &assign {
            let who = a.assignee.as_deref().map(check_assignee).transpose()?;
            assignees.push((a.number, who));
        }
        let mut tx = self.pool.begin().await?;
        managed_for_change(&mut tx, project_id, plan_id, false).await?;
        let added = resolve_numbers(&mut tx, project_id, &add).await?;
        add_items(&mut tx, plan_id, &added, false, limits.max_plan_cases).await?;
        if !remove.is_empty() {
            sqlx::query(
                "delete from test_plan_items i using cases c where i.plan_id = $1 and c.id = i.case_id \
                 and c.project_id = $2 and c.number = any($3)",
            )
            .bind(plan_id)
            .bind(project_id)
            .bind(&remove)
            .execute(&mut *tx)
            .await?;
        }
        if !assignees.is_empty() {
            let numbers: Vec<i64> = assignees.iter().map(|a| a.0).collect();
            let who: Vec<Option<String>> = assignees.iter().map(|a| a.1.clone()).collect();
            sqlx::query(
                "update test_plan_items i set assignee = t.a from unnest($3::bigint[], $4::text[]) as t(n, a), cases c \
                 where i.plan_id = $1 and c.id = i.case_id and c.project_id = $2 and c.number = t.n",
            )
            .bind(plan_id)
            .bind(project_id)
            .bind(&numbers)
            .bind(&who)
            .execute(&mut *tx)
            .await?;
        }
        touch_plan(&mut tx, plan_id, by).await?;
        let plan = plan_in(&mut tx, project_id, plan_id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        tx.commit().await?;
        Ok(plan.plan)
    }

    /// The diff between a plan's filter and its items: cases the filter matches that are not
    /// in the plan (`added`), and cases the filter added earlier that it no longer matches
    /// (`removed`). Applied unless `dry_run`.
    pub async fn sync_plan(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
        dry_run: bool,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<super::PlanSyncDiff> {
        let mut tx = self.pool.begin().await?;
        let row = managed_for_change(&mut tx, project_id, plan_id, false).await?;
        let Some(filter) = row.plan.filter else {
            return Err(bad("the plan has no filter"));
        };
        let matched =
            filter_case_ids(&mut tx, project_id, &filter, limits.max_plan_cases + 1).await?;
        if matched.len() > limits.max_plan_cases {
            return Err(TmError::limit("maxPlanCases", limits.max_plan_cases as u64));
        }
        let current: Vec<(Uuid, i64, bool)> = sqlx::query_as(
            "select i.case_id, c.number, i.via_filter from test_plan_items i join cases c on c.id = i.case_id \
             where i.plan_id = $1",
        )
        .bind(plan_id)
        .fetch_all(&mut *tx)
        .await?;
        let in_plan: HashMap<Uuid, (i64, bool)> =
            current.iter().map(|(c, n, v)| (*c, (*n, *v))).collect();
        let matched_set: BTreeSet<Uuid> = matched.iter().copied().collect();
        let to_add: Vec<Uuid> = matched
            .iter()
            .filter(|c| !in_plan.contains_key(c))
            .copied()
            .collect();
        let to_remove: Vec<Uuid> = current
            .iter()
            .filter(|(c, _, via)| *via && !matched_set.contains(c))
            .map(|(c, _, _)| *c)
            .collect();
        let numbers: HashMap<Uuid, i64> = if to_add.is_empty() {
            HashMap::new()
        } else {
            sqlx::query_as::<_, (Uuid, i64)>("select id, number from cases where id = any($1)")
                .bind(&to_add)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect()
        };
        let mut added: Vec<i64> = to_add
            .iter()
            .filter_map(|c| numbers.get(c).copied())
            .collect();
        added.sort_unstable();
        let mut removed: Vec<i64> = to_remove
            .iter()
            .filter_map(|c| in_plan.get(c).map(|x| x.0))
            .collect();
        removed.sort_unstable();
        if dry_run {
            tx.rollback().await?;
        } else {
            if !to_remove.is_empty() {
                sqlx::query("delete from test_plan_items where plan_id = $1 and case_id = any($2)")
                    .bind(plan_id)
                    .bind(&to_remove)
                    .execute(&mut *tx)
                    .await?;
            }
            add_items(&mut tx, plan_id, &to_add, true, limits.max_plan_cases).await?;
            touch_plan(&mut tx, plan_id, by).await?;
            tx.commit().await?;
        }
        Ok(super::PlanSyncDiff { added, removed })
    }

    /// `testplan.json` of a plan: the stored tests of an adhoc plan, or the live automated
    /// cases of a managed one. `None` when the plan is not in the project.
    pub async fn plan_testplan(
        &self,
        project_id: Uuid,
        plan_id: Uuid,
    ) -> TmResult<Option<TestPlanJson>> {
        let mut conn = self.pool.acquire().await?;
        let kind: Option<String> =
            sqlx::query_scalar("select kind from test_plans where project_id = $1 and id = $2")
                .bind(project_id)
                .bind(plan_id)
                .fetch_optional(&mut *conn)
                .await?;
        match kind.as_deref() {
            None => Ok(None),
            Some("managed") => Ok(Some(managed_testplan(&mut conn, plan_id).await?)),
            Some(_) => {
                drop(conn);
                Ok(self.test_plan_json(project_id, plan_id).await?)
            }
        }
    }

    /// `testplan.json` of a launch started from a managed plan (computed live from the plan).
    pub async fn launch_testplan(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
    ) -> TmResult<TestPlanJson> {
        let plan: Option<Option<Uuid>> = sqlx::query_scalar(
            "select test_plan_id from launches where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        let plan = plan.ok_or(TmError::NotFound("launch"))?;
        let plan = plan.ok_or(TmError::NotFound("test plan"))?;
        self.plan_testplan(project_id, plan)
            .await?
            .ok_or(TmError::NotFound("test plan"))
    }

    /// Starts a launch from a managed plan in one transaction: the launch (with
    /// `test_plan_id`), run items for the plan's manual cases (automated cases get none: CI runs
    /// them from `testplan.json` and uploads their results), assignees per `assign`, and
    /// `manual_state = open` when there are run items. `start` carries the launch fields
    /// (validated by the caller). An external id of an existing launch of the same plan returns
    /// that launch; of another launch, a 409 `launch_exists`.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_plan_launch(
        &self,
        project: &Project,
        plan_id: Uuid,
        req: &StartPlanLaunch,
        start: &LaunchStart,
        by: &str,
        can_sync: bool,
        limits: &TmLimits,
    ) -> TmResult<StartedPlanLaunch> {
        let mut tx = self.pool.begin().await?;
        let row = plan_in(&mut tx, project.id, plan_id, false)
            .await?
            .ok_or(TmError::NotFound("test plan"))?;
        if row.plan.kind != TestPlanKind::Managed {
            return Err(TmError::conflict(
                "plan_not_managed",
                "only managed plans start launches",
            ));
        }
        if row.plan.archived {
            return Err(TmError::conflict("plan_archived", "the plan is archived"));
        }
        if let Some(ext) = &start.external_id {
            let existing: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
                "select id, test_plan_id from launches where project_id = $1 and external_id = $2",
            )
            .bind(project.id)
            .bind(no_nul(ext))
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((id, plan)) = existing {
                tx.rollback().await?;
                if plan != Some(plan_id) {
                    return Err(TmError::conflict(
                        "launch_exists",
                        "a launch with this externalId exists and was not started from this plan",
                    ));
                }
                let launch = self
                    .launch(project.id, id)
                    .await?
                    .ok_or(TmError::NotFound("launch"))?;
                let items: i64 =
                    sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
                        .bind(id)
                        .fetch_one(&self.pool)
                        .await?;
                return Ok(StartedPlanLaunch {
                    launch,
                    created: false,
                    run_items: items,
                    automated: row.plan.counts.automated,
                });
            }
        }
        if row.plan.counts.manual as usize > limits.max_run_items {
            return Err(TmError::limit("maxRunItems", limits.max_run_items as u64));
        }
        if row.plan.counts.manual > 0 {
            check_open_plan_launches(&mut tx, project.id, by, limits).await?;
        }
        let manual: Vec<(Uuid, i64, String, i32, Option<String>)> = sqlx::query_as(
            "select c.id, c.number, c.name, c.version, i.assignee from test_plan_items i \
             join cases c on c.id = i.case_id \
             where i.plan_id = $1 and c.archived_at is null and not c.automated \
             order by i.position, c.number limit $2",
        )
        .bind(plan_id)
        .bind(limits.max_run_items as i64 + 1)
        .fetch_all(&mut *tx)
        .await?;
        if manual.len() > limits.max_run_items {
            return Err(TmError::limit("maxRunItems", limits.max_run_items as u64));
        }
        let Some(launch_id) =
            crate::launches::insert_launch(&mut tx, project, start, by, can_sync).await?
        else {
            return Err(TmError::conflict(
                "launch_exists",
                "a launch with this externalId was created at the same time",
            ));
        };
        let created_at: DateTime<Utc> = sqlx::query_scalar(
            "update launches set test_plan_id = $2, manual_state = $3 where id = $1 returning created_at",
        )
        .bind(launch_id)
        .bind(plan_id)
        .bind((!manual.is_empty()).then_some("open"))
        .fetch_one(&mut *tx)
        .await?;
        let rows: Vec<super::runs::NewRunItem> = manual
            .into_iter()
            .map(
                |(case_id, number, name, version, assignee)| super::runs::NewRunItem {
                    case_id,
                    number,
                    name,
                    version,
                    assignee: match req.assign {
                        PlanAssign::Plan => assignee,
                        PlanAssign::Me => Some(by.to_string()),
                        PlanAssign::None => None,
                    },
                },
            )
            .collect();
        let n = super::runs::insert_run_items(&mut tx, project.id, launch_id, created_at, &rows)
            .await?;
        Store::recount_launch(&mut tx, launch_id).await?;
        super::runs::manual_changed(&mut tx, &project.key, launch_id).await?;
        tx.commit().await?;
        let launch = self
            .launch(project.id, launch_id)
            .await?
            .ok_or(TmError::NotFound("launch"))?;
        Ok(StartedPlanLaunch {
            launch,
            created: true,
            run_items: n as i64,
            automated: row.plan.counts.automated,
        })
    }
}
