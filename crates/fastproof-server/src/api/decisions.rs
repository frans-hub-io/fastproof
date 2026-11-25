//! FastProof Analysis core routes (contract `76` §7.1, §8): what Analysis answers for a
//! project, the analysis state of a launch, and the decision journal with its undo. The
//! journal and undo work with Analysis on or off.

use axum::Json;
use axum::extract::State;
use fastproof_store::analysis::types::{
    AnalysisCapabilities, AnalysisLaunchState, AnalysisLaunchStatus, TriageHistory,
    TriageHistoryQuery, TriageUndo, TriageUndoResult, UndoActor,
};
use franshub_auth::Auth;
use franshub_http::{ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(capabilities))
        .routes(routes!(launch_status))
        .routes(routes!(history))
        .routes(routes!(undo))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analysis/capabilities", tag = "analysis",
    operation_id = "analysis_capabilities",
    description = "What FastProof Analysis answers for the project: the instance and project switches, ClickHouse, \
        `pg_trgm`, the analysis replica gate, the features and the data windows. Needs `projects:read`.",
    params(("key" = String, Path)),
    responses((status = 200, body = AnalysisCapabilities), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn capabilities(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<AnalysisCapabilities>> {
    let p = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    let inputs = crate::analysis::capability_inputs(&s);
    Ok(Json(s.store.analysis_capabilities(p.id, &inputs).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/analysis", tag = "analysis",
    operation_id = "analysis_launch_status",
    description = "The analysis of a launch: queued, running, done, failed, or `none` when it was never queued, with \
        the outcome of each step. Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = AnalysisLaunchStatus), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn launch_status(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<AnalysisLaunchStatus>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let l = s.launch_of(&p, id).await?;
    Ok(Json(s.store.analysis_launch_status(l.id).await?.unwrap_or(
        AnalysisLaunchStatus {
            state: AnalysisLaunchState::None,
            queued_at: None,
            finished_at: None,
            steps: Default::default(),
        },
    )))
}

/// Query of the triage history: one of `resultId` and `batchId`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    /// The rows of a result, newest first.
    pub result_id: Option<Uuid>,
    /// The rows of a batch.
    pub batch_id: Option<Uuid>,
    /// The `next` of the previous page.
    pub before: Option<String>,
    /// 1 to 500, default 50.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/triage/history", tag = "analysis",
    operation_id = "analysis_triage_history",
    description = "The decision journal of a result (`resultId`, newest first) or of a batch (`batchId`): every \
        change of a defect or mute with its path (`via`), source, author, the state before and after, and whether \
        the batch can still be undone. Needs `launches:read`.",
    params(("key" = String, Path), HistoryQuery),
    responses((status = 200, body = TriageHistory), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn history(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<HistoryQuery>,
) -> ApiResult<Json<TriageHistory>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if q.before.as_ref().is_some_and(|b| b.len() > 32) {
        return Err(franshub_http::ApiError::bad_request("before is too long"));
    }
    let query = TriageHistoryQuery {
        result_id: q.result_id,
        batch_id: q.batch_id,
        before: q.before,
        limit: super::page_limit(q.limit)?,
    };
    Ok(Json(
        s.store
            .triage_history(p.id, &query)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/triage/undo", tag = "analysis",
    operation_id = "analysis_triage_undo", request_body = TriageUndo,
    description = "Undoes a batch: every result still at the state the batch left is put back to its state before \
        (mutes included); results changed since, gone or merged are skipped. The restore is a new batch \
        (`via: undo`), so an undo can be undone too. Rule batches at ingest are not undoable (409 \
        `batch_not_undoable`; edit the rule); batches above 20000 results answer 409 `batch_too_large`. Needs \
        `results:triage`, and `results:mute` for a batch that changed mutes; an AI agent undoes only its own batches \
        (403 `not_your_batch`).",
    params(("key" = String, Path)),
    responses((status = 200, body = TriageUndoResult), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn undo(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<TriageUndo>,
) -> ApiResult<Json<TriageUndoResult>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let actor = UndoActor {
        can_mute: auth.allowed(&s.perms.results_mute, Some(&p.key)),
        agent: crate::mcp_tm::is_agent(&s, &auth, &p.key),
        name: auth.principal.name.clone(),
    };
    let out = s
        .store
        .undo_triage(p.id, &p.key, body.batch_id, &auth.principal.name, &actor)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "triage.undo",
        Some(&p.key),
        serde_json::json!({"batchId": out.batch_id, "reverts": out.reverts, "restored": out.restored,
            "skipped": out.skipped.len()}),
    )
    .await;
    Ok(Json(out))
}
