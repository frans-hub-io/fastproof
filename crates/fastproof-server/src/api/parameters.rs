//! Parameters of data-driven cases: the row generator. The parameters themselves are part of
//! the case body (`PUT /cases/{n}`); iterations of runs are in `runs.rs`.

use axum::Json;
use axum::extract::State;
use fastproof_store::tm::{ParameterGenerate, ParameterRows};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::cases::case_number;
use super::{ErrorBody, P, tm_error};
use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(generate_case_parameters))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/parameters/generate", tag = "cases",
    operation_id = "generate_case_parameters",
    description = "Generates parameter rows from values per parameter; nothing is saved. `all` gives every \
        combination, `pairwise` a deterministic set of rows that covers every pair of values of two parameters. \
        At most `maxParameterColumns` parameters and 500 values in all; more rows than `maxParameterRows`: 409 \
        `limit_reached`. Rows get new ids and are enabled; put them into `body.parameters.rows` of the case to keep them.",
    request_body = ParameterGenerate,
    params(("key" = String, Path), ("n" = i64, Path, description = "case number")),
    responses((status = 200, body = ParameterRows), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`limit_reached`"),
        (status = 429, body = ErrorBody)))]
pub async fn generate_case_parameters(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<ParameterGenerate>,
) -> ApiResult<Json<ParameterRows>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    s.store
        .case_by_number(p.id, n)
        .await?
        .ok_or_else(|| ApiError::not_found("case"))?;
    crate::tm_rate::admit(&s, &auth)?;
    let limits = s.cfg.tm;
    let rows = tokio::task::spawn_blocking(move || {
        fastproof_store::tm::parameters::generate(body.mode, &body.values, &limits)
    })
    .await
    .map_err(|e| ApiError::from(anyhow::anyhow!("generator task failed: {e}")))?
    .map_err(tm_error)?;
    Ok(Json(ParameterRows { rows }))
}
