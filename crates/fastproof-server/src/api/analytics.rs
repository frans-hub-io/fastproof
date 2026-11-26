//! Routes of failure analytics, portfolio and cross-project similar failures (tag `analytics`,
//! contract `76` §7.9, §8). The project endpoints answer from ClickHouse when it is configured
//! for Analysis (at most 365 days) and from PostgreSQL otherwise (at most 30 days,
//! `window.partial` beyond; also when ClickHouse fails); the portfolio and cross-project
//! similar failures need ClickHouse (409 `clickhouse_required`). Every query reaches
//! ClickHouse with the project key (or the caller's readable projects) as parameters.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use fastproof_store::Project;
use fastproof_store::analysis::analytics::{
    BEFORE_DAYS, Buckets, RECENT_DAYS, failure_groups, mttr_stats, window,
};
use fastproof_store::analysis::types::{
    AnalyticsBackend, AnalyticsWindow, CrossProjectSimilar, DurationRegressions, FailureAnalytics,
    FailureGroupBy, FailureQuery, FlakyTrend, MttrStats, Portfolio, PortfolioProject, RetryCosts,
    SlowSteps,
};
use fastproof_stream::clickhouse::{ClickHouse, MAX_ALIASES, Scope};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q};
use crate::state::AppState;

/// Groups of the failure analytics, at most.
pub const MAX_GROUPS: i64 = 100;
/// Default groups or items of an analytics answer.
pub const DEFAULT_ITEMS: i64 = 20;
/// Similar failures in other projects, at most.
pub const MAX_SIMILAR: i64 = 50;
/// Length of `branch` and `env`, at most.
const MAX_FILTER: usize = 200;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(failures))
        .routes(routes!(durations))
        .routes(routes!(retries))
        .routes(routes!(flaky_trend))
        .routes(routes!(mttr))
        .routes(routes!(slow_steps))
        .routes(routes!(portfolio))
        .routes(routes!(similar))
}

/// Query of the project analytics.
#[derive(Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyticsQuery {
    /// `fingerprint` (default), `defectGroup`, `suite` or `env` (failures only).
    pub group_by: Option<FailureGroupBy>,
    /// Start of the window (RFC 3339), default 30 days before `to`.
    pub from: Option<DateTime<Utc>>,
    /// End of the window (RFC 3339, exclusive), default now.
    pub to: Option<DateTime<Utc>>,
    /// Only launches of this branch (`""` for launches without one).
    pub branch: Option<String>,
    /// Only launches of this environment key.
    pub env: Option<String>,
    /// Groups or items, 1 to 100, default 20.
    pub limit: Option<i64>,
}

