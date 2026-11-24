//! Test cases (T2a): cases, the tree, bulk actions, versions, history, identities, attachments,
//! AI suggestions, Markdown preview and the backfill of cases from existing tests.

use axum::Json;
use axum::extract::{RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use fastproof_blob::{BlobKey, spool_stream};
use fastproof_store::Project;
use fastproof_store::tm::cases::{
    CaseAttachmentUpload, CaseCtx, CaseQuery, CaseSort, HistoryQuery, IdentityQuery,
};
use fastproof_store::tm::{
    AttachmentRef, Case, CaseBulk, CaseBulkResult, CaseHistoryEntry, CaseList, CasePatch, CaseRow,
    CaseStatus, CaseSuggestion, CaseTree, CaseTreeGroupBy, CaseVersion, CaseVersionSummary,
    CaseWrite, ResultSource, SuggestionStatus, TestIdentity, TmJob,
};
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiJson, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::attachments::{NAME_HEADER, clean_content_type};
use super::uploads::{clean_file_name, identity_encoding, spool_error};
use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::markdown;
use crate::state::AppState;

/// Bytes of Markdown `POST /markdown/preview` renders, at most.
pub const MAX_PREVIEW_BYTES: usize = 64 * 1024;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_cases, create_case))
        .routes(routes!(case_tree))
        .routes(routes!(bulk_cases))
        .routes(routes!(start_case_backfill))
        .routes(routes!(get_case, update_case, patch_case, archive_case))
        .routes(routes!(restore_case))
        .routes(routes!(review_case))
        .routes(routes!(list_case_versions))
        .routes(routes!(get_case_version))
        .routes(routes!(restore_case_version))
        .routes(routes!(case_history))
        .routes(routes!(link_case_identity))
        .routes(routes!(unlink_case_identity))
        .routes(routes!(list_test_identities))
        .routes(routes!(upload_case_attachment))
        .routes(routes!(markdown_preview))
        .routes(routes!(list_case_suggestions))
        .routes(routes!(accept_case_suggestion))
        .routes(routes!(reject_case_suggestion))
}

/// A case number from the path: digits only, otherwise 404.
pub fn case_number(s: &str) -> ApiResult<i64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::not_found("case"));
    }
    s.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::not_found("case"))
}

pub use super::if_match;

