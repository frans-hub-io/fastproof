//! Shared steps (S): steps reused by many cases, with versions, usages, inline and extract
//! (`74-tm-next-interfaces.md` §7.1, §9).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_store::Project;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::shared_steps::{InlineOutcome, SharedStepQuery};
use fastproof_store::tm::{
    Case, CaseBody, ExtractSharedStep, ExtractedSharedStep, RunItem, SharedStep, SharedStepBrief,
    SharedStepChangeList, SharedStepHtml, SharedStepList, SharedStepOrigin, SharedStepRow,
    SharedStepUsageList, SharedStepVersion, SharedStepVersionList, SharedStepWrite, TmJob,
};
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::cases::{case_number, etag, if_match};
use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::markdown;
use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_shared_steps, create_shared_step))
        .routes(routes!(
            get_shared_step,
            update_shared_step,
            archive_shared_step
        ))
        .routes(routes!(restore_shared_step))
        .routes(routes!(list_shared_step_versions))
        .routes(routes!(get_shared_step_version))
        .routes(routes!(restore_shared_step_version))
        .routes(routes!(list_shared_step_usages))
        .routes(routes!(inline_shared_step))
        .routes(routes!(extract_shared_step))
        .routes(routes!(list_case_shared_step_changes))
        .routes(routes!(refresh_run_item))
}

fn ctx<'a>(s: &'a AppState, p: &'a Project, auth: &'a Auth) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: &auth.principal.name,
        limits: &s.cfg.tm,
        can_review: auth.principal.allowed(&s.perms.cases_review, Some(&p.key)),
    }
}

/// A shared step number from the path: digits only, otherwise 404.
fn number(s: &str) -> ApiResult<i64> {
    case_number(s).map_err(|_| ApiError::not_found("shared step"))
}

fn version_number(s: &str) -> ApiResult<i32> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::not_found("version"));
    }
    s.parse::<i32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::not_found("version"))
}

/// Renders the Markdown of the shared steps a case lists (`Case.sharedSteps`).
pub fn render_briefs(briefs: &mut [SharedStepBrief], project_key: &str) {
    for b in briefs {
        b.steps_html = markdown::render_steps(&b.steps, project_key);
    }
}

fn render_view(v: &mut SharedStep, project_key: &str) {
    v.html = SharedStepHtml {
        description: markdown::render(&v.description, project_key),
        steps: markdown::render_steps(&v.steps, project_key),
    };
}

type StepReply = ([(header::HeaderName, HeaderValue); 1], Json<SharedStep>);

async fn reply(s: &AppState, p: &Project, row: &SharedStepRow) -> ApiResult<StepReply> {
    let mut v = s
        .store
        .shared_step_view_of(p.id, row)
        .await
        .map_err(tm_error)?;
    render_view(&mut v, &p.key);
    Ok((etag(v.revision), Json(v)))
}

