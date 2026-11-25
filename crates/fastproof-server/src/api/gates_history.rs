//! Routes of the gate history and simulation (contract §7.5, §8).

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Duration, Utc};
use fastproof_store::analysis::types::{
    GateHistory, GateHistoryQuery, GateSimulate, GateSimulation,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::integrations::gates::{self, DEFAULT_SIMULATED_LAUNCHES, MAX_SIMULATED_LAUNCHES};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

/// Whether this build answers the gate analytics (`AnalysisFeatures.gateAnalytics`).
pub const AVAILABLE: bool = true;

/// The longest window of the history, in days.
pub const MAX_WINDOW_DAYS: i64 = 365;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(history))
        .routes(routes!(simulate))
}

/// Query of `GET .../gates/history`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    /// Only this gate (by name): its evaluations, its failed rules, its aggregates.
    pub gate: Option<String>,
    /// Only launches of this branch (exact name).
    pub branch: Option<String>,
    /// RFC 3339; default 30 days before `to`.
    pub from: Option<String>,
    /// RFC 3339; default now. The window is at most 365 days.
    pub to: Option<String>,
    /// The `next` of the previous page.
    pub before: Option<String>,
    /// 1 to 500, default 50.
    pub limit: Option<i64>,
}

fn time(v: Option<&str>, what: &str) -> ApiResult<Option<DateTime<Utc>>> {
    match v.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => DateTime::parse_from_rfc3339(v)
            .map(|t| Some(t.with_timezone(&Utc)))
            .map_err(|_| ApiError::bad_request(format!("{what} must be an RFC 3339 time"))),
    }
}

fn text(v: Option<String>, what: &str, max: usize) -> ApiResult<Option<String>> {
    match v {
        Some(v) if v.is_empty() || v.len() > max => Err(ApiError::bad_request(format!(
            "{what} must have 1 to {max} bytes"
        ))),
        v => Ok(v),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/gates/history", tag = "analysis",
    operation_id = "analysis_gate_history",
    description = "The stored quality gate evaluations of the project, newest first: status and failed rules per \
        launch, and (on the first page) per gate the evaluations, failures, flips between passed and failed on a \
        branch, the mean time from the first failure of a streak to the next pass, and the metric that failed \
        most, and per branch the evaluations and failures. The window is 30 days by default and at most 365; the \
        aggregates cover the newest 20000 evaluations of it. Needs `launches:read`.",
    params(("key" = String, Path), HistoryQuery),
    responses((status = 200, body = GateHistory), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn history(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<HistoryQuery>,
) -> ApiResult<Json<GateHistory>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let to = time(q.to.as_deref(), "to")?;
    let from = time(q.from.as_deref(), "from")?;
    let end = to.unwrap_or_else(Utc::now);
    if let Some(f) = from
        && (f > end || end - f > Duration::days(MAX_WINDOW_DAYS))
    {
        return Err(ApiError::bad_request(format!(
            "from must not be after to, and the window is at most {MAX_WINDOW_DAYS} days"
        )));
    }
    let query = GateHistoryQuery {
        gate: text(q.gate, "gate", 100)?,
        branch: text(q.branch, "branch", 256)?,
        from,
        to,
        before: text(q.before, "before", 80)?,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store.gate_history(p.id, &query).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/gates/simulate", tag = "analysis",
    operation_id = "analysis_simulate_gate", request_body = GateSimulate,
    description = "Evaluates a draft gate (validated like the project settings, enabled or not) on the newest \
        finished launches of the project (1 to 200, default 50; optionally of one `branch`) from stored data, \
        without writing anything. Launches on branches the gate does not apply to, with unprocessed uploads, or \
        whose results are gone are counted as `skipped`. Needs `launches:read`.",
    params(("key" = String, Path)),
    responses((status = 200, body = GateSimulation), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn simulate(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<GateSimulate>,
) -> ApiResult<Json<GateSimulation>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launches = body.launches.unwrap_or(DEFAULT_SIMULATED_LAUNCHES);
    if !(1..=MAX_SIMULATED_LAUNCHES).contains(&launches) {
        return Err(ApiError::bad_request(format!(
            "launches must be 1..={MAX_SIMULATED_LAUNCHES}"
        )));
    }
    let branch = text(body.branch, "branch", 256)?;
    let doc = serde_json::json!({"gates": [body.gate]});
    let mut settings = IntegrationSettings::parse(&doc).map_err(ApiError::bad_request)?;
    let Some(gate) = settings.gates.pop() else {
        return Err(ApiError::bad_request("gate is required"));
    };
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    Ok(Json(
        gates::simulate(&s, &p, &gate, launches, branch.as_deref())
            .await
            .map_err(crate::limits::heavy_error)?,
    ))
}