/// `ETag: "r<revision>"`.
pub fn etag(revision: i64) -> [(header::HeaderName, HeaderValue); 1] {
    [(
        header::ETAG,
        HeaderValue::from_str(&format!("\"r{revision}\""))
            .unwrap_or(HeaderValue::from_static("\"r0\"")),
    )]
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

type CaseReply = ([(header::HeaderName, HeaderValue); 1], Json<Case>);

async fn reply(s: &AppState, p: &Project, row: &CaseRow) -> ApiResult<CaseReply> {
    let mut c = s.store.case_view_of(p.id, row).await.map_err(tm_error)?;
    c.html = markdown::render_body(&c.body, &p.key);
    Ok((etag(c.revision), Json(c)))
}

/// Query pairs (repeated keys kept).
fn pairs(raw: Option<&str>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .into_owned()
        .take(1000)
        .collect()
}

fn parse_bool(k: &str, v: &str) -> ApiResult<bool> {
    match v {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(ApiError::bad_request(format!("{k} must be true or false"))),
    }
}

/// Query of `GET /cases` (for the documentation; `status`, `field` and `tag` may repeat).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct CaseListQuery {
    /// A folder id: its cases, with `recursive=true` also those of its subfolders.
    pub folder: Option<Uuid>,
    pub recursive: Option<bool>,
    /// Words of the name, description, preconditions or steps, or a number (`TC-12`).
    pub q: Option<String>,
    /// `draft`, `active` or `outdated`; repeated or comma-separated.
    pub status: Option<String>,
    pub automation: Option<ResultSource>,
    /// `key:value` of a case field; repeated: the same key matches any value, different keys all.
    pub field: Option<String>,
    /// A tag; repeated: all of them.
    pub tag: Option<String>,
    /// Automated cases without a trusted result for this many days.
    pub stale_days: Option<u32>,
    pub ai_unreviewed: Option<bool>,
    /// `true` lists the archived cases (the trash).
    pub archived: Option<bool>,
    /// `number` (default), `-number` or `updated` (most recent first).
    pub sort: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

pub fn case_query(raw: Option<&str>) -> ApiResult<CaseQuery> {
    let mut q = CaseQuery::default();
    let mut limit = None;
    let mut fields: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut statuses = Vec::new();
    let mut tags = Vec::new();
    for (k, v) in pairs(raw) {
        match k.as_str() {
            "folder" => {
                let id: Uuid = v
                    .parse()
                    .map_err(|_| ApiError::bad_request("folder must be a folder id"))?;
                q.filter.folder_ids = Some(vec![id]);
            }
            "recursive" => q.filter.recursive = Some(parse_bool(&k, &v)?),
            "q" => q.filter.q = Some(v),
            "status" => {
                for s in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    statuses.push(CaseStatus::parse(s).ok_or_else(|| {
                        ApiError::bad_request("status must be draft, active or outdated")
                    })?);
                }
            }
            "automation" => {
                q.filter.automation = Some(ResultSource::parse(&v).ok_or_else(|| {
                    ApiError::bad_request("automation must be manual or automated")
                })?)
            }
            "field" => {
                let (fk, fv) = super::key_value(Some(&v), "field")?.unwrap_or_default();
                fields.entry(fk).or_default().push(fv);
            }
            "tag" => tags.push(v),
            "staleDays" => {
                q.stale_days = Some(
                    v.parse()
                        .ok()
                        .filter(|d| (1..=36_500).contains(d))
                        .ok_or_else(|| ApiError::bad_request("staleDays must be 1..=36500"))?,
                )
            }
            "aiUnreviewed" => q.ai_unreviewed = Some(parse_bool(&k, &v)?),
            "archived" => q.archived = parse_bool(&k, &v)?,
            "sort" => {
                q.sort = CaseSort::parse(&v).ok_or_else(|| {
                    ApiError::bad_request("sort must be number, -number or updated")
                })?
            }
            "before" => q.before = Some(v),
            "limit" => {
                limit = Some(
                    v.parse::<i64>()
                        .map_err(|_| ApiError::bad_request("limit must be a number"))?,
                )
            }
            _ => {}
        }
    }
    if !statuses.is_empty() {
        q.filter.statuses = Some(statuses);
    }
    if !tags.is_empty() {
        q.filter.tags = Some(tags);
    }
    if !fields.is_empty() {
        q.filter.fields = Some(fields);
    }
    q.limit = page_limit(limit)?;
    Ok(q)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases", tag = "cases", operation_id = "list_cases",
    description = "A page of cases (not archived unless `archived=true`) with the newest result over all identities.",
    params(("key" = String, Path), CaseListQuery),
    responses((status = 200, body = CaseList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_cases(
    State(s): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    P(key): P<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<CaseList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    crate::tm_people::note(&s, &auth, &headers);
    let q = case_query(raw.as_deref())?;
    Ok(Json(s.store.list_cases(p.id, &q).await.map_err(tm_error)?))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases", tag = "cases", operation_id = "create_case",
    request_body = CaseWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody,
        description = "`invalid_case` or `invalid_markdown`"), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody, description = "folder or attachment of another project"),
        (status = 409, body = ErrorBody, description = "`limit_reached`")))]
pub async fn create_case(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseWrite>,
) -> ApiResult<(StatusCode, CaseReply)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let row = s
        .store
        .create_case(&ctx(&s, &p, &auth), &body)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.create",
        Some(&p.key),
        serde_json::json!({"number": row.number, "name": row.name}),
    )
    .await;
    Ok((StatusCode::CREATED, reply(&s, &p, &row).await?))
}

