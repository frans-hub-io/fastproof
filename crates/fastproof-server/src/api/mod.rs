//! The REST API (`/api/v1`), documented with utoipa.

pub mod admin;
pub mod analysis;
pub mod attachments;
pub mod events;
pub mod integrations;
pub mod launches;
pub mod logs;
pub mod projects;
pub mod results;
pub mod search;
pub mod uploads;

use axum::extract::{FromRequestParts, Path, Query};
use axum::http::request::Parts;
use fastproof_store::{Launch, Project};
use franshub_access::Permission;
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiResult};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use uuid::Uuid;

use crate::state::AppState;

/// Default and largest page size of lists.
pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 500;

/// The uniform error body (`franshub_http::ErrorBody`), for the documentation.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let c = openapi.components.get_or_insert_with(Default::default);
        c.add_security_scheme(
            "bearer",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

/// Types the integration parts of `ProjectSettings` (stored as free JSON by the store and
/// validated by [`crate::integrations::settings`]) in the document.
pub fn type_project_settings(doc: &mut utoipa::openapi::OpenApi) {
    let Some(c) = doc.components.as_mut() else {
        return;
    };
    let Some(schema) = c.schemas.get("ProjectSettings") else {
        return;
    };
    let Ok(mut v) = serde_json::to_value(schema) else {
        return;
    };
    let reference =
        |name: &str| serde_json::json!({"$ref": format!("#/components/schemas/{name}")});
    let nullable = |name: &str| serde_json::json!({"oneOf": [reference(name), {"type": "null"}]});
    if let Some(props) = v.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (key, name) in [
            ("gates", "Gate"),
            ("notifications", "Notification"),
            ("issueTrackers", "IssueTracker"),
        ] {
            if let Some(p) = props.get_mut(key) {
                p["items"] = reference(name);
            }
        }
        props.insert("fastmock".into(), nullable("FastMockSettings"));
        props.insert("vcs".into(), nullable("VcsSettings"));
    }
    if let Ok(schema) = serde_json::from_value(v) {
        c.schemas.insert("ProjectSettings".into(), schema);
    }
}

#[derive(OpenApi)]
#[openapi(
    info(title = "FastProof API", description = "Test reporting server: projects, launches, uploads, results and attachments.", license(name = "Apache-2.0")),
    modifiers(&BearerAuth),
    security(("bearer" = [])),
    components(schemas(ErrorBody, fastproof_model::ReportResult, logs::LogStreamBatch, logs::LogGap,
        crate::integrations::settings::Gate, crate::integrations::settings::Notification,
        crate::integrations::settings::NotificationFilter, crate::integrations::settings::VcsSettings,
        crate::integrations::settings::IssueTracker, crate::integrations::settings::FastMockSettings)),
    tags(
        (name = "projects"), (name = "launches"), (name = "uploads"), (name = "results"),
        (name = "attachments"), (name = "events"), (name = "admin"), (name = "integrations"),
        (name = "analysis"), (name = "logs")
    )
)]
pub struct ApiDoc;

/// Query parameters whose rejection answers the uniform 400.
pub struct Q<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for Q<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(Q(v)),
            Err(e) => Err(ApiError::bad_request(format!(
                "invalid query: {}",
                e.body_text()
            ))),
        }
    }
}

/// Path parameters; a malformed id is a 404 like an unknown one.
pub struct P<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for P<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(v)) => Ok(P(v)),
            Err(_) => Err(ApiError::not_found("resource")),
        }
    }
}

/// Page size from `limit`.
pub fn page_limit(limit: Option<i64>) -> ApiResult<i64> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(ApiError::bad_request(format!(
            "limit must be 1..={MAX_LIMIT}"
        ))),
    }
}

/// `k:v` filters.
pub fn key_value(s: Option<&str>, what: &str) -> ApiResult<Option<(String, String)>> {
    match s {
        None => Ok(None),
        Some(s) => match s.split_once(':') {
            Some((k, v)) if !k.is_empty() => Ok(Some((k.to_string(), v.to_string()))),
            _ => Err(ApiError::bad_request(format!("{what} must be key:value"))),
        },
    }
}

impl AppState {
    /// The project, after checking `perm` in its scope. The permission is checked first, so a
    /// caller without access gets the same 403 whether the project exists or not.
    pub async fn project_for(
        &self,
        auth: &Auth,
        key: &str,
        perm: &Permission,
    ) -> ApiResult<Project> {
        require(auth, perm, Some(key))?;
        if !fastproof_blob::valid_project_key(key) {
            return Err(ApiError::not_found("project"));
        }
        self.store
            .project(key)
            .await?
            .ok_or_else(|| ApiError::not_found("project"))
    }

    pub async fn launch_of(&self, project: &Project, id: Uuid) -> ApiResult<Launch> {
        self.store
            .launch(project.id, id)
            .await?
            .ok_or_else(|| ApiError::not_found("launch"))
    }

    pub async fn audit(
        &self,
        auth: &Auth,
        action: &str,
        scope: Option<&str>,
        detail: serde_json::Value,
    ) {
        self.platform
            .audit(franshub_access::AuditEvent::new(
                &auth.principal,
                action,
                scope,
                detail,
            ))
            .await;
    }
}
