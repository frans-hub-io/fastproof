//! The route of flaky-cause hints (contract §7.8, §8): the likely causes of one flaky series
//! with their evidence.

use axum::Json;
use axum::extract::State;
use fastproof_store::analysis::types::FlakyHints;
use franshub_auth::Auth;
use franshub_http::ApiResult;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, tm_error};
use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(hints))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/flaky/{seriesId}/hints", tag = "analysis",
    operation_id = "analysis_flaky_hints",
    description = "The likely causes of a flaky test (one series: the test in one environment), most confident \
        first, each with its evidence and the actions it suggests (`mute` with an end, `issue`, `rule`; a hint \
        never mutes by itself): `timing`, `environment`, `order`, `retry`, `infra` and, with ClickHouse, `time`. \
        A hint appears only above its minimum of data. Computed by the analysis role after a launch in which the \
        series changed status and daily; `computedAt` is `null` before the first computation. `infraShare` is the \
        share of the failures with infrastructure evidence (they still count in the flaky score). Needs \
        `launches:read`.",
    params(("key" = String, Path), ("seriesId" = Uuid, Path)),
    responses((status = 200, body = FlakyHints), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn hints(
    State(s): State<AppState>,
    auth: Auth,
    P((key, series_id)): P<(String, Uuid)>,
) -> ApiResult<Json<FlakyHints>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(
        s.store
            .flaky_hints(p.id, series_id)
            .await
            .map_err(tm_error)?,
    ))
}
