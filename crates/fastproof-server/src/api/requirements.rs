//! Requirements (R): a tree of requirements per project, their links to cases (manual, from
//! code, AI proposals) and the import from issue trackers (`74-tm-next-interfaces.md` §7.6,
//! §9).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use fastproof_store::Project;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::requirements::{
    LinksChanged, RequirementImportParams, RequirementQuery, number_of, valid_tracker_id,
};
use fastproof_store::tm::{
    CaseRequirement, Requirement, RequirementImport, RequirementLinkList, RequirementLinkOrigin,
    RequirementLinkStatus, RequirementLinksChange, RequirementList, RequirementOrigin,
    RequirementPatch, RequirementRow, RequirementStatus, RequirementWrite, TmJob,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
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
        .routes(routes!(list_requirements, create_requirement))
        .routes(routes!(requirement_tree))
        .routes(routes!(import_requirements))
        .routes(routes!(apply_requirement_import))
        .routes(routes!(
            get_requirement,
            update_requirement,
            patch_requirement,
            archive_requirement
        ))
        .routes(routes!(restore_requirement))
        .routes(routes!(change_requirement_links))
        .routes(routes!(list_requirement_cases))
        .routes(routes!(list_case_requirements))
}

pub(crate) fn ctx<'a>(s: &'a AppState, p: &'a Project, auth: &'a Auth) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: &auth.principal.name,
        limits: &s.cfg.tm,
        can_review: auth.principal.allowed(&s.perms.cases_review, Some(&p.key)),
    }
}

/// A requirement number from the path: digits only, otherwise 404.
fn number(s: &str) -> ApiResult<i64> {
    case_number(s).map_err(|_| ApiError::not_found("requirement"))
}

/// A requirement number of a query parameter: `12` or `REQ-12`.
fn number_param(s: Option<&str>, what: &str) -> ApiResult<Option<i64>> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(v) => number_of(v)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request(format!("{what} must be a requirement number"))),
    }
}

type ReqReply = ([(header::HeaderName, HeaderValue); 1], Json<Requirement>);

async fn reply(s: &AppState, p: &Project, row: &RequirementRow) -> ApiResult<ReqReply> {
    let mut v = s.store.requirement_view_of(row).await.map_err(tm_error)?;
    v.description_html = markdown::render(&v.description, &p.key);
    Ok((etag(v.revision), Json(v)))
}

/// Query of `GET /requirements`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RequirementListQuery {
    /// `REQ-12`, `12`, or words of the title, description or issue key.
    pub q: Option<String>,
    /// A requirement number: requirements anywhere under it.
    pub parent: Option<String>,
    pub status: Option<RequirementStatus>,
    /// A requirement type (`epic`, `story`, ...).
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub req_type: Option<String>,
    /// FPQL over requirements.
    pub query: Option<String>,
    /// `true` lists the archived requirements.
    pub archived: Option<bool>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/requirements", tag = "requirements", operation_id = "list_requirements",
    description = "Requirements by number. `parent` lists the ones anywhere under a requirement; `query` is FPQL over \
        requirements (`number, title, type, status, parent, issue, externalState, coverage`). A search (`q` or \
        `query`) is a heavy read (429 or 503 when the caller or the server runs too many) and stops after 5 s.",
    params(("key" = String, Path), RequirementListQuery),
    responses((status = 200, body = RequirementList), (status = 400, body = ErrorBody, description = "`invalid_query`"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody),
        (status = 503, body = ErrorBody)))]
pub async fn list_requirements(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<RequirementListQuery>,
) -> ApiResult<Json<RequirementList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let searches = [&q.q, &q.query]
        .iter()
        .any(|v| v.as_deref().is_some_and(|t| !t.trim().is_empty()));
    let _permit = match searches {
        true => Some(s.guards.heavy(&auth).await?),
        false => None,
    };
    let query = RequirementQuery {
        q: q.q,
        parent: number_param(q.parent.as_deref(), "parent")?,
        status: q.status,
        req_type: q.req_type,
        query: q.query,
        ctx: super::queries::caller(&auth),
        archived: q.archived.unwrap_or(false),
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store
            .list_requirements(p.id, &query)
            .await
            .map_err(tm_error)?,
    ))
}

