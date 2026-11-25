//! Routes of unique errors and their timeline (contract `76` §7.4, §8): the errors of a
//! project with their state across branches, one error with its branches, commits, decisions,
//! tests and timeline, and the patch that ignores, activates, merges or splits errors.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Duration, Utc};
use fastproof_store::analysis::types::{
    AnalyticsBackend, AnalyticsWindow, ErrorDetail, ErrorList, ErrorPatch, ErrorQuery, ErrorSort,
    ErrorState, ErrorTimeline,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

/// Days the timeline covers by default.
pub const TIMELINE_DAYS: i64 = 30;
/// Days the timeline covers at most (ClickHouse; PostgreSQL keeps 30).
pub const TIMELINE_MAX_DAYS: i64 = 365;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(detail, patch))
        .routes(routes!(timeline))
}

/// Query of the errors list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Only errors in this state.
    pub state: Option<ErrorState>,
    /// Only errors seen on this branch (`*` for the untracked branches).
    pub branch: Option<String>,
    /// A substring of the message, at least 3 characters.
    pub q: Option<String>,
    /// `lastSeen` (default), `firstSeen`, `occurrences` or `launches`, newest or largest first.
    pub sort: Option<ErrorSort>,
    /// The `next` of the previous page.
    pub before: Option<String>,
    /// 1 to 500, default 50.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/errors", tag = "analysis",
    operation_id = "analysis_errors",
    description = "The unique errors of the project (canonical fingerprints; aliases are counted in their error) \
        with their state (`new`, `active`, `resolved`, `regressed`, `ignored`), branches, first and last occurrence, \
        launches, tests, linked issue and a 14-day sparkline. Filters: `state`, `branch`, `q` (a substring of the \
        message, at least 3 characters); `sort`; paged with `before`. Needs `launches:read`.",
    params(("key" = String, Path), ListQuery),
    responses((status = 200, body = ErrorList), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<ListQuery>,
) -> ApiResult<Json<ErrorList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let query = ErrorQuery {
        state: q.state,
        branch: q.branch,
        q: q.q,
        sort: q.sort.unwrap_or_default(),
        before: q.before,
        limit: super::page_limit(q.limit)?,
    };
    Ok(Json(s.store.errors(p.id, &query).await.map_err(tm_error)?))
}