/// Query of `GET /shared-steps`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SharedStepListQuery {
    /// A number (`SS-12` or `12`) or words of the name, description or steps.
    pub q: Option<String>,
    /// `true` lists the archived shared steps.
    pub archived: Option<bool>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps", tag = "shared-steps", operation_id = "list_shared_steps",
    description = "Shared steps by number with their usage count; `q` searches the number, name and steps.",
    params(("key" = String, Path), SharedStepListQuery),
    responses((status = 200, body = SharedStepList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_shared_steps(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<SharedStepListQuery>,
) -> ApiResult<Json<SharedStepList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let query = SharedStepQuery {
        q: q.q,
        archived: q.archived.unwrap_or(false),
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store
            .list_shared_steps(p.id, &query)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/shared-steps", tag = "shared-steps", operation_id = "create_shared_step",
    description = "Creates a shared step (version 1). Steps are at most 2 levels deep, cannot contain shared steps or \
        attachments, and are limited by `maxSharedStepSteps`, `maxSharedStepBytes` and `maxSharedSteps`.",
    request_body = SharedStepWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = SharedStep, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`limit_reached`")))]
pub async fn create_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<SharedStepWrite>,
) -> ApiResult<(StatusCode, StepReply)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let row = s
        .store
        .create_shared_step(&ctx(&s, &p, &auth), &body, SharedStepOrigin::Ui)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "sharedStep.create",
        Some(&p.key),
        serde_json::json!({"number": row.number, "name": row.name}),
    )
    .await;
    Ok((StatusCode::CREATED, reply(&s, &p, &row).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps/{n}", tag = "shared-steps", operation_id = "get_shared_step",
    params(("key" = String, Path), ("n" = i64, Path, description = "shared step number")),
    responses((status = 200, body = SharedStep, headers(("ETag" = String, description = "\"r<revision>\""))),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<StepReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let mut v = s.store.shared_step_view(p.id, n).await.map_err(tm_error)?;
    render_view(&mut v, &p.key);
    Ok((etag(v.revision), Json(v)))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/shared-steps/{n}", tag = "shared-steps", operation_id = "update_shared_step",
    description = "Replaces the name, description and steps (needs `If-Match`). A change makes a new version that cases \
        pick up in new runs; case versions and runs that started are untouched.",
    request_body = SharedStepWrite,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header, description = "\"r<revision>\"")),
    responses((status = 200, body = SharedStep, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`shared_step_archived` or `limit_reached`"),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn update_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<SharedStepWrite>,
) -> ApiResult<StepReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .update_shared_step(&ctx(&s, &p, &auth), n, expected, &body)
        .await
        .map_err(tm_error)?;
    if before.revision != after.revision {
        s.audit(
            &auth,
            "sharedStep.update",
            Some(&p.key),
            serde_json::json!({"number": after.number, "version": after.version, "revision": after.revision}),
        )
        .await;
    }
    reply(&s, &p, &after).await
}

/// Query of `DELETE /shared-steps/{n}`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArchiveSharedQuery {
    /// Deletes the shared step for good (`settings:write`).
    pub purge: Option<bool>,
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/shared-steps/{n}", tag = "shared-steps", operation_id = "archive_shared_step",
    description = "Archives a shared step (allowed while used: cases keep working, new references are refused). \
        `purge=true` deletes it with its versions (needs `settings:write`; 409 `shared_step_in_use {cases, openRuns}` \
        while a case uses it or an open run pins it).",
    params(("key" = String, Path), ("n" = i64, Path), ArchiveSharedQuery),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn archive_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<ArchiveSharedQuery>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    if q.purge == Some(true) {
        require(&auth, &s.perms.settings_write, Some(&p.key))?;
        let row = s
            .store
            .purge_shared_step(&ctx(&s, &p, &auth), n)
            .await
            .map_err(tm_error)?;
        s.audit(
            &auth,
            "sharedStep.purge",
            Some(&p.key),
            serde_json::json!({"number": row.number, "name": row.name}),
        )
        .await;
        return Ok(StatusCode::NO_CONTENT);
    }
    let row = s
        .store
        .set_shared_step_archived(&ctx(&s, &p, &auth), n, true)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "sharedStep.archive",
        Some(&p.key),
        serde_json::json!({"number": row.number}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/shared-steps/{n}/restore", tag = "shared-steps", operation_id = "restore_shared_step",
    description = "Takes a shared step out of the archive.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = SharedStep, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn restore_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<StepReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let row = s
        .store
        .set_shared_step_archived(&ctx(&s, &p, &auth), n, false)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "sharedStep.restore",
        Some(&p.key),
        serde_json::json!({"number": row.number}),
    )
    .await;
    reply(&s, &p, &row).await
}

/// Query of paged lists.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps/{n}/versions", tag = "shared-steps", operation_id = "list_shared_step_versions",
    params(("key" = String, Path), ("n" = i64, Path), PageQuery),
    responses((status = 200, body = SharedStepVersionList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_shared_step_versions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<PageQuery>,
) -> ApiResult<Json<SharedStepVersionList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .shared_step_versions(p.id, n, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps/{n}/versions/{v}", tag = "shared-steps", operation_id = "get_shared_step_version",
    params(("key" = String, Path), ("n" = i64, Path), ("v" = i32, Path)),
    responses((status = 200, body = SharedStepVersion), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_shared_step_version(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n, v)): P<(String, String, String)>,
) -> ApiResult<Json<SharedStepVersion>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let v = version_number(&v)?;
    Ok(Json(
        s.store
            .shared_step_version(p.id, n, v)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/shared-steps/{n}/versions/{v}/restore", tag = "shared-steps",
    operation_id = "restore_shared_step_version",
    description = "Makes the content of version `v` current as a new version (needs `If-Match`).",
    params(("key" = String, Path), ("n" = i64, Path), ("v" = i32, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = SharedStep, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 412, body = ErrorBody),
        (status = 428, body = ErrorBody)))]
pub async fn restore_shared_step_version(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n, v)): P<(String, String, String)>,
) -> ApiResult<StepReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let v = version_number(&v)?;
    let expected = if_match(&headers)?;
    let (_, after) = s
        .store
        .restore_shared_step_version(&ctx(&s, &p, &auth), n, v, expected)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "sharedStep.restoreVersion",
        Some(&p.key),
        serde_json::json!({"number": after.number, "from": v, "version": after.version}),
    )
    .await;
    reply(&s, &p, &after).await
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps/{n}/usages", tag = "shared-steps", operation_id = "list_shared_step_usages",
    description = "The cases whose head or runnable version references the shared step, by number, and the open manual \
        runs that pin it.",
    params(("key" = String, Path), ("n" = i64, Path), PageQuery),
    responses((status = 200, body = SharedStepUsageList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_shared_step_usages(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<PageQuery>,
) -> ApiResult<Json<SharedStepUsageList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .shared_step_usages(p.id, n, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

/// What inlining a shared step did in the request.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SharedStepInlined {
    /// Cases that now hold the steps themselves.
    pub updated: i64,
    /// Cases that refused the change (code-owned steps, limits, a conflict).
    pub skipped: i64,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/shared-steps/{n}/inline", tag = "shared-steps", operation_id = "inline_shared_step",
    description = "Replaces the references to the shared step in every using case by the steps of its runnable version \
        (new case versions with `source = bulk`). Above 1000 usages a `shared_inline` job runs it and the answer is 202.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = SharedStepInlined), (status = 202, body = TmJob), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn inline_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    let out = s
        .store
        .inline_shared_step(&ctx(&s, &p, &auth), n)
        .await
        .map_err(tm_error)?;
    match out {
        InlineOutcome::Done { updated, skipped } => {
            s.audit(
                &auth,
                "sharedStep.inline",
                Some(&p.key),
                serde_json::json!({"number": n, "updated": updated, "skipped": skipped}),
            )
            .await;
            Ok(Json(SharedStepInlined { updated, skipped }).into_response())
        }
        InlineOutcome::Job(job) => {
            crate::tm_jobs::kick(&s);
            s.audit(
                &auth,
                "sharedStep.inline",
                Some(&p.key),
                serde_json::json!({"number": n, "job": job.id}),
            )
            .await;
            Ok((StatusCode::ACCEPTED, Json(*job)).into_response())
        }
    }
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/extract-shared", tag = "shared-steps", operation_id = "extract_shared_step",
    description = "Moves adjacent top-level steps of the case into a new shared step and replaces them with a reference, \
        in one transaction (needs the case's `If-Match`).",
    request_body = ExtractSharedStep,
    params(("key" = String, Path), ("n" = i64, Path, description = "case number"), ("If-Match" = String, Header)),
    responses((status = 201, body = ExtractedSharedStep), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 412, body = ErrorBody),
        (status = 428, body = ErrorBody)))]
