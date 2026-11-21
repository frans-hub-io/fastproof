//! Integrations: quality gates, merge request comments and commit statuses, notifications,
//! issue links, project secrets, the egress allowlist, quality metrics, FastMock correlation
//! and the analysis operations behind the MCP tools.

pub mod analysis;
pub mod config;
pub mod dispatch;
pub mod egress;
pub mod fastmock;
pub mod gates;
pub mod issues;
pub mod notify;
pub mod quality;
pub mod secrets;
pub mod settings;
pub mod vcs;

use fastproof_blob::Secret;
use uuid::Uuid;

use crate::state::AppState;

/// A secret could not be used.
#[derive(Debug)]
pub struct SecretError(pub String);

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

impl AppState {
    /// The value of a project secret.
    pub async fn project_secret(
        &self,
        project_id: Uuid,
        name: &str,
    ) -> Result<Secret, SecretError> {
        let Some(keys) = &self.cfg.integrations.secrets else {
            return Err(SecretError(
                "secrets need FASTPROOF_SECRETS_KEY_FILE".into(),
            ));
        };
        let sealed = self
            .store
            .sealed_secret(project_id, name)
            .await
            .map_err(|_| SecretError("reading the secret failed".into()))?
            .ok_or_else(|| SecretError(format!("the secret {name:?} does not exist")))?;
        keys.open(project_id, name, &sealed)
            .map_err(|e| SecretError(e.to_string()))
    }
}
