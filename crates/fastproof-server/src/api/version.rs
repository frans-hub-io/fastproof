//! The version of the server, for the UI footer and support requests.

use axum::Json;
use franshub_auth::Auth;
use serde::Serialize;
use utoipa::ToSchema;

use super::ErrorBody;

/// The running build.
#[derive(Serialize, ToSchema)]
pub struct VersionInfo {
    /// The release tag without the `v` (such as `0.2.0`), or the Cargo version of a local build.
    pub version: String,
}

#[utoipa::path(get, path = "/api/v1/version", tag = "admin", operation_id = "server_version",
    description = "The version of the server. Any signed-in principal may read it.",
    responses((status = 200, body = VersionInfo), (status = 401, body = ErrorBody)))]
pub async fn get(_auth: Auth) -> Json<VersionInfo> {
    Json(VersionInfo {
        version: crate::VERSION.to_string(),
    })
}
