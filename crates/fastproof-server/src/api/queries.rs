//! FPQL (§8): the schema for completion, validation with a capped count, and saved queries
//! (personal ones visible only to their owner, shared ones changed with `cases:write`).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::tm::query::QueryCtx;
use fastproof_store::tm::{
    QueryEntity, QuerySchema, QueryValidate, QueryValidation, SavedQuery, SavedQueryList,
    SavedQueryPatch, SavedQueryWrite,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

/// The routes of this area, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(query_schema))
        .routes(routes!(validate_query))
        .routes(routes!(list_saved_queries, create_saved_query))
        .routes(routes!(update_saved_query, delete_saved_query))
}

/// `?entity=` of the query routes.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EntityQuery {
    /// `cases` (default) or `requirements`.
    pub entity: Option<String>,
}

fn entity_of(q: &EntityQuery) -> ApiResult<QueryEntity> {
    match q.entity.as_deref() {
        None | Some("") => Ok(QueryEntity::Cases),
        Some(e) => QueryEntity::parse(e)
            .ok_or_else(|| ApiError::bad_request("entity must be cases or requirements")),
    }
}

/// The caller of a query: the principal name for `me()` and the owner key of personal saved
/// queries (`caller_key`: the managed token id, or the kind and name), so that a token named
/// like a user never reaches that user's personal queries.
pub fn caller(auth: &Auth) -> QueryCtx {
    QueryCtx::caller(&auth.principal.name, &crate::limits::caller_key(auth))
}

fn can_write(s: &AppState, auth: &Auth, key: &str) -> bool {
    auth.principal.allowed(&s.perms.cases_write, Some(key))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/query/schema", tag = "queries", operation_id = "query_schema",
    description = "What FPQL offers for an entity: attributes with their operators and values, functions, the project's \
        fields and the keywords (for completion). `entity=requirements` answers 400 until requirement queries exist.",
    params(("key" = String, Path), EntityQuery),
    responses((status = 200, body = QuerySchema), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn query_schema(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<EntityQuery>,
) -> ApiResult<Json<QuerySchema>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let entity = entity_of(&q)?;
    Ok(Json(
        s.store.query_schema(p.id, entity).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/query/validate", tag = "queries", operation_id = "validate_query",
    description = "Checks a query: 200 with `ok: true`, the normalized text and the number of matching cases (not archived \
        unless the query names `archived`; at most 10 000, then `countCapped`), or 200 with `ok: false` and the `error` \
        (position and length in UTF-16 code units). The count runs with a 5 s statement timeout.",
    request_body = QueryValidate,
    params(("key" = String, Path)),
    responses((status = 200, body = QueryValidation), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn validate_query(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<QueryValidate>,
) -> ApiResult<Json<QueryValidation>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let _permit = s.guards.heavy(&auth).await?;
    Ok(Json(
        s.store
            .validate_query(p.id, body.entity, &body.query, &caller(&auth))
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/saved-queries", tag = "queries", operation_id = "list_saved_queries",
    description = "The caller's saved queries and the shared ones of an entity, by position.",
    params(("key" = String, Path), EntityQuery),
    responses((status = 200, body = SavedQueryList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_saved_queries(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<EntityQuery>,
) -> ApiResult<Json<SavedQueryList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let entity = entity_of(&q)?;
    Ok(Json(
        s.store
            .list_saved_queries(p.id, entity, &caller(&auth))
            .await
            .map_err(tm_error)?,
    ))
}

fn audit_detail(q: &SavedQuery) -> serde_json::Value {
    let mut d =
        serde_json::json!({"id": q.id, "entity": q.entity, "name": q.name, "shared": q.shared});
    if q.shared {
        d["query"] = serde_json::Value::String(q.query.clone());
    }
    d
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/saved-queries", tag = "queries", operation_id = "create_saved_query",
    description = "Saves a query of the caller (checked like a query that runs). `shared: true` needs `cases:write`. At most \
        100 per owner and entity and 200 shared ones per project and entity (409 `limit_reached`); a name the caller \
        already uses is 409 `saved_query_name_taken`.",
    request_body = SavedQueryWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = SavedQuery), (status = 400, body = ErrorBody, description = "`invalid_query`"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_saved_query(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<SavedQueryWrite>,
) -> ApiResult<(StatusCode, Json<SavedQuery>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    if body.shared == Some(true) {
        s.project_for(&auth, &key, &s.perms.cases_write).await?;
    }
    crate::tm_rate::admit(&s, &auth)?;
    let q = s
        .store
        .create_saved_query(
            p.id,
            &caller(&auth),
            can_write(&s, &auth, &p.key),
            &body,
            &s.cfg.tm,
        )
        .await
        .map_err(tm_error)?;
    s.audit(&auth, "savedQuery.create", Some(&p.key), audit_detail(&q))
        .await;
    Ok((StatusCode::CREATED, Json(q)))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/saved-queries/{id}", tag = "queries", operation_id = "update_saved_query",
    description = "Changes a saved query: the caller's own, or a shared one with `cases:write` (sharing and unsharing: the \
        owner with `cases:write`). Another user's personal query is 404.",
    request_body = SavedQueryPatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = SavedQuery), (status = 400, body = ErrorBody, description = "`invalid_query`"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn update_saved_query(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<SavedQueryPatch>,
) -> ApiResult<Json<SavedQuery>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let (before, after) = s
        .store
        .update_saved_query(
            p.id,
            id,
            &caller(&auth),
            can_write(&s, &auth, &p.key),
            &body,
            &s.cfg.tm,
        )
        .await
        .map_err(tm_error)?;
    let mut d = audit_detail(&after);
    d["before"] = audit_detail(&before);
    s.audit(&auth, "savedQuery.update", Some(&p.key), d).await;
    Ok(Json(after))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/saved-queries/{id}", tag = "queries", operation_id = "delete_saved_query",
    description = "Deletes a saved query: the caller's own, or a shared one with `cases:write`. Another user's personal \
        query is 404.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete_saved_query(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let q = s
        .store
        .delete_saved_query(p.id, id, &caller(&auth), can_write(&s, &auth, &p.key))
        .await
        .map_err(tm_error)?;
    s.audit(&auth, "savedQuery.delete", Some(&p.key), audit_detail(&q))
        .await;
    Ok(StatusCode::NO_CONTENT)
}
