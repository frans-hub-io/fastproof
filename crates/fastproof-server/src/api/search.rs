//! Log search and similar failures, routed to `FASTPROOF_SEARCH_BACKEND` (PostgreSQL by
//! default, ClickHouse or OpenSearch when the sinks fill them).

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use fastproof_store::logs::{LogHits, LogSearch};
use fastproof_store::stream::SimilarResult;
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{ErrorBody, P, Q};
use crate::limits::heavy_error;
use crate::state::AppState;

/// Default search window before `to`, in days.
pub const DEFAULT_SEARCH_DAYS: i64 = 7;
/// Longest search window, in days.
pub const MAX_SEARCH_DAYS: i64 = 31;

/// Query of the log search.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LogSearchQuery {
    /// Text to find (1..=256 bytes; a substring in PostgreSQL and ClickHouse, a phrase in OpenSearch).
    pub q: String,
    pub launch_id: Option<Uuid>,
    /// RFC 3339; default 7 days before `to`; at most 31 days before `to`.
    pub from: Option<String>,
    /// RFC 3339; default now.
    pub to: Option<String>,
    /// Lowest level: `trace`, `debug`, `info`, `warn` or `error`.
    pub level: Option<String>,
    /// 1..=1000, default 100.
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

fn backend_error(e: anyhow::Error) -> ApiError {
    tracing::warn!(error = %format!("{e:#}"), "the search backend failed");
    ApiError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "the search backend is not available",
    )
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/search/logs", tag = "logs", operation_id = "logs_search",
    params(("key" = String, Path), LogSearchQuery),
    responses((status = 200, body = LogHits), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn logs(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<LogSearchQuery>,
) -> ApiResult<Json<LogHits>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let text = q.q.trim().to_string();
    if text.is_empty() || text.len() > 256 {
        return Err(ApiError::bad_request("q must have 1 to 256 bytes"));
    }
    let limit = match q.limit {
        None => 100,
        Some(n) if (1..=1000).contains(&n) => n,
        Some(_) => return Err(ApiError::bad_request("limit must be 1..=1000")),
    };
    let level = match q.level.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(fastproof_store::logs::LogLevel::parse(v).ok_or_else(|| {
            ApiError::bad_request("level must be trace, debug, info, warn or error")
        })?),
    };
    let to = time(q.to.as_deref(), "to")?.unwrap_or_else(Utc::now);
    let from = time(q.from.as_deref(), "from")?
        .unwrap_or_else(|| to - chrono::Duration::days(DEFAULT_SEARCH_DAYS));
    if from > to {
        return Err(ApiError::bad_request("from must not be after to"));
    }
    if to - from > chrono::Duration::days(MAX_SEARCH_DAYS) {
        return Err(ApiError::bad_request(format!(
            "the search window must be at most {MAX_SEARCH_DAYS} days"
        )));
    }
    let search = LogSearch {
        q: text,
        launch_id: q.launch_id,
        from: Some(from),
        to: Some(to),
        level,
        limit,
    };
    let _permit = s.guards.heavy(&auth).await?;
    let hits = match &s.search {
        Some(b) => {
            let mut hits = b
                .search_logs(&project.key, &search)
                .await
                .map_err(backend_error)?;
            let live = s
                .store
                .live_launches(project.id, hits.items.iter().map(|h| h.launch_id))
                .await?;
            let before = hits.items.len();
            hits.items.retain(|h| live.contains(&h.launch_id));
            if hits.items.len() < before {
                metrics::counter!("fastproof_search_stale_hits_total")
                    .increment((before - hits.items.len()) as u64);
            }
            hits
        }
        None => s
            .store
            .search_logs(project.id, &search)
            .await
            .map_err(heavy_error)?,
    };
    Ok(Json(hits))
}

/// Query of similar failures.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SimilarQuery {
    /// 1..=100, default 20.
    pub limit: Option<i64>,
}

/// Similar failures of a result.
#[derive(Serialize, ToSchema)]
pub struct SimilarList {
    pub items: Vec<SimilarResult>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}/similar", tag = "results", operation_id = "results_similar",
    params(("key" = String, Path), ("id" = Uuid, Path), SimilarQuery),
    responses((status = 200, body = SimilarList, description = "The same fingerprint first (the normalized message hash while results carry no fingerprint), then text similarity"),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn similar(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<SimilarQuery>,
) -> ApiResult<Json<SimilarList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let limit = match q.limit {
        None => 20,
        Some(n) if (1..=100).contains(&n) => n,
        Some(_) => return Err(ApiError::bad_request("limit must be 1..=100")),
    };
    let _permit = s.guards.heavy(&auth).await?;
    let src = s
        .store
        .similar_source(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("result"))?;
    let items = match &s.search {
        Some(b) => {
            let mut items = b
                .similar(&project.key, &src, limit)
                .await
                .map_err(backend_error)?;
            let live = s
                .store
                .live_launches(project.id, items.iter().map(|i| i.launch_id))
                .await?;
            items.retain(|i| live.contains(&i.launch_id));
            items
        }
        None => s
            .store
            .similar_results(project.id, src.id, limit, None)
            .await
            .map_err(heavy_error)?
            .unwrap_or_default()
            .into_iter()
            .map(|i| SimilarResult {
                result_id: i.result_id,
                launch_id: i.launch_id,
                launch_number: i.launch_number,
                name: i.name,
                message: i.message,
                score: i.score,
                defect: i.defect.and_then(|d| serde_json::to_value(d).ok()),
            })
            .collect(),
    };
    Ok(Json(SimilarList { items }))
}
