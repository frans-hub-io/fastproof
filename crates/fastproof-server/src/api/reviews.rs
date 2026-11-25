//! Version review of cases and shared steps (§7.3 of the TM next contract): the review of one
//! object, votes, bulk review "as one PR" and the queue.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use fastproof_store::Project;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::review::ReviewQueueQuery;
use fastproof_store::tm::{
    ReviewBulk, ReviewBulkResult, ReviewKind, ReviewQueue, ReviewState, ReviewThread,
    ReviewVoteWrite,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::cases::{case_number, etag};
use super::{ErrorBody, P, Q, if_match, page_limit, tm_error};
use crate::state::AppState;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_reviews))
        .routes(routes!(bulk_review))
        .routes(routes!(get_case_review, vote_case_review))
        .routes(routes!(get_shared_step_review, vote_shared_step_review))
}

/// A shared step number from the path: digits only, otherwise 404.
pub fn shared_step_number(s: &str) -> ApiResult<i64> {
    case_number(s).map_err(|_| ApiError::not_found("shared step"))
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

type ThreadReply = ([(header::HeaderName, HeaderValue); 1], Json<ReviewThread>);

/// Query of `GET /reviews`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ReviewListQuery {
    /// `case` or `sharedStep`.
    pub kind: Option<ReviewKind>,
    /// `pending` or `changes_requested` (default: both).
    pub state: Option<ReviewState>,
    /// `me`: what the caller may approve and has not voted on at the head.
    pub reviewer: Option<String>,
    /// `me`: what the caller changed last.
    pub author: Option<String>,
    /// A folder id: its cases and those of its subfolders (no shared steps).
    pub folder: Option<Uuid>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

fn me_flag(name: &str, v: Option<&str>) -> ApiResult<bool> {
    match v {
        None => Ok(false),
        Some("me") => Ok(true),
        Some(_) => Err(ApiError::bad_request(format!("{name} must be me"))),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/reviews", tag = "reviews", operation_id = "list_reviews",
    description = "Cases and shared steps waiting for review (`pending` or `changes_requested`), most recently changed \
        first. `reviewer=me` keeps what the caller may approve and has not voted on at the head; `author=me` what the \
        caller changed last. Empty while review is off.",
    params(("key" = String, Path), ReviewListQuery),
    responses((status = 200, body = ReviewQueue), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_reviews(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<ReviewListQuery>,
) -> ApiResult<Json<ReviewQueue>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let limit = page_limit(q.limit)?;
    let query = ReviewQueueQuery {
        kind: q.kind,
        state: q.state,
        reviewer_me: me_flag("reviewer", q.reviewer.as_deref())?,
        author_me: me_flag("author", q.author.as_deref())?,
        folder: q.folder,
        before: q.before,
        limit,
    };
    Ok(Json(
        s.store
            .review_queue(
                p.id,
                &query,
                &auth.principal.name,
                s.can_review(&auth, &p.key),
            )
            .await
            .map_err(tm_error)?,
    ))
}

async fn thread(
    s: &AppState,
    auth: &Auth,
    key: &str,
    kind: ReviewKind,
    n: i64,
) -> ApiResult<ThreadReply> {
    let p = s.project_for(auth, key, &s.perms.cases_read).await?;
    let (t, revision) = s
        .store
        .review_thread_tagged(
            p.id,
            kind,
            n,
            &auth.principal.name,
            s.can_review(auth, &p.key),
        )
        .await
        .map_err(tm_error)?;
    Ok((etag(revision), Json(t)))
}

async fn vote(
    s: &AppState,
    auth: &Auth,
    headers: &HeaderMap,
    key: &str,
    kind: ReviewKind,
    n: i64,
    body: &ReviewVoteWrite,
) -> ApiResult<ThreadReply> {
    let p = s.project_for(auth, key, &s.perms.cases_review).await?;
    let expected = if_match(headers)?;
    crate::tm_rate::admit(s, auth)?;
    let (t, revision) = s
        .store
        .vote(&ctx(s, &p, auth), kind, n, expected, body)
        .await
        .map_err(tm_error)?;
    s.audit(
        auth,
        "caseReview.vote",
        Some(&p.key),
        serde_json::json!({"kind": kind, "number": n, "version": body.version,
            "decision": body.decision, "reviewState": t.review_state}),
    )
    .await;
    Ok((etag(revision), Json(t)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/{n}/reviews", tag = "reviews", operation_id = "get_case_review",
    description = "The review of a case: head and approved version, votes (newest first), whether the caller may vote \
        (`whyNot` otherwise) and the workflow transitions from its state. `ETag` is the case revision a vote sends.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = ReviewThread, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn get_case_review(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<ThreadReply> {
    let n = case_number(&n)?;
    thread(&s, &auth, &key, ReviewKind::Case, n).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/{n}/reviews", tag = "reviews", operation_id = "vote_case_review",
    description = "Approves or requests changes to the head version of a case (`cases:review`). The version must be the \
        head (409 `version_moved {currentVersion}`) and waiting for review (409 `not_pending`); review must be on for the \
        case (409 `review_not_enabled`); the head's author does not vote unless `allowSelfApproval` (403 \
        `self_approval`), nor the creator of an unreviewed AI draft (403 `ai_self_review`). A later vote of the same \
        reviewer on the same version replaces the earlier one. Audited as `caseReview.vote`.",
    request_body = ReviewVoteWrite,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = ReviewThread, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn vote_case_review(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<ReviewVoteWrite>,
) -> ApiResult<ThreadReply> {
    let n = case_number(&n)?;
    vote(&s, &auth, &headers, &key, ReviewKind::Case, n, &body).await
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/shared-steps/{n}/reviews", tag = "reviews",
    operation_id = "get_shared_step_review",
    description = "The review of a shared step, as for a case (no transitions). `ETag` is the shared step revision.",
    params(("key" = String, Path), ("n" = i64, Path)),
    responses((status = 200, body = ReviewThread, headers(("ETag" = String))), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn get_shared_step_review(
    State(s): State<AppState>,
    auth: Auth,
    P((key, n)): P<(String, String)>,
) -> ApiResult<ThreadReply> {
    let n = shared_step_number(&n)?;
    thread(&s, &auth, &key, ReviewKind::SharedStep, n).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/shared-steps/{n}/reviews", tag = "reviews",
    operation_id = "vote_shared_step_review",
    description = "A vote on the head version of a shared step, with the rules of a case vote.",
    request_body = ReviewVoteWrite,
    params(("key" = String, Path), ("n" = i64, Path), ("If-Match" = String, Header)),
    responses((status = 200, body = ReviewThread, headers(("ETag" = String))), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn vote_shared_step_review(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, n)): P<(String, String)>,
    ApiJson(body): ApiJson<ReviewVoteWrite>,
) -> ApiResult<ThreadReply> {
    let n = shared_step_number(&n)?;
    vote(&s, &auth, &headers, &key, ReviewKind::SharedStep, n, &body).await
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/reviews/bulk", tag = "reviews", operation_id = "bulk_review",
    description = "One decision on up to `maxBulkReview` heads (the changed cases of a folder, plan or query reviewed as \
        one change). Each item is checked like a single vote and skipped with the refusal's code (`version_moved`, \
        `self_approval`, `ai_self_review`, `not_pending`, `review_not_enabled`, `not_found`). Audited as \
        `caseReview.bulk`.",
    request_body = ReviewBulk,
    params(("key" = String, Path)),
    responses((status = 200, body = ReviewBulkResult), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`limit_reached`"),
        (status = 429, body = ErrorBody)))]
pub async fn bulk_review(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<ReviewBulk>,
) -> ApiResult<Json<ReviewBulkResult>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_review).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    let r = s
        .store
        .bulk_review(&ctx(&s, &p, &auth), &body)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseReview.bulk",
        Some(&p.key),
        serde_json::json!({"count": body.items.len(), "decision": body.decision, "applied": r.applied,
            "skipped": r.skipped.len()}),
    )
    .await;
    Ok(Json(r))
}