/// The error with the compare link of its commits (from the project's `vcs` settings) and,
/// with ClickHouse, its timeline from there.
async fn full_detail(
    s: &AppState,
    p: &fastproof_store::Project,
    fingerprint: &str,
) -> ApiResult<ErrorDetail> {
    let mut d = s
        .store
        .error_detail(p.id, fingerprint)
        .await
        .map_err(tm_error)?;
    if let Some(c) = d.commits.as_mut()
        && let (Some(last_good), Some(first)) = (c.last_good.as_deref(), c.first.as_deref())
    {
        let settings = s.store.project_settings(p.id).await?;
        if let Some(vcs) = settings.other.get("vcs") {
            c.compare_url = fastproof_store::analysis::errors::compare_url(vcs, last_good, first);
        }
    }
    let (from, to) = (d.window.from, d.window.to);
    if let Some(items) =
        crate::analysis::clickhouse::error_timeline(s, p, fingerprint, from, to).await?
    {
        d.timeline = items;
        d.window = AnalyticsWindow {
            backend: AnalyticsBackend::Clickhouse,
            from,
            to,
            partial: false,
            reason: None,
        };
    }
    Ok(d)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/errors/{fingerprint}", tag = "analysis",
    operation_id = "analysis_error",
    description = "One unique error: its summary, aliases, branches (with the launches since it was last seen on \
        each), the commit range it appeared in (with a compare link for GitHub and GitLab), the time to resolve, \
        the triage decisions people made for it, the tests it occurs in and the timeline of the last 30 days. \
        Needs `launches:read`.",
    params(("key" = String, Path), ("fingerprint" = String, Path)),
    responses((status = 200, body = ErrorDetail), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn detail(
    State(s): State<AppState>,
    auth: Auth,
    P((key, fingerprint)): P<(String, String)>,
) -> ApiResult<Json<ErrorDetail>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(full_detail(&s, &p, &fingerprint).await?))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/errors/{fingerprint}", tag = "analysis",
    operation_id = "analysis_patch_error", request_body = ErrorPatch,
    description = "Changes an error: `state: ignored` (left out of `newErrors` and of notifications, never changes \
        by itself) or `active` (computed again from its occurrences); `aliasOf` makes it an alias of another error \
        of the project (its occurrences count there; an alias of an alias points at the canonical error), `null` \
        splits it again. A cycle or an error of another project answers 409 `alias_cycle`; an alias has no state \
        of its own (400). Needs `results:triage`.",
    params(("key" = String, Path), ("fingerprint" = String, Path)),
    responses((status = 200, body = ErrorDetail), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn patch(
    State(s): State<AppState>,
    auth: Auth,
    P((key, fingerprint)): P<(String, String)>,
    ApiJson(body): ApiJson<ErrorPatch>,
) -> ApiResult<Json<ErrorDetail>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    crate::tm_rate::admit(&s, &auth)?;
    if body.state.is_none() && body.alias_of.is_none() {
        return Err(ApiError::bad_request("give state or aliasOf"));
    }
    s.store
        .patch_error(p.id, &fingerprint, &body, &auth.principal.name)
        .await
        .map_err(tm_error)?;
    let mut detail = serde_json::json!({"fingerprint": fingerprint});
    if let Some(st) = body.state {
        detail["state"] = serde_json::to_value(st).unwrap_or_default();
    }
    if let Some(a) = &body.alias_of {
        detail["aliasOf"] = serde_json::json!(a);
    }
    s.audit(&auth, "error.update", Some(&p.key), detail).await;
    Ok(Json(full_detail(&s, &p, &fingerprint).await?))
}

/// Window of the timeline.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    /// RFC 3339; default 30 days before `to`.
    pub from: Option<DateTime<Utc>>,
    /// RFC 3339; default now.
    pub to: Option<DateTime<Utc>>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/errors/{fingerprint}/timeline", tag = "analysis",
    operation_id = "analysis_error_timeline",
    description = "Daily occurrences and launches of an error between `from` and `to` (default the last 30 days, at \
        most 365). Answered from ClickHouse when it is configured, else from PostgreSQL, which keeps 30 days here \
        (`window.partial` when the window was cut). Needs `launches:read`.",
    params(("key" = String, Path), ("fingerprint" = String, Path), TimelineQuery),
    responses((status = 200, body = ErrorTimeline), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn timeline(
    State(s): State<AppState>,
    auth: Auth,
    P((key, fingerprint)): P<(String, String)>,
    Q(q): Q<TimelineQuery>,
) -> ApiResult<Json<ErrorTimeline>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let to = q.to.unwrap_or_else(Utc::now);
    let from = q.from.unwrap_or(to - Duration::days(TIMELINE_DAYS));
    if from >= to {
        return Err(ApiError::bad_request("from must be before to"));
    }
    let floor = to - Duration::days(TIMELINE_MAX_DAYS);
    let cut = from < floor;
    let from = from.max(floor);
    let _permit = s.guards.heavy(&auth).await?;
    if s.clickhouse.is_some() {
        if !s.store.error_exists(p.id, &fingerprint).await? {
            return Err(ApiError::not_found("error"));
        }
        if let Some(items) =
            crate::analysis::clickhouse::error_timeline(&s, &p, &fingerprint, from, to).await?
        {
            return Ok(Json(ErrorTimeline {
                window: AnalyticsWindow {
                    backend: AnalyticsBackend::Clickhouse,
                    from,
                    to,
                    partial: cut,
                    reason: cut.then(|| "at most 365 days".into()),
                },
                items,
            }));
        }
    }
    let (window, items) = s
        .store
        .error_timeline_pg(p.id, &fingerprint, from, to)
        .await
        .map_err(tm_error)?;
    Ok(Json(ErrorTimeline { window, items }))
}