fn conflict(code: &str, message: &str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

fn clickhouse_required() -> ApiError {
    conflict(
        "clickhouse_required",
        "this view needs ClickHouse (FASTPROOF_CLICKHOUSE_URL with the clickhouse sink and FASTPROOF_ANALYSIS_CLICKHOUSE=auto)",
    )
}

fn check_filter(v: &Option<String>, what: &str) -> ApiResult<()> {
    match v {
        Some(s) if s.len() > MAX_FILTER || s.contains('\0') => Err(ApiError::bad_request(format!(
            "{what} must be at most {MAX_FILTER} bytes"
        ))),
        _ => Ok(()),
    }
}

/// The validated query (window not yet cut to a backend).
fn query(q: &AnalyticsQuery, now: DateTime<Utc>) -> ApiResult<FailureQuery> {
    let limit = match q.limit {
        None => DEFAULT_ITEMS,
        Some(n) if (1..=MAX_GROUPS).contains(&n) => n,
        Some(_) => {
            return Err(ApiError::bad_request(format!(
                "limit must be 1..={MAX_GROUPS}"
            )));
        }
    };
    let to = q.to.unwrap_or(now);
    if let Some(from) = q.from
        && from >= to
    {
        return Err(ApiError::bad_request("from must be before to"));
    }
    check_filter(&q.branch, "branch")?;
    check_filter(&q.env, "env")?;
    Ok(FailureQuery {
        group_by: q.group_by.unwrap_or_default(),
        from: q.from,
        to: Some(to),
        branch: q.branch.clone(),
        env: q.env.clone(),
        limit,
    })
}

fn scope<'a>(p: &'a Project, q: &'a FailureQuery, w: &AnalyticsWindow) -> Scope<'a> {
    Scope {
        project: &p.key,
        from: w.from,
        to: w.to,
        branch: q.branch.as_deref(),
        env: q.env.as_deref(),
    }
}

/// The project after `launches:read`, and the ClickHouse window when ClickHouse is there.
async fn start(
    s: &AppState,
    auth: &Auth,
    key: &str,
    q: &AnalyticsQuery,
) -> ApiResult<(Project, FailureQuery, Option<AnalyticsWindow>)> {
    let p = s.project_for(auth, key, &s.perms.launches_read).await?;
    let fq = query(q, Utc::now())?;
    let w = s
        .clickhouse
        .is_some()
        .then(|| window(fq.from, fq.to, Utc::now(), AnalyticsBackend::Clickhouse));
    Ok((p, fq, w))
}

fn ch(s: &AppState) -> Option<&ClickHouse> {
    s.clickhouse.as_deref()
}

/// Logs a failed ClickHouse read; the caller answers from PostgreSQL.
fn fell_back(what: &'static str, e: anyhow::Error) {
    metrics::counter!("fastproof_analysis_clickhouse_errors_total", "read" => what).increment(1);
    tracing::warn!(read = what, error = %format!("{e:#}"), "ClickHouse analytics failed; answering from PostgreSQL");
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/failures", tag = "analytics",
    operation_id = "analytics_failures",
    description = "Failed and broken results of the window grouped by `groupBy`: `fingerprint` (unique errors, \
        merged fingerprints counted in their error), `defectGroup` (the current triage; `toInvestigate` without one), \
        `suite` or `env`. Each group has its failures, tests, launches, share of all failures and a trend of up to 14 \
        buckets of whole days. ClickHouse answers up to 365 days; without it PostgreSQL answers the last 30 days \
        (`window.partial`). Filters: `branch`, `env`. Needs `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = FailureAnalytics), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn failures(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<FailureAnalytics>> {
    Ok(Json(failures_of(&s, &auth, &key, &q).await?))
}

/// The failure analytics of a project for a caller: `launches:read` in the project's scope,
/// the heavy guard held for the whole read (a dropped read is cancelled in ClickHouse), the
/// window and filters validated as on the route. Shared by the route and the MCP tool.
pub(crate) async fn failures_of(
    s: &AppState,
    auth: &Auth,
    key: &str,
    q: &AnalyticsQuery,
) -> ApiResult<FailureAnalytics> {
    let (p, fq, w) = start(s, auth, key, q).await?;
    let _permit = s.guards.heavy(auth).await?;
    if let (Some(ch), Some(w)) = (ch(s), w) {
        let aliases = if fq.group_by == FailureGroupBy::Fingerprint {
            s.store.alias_map(p.id, MAX_ALIASES).await?
        } else {
            Vec::new()
        };
        match ch
            .failures(
                &scope(&p, &fq, &w),
                fq.group_by,
                &aliases,
                fq.limit as usize,
            )
            .await
        {
            Ok((total, groups, days)) => {
                let keys: Vec<String> = groups.iter().map(|g| g.key.clone()).collect();
                let labels = if fq.group_by == FailureGroupBy::Fingerprint {
                    s.store.fingerprint_labels(p.id, &keys).await?
                } else {
                    Default::default()
                };
                let counts = groups
                    .into_iter()
                    .map(|g| (g.key, g.failures, g.tests, g.launches))
                    .collect();
                let buckets = Buckets::new(w.from, w.to);
                return Ok(FailureAnalytics {
                    items: failure_groups(fq.group_by, total, counts, &days, &buckets, &labels),
                    window: w,
                    group_by: fq.group_by,
                    total,
                });
            }
            Err(e) => fell_back("failures", e),
        }
    }
    Ok(s.store.analytics_failures_pg(p.id, &fq).await?)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/durations", tag = "analytics",
    operation_id = "analytics_durations",
    description = "Tests that became slower: series whose median duration of the 7 days before `to` is at least \
        1.3 times the median of the 28 days before that, with at least 10 runs in each part (passed, failed or \
        broken runs). Without ClickHouse the comparison covers the 23 days before (`window.partial`). Filters: \
        `branch`, `env`. Needs `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = DurationRegressions), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn durations(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<DurationRegressions>> {
    let (p, fq, w) = start(&s, &auth, &key, &q).await?;
    let _permit = s.guards.heavy(&auth).await?;
    if let (Some(ch), Some(_)) = (ch(&s), w) {
        let to = fq.to.unwrap_or_else(Utc::now);
        let w = AnalyticsWindow {
            backend: AnalyticsBackend::Clickhouse,
            from: to - Duration::days(RECENT_DAYS + BEFORE_DAYS),
            to,
            partial: false,
            reason: None,
        };
        match ch
            .durations(
                &scope(&p, &fq, &w),
                to - Duration::days(RECENT_DAYS),
                fq.limit as usize,
            )
            .await
        {
            Ok(items) => return Ok(Json(DurationRegressions { window: w, items })),
            Err(e) => fell_back("durations", e),
        }
    }
    Ok(Json(s.store.analytics_durations_pg(p.id, &fq).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/retries", tag = "analytics",
    operation_id = "analytics_retries",
    description = "What retries cost: the time of attempts that did not become final (every attempt of a result \
        but its last), in total and per suite (most time first), with the retries and the tests that retried. \
        Filters: `branch`, `env`. Needs `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = RetryCosts), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn retries(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<RetryCosts>> {
    let (p, fq, w) = start(&s, &auth, &key, &q).await?;
    let _permit = s.guards.heavy(&auth).await?;
    if let (Some(ch), Some(w)) = (ch(&s), w) {
        match ch.retries(&scope(&p, &fq, &w), fq.limit as usize).await {
            Ok((total_wasted_ms, items)) => {
                return Ok(Json(RetryCosts {
                    window: w,
                    total_wasted_ms,
                    items,
                }));
            }
            Err(e) => fell_back("retries", e),
        }
    }
    Ok(Json(s.store.analytics_retries_pg(p.id, &fq).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/flaky-trend", tag = "analytics",
    operation_id = "analytics_flaky_trend",
    description = "Flaky results (passed after a retry) and results that ran, per day of the window (UTC). \
        Filters: `branch`, `env`. Needs `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = FlakyTrend), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn flaky_trend(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<FlakyTrend>> {
    let (p, fq, w) = start(&s, &auth, &key, &q).await?;
    let _permit = s.guards.heavy(&auth).await?;
    if let (Some(ch), Some(w)) = (ch(&s), w) {
        match ch.flaky_trend(&scope(&p, &fq, &w)).await {
            Ok(items) => return Ok(Json(FlakyTrend { window: w, items })),
            Err(e) => fell_back("flaky_trend", e),
        }
    }
    Ok(Json(s.store.analytics_flaky_trend_pg(p.id, &fq).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/mttr", tag = "analytics",
    operation_id = "analytics_mttr",
    description = "Time to resolve unique errors resolved in the window (from first seen to resolved): the count, \
        the median and the 90th percentile, and cumulative buckets (1 h, 6 h, 1 d, 3 d, 7 d, 30 d, 90 d). It reads \
        the error catalog, so both backends answer the same; the window follows the backend's limit. Needs \
        `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = MttrStats), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn mttr(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<MttrStats>> {
    let (p, fq, w) = start(&s, &auth, &key, &q).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let w = w.unwrap_or_else(|| window(fq.from, fq.to, Utc::now(), AnalyticsBackend::Postgres));
    let secs = s.store.resolved_seconds(p.id, w.from, w.to).await?;
    Ok(Json(mttr_stats(w, secs)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analytics/slow-steps", tag = "analytics",
    operation_id = "analytics_slow_steps",
    description = "The slowest step names of the window by their 95th percentile duration, with the median and \
        the number of runs. Without ClickHouse, PostgreSQL reads the steps of the newest 2000 attempts \
        (`window.partial` when there are more). Needs `launches:read`.",
    params(("key" = String, Path), AnalyticsQuery),
    responses((status = 200, body = SlowSteps), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn slow_steps(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<AnalyticsQuery>,
) -> ApiResult<Json<SlowSteps>> {
    let (p, fq, w) = start(&s, &auth, &key, &q).await?;
    let _permit = s.guards.heavy(&auth).await?;
    if let (Some(ch), Some(w)) = (ch(&s), w) {
        match ch.slow_steps(&scope(&p, &fq, &w), fq.limit as usize).await {
            Ok(items) => return Ok(Json(SlowSteps { window: w, items })),
            Err(e) => fell_back("slow_steps", e),
        }
    }
    let fq = FailureQuery {
        branch: None,
        env: None,
        ..fq
    };
    Ok(Json(s.store.analytics_slow_steps_pg(p.id, &fq).await?))
}

/// Query of the portfolio.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortfolioQuery {
    /// Comma-separated project keys to show (default: every project the caller can read);
    /// keys the caller cannot read are left out.
    pub projects: Option<String>,
    /// Start of the window (RFC 3339), default 30 days before `to`.
    pub from: Option<DateTime<Utc>>,
    /// End of the window (RFC 3339, exclusive), default now.
    pub to: Option<DateTime<Utc>>,
}

/// The projects the caller can read (`launches:read`), narrowed by `projects=`, at most
/// [`fastproof_analysis::PORTFOLIO_MAX_PROJECTS`], by key.
async fn readable(s: &AppState, auth: &Auth, only: Option<&str>) -> ApiResult<Vec<Project>> {
    let wanted: Option<BTreeSet<&str>> = match only {
        None => None,
        Some(list) => {
            let keys: BTreeSet<&str> = list
                .split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .collect();
            if keys.len() > fastproof_analysis::PORTFOLIO_MAX_PROJECTS {
                return Err(ApiError::bad_request(format!(
                    "projects lists at most {} keys",
                    fastproof_analysis::PORTFOLIO_MAX_PROJECTS
                )));
            }
            Some(keys)
        }
    };
    let mut out: Vec<Project> = s
        .store
        .projects()
        .await?
        .into_iter()
        .filter(|p| auth.allowed(&s.perms.launches_read, Some(&p.key)))
        .filter(|p| wanted.as_ref().is_none_or(|w| w.contains(p.key.as_str())))
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out.truncate(fastproof_analysis::PORTFOLIO_MAX_PROJECTS);
    Ok(out)
}

#[utoipa::path(get, path = "/api/v1/analytics/portfolio", tag = "analytics",
    operation_id = "analytics_portfolio",
    description = "Health of the projects the caller can read (`launches:read` in each; at most 200, narrowed by \
        `projects`): launches, pass rate of the results that ran, failures, unique errors first seen and regressed \
        in the window, flaky tests and the median time to resolve. A project the caller cannot read never appears \
        or counts. Needs ClickHouse (409 `clickhouse_required`).",
    params(PortfolioQuery),
    responses((status = 200, body = Portfolio), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn portfolio(
    State(s): State<AppState>,
    auth: Auth,
    Q(q): Q<PortfolioQuery>,
) -> ApiResult<Json<Portfolio>> {
    let to = q.to.unwrap_or_else(Utc::now);
    if q.from.is_some_and(|f| f >= to) {
        return Err(ApiError::bad_request("from must be before to"));
    }
    let projects = readable(&s, &auth, q.projects.as_deref()).await?;
    let Some(ch) = ch(&s) else {
        return Err(clickhouse_required());
    };
    let _permit = s.guards.heavy(&auth).await?;
    let w = window(q.from, Some(to), Utc::now(), AnalyticsBackend::Clickhouse);
    let keys: Vec<String> = projects.iter().map(|p| p.key.clone()).collect();
    let counts = ch
        .portfolio(&keys, w.from, w.to)
        .await
        .map_err(|e| ApiError::from(e.context("reading the portfolio from ClickHouse")))?;
    let ids: Vec<Uuid> = projects.iter().map(|p| p.id).collect();
    let errors = s.store.portfolio_errors(&ids, w.from, w.to).await?;
    let items = projects
        .into_iter()
        .map(|p| {
            let c = counts
                .iter()
                .find(|c| c.project == p.key)
                .cloned()
                .unwrap_or_default();
            let (new_errors, regressed_errors, mttr) =
                errors.get(&p.id).copied().unwrap_or_default();
            PortfolioProject {
                pass_rate: (c.ran > 0)
                    .then(|| ((c.passed as f64 / c.ran as f64) * 1000.0).round() / 1000.0),
                key: p.key,
                name: p.name,
                launches: c.launches,
                failures: c.failures,
                new_errors,
                regressed_errors,
                flaky_tests: c.flaky,
                mttr_p50_seconds: mttr,
            }
        })
        .collect();
    Ok(Json(Portfolio { window: w, items }))
}

/// Query of cross-project similar failures.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimilarQuery {
    /// The project of the result.
    pub project: String,
    /// The failed result to compare.
    pub result_id: Uuid,
    /// 1 to 50, default 10.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/analytics/similar", tag = "analytics",
    operation_id = "analytics_similar",
    description = "Failures like a result of `project` in the other projects the caller can read (`launches:read` \
        there): the same fingerprint (score 1) or a close normalized message, most similar first, with each \
        match's current defect group. Needs `launches:read` on `project` and ClickHouse (409 \
        `clickhouse_required`).",
    params(SimilarQuery),
    responses((status = 200, body = CrossProjectSimilar), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn similar(
    State(s): State<AppState>,
    auth: Auth,
    Q(q): Q<SimilarQuery>,
) -> ApiResult<Json<CrossProjectSimilar>> {
    let p = s
        .project_for(&auth, &q.project, &s.perms.launches_read)
        .await?;
    let limit = match q.limit {
        None => 10,
        Some(n) if (1..=MAX_SIMILAR).contains(&n) => n,
        Some(_) => {
            return Err(ApiError::bad_request(format!(
                "limit must be 1..={MAX_SIMILAR}"
            )));
        }
    };
    let src = s
        .store
        .similar_source(p.id, q.result_id)
        .await?
        .ok_or_else(|| ApiError::not_found("result"))?;
    let Some(ch) = ch(&s) else {
        return Err(clickhouse_required());
    };
    let _permit = s.guards.heavy(&auth).await?;
    let others: Vec<String> = readable(&s, &auth, None)
        .await?
        .into_iter()
        .filter(|o| o.id != p.id)
        .map(|o| o.key)
        .collect();
    let items = ch
        .similar_across(&others, &src, limit as usize)
        .await
        .map_err(|e| ApiError::from(e.context("reading similar failures from ClickHouse")))?;
    Ok(Json(CrossProjectSimilar { items }))
}