/// Query of `GET /requirements/tree`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RequirementTreeQuery {
    /// A requirement number; top-level requirements when absent.
    pub parent: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/requirements/tree", tag = "requirements", operation_id = "requirement_tree",
    description = "The direct children of `parent` (the top-level requirements without it) that are not archived, by number.",
    params(("key" = String, Path), RequirementTreeQuery),
    responses((status = 200, body = RequirementList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn requirement_tree(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<RequirementTreeQuery>,
) -> ApiResult<Json<RequirementList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let parent = number_param(q.parent.as_deref(), "parent")?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .requirement_tree(p.id, parent, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/requirements", tag = "requirements", operation_id = "create_requirement",
    description = "Creates a requirement `REQ-<n>`, under `parentNumber` when given. Limits: `maxRequirements`, \
        `maxRequirementDepth`; an issue key is unique per tracker (409 `issue_key_taken`).",
    request_body = RequirementWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = Requirement, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`limit_reached`, `issue_key_taken` or an archived parent")))]
pub async fn create_requirement(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<RequirementWrite>,
) -> ApiResult<(StatusCode, ReqReply)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let row = s
        .store
        .create_requirement(&ctx(&s, &p, &auth), &body, RequirementOrigin::Ui)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "requirement.create",
        Some(&p.key),
        serde_json::json!({"number": row.number, "title": row.title}),
    )
    .await;
    Ok((StatusCode::CREATED, reply(&s, &p, &row).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/requirements/{n}", tag = "requirements", operation_id = "get_requirement",
    params(("key" = String, Path), ("n" = i64, Path, description = "requirement number")),
    responses((status = 200, body = Requirement, headers(("ETag" = String, description = "\"r<revision>\""))),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_requirement(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<ReqReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let row = s.store.requirement_row(p.id, n).await.map_err(tm_error)?;
    reply(&s, &p, &row).await
}

fn audit_change(before: &RequirementRow, after: &RequirementRow) -> serde_json::Value {
    let mut changed: Vec<&str> = Vec::new();
    if before.title != after.title {
        changed.push("title");
    }
    if before.description != after.description {
        changed.push("description");
    }
    if before.req_type != after.req_type {
        changed.push("type");
    }
    if before.status != after.status {
        changed.push("status");
    }
    if before.parent_id != after.parent_id {
        changed.push("parent");
    }
    if before.issue_key != after.issue_key
        || before.tracker_id != after.tracker_id
        || before.url != after.url
    {
        changed.push("tracker");
    }
    serde_json::json!({"number": after.number, "revision": after.revision, "changed": changed})
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/requirements/{n}", tag = "requirements", operation_id = "update_requirement",
    description = "Replaces a requirement (needs `If-Match`); fields left out take their defaults (top level, no tracker \
        issue). Moving it under its own subtree is 409 `requirement_cycle`.",
    request_body = RequirementWrite,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header, description = "\"r<revision>\"")),
    responses((status = 200, body = Requirement, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`requirement_cycle`, `limit_reached`, `issue_key_taken`"),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn update_requirement(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<RequirementWrite>,
) -> ApiResult<ReqReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .update_requirement(&ctx(&s, &p, &auth), n, expected, &body)
        .await
        .map_err(tm_error)?;
    if before.revision != after.revision {
        s.audit(
            &auth,
            "requirement.update",
            Some(&p.key),
            audit_change(&before, &after),
        )
        .await;
    }
    reply(&s, &p, &after).await
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/requirements/{n}", tag = "requirements", operation_id = "patch_requirement",
    description = "Changes the given fields (needs `If-Match`); `parentNumber: null` moves it to the top level.",
    request_body = RequirementPatch,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header, description = "\"r<revision>\"")),
    responses((status = 200, body = Requirement, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`requirement_cycle` or `limit_reached`"),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn patch_requirement(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<RequirementPatch>,
) -> ApiResult<ReqReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .patch_requirement(&ctx(&s, &p, &auth), n, expected, &body)
        .await
        .map_err(tm_error)?;
    if before.revision != after.revision {
        s.audit(
            &auth,
            "requirement.update",
            Some(&p.key),
            audit_change(&before, &after),
        )
        .await;
    }
    reply(&s, &p, &after).await
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/requirements/{n}", tag = "requirements", operation_id = "archive_requirement",
    description = "Archives a requirement with everything under it; their links stay.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn archive_requirement(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let (row, changed) = s
        .store
        .set_requirement_archived(&ctx(&s, &p, &auth), n, true)
        .await
        .map_err(tm_error)?;
    if changed > 0 {
        s.audit(
            &auth,
            "requirement.archive",
            Some(&p.key),
            serde_json::json!({"number": row.number, "requirements": changed}),
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/requirements/{n}/restore", tag = "requirements", operation_id = "restore_requirement",
    description = "Takes a requirement and what was archived with it out of the archive (409 while its parent is archived).",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = Requirement, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn restore_requirement(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<ReqReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    let (row, changed) = s
        .store
        .set_requirement_archived(&ctx(&s, &p, &auth), n, false)
        .await
        .map_err(tm_error)?;
    if changed > 0 {
        s.audit(
            &auth,
            "requirement.restore",
            Some(&p.key),
            serde_json::json!({"number": row.number, "requirements": changed}),
        )
        .await;
    }
    reply(&s, &p, &row).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/requirements/{n}/links", tag = "requirements", operation_id = "change_requirement_links",
    description = "Adds, removes and confirms links to cases by case number; `confirm` (or `add`) turns AI-proposed links \
        into active ones. At most `maxRequirementCases` links per requirement (409 `limit_reached`); an unknown case is 404. \
        Links from code come back with the next trusted sync while the code names the requirement.",
    request_body = RequirementLinksChange,
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = Requirement, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn change_requirement_links(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<RequirementLinksChange>,
) -> ApiResult<ReqReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = number(&n)?;
    crate::tm_rate::admit(&s, &auth)?;
    let (row, ch): (RequirementRow, LinksChanged) = s
        .store
        .change_requirement_links(&ctx(&s, &p, &auth), n, &body, RequirementLinkOrigin::Ui)
        .await
        .map_err(tm_error)?;
    if !ch.is_empty() {
        s.audit(
            &auth,
            "requirement.links",
            Some(&p.key),
            serde_json::json!({"number": row.number, "added": ch.added, "removed": ch.removed,
                "confirmed": ch.confirmed}),
        )
        .await;
    }
    reply(&s, &p, &row).await
}

/// Query of `GET /requirements/{n}/cases`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RequirementCasesQuery {
    /// Only active links, or only AI proposals.
    #[serde(rename = "linkStatus")]
    #[param(rename = "linkStatus")]
    pub link_status: Option<RequirementLinkStatus>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/requirements/{n}/cases", tag = "requirements", operation_id = "list_requirement_cases",
    description = "The cases linked to a requirement (not those of the requirements under it), by case number, with \
        their last result.",
    params(("key" = String, Path), ("n" = i64, Path), RequirementCasesQuery),
    responses((status = 200, body = RequirementLinkList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_requirement_cases(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<RequirementCasesQuery>,
) -> ApiResult<Json<RequirementLinkList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = number(&n)?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .requirement_cases(p.id, n, q.link_status, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/requirements", tag = "requirements", operation_id = "list_case_requirements",
    description = "The requirements (not archived) linked to a case, by number; at most 1000.",
    params(("key" = String, Path), ("n" = i64, Path, description = "case number")),
    responses((status = 200, body = Vec<CaseRequirement>), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list_case_requirements(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<Json<Vec<CaseRequirement>>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    Ok(Json(
        s.store.case_requirements(p.id, n).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/requirements/import", tag = "requirements", operation_id = "import_requirements",
    description = "Starts a dry run of an import of requirements from a configured issue tracker (`trackerId` with `api` \
        settings): `query` is JQL (Jira), a label (GitLab, GitHub) or WIQL (Azure DevOps). The `requirements` job reads \
        at most 1000 issues of the tracker's project, then stops at `ready` with the counts; \
        `POST /requirements/import/{id}/apply` applies it. Issues are matched by tracker and issue key, so a repeated \
        import updates instead of duplicating. Nothing is written to the tracker.",
    request_body = RequirementImport,
    params(("key" = String, Path)),
    responses((status = 202, body = TmJob), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`import_busy`"),
        (status = 429, body = ErrorBody)))]
pub async fn import_requirements(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<RequirementImport>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let tracker_id = body.tracker_id.trim().to_string();
    if !valid_tracker_id(&tracker_id) {
        return Err(ApiError::bad_request("trackerId is not a tracker id"));
    }
    let query = crate::integrations::requirements_import::check_query(&body.query)?;
    crate::integrations::requirements_import::check_tracker(&s, p.id, &tracker_id, &query).await?;
    let params = RequirementImportParams {
        tracker_id: tracker_id.clone(),
        query: query.clone(),
        parent_number: body.parent_number,
        phase: "dryRun".into(),
        items: Vec::new(),
    };
    let job = s
        .store
        .start_requirement_import(
            &ctx(&s, &p, &auth),
            &crate::limits::caller_key(&auth),
            &params,
        )
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "requirement.import",
        Some(&p.key),
        serde_json::json!({"jobId": job.id, "phase": "dryRun", "trackerId": tracker_id, "query": query,
            "parentNumber": body.parent_number}),
    )
    .await;
    crate::tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/requirements/import/{id}/apply", tag = "requirements", operation_id = "apply_requirement_import",
    description = "Applies a requirement import whose dry run is `ready` (409 `job_not_ready` otherwise).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 202, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn apply_requirement_import(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    let job = s
        .store
        .apply_requirement_import(p.id, &p.key, id)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "requirement.import",
        Some(&p.key),
        serde_json::json!({"jobId": id, "phase": "apply"}),
    )
    .await;
    crate::tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}