pub async fn extract_shared_step(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<ExtractSharedStep>,
) -> ApiResult<(StatusCode, Json<ExtractedSharedStep>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let expected = if_match(&headers)?;
    let (shared, case) = s
        .store
        .extract_shared_step(&ctx(&s, &p, &auth), n, expected, &body)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "sharedStep.extract",
        Some(&p.key),
        serde_json::json!({"number": shared.number, "case": case.number, "steps": body.step_ids.len()}),
    )
    .await;
    let mut sv = s
        .store
        .shared_step_view_of(p.id, &shared)
        .await
        .map_err(tm_error)?;
    render_view(&mut sv, &p.key);
    let mut cv: Case = s.store.case_view_of(p.id, &case).await.map_err(tm_error)?;
    cv.html = markdown::render_body(&cv.body, &p.key);
    render_briefs(&mut cv.shared_steps, &p.key);
    Ok((
        StatusCode::CREATED,
        Json(ExtractedSharedStep {
            shared_step: sv,
            case: cv,
        }),
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/shared-step-changes", tag = "shared-steps",
    operation_id = "list_case_shared_step_changes",
    description = "The versions written to the shared steps the case uses, newest first (the case's Versions tab).",
    params(("key" = String, Path), ("n" = i64, Path, description = "case number"), PageQuery),
    responses((status = 200, body = SharedStepChangeList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_case_shared_step_changes(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<PageQuery>,
) -> ApiResult<Json<SharedStepChangeList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .case_shared_step_changes(p.id, n, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

async fn rendered_item(s: &AppState, p: &Project, id: Uuid) -> ApiResult<RunItem> {
    let mut item = s.store.run_item(p.id, id).await.map_err(tm_error)?;
    let html = markdown::render_body(
        &CaseBody {
            description: String::new(),
            preconditions: item.preconditions.clone(),
            steps: item.steps.clone(),
            links: Vec::new(),
            parameters: None,
        },
        &p.key,
    );
    item.preconditions_html = html.preconditions;
    item.steps_html = html.steps;
    Ok(item)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/run-items/{itemId}/refresh", tag = "shared-steps", operation_id = "refresh_run_item",
    description = "Pins a run item that has not started (status `untested`, no step results) to the current runnable \
        versions of its shared steps. 409 `run_item_started` otherwise.",
    params(("key" = String, Path), ("itemId" = Uuid, Path)),
    responses((status = 200, body = RunItem, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`run_item_started`, `manual_run_closed`")))]
pub async fn refresh_run_item(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    let (launch_id, _) = s
        .store
        .refresh_run_item(p.id, &p.key, id)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "runItem.refresh",
        Some(&p.key),
        serde_json::json!({"launchId": launch_id, "id": id}),
    )
    .await;
    let item = rendered_item(&s, &p, id).await?;
    let rev = item.revision;
    let mut r = Json(item).into_response();
    r.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"r{rev}\"")).unwrap_or(HeaderValue::from_static("\"r0\"")),
    );
    Ok(r)
}