/// Query of `GET /cases/tree`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct CaseTreeQuery {
    /// `folder` (default), `behavior` (epic → feature → story) or `suite`.
    pub group_by: Option<CaseTreeGroupBy>,
    /// The node whose children are listed (a folder id, or a value path joined by U+001F).
    pub parent: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/tree", tag = "cases", operation_id = "case_tree",
    params(("key" = String, Path), CaseTreeQuery),
    responses((status = 200, body = CaseTree), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn case_tree(
    State(s): State<AppState>,
    auth: Auth,
    headers: axum::http::HeaderMap,
    P(key): P<String>,
    Q(q): Q<CaseTreeQuery>,
) -> ApiResult<Json<CaseTree>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    crate::tm_people::note(&s, &auth, &headers);
    if q.parent.as_ref().is_some_and(|v| v.len() > 4096) {
        return Err(ApiError::bad_request("parent has at most 4096 bytes"));
    }
    Ok(Json(
        s.store
            .case_tree(
                p.id,
                q.group_by.unwrap_or(CaseTreeGroupBy::Folder),
                q.parent.as_deref(),
            )
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}", tag = "cases", operation_id = "get_case",
    params(("key" = String, Path), ("n" = i64, Path, description = "case number")),
    responses((status = 200, body = Case, headers(("ETag" = String, description = "\"r<revision>\""))),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_case(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    let mut c = s.store.case_view(p.id, n).await.map_err(tm_error)?;
    c.html = markdown::render_body(&c.body, &p.key);
    Ok((etag(c.revision), Json(c)))
}

fn audit_change(before: &CaseRow, after: &CaseRow) -> Vec<(&'static str, serde_json::Value)> {
    if before.revision == after.revision {
        return Vec::new();
    }
    let mut out = vec![(
        "case.update",
        serde_json::json!({"number": after.number, "revision": after.revision, "version": after.version}),
    )];
    if before.folder_id != after.folder_id {
        out.push((
            "case.move",
            serde_json::json!({"number": after.number, "from": before.folder_id, "to": after.folder_id}),
        ));
    }
    out
}

/// Body of `PUT /cases/{n}`: a [`CaseWrite`] and whether it named `folderId` (absent keeps the
/// folder, `null` moves the case to the root).
pub struct CasePut {
    pub write: CaseWrite,
    pub folder_given: bool,
}

impl<'de> Deserialize<'de> for CasePut {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let folder_given = v.get("folderId").is_some();
        let write = CaseWrite::deserialize(v).map_err(serde::de::Error::custom)?;
        Ok(Self {
            write,
            folder_given,
        })
    }
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/cases/{n}", tag = "cases", operation_id = "update_case",
    description = "Replaces the content of a case. Fields, tags, status and `folderId` absent from the body keep their \
        values (`folderId: null` moves the case to the root). Values that code owns on an automated case cannot change \
        (409 `field_from_code`).",
    request_body = CaseWrite,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header, description = "\"r<revision>\"")),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`field_from_code` or `limit_reached`"),
        (status = 412, body = ErrorBody, description = "`precondition_failed` with `{currentRevision, changedBy, source}`"),
        (status = 428, body = ErrorBody)))]
