//! Dashboards: list, create, read, change (with `If-Match`), delete, lock, clone within or
//! across projects, and the data of their widgets. Reading needs `launches:read`; changing
//! needs `dashboards:write`, and a locked dashboard is changed, unlocked or deleted only by its
//! owner or by a holder of `settings:write` (a project manager).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use fastproof_store::Project;
use fastproof_store::analysis::boards::{
    Board, BoardData, BoardError, BoardSummary, BoardWidget, BoardWrite, MAX_WIDGETS, validate,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::plans::etag;
use super::{ErrorBody, P, if_match};
use crate::state::AppState;

/// `{items}` of dashboards.
#[derive(Serialize, ToSchema)]
pub struct BoardList {
    pub items: Vec<BoardSummary>,
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_boards, create_board))
        .routes(routes!(get_board, update_board, delete_board))
        .routes(routes!(lock_board))
        .routes(routes!(clone_board))
        .routes(routes!(board_data))
        .routes(routes!(preview))
}

fn board_error(e: BoardError) -> ApiError {
    match e {
        BoardError::NotFound => ApiError::not_found("dashboard"),
        BoardError::Locked => ApiError::new(
            StatusCode::FORBIDDEN,
            "dashboard_locked",
            "the dashboard is locked; only its owner or a project manager can change it",
        ),
        BoardError::Limit => ApiError::new(
            StatusCode::CONFLICT,
            "dashboard_limit",
            format!(
                "a project has at most {} dashboards",
                fastproof_store::analysis::boards::MAX_BOARDS
            ),
        ),
        BoardError::Stale {
            current_revision,
            changed_by,
        } => ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            "the resource changed since it was read",
        )
        .with_details(serde_json::json!({"currentRevision": current_revision,
            "changedBy": changed_by, "source": "ui"})),
        BoardError::Invalid(m) => ApiError::bad_request(m),
        BoardError::Other(e) => ApiError::from(e),
    }
}

fn with_etag<T: Serialize>(status: StatusCode, revision: i64, body: T) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert(axum::http::header::ETAG, etag(revision));
    r
}

