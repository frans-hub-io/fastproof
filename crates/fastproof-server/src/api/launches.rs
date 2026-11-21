//! `/api/v1/projects/{key}/launches`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::launches::{LaunchFilter, LaunchStart};
use fastproof_store::{Launch, Page};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{ErrorBody, P, Q, key_value, page_limit};
use crate::state::AppState;

/// Query of the launch list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LaunchQuery {
    pub branch: Option<String>,
    pub environment: Option<String>,
    /// `running`, `finished` or `interrupted`.
    pub status: Option<String>,
    /// `key:value` attribute.
    pub attr: Option<String>,
    /// Substring of the name.
    pub q: Option<String>,
    /// `false` (default): regular launches; `true`: debug launches only.
    pub debug: Option<bool>,
    /// Cursor from `next`.
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// Body of `POST .../launches/{id}/finish`.
#[derive(Deserialize, ToSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct FinishLaunch {
    /// `finished` (default) or `interrupted`.
    pub status: Option<String>,
}

fn bounded(what: &str, v: Option<&str>, max: usize) -> Result<(), ApiError> {
    if let Some(v) = v
        && v.len() > max
    {
        return Err(ApiError::bad_request(format!(
            "{what} must have at most {max} bytes"
        )));
    }
    Ok(())
}

/// Checks the sizes of a launch start request.
pub fn validate_start(b: &LaunchStart) -> ApiResult<()> {
    if b.name.trim().is_empty() || b.name.len() > 300 {
        return Err(ApiError::bad_request("name must have 1 to 300 bytes"));
    }
    if let Some(e) = &b.external_id
        && (e.trim().is_empty() || e.len() > 256)
    {
        return Err(ApiError::bad_request("externalId must have 1 to 256 bytes"));
    }
    if let Some(a) = &b.attributes {
        if a.len() > 100 {
            return Err(ApiError::bad_request("at most 100 attributes"));
        }
        for kv in a {
            if kv.key.is_empty() || kv.key.len() > 256 || kv.value.len() > 1024 {
                return Err(ApiError::bad_request(
                    "attribute keys have 1 to 256 bytes, values at most 1024",
                ));
            }
        }
    }
    if let Some(c) = &b.ci {
        for (n, v) in [
            ("ci.provider", &c.provider),
            ("ci.pipelineId", &c.pipeline_id),
            ("ci.jobId", &c.job_id),
            ("ci.url", &c.url),
        ] {
            bounded(n, v.as_deref(), 2048)?;
        }
    }
    if let Some(v) = &b.vcs {
        for (n, x) in [
            ("vcs.repository", &v.repository),
            ("vcs.branch", &v.branch),
            ("vcs.commit", &v.commit),
            ("vcs.commitUrl", &v.commit_url),
        ] {
            bounded(n, x.as_deref(), 2048)?;
        }
        if let Some(u) = &v.commit_url
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            return Err(ApiError::bad_request(
                "vcs.commitUrl must be an http(s) URL",
            ));
        }
    }
    if let Some(e) = &b.environment {
        bounded("environment.key", e.key.as_deref(), 256)?;
        if e.values.len() > 200 {
            return Err(ApiError::bad_request("at most 200 environment values"));
        }
        for (k, v) in &e.values {
            if k.len() > 256 || v.len() > 2048 {
                return Err(ApiError::bad_request(
                    "environment keys have at most 256 bytes, values 2048",
                ));
            }
        }
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches", tag = "launches", request_body = LaunchStart,
    params(("key" = String, Path)),
    responses((status = 201, description = "created", body = Launch),
        (status = 200, description = "a launch with this externalId exists", body = Launch),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<LaunchStart>,
) -> ApiResult<(StatusCode, Json<Launch>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    validate_start(&body)?;
    let (launch, created) = s
        .store
        .start_launch(&project, &body, &auth.principal.name)
        .await?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(launch),
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches", tag = "launches",
    params(("key" = String, Path), LaunchQuery),
    responses((status = 200, body = Page<Launch>), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<LaunchQuery>,
) -> ApiResult<Json<Page<Launch>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let before = match q.before.as_deref() {
        None => None,
        Some(b) => Some(
            b.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid cursor"))?,
        ),
    };
    let f = LaunchFilter {
        branch: q.branch,
        environment: q.environment,
        status: q.status,
        attr: key_value(q.attr.as_deref(), "attr")?,
        q: q.q.filter(|v| !v.is_empty()),
        debug: q.debug.unwrap_or(false),
        before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(s.store.launches(project.id, &f).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}", tag = "launches",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Launch), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<Launch>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(s.launch_of(&project, id).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/finish", tag = "launches",
    request_body(content = FinishLaunch, description = "optional"),
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Launch), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn finish(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Launch>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let body: FinishLaunch = if body.iter().all(|b| b.is_ascii_whitespace()) {
        FinishLaunch::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?
    };
    let status = body.status.unwrap_or_else(|| "finished".to_string());
    if !matches!(status.as_str(), "finished" | "interrupted") {
        return Err(ApiError::bad_request(
            "status must be finished or interrupted",
        ));
    }
    let launch = s
        .store
        .finish_launch(project.id, &project.key, id, &status)
        .await?
        .ok_or_else(|| ApiError::not_found("launch"))?;
    s.audit(
        &auth,
        "launch.finish",
        Some(&project.key),
        serde_json::json!({"launchId": id, "number": launch.number, "status": status}),
    )
    .await;
    Ok(Json(launch))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/launches/{id}", tag = "launches",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let launch = s.launch_of(&project, id).await?;
    if !s.store.delete_launch(project.id, &project.key, id).await? {
        return Err(ApiError::not_found("launch"));
    }
    s.audit(
        &auth,
        "launch.delete",
        Some(&project.key),
        serde_json::json!({"launchId": id, "number": launch.number, "name": launch.name}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