pub async fn update_case(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<CasePut>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .update_case(
            &ctx(&s, &p, &auth),
            n,
            expected,
            &body.write,
            !body.folder_given,
        )
        .await
        .map_err(tm_error)?;
    for (action, detail) in audit_change(&before, &after) {
        s.audit(&auth, action, Some(&p.key), detail).await;
    }
    reply(&s, &p, &after).await
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/cases/{n}", tag = "cases", operation_id = "patch_case",
    description = "Merges folder, status, field values (`null` removes one) and tags into a case.",
    request_body = CasePatch,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn patch_case(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<CasePatch>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .patch_case(&ctx(&s, &p, &auth), n, expected, &body)
        .await
        .map_err(tm_error)?;
    for (action, detail) in audit_change(&before, &after) {
        s.audit(&auth, action, Some(&p.key), detail).await;
    }
    reply(&s, &p, &after).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/review", tag = "cases", operation_id = "review_case",
    description = "Marks a case an AI agent created as reviewed: clears `aiUnreviewed` (no new version). Audited as \
        `case.review`; a case that is already reviewed is returned unchanged.",
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn review_case(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let expected = if_match(&headers)?;
    let (before, after) = s
        .store
        .mark_case_reviewed(&ctx(&s, &p, &auth), n, expected)
        .await
        .map_err(tm_error)?;
    if before.ai_unreviewed {
        s.audit(
            &auth,
            "case.review",
            Some(&p.key),
            serde_json::json!({"number": after.number}),
        )
        .await;
    }
    reply(&s, &p, &after).await
}

/// Query of `DELETE /cases/{n}`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ArchiveQuery {
    /// Deletes the case for good (`settings:write`).
    pub purge: Option<bool>,
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/cases/{n}", tag = "cases", operation_id = "archive_case",
    description = "Moves a case to the trash (archive); `purge=true` deletes it with its versions (needs `settings:write`; \
        cases executed in manual runs answer 409 `case_has_runs`).",
    params(("key" = String, Path), ("n" = i64, Path), ArchiveQuery),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn archive_case(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<ArchiveQuery>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    if q.purge == Some(true) {
        require(&auth, &s.perms.settings_write, Some(&p.key))?;
        let row = s
            .store
            .purge_case(&ctx(&s, &p, &auth), n)
            .await
            .map_err(tm_error)?;
        s.audit(
            &auth,
            "case.purge",
            Some(&p.key),
            serde_json::json!({"number": row.number, "name": row.name}),
        )
        .await;
        return Ok(StatusCode::NO_CONTENT);
    }
    let row = s
        .store
        .set_case_archived(&ctx(&s, &p, &auth), n, true)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.archive",
        Some(&p.key),
        serde_json::json!({"number": row.number}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/restore", tag = "cases", operation_id = "restore_case",
    description = "Takes a case out of the trash.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn restore_case(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let row = s
        .store
        .set_case_archived(&ctx(&s, &p, &auth), n, false)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.restore",
        Some(&p.key),
        serde_json::json!({"number": row.number}),
    )
    .await;
    reply(&s, &p, &row).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/bulk", tag = "cases", operation_id = "bulk_cases",
    description = "One action on cases named by number or matched by a filter (at most `maxBulk`). `move` takes a folder \
        id or null, `setStatus` a status, `setField` `{key, value}`, `addTags`/`removeTags` a list. Each changed case gets one \
        version with `source = bulk`; the others are listed in `skipped` with a code.",
    request_body = CaseBulk,
    params(("key" = String, Path)),
    responses((status = 200, body = CaseBulkResult), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`limit_reached`")))]
pub async fn bulk_cases(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseBulk>,
) -> ApiResult<Json<CaseBulkResult>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    let r = s
        .store
        .bulk_cases(&ctx(&s, &p, &auth), &body)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.bulk",
        Some(&p.key),
        serde_json::json!({"action": body.action, "updated": r.updated, "skipped": r.skipped.len(),
            "byFilter": body.filter.is_some()}),
    )
    .await;
    Ok(Json(r))
}

/// A page of versions.
#[derive(Serialize, ToSchema)]
pub struct CaseVersionList {
    pub items: Vec<CaseVersionSummary>,
    pub next: Option<String>,
}

/// Query of paged TM lists.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/versions", tag = "cases", operation_id = "list_case_versions",
    params(("key" = String, Path), ("n" = i64, Path), PageQuery),
    responses((status = 200, body = CaseVersionList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_case_versions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<PageQuery>,
) -> ApiResult<Json<CaseVersionList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    let limit = page_limit(q.limit)?;
    let (items, next) = s
        .store
        .case_versions(p.id, n, q.before.as_deref(), limit)
        .await
        .map_err(tm_error)?;
    Ok(Json(CaseVersionList { items, next }))
}

fn version_number(v: &str) -> ApiResult<i32> {
    if v.is_empty() || v.len() > 9 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::not_found("version"));
    }
    v.parse()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::not_found("version"))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/versions/{v}", tag = "cases", operation_id = "get_case_version",
    params(("key" = String, Path), ("n" = i64, Path), ("v" = i32, Path)),
    responses((status = 200, body = CaseVersion), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_case_version(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n, v)): P<(String, String, String)>,
) -> ApiResult<Json<CaseVersion>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    let v = version_number(&v)?;
    Ok(Json(
        s.store.case_version(p.id, n, v).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/versions/{v}/restore", tag = "cases",
    operation_id = "restore_case_version",
    description = "Makes the content of a version current as a new version (`source = restore`); values code owns on an \
        automated case stay.",
    params(("key" = String, Path), ("n" = i64, Path), ("v" = i32, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn restore_case_version(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n, v)): P<(String, String, String)>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let v = version_number(&v)?;
    let expected = if_match(&headers)?;
    let row = s
        .store
        .restore_case_version(&ctx(&s, &p, &auth), n, v, expected)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.update",
        Some(&p.key),
        serde_json::json!({"number": row.number, "restoredVersion": v, "version": row.version}),
    )
    .await;
    reply(&s, &p, &row).await
}

/// Query of `GET /cases/{n}/history`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub source: Option<ResultSource>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// A page of the history of a case.
#[derive(Serialize, ToSchema)]
pub struct CaseHistoryList {
    pub items: Vec<CaseHistoryEntry>,
    pub next: Option<String>,
}

fn short(name: &str, v: &Option<String>) -> ApiResult<()> {
    if v.as_ref().is_some_and(|v| v.len() > 500) {
        return Err(ApiError::bad_request(format!(
            "{name} has at most 500 bytes"
        )));
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/history", tag = "cases", operation_id = "case_history",
    description = "Results of the case over all its identities (manual and automated), newest first.",
    params(("key" = String, Path), ("n" = i64, Path), HistoryParams),
    responses((status = 200, body = CaseHistoryList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn case_history(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<HistoryParams>,
) -> ApiResult<Json<CaseHistoryList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    short("branch", &q.branch)?;
    short("environment", &q.environment)?;
    let hq = HistoryQuery {
        branch: q.branch,
        environment: q.environment,
        source: q.source,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    let (items, next) = s.store.case_history(p.id, n, &hq).await.map_err(tm_error)?;
    Ok(Json(CaseHistoryList { items, next }))
}

/// Body of `POST /cases/{n}/identities`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseIdentityLink {
    pub test_case_id: Uuid,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/identities", tag = "cases", operation_id = "link_case_identity",
    description = "Links a code identity (a test as uploads name it) to the case, e.g. after a rename. When it was the last \
        identity of another case, that case is archived with a link to this one.",
    request_body = CaseIdentityLink,
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn link_case_identity(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<CaseIdentityLink>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let (row, dup) = s
        .store
        .link_case_identity(&ctx(&s, &p, &auth), n, body.test_case_id)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.linkIdentity",
        Some(&p.key),
        serde_json::json!({"number": row.number, "testCaseId": body.test_case_id, "archivedDuplicate": dup}),
    )
    .await;
    reply(&s, &p, &row).await
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/cases/{n}/identities/{testCaseId}", tag = "cases",
    operation_id = "unlink_case_identity",
    params(("key" = String, Path), ("n" = i64, Path), ("testCaseId" = Uuid, Path)),
    responses((status = 200, body = Case, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn unlink_case_identity(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n, tc)): P<(String, String, Uuid)>,
) -> ApiResult<CaseReply> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    let row = s
        .store
        .unlink_case_identity(&ctx(&s, &p, &auth), n, tc)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.unlinkIdentity",
        Some(&p.key),
        serde_json::json!({"number": row.number, "testCaseId": tc}),
    )
    .await;
    reply(&s, &p, &row).await
}

/// Query of `GET /test-identities`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IdentityParams {
    /// Only tests without a case (feature branches, untrusted launches, new tests).
    pub unlinked: Option<bool>,
    /// Seen on this branch.
    pub branch: Option<String>,
    /// Substring of the full or display name.
    pub q: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// A page of test identities.
#[derive(Serialize, ToSchema)]
pub struct TestIdentityList {
    pub items: Vec<TestIdentity>,
    pub next: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-identities", tag = "cases", operation_id = "list_test_identities",
    description = "Code identities of the project, most recently seen first, with the branches they were seen on.",
    params(("key" = String, Path), IdentityParams),
    responses((status = 200, body = TestIdentityList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_test_identities(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<IdentityParams>,
) -> ApiResult<Json<TestIdentityList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    short("branch", &q.branch)?;
    short("q", &q.q)?;
    let iq = IdentityQuery {
        unlinked: q.unlinked.unwrap_or(false),
        branch: q.branch,
        q: q.q,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    let (items, next) = s.store.test_identities(p.id, &iq).await.map_err(tm_error)?;
    Ok(Json(TestIdentityList { items, next }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/attachments", tag = "cases", operation_id = "upload_case_attachment",
    description = "Stores an attachment of the case (raw body, content-addressed per project); reference it from a step's \
        `attachments` or as `![alt](attachment:<id>)` in Markdown.",
    params(("key" = String, Path), ("n" = i64, Path),
        ("X-FastProof-Attachment-Name" = Option<String>, Header, description = "file name")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = AttachmentRef), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`limit_reached` (maxCaseAttachments)"),
        (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn upload_case_attachment(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<AttachmentRef>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let n = case_number(&n)?;
    if s.store.case_by_number(p.id, n).await?.is_none() {
        return Err(ApiError::not_found("case"));
    }
    identity_encoding(&req)?;
    let ct = clean_content_type(
        req.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    );
    let name = req
        .headers()
        .get(NAME_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(clean_file_name)
        .unwrap_or_else(|| "attachment".to_string());
    let _spool = s.guards.spool().await?;
    let dir = s.cfg.spool_dir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(Uuid::now_v7().to_string());
    let spooled = spool_stream(
        &path,
        req.into_body().into_data_stream().boxed(),
        s.cfg.tm.max_attachment_bytes,
    )
    .await
    .map_err(|e| spool_error(e, "maxAttachmentBytes"))?;
    let result = async {
        let existing = s
            .store
            .touch_attachments(p.id, std::slice::from_ref(&spooled.sha256))
            .await?;
        let blob_key = BlobKey::attachment(&p.key, &spooled.sha256)?;
        if !existing.contains_key(&spooled.sha256) {
            s.store
                .cancel_blob_deletions(&[blob_key.to_string()])
                .await?;
            s.blobs.put_file(&blob_key, &spooled.path).await?;
        }
        Ok::<_, anyhow::Error>(blob_key.to_string())
    }
    .await;
    spooled.remove();
    let blob_key = result?;
    let a = s
        .store
        .add_case_attachment(
            &ctx(&s, &p, &auth),
            n,
            &CaseAttachmentUpload {
                sha256: spooled.sha256.clone(),
                size: spooled.size as i64,
                content_type: ct,
                name,
                blob_key,
            },
        )
        .await
        .map_err(tm_error)?;
    Ok((StatusCode::CREATED, Json(a)))
}

/// Body of `POST /markdown/preview`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkdownPreview {
    pub markdown: String,
}

/// Rendered Markdown.
#[derive(Serialize, ToSchema)]
pub struct MarkdownHtml {
    pub html: String,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/markdown/preview", tag = "cases", operation_id = "markdown_preview",
    description = "Renders Markdown as cases show it (at most 64 KiB; raw HTML becomes text).",
    request_body = MarkdownPreview,
    params(("key" = String, Path)),
    responses((status = 200, body = MarkdownHtml), (status = 400, body = ErrorBody, description = "`invalid_markdown`"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn markdown_preview(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<MarkdownPreview>,
) -> ApiResult<Json<MarkdownHtml>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let html = markdown::render_checked("markdown", &body.markdown, &p.key, MAX_PREVIEW_BYTES)?;
    Ok(Json(MarkdownHtml { html }))
}

/// Query of `GET /cases/{n}/suggestions`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestionQuery {
    pub status: Option<SuggestionStatus>,
    pub limit: Option<i64>,
}

/// Suggestions of a case.
#[derive(Serialize, ToSchema)]
pub struct CaseSuggestionList {
    pub items: Vec<CaseSuggestion>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/suggestions", tag = "cases", operation_id = "list_case_suggestions",
    params(("key" = String, Path), ("n" = i64, Path), SuggestionQuery),
    responses((status = 200, body = CaseSuggestionList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_case_suggestions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
    Q(q): Q<SuggestionQuery>,
) -> ApiResult<Json<CaseSuggestionList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let n = case_number(&n)?;
    let items = s
        .store
        .case_suggestions(p.id, n, q.status, page_limit(q.limit)?)
        .await
        .map_err(tm_error)?;
    Ok(Json(CaseSuggestionList { items }))
}

/// A decided suggestion and the case.
#[derive(Serialize, ToSchema)]
pub struct CaseSuggestionAccepted {
    pub suggestion: CaseSuggestion,
    pub case: Case,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/case-suggestions/{id}/accept", tag = "cases",
    operation_id = "accept_case_suggestion",
    description = "Applies an AI suggestion as a version with `source = ai`. `If-Match` is the ETag of the case; a case \
        that moved since the suggestion was made answers 409 `suggestion_stale`.",
    params(("key" = String, Path), ("id" = Uuid, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = CaseSuggestionAccepted), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 412, body = ErrorBody),
        (status = 428, body = ErrorBody)))]
pub async fn accept_case_suggestion(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<(
    [(header::HeaderName, HeaderValue); 1],
    Json<CaseSuggestionAccepted>,
)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let expected = if_match(&headers)?;
    let (sug, row) = s
        .store
        .accept_case_suggestion(&ctx(&s, &p, &auth), id, expected)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseSuggestion.accept",
        Some(&p.key),
        serde_json::json!({"id": id, "number": row.number, "agent": sug.agent, "version": row.version}),
    )
    .await;
    let (tag, Json(case)) = reply(&s, &p, &row).await?;
    Ok((
        tag,
        Json(CaseSuggestionAccepted {
            suggestion: sug,
            case,
        }),
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/case-suggestions/{id}/reject", tag = "cases",
    operation_id = "reject_case_suggestion",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = CaseSuggestion), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn reject_case_suggestion(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<CaseSuggestion>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let sug = s
        .store
        .reject_case_suggestion(&ctx(&s, &p, &auth), id)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseSuggestion.reject",
        Some(&p.key),
        serde_json::json!({"id": id, "number": sug.case_number, "agent": sug.agent}),
    )
    .await;
    Ok(Json(sug))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/backfill", tag = "cases", operation_id = "start_case_backfill",
    description = "Starts creating cases for the existing tests seen on trusted branches (`caseSync`), or returns the \
        backfill already queued or running. Runs once by itself after the upgrade.",
    params(("key" = String, Path)),
    responses((status = 202, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn start_case_backfill(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let job = s
        .store
        .start_case_backfill(p.id, Some(&auth.principal.name))
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "case.backfill",
        Some(&p.key),
        serde_json::json!({"job": job.id}),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(job)))
}