async fn manager(s: &AppState, auth: &Auth, key: &str) -> bool {
    auth.principal.allowed(&s.perms.settings_write, Some(key))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/dashboards", tag = "dashboards", operation_id = "list_dashboards",
    params(("key" = String, Path)),
    responses((status = 200, body = BoardList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list_boards(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<BoardList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(BoardList {
        items: s.store.list_boards(p.id).await?,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/dashboards", tag = "dashboards", operation_id = "create_dashboard",
    request_body = BoardWrite,
    params(("key" = String, Path)),
    responses((status = 201, body = Board), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_board(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<BoardWrite>,
) -> ApiResult<Response> {
    let p = s
        .project_for(&auth, &key, &s.perms.dashboards_write)
        .await?;
    let b = s
        .store
        .create_board(p.id, &auth.principal.name, &body)
        .await
        .map_err(board_error)?;
    s.audit(
        &auth,
        "dashboard.create",
        Some(&p.key),
        serde_json::json!({"id": b.id, "name": b.name, "widgets": b.widgets.len()}),
    )
    .await;
    Ok(with_etag(StatusCode::CREATED, b.revision, b))
}

async fn find(s: &AppState, p: &Project, id: Uuid) -> ApiResult<Board> {
    s.store
        .board(p.id, id)
        .await
        .map_err(board_error)?
        .ok_or_else(|| ApiError::not_found("dashboard"))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/dashboards/{id}", tag = "dashboards", operation_id = "get_dashboard",
    description = "A dashboard with `ETag: \"r<revision>\"`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Board), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_board(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let b = find(&s, &p, id).await?;
    Ok(with_etag(StatusCode::OK, b.revision, b))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/dashboards/{id}", tag = "dashboards", operation_id = "update_dashboard",
    request_body = BoardWrite,
    params(("key" = String, Path), ("id" = Uuid, Path), ("If-Match" = String, Header, description = "ETag of the dashboard")),
    responses((status = 200, body = Board), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn update_board(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<BoardWrite>,
) -> ApiResult<Response> {
    let p = s
        .project_for(&auth, &key, &s.perms.dashboards_write)
        .await?;
    let revision = if_match(&headers)?;
    let mgr = manager(&s, &auth, &key).await;
    let b = s
        .store
        .update_board(p.id, id, revision, &auth.principal.name, mgr, &body)
        .await
        .map_err(board_error)?;
    s.audit(
        &auth,
        "dashboard.update",
        Some(&p.key),
        serde_json::json!({"id": b.id, "revision": b.revision, "widgets": b.widgets.len()}),
    )
    .await;
    Ok(with_etag(StatusCode::OK, b.revision, b))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/dashboards/{id}", tag = "dashboards", operation_id = "delete_dashboard",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete_board(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s
        .project_for(&auth, &key, &s.perms.dashboards_write)
        .await?;
    let mgr = manager(&s, &auth, &key).await;
    s.store
        .delete_board(p.id, id, &auth.principal.name, mgr)
        .await
        .map_err(board_error)?;
    s.audit(
        &auth,
        "dashboard.delete",
        Some(&p.key),
        serde_json::json!({"id": id}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockBody {
    pub locked: bool,
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/dashboards/{id}/lock", tag = "dashboards", operation_id = "lock_dashboard",
    description = "Locks or unlocks a dashboard; only its owner or a holder of `settings:write` may.",
    request_body = LockBody,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Board), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn lock_board(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<LockBody>,
) -> ApiResult<Response> {
    let p = s
        .project_for(&auth, &key, &s.perms.dashboards_write)
        .await?;
    let mgr = manager(&s, &auth, &key).await;
    let b = s
        .store
        .set_board_lock(p.id, id, body.locked, &auth.principal.name, mgr)
        .await
        .map_err(board_error)?;
    s.audit(
        &auth,
        if body.locked {
            "dashboard.lock"
        } else {
            "dashboard.unlock"
        },
        Some(&p.key),
        serde_json::json!({"id": b.id}),
    )
    .await;
    Ok(with_etag(StatusCode::OK, b.revision, b))
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloneBody {
    /// Name of the copy; default is the source name with " (copy)".
    pub name: Option<String>,
    /// Project key to copy into; default is the source project.
    pub target_project: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Cloned {
    /// Key of the project that holds the copy.
    pub project: String,
    #[serde(flatten)]
    pub board: Board,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/dashboards/{id}/clone", tag = "dashboards", operation_id = "clone_dashboard",
    description = "Copies a dashboard (read access to the source, `dashboards:write` in the target). The copy is \
        unlocked and owned by the caller; launch ids of widgets are dropped when the target is another project.",
    request_body = CloneBody,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 201, body = Cloned), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn clone_board(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<CloneBody>,
) -> ApiResult<Response> {
    let src_project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let target_key = body.target_project.clone().unwrap_or_else(|| key.clone());
    let target = s
        .project_for(&auth, &target_key, &s.perms.dashboards_write)
        .await?;
    let src = find(&s, &src_project, id).await?;
    let cross = target.id != src_project.id;
    let mut widgets = src.widgets.clone();
    if cross {
        for w in &mut widgets {
            w.settings.launch_id = None;
        }
    }
    let name = body
        .name
        .unwrap_or_else(|| format!("{} (copy)", src.name).chars().take(120).collect());
    let b = s
        .store
        .create_board(
            target.id,
            &auth.principal.name,
            &BoardWrite {
                name,
                description: src.description.clone(),
                widgets,
            },
        )
        .await
        .map_err(board_error)?;
    s.audit(
        &auth,
        "dashboard.clone",
        Some(&target.key),
        serde_json::json!({"from": src.id, "fromProject": src_project.key, "id": b.id}),
    )
    .await;
    let rev = b.revision;
    Ok(with_etag(
        StatusCode::CREATED,
        rev,
        Cloned {
            project: target.key,
            board: b,
        },
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/dashboards/{id}/data", tag = "dashboards", operation_id = "dashboard_data",
    description = "The data of every widget; a widget that fails answers an error entry.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = BoardData), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn board_data(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<BoardData>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let b = find(&s, &p, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let settings = s.store.project_settings(p.id).await?;
    Ok(Json(s.store.board_data(p.id, &settings, &b.widgets).await))
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewBody {
    pub widgets: Vec<BoardWidget>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/dashboard-preview", tag = "dashboards", operation_id = "dashboard_preview",
    description = "The data of widgets that are not stored yet (the editor's draft). Reading only; same limits as a dashboard.",
    request_body = PreviewBody,
    params(("key" = String, Path)),
    responses((status = 200, body = BoardData), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn preview(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<PreviewBody>,
) -> ApiResult<Json<BoardData>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if body.widgets.len() > MAX_WIDGETS {
        return Err(ApiError::bad_request(format!(
            "a dashboard has at most {MAX_WIDGETS} widgets"
        )));
    }
    validate(&BoardWrite {
        name: "preview".into(),
        description: String::new(),
        widgets: body.widgets.clone(),
    })
    .map_err(ApiError::bad_request)?;
    let _permit = s.guards.heavy(&auth).await?;
    let settings = s.store.project_settings(p.id).await?;
    Ok(Json(
        s.store.board_data(p.id, &settings, &body.widgets).await,
    ))
}
