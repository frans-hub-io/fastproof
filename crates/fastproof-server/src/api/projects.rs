//! `/api/v1/projects`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::Project;
use fastproof_store::projects::{CreateProject, ProjectPatch};
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{ErrorBody, P};
use crate::state::AppState;

/// Projects the caller can read.
#[derive(Serialize, ToSchema)]
pub struct ProjectList {
    pub items: Vec<Project>,
}

/// Body of `POST /api/v1/projects`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewProject {
    /// `^[a-z0-9][a-z0-9-]{1,62}$`
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Body of `PATCH /api/v1/projects/{key}`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchProject {
    pub name: Option<String>,
    pub description: Option<String>,
}

const MAX_NAME: usize = 200;
const MAX_DESCRIPTION: usize = 4000;

fn check_name(n: &str) -> ApiResult<()> {
    if n.trim().is_empty() || n.len() > MAX_NAME {
        return Err(ApiError::bad_request(format!(
            "name must have 1 to {MAX_NAME} bytes"
        )));
    }
    Ok(())
}

fn check_description(d: &str) -> ApiResult<()> {
    if d.len() > MAX_DESCRIPTION {
        return Err(ApiError::bad_request(format!(
            "description must have at most {MAX_DESCRIPTION} bytes"
        )));
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/projects", tag = "projects", operation_id = "projects_list",
    responses((status = 200, body = ProjectList), (status = 401, body = ErrorBody)))]
pub async fn list(State(s): State<AppState>, auth: Auth) -> ApiResult<Json<ProjectList>> {
    let items = s
        .store
        .projects()
        .await?
        .into_iter()
        .filter(|p| auth.allowed(&s.perms.projects_read, Some(&p.key)))
        .collect();
    Ok(Json(ProjectList { items }))
}

#[utoipa::path(post, path = "/api/v1/projects", tag = "projects", operation_id = "projects_create", request_body = NewProject,
    responses((status = 201, body = Project), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    ApiJson(body): ApiJson<NewProject>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    require(&auth, &s.perms.projects_write, None)?;
    if !fastproof_blob::valid_project_key(&body.key) {
        return Err(ApiError::bad_request(
            "key must match ^[a-z0-9][a-z0-9-]{1,62}$",
        ));
    }
    check_name(&body.name)?;
    let description = body.description.unwrap_or_default();
    check_description(&description)?;
    match s
        .store
        .create_project(&body.key, body.name.trim(), &description)
        .await?
    {
        CreateProject::Created(p) => {
            s.audit(
                &auth,
                "project.create",
                Some(&p.key),
                serde_json::json!({"name": p.name}),
            )
            .await;
            Ok((StatusCode::CREATED, Json(*p)))
        }
        CreateProject::Exists => Err(ApiError::conflict(
            "a project with this key exists or is being deleted",
        )),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}", tag = "projects", operation_id = "projects_get",
    params(("key" = String, Path)),
    responses((status = 200, body = Project), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<Project>> {
    Ok(Json(
        s.project_for(&auth, &key, &s.perms.projects_read).await?,
    ))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}", tag = "projects", operation_id = "projects_patch", request_body = PatchProject,
    params(("key" = String, Path)),
    responses((status = 200, body = Project), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn patch(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<PatchProject>,
) -> ApiResult<Json<Project>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if let Some(n) = &body.name {
        check_name(n)?;
    }
    if let Some(d) = &body.description {
        check_description(d)?;
    }
    let patch = ProjectPatch {
        name: body.name.map(|n| n.trim().to_string()),
        description: body.description,
    };
    let updated = s
        .store
        .update_project(&p.key, &patch)
        .await?
        .ok_or_else(|| ApiError::not_found("project"))?;
    s.audit(
        &auth,
        "project.update",
        Some(&p.key),
        serde_json::json!({"name": patch.name, "description": patch.description.is_some()}),
    )
    .await;
    Ok(Json(updated))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}", tag = "projects", operation_id = "projects_delete",
    params(("key" = String, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<StatusCode> {
    require(&auth, &s.perms.projects_write, None)?;
    if !fastproof_blob::valid_project_key(&key) || !s.store.delete_project(&key).await? {
        return Err(ApiError::not_found("project"));
    }
    s.audit(&auth, "project.delete", Some(&key), serde_json::json!({}))
        .await;
    Ok(StatusCode::NO_CONTENT)
}
