//! Case folders: one level of the tree at a time with subtree counts, create, rename, move,
//! and delete with `mode=moveUp|archiveCases`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::tm::{
    CaseFolder, CaseFolderList, CaseFolderPatch, CaseFolderWrite, FolderDeleteMode,
};
use franshub_auth::Auth;
use franshub_http::{ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_case_folders, create_case_folder))
        .routes(routes!(update_case_folder, delete_case_folder))
}

/// Query of `GET /case-folders`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FolderLevelQuery {
    /// The folder whose children are listed; absent lists the top level.
    pub parent: Option<Uuid>,
}

/// Query of `DELETE /case-folders/{id}`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FolderDeleteQuery {
    /// Required when the folder has subfolders or cases.
    pub mode: Option<FolderDeleteMode>,
}

/// What deleting a folder did.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaseFolderDeleted {
    pub folders: i64,
    pub cases_moved: i64,
    pub cases_archived: i64,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/case-folders", tag = "cases", operation_id = "list_case_folders",
    description = "One level of the folder tree (`parent` absent = top level), by position and name, with the counts of \
        the cases (not archived) in each folder and its subfolders.",
    params(("key" = String, Path), FolderLevelQuery),
    responses((status = 200, body = CaseFolderList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_case_folders(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<FolderLevelQuery>,
) -> ApiResult<Json<CaseFolderList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let items = s
        .store
        .case_folder_level(p.id, q.parent)
        .await
        .map_err(tm_error)?;
    Ok(Json(CaseFolderList { items }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/case-folders", tag = "cases", operation_id = "create_case_folder",
    request_body = CaseFolderWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = CaseFolder), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`folder_name_taken` or `limit_reached`")))]
pub async fn create_case_folder(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseFolderWrite>,
) -> ApiResult<(StatusCode, Json<CaseFolder>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let f = s
        .store
        .create_case_folder(p.id, &body, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseFolder.create",
        Some(&p.key),
        serde_json::json!({"id": f.id, "name": f.name, "parentId": f.parent_id}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(f)))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/case-folders/{id}", tag = "cases", operation_id = "update_case_folder",
    description = "Renames, reorders or moves a folder with its subtree (`parentId: null` moves it to the top level).",
    request_body = CaseFolderPatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = CaseFolder), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`folder_name_taken` or `limit_reached`")))]
pub async fn update_case_folder(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<CaseFolderPatch>,
) -> ApiResult<Json<CaseFolder>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let f = s
        .store
        .update_case_folder(p.id, id, &body, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseFolder.update",
        Some(&p.key),
        serde_json::json!({"id": f.id, "name": f.name, "parentId": f.parent_id,
            "moved": body.parent_id.is_some(), "position": f.position}),
    )
    .await;
    Ok(Json(f))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/case-folders/{id}", tag = "cases", operation_id = "delete_case_folder",
    description = "Deletes a folder. One with subfolders or cases needs `mode`: `moveUp` moves them to the parent, \
        `archiveCases` archives the cases of the whole subtree and deletes the subfolders.",
    params(("key" = String, Path), ("id" = Uuid, Path), FolderDeleteQuery),
    responses((status = 200, body = CaseFolderDeleted), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`folder_not_empty` or `folder_name_taken`")))]
pub async fn delete_case_folder(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<FolderDeleteQuery>,
) -> ApiResult<Json<CaseFolderDeleted>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_write).await?;
    let d = s
        .store
        .delete_case_folder(p.id, id, q.mode, &auth.principal.name)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "caseFolder.delete",
        Some(&p.key),
        serde_json::json!({"id": id, "mode": q.mode, "folders": d.folders,
            "casesMoved": d.cases_moved, "casesArchived": d.cases_archived}),
    )
    .await;
    Ok(Json(CaseFolderDeleted {
        folders: d.folders,
        cases_moved: d.cases_moved,
        cases_archived: d.cases_archived,
    }))
}
