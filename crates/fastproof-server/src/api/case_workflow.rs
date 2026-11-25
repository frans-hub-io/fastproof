//! The case workflow (§7.2 of the TM next contract): the workflow settings with the remap of
//! removed states, and transitions of one case.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use fastproof_store::Project;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::{
    Case, CaseTransition, CaseWorkflowPut, CaseWorkflowSaved, CaseWorkflowView,
};
use franshub_auth::Auth;
use franshub_http::{ApiJson, ApiResult};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::cases::{case_number, etag};
use super::{ErrorBody, P, if_match, tm_error};
use crate::markdown;
use crate::state::AppState;
use crate::tm_jobs;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_case_workflow, put_case_workflow))
        .routes(routes!(transition_case))
}

fn ctx<'a>(s: &'a AppState, p: &'a Project, auth: &'a Auth) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: &auth.principal.name,
        limits: &s.cfg.tm,
        can_review: s.can_review(auth, &p.key),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/case-workflow", tag = "reviews", operation_id = "get_case_workflow",
    description = "The case workflow (the suggested one with `configured: false` when the project has none) and the \
        review policy.",
    params(("key" = String, Path)),
    responses((status = 200, body = CaseWorkflowView), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_case_workflow(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<CaseWorkflowView>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    Ok(Json(s.store.case_workflow(p.id).await.map_err(tm_error)?))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/case-workflow", tag = "reviews", operation_id = "put_case_workflow",
    description = "Replaces the case workflow (`workflow: null` removes it). States that cases use and the new workflow \
        drops, or keeps with another category, need `remap` (old key to a key of the new workflow; else 409 \
        `workflow_status_in_use {statuses: [{key, cases}]}`); a `workflow_remap` job then moves those cases (status \
        follows the new state's category, with a version `source = bulk`). Removing the workflow clears every state in \
        that job. Audited as `caseWorkflow.settings`.",
    request_body = CaseWorkflowPut,
    params(("key" = String, Path)),
    responses((status = 200, body = CaseWorkflowSaved), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn put_case_workflow(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseWorkflowPut>,
) -> ApiResult<Json<CaseWorkflowSaved>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let (old, saved) = s
        .store
        .put_case_workflow(&ctx(&s, &p, &auth), &body)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseWorkflow.settings",
        Some(&p.key),
        serde_json::json!({"old": old, "new": body.workflow, "remap": body.remap,
            "jobId": saved.job.as_ref().map(|j| j.id)}),
    )
    .await;
    if saved.job.is_some() {
        tm_jobs::kick(&s);
    }
    Ok(Json(saved))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/transition", tag = "reviews", operation_id = "transition_case",
    description = "Moves a case to a workflow state: `state` and `status` (the state's category) change together, \
        `revision + 1`, and a version is written only when the category changes. The rule of the case's flow (manual or \
        automated), its permission and guards (`approved`, `requiredFields`, `notAuthor`) apply: 409 \
        `invalid_transition {from, to, missing}` otherwise. Audited as `case.transition`.",
    request_body = CaseTransition,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn transition_case(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<CaseTransition>,
) -> ApiResult<([(header::HeaderName, HeaderValue); 1], Json<Case>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let expected = if_match(&headers)?;
    crate::tm_rate::admit(&s, &auth)?;
    let (before, after) = s
        .store
        .transition_case(&ctx(&s, &p, &auth), n, expected, &body.to)
        .await
        .map_err(tm_error)?;
    if before.revision != after.revision {
        s.audit(
            &auth,
            "case.transition",
            Some(&p.key),
            serde_json::json!({"number": after.number, "from": before.state, "to": after.state,
                "fromStatus": before.status, "toStatus": after.status}),
        )
        .await;
    }
    let mut c = s.store.case_view_of(p.id, &after).await.map_err(tm_error)?;
    c.html = markdown::render_body(&c.body, &p.key);
    Ok((etag(c.revision), Json(c)))
}
