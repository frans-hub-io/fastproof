//! Case field definitions: built-in and custom fields of a project. Reading needs
//! `cases:read`, changing `settings:write`; the type of a field never changes.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::tm::{CaseFieldDef, CaseFieldList, CaseFieldPatch, CaseFieldWrite};
use franshub_auth::Auth;
use franshub_http::{ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_case_fields, create_case_field))
        .routes(routes!(update_case_field, delete_case_field))
}

/// Query of `GET /case-fields`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FieldListQuery {
    /// Include archived fields.
    pub archived: Option<bool>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/case-fields", tag = "cases", operation_id = "list_case_fields",
    description = "Field definitions by position: the built-in ones (priority, severity, layer, owner, epic, feature, story, \
        suite, component) and the custom ones.",
    params(("key" = String, Path), FieldListQuery),
    responses((status = 200, body = CaseFieldList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_case_fields(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<FieldListQuery>,
) -> ApiResult<Json<CaseFieldList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let items = s
        .store
        .case_fields(p.id, q.archived.unwrap_or(false))
        .await?;
    Ok(Json(CaseFieldList { items }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/case-fields", tag = "cases", operation_id = "create_case_field",
    request_body = CaseFieldWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = CaseFieldDef), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "key or label taken, `limit_reached`")))]
pub async fn create_case_field(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseFieldWrite>,
) -> ApiResult<(StatusCode, Json<CaseFieldDef>)> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let d = s
        .store
        .create_case_field(p.id, &body, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseField.create",
        Some(&p.key),
        serde_json::json!({"id": d.id, "key": d.key, "type": d.field_type,
            "options": d.options.len(), "sourceLabel": d.source_label}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(d)))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/case-fields/{id}", tag = "cases", operation_id = "update_case_field",
    description = "Changes a field; every key is optional. `type` may only be sent unchanged; `archived` archives or restores.",
    request_body = CaseFieldPatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = CaseFieldDef), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn update_case_field(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<CaseFieldPatch>,
) -> ApiResult<Json<CaseFieldDef>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let d = s
        .store
        .update_case_field(p.id, id, &body, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    let changed: Vec<&str> = [
        ("key", body.key.is_some()),
        ("name", body.name.is_some()),
        ("options", body.options.is_some()),
        ("required", body.required.is_some()),
        ("defaultValue", body.default_value.is_some()),
        ("sourceLabel", body.source_label.is_some()),
        ("position", body.position.is_some()),
        ("archived", body.archived.is_some()),
    ]
    .into_iter()
    .filter_map(|(k, set)| set.then_some(k))
    .collect();
    s.audit(
        &auth,
        "caseField.update",
        Some(&p.key),
        serde_json::json!({"id": d.id, "key": d.key, "changed": changed, "archived": d.archived}),
    )
    .await;
    Ok(Json(d))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/case-fields/{id}", tag = "cases", operation_id = "delete_case_field",
    description = "Archives a field: values on cases are kept and it can be restored with `PATCH {archived: false}`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete_case_field(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let d = s
        .store
        .delete_case_field(p.id, id)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseField.delete",
        Some(&p.key),
        serde_json::json!({"id": d.id, "key": d.key}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
