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
pub mod teams;
pub mod vcs;

use fastproof_blob::Secret;
use uuid::Uuid;

use self::secrets::{SecretPurpose, origin_of};
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

/// Checks that a secret bound to `bound_purpose` and `bound_origin` may be used for `purpose`
/// with a request to `destination` (a URL; `None` when the destination is the secret itself).
pub fn check_binding(
    name: &str,
    bound_purpose: Option<&str>,
    bound_origin: Option<&str>,
    purpose: SecretPurpose,
    destination: Option<&str>,
) -> Result<(), SecretError> {
    match bound_purpose {
        None => {
            return Err(SecretError(format!(
                "the secret {name:?} has no purpose; write it again with purpose {}",
                purpose.as_str()
            )));
        }
        Some(p) if p != purpose.as_str() => {
            return Err(SecretError(format!(
                "the secret {name:?} is for {p}, not {}",
                purpose.as_str()
            )));
        }
        Some(_) => {}
    }
    match (bound_origin, destination) {
        (None, _) if purpose.needs_origin() => Err(SecretError(format!(
            "the secret {name:?} is not bound to an origin; write it again with one"
        ))),
        (Some(o), Some(d)) if origin_of(d).as_deref() != Some(o) => Err(SecretError(format!(
            "the secret {name:?} may only be sent to {o}; write it again to use it with another URL"
        ))),
        _ => Ok(()),
    }
}

impl AppState {
    /// The value of a project secret, for `purpose` and a request to `destination` (see
    /// [`check_binding`]).
    pub async fn project_secret(
        &self,
        project_id: Uuid,
        name: &str,
        purpose: SecretPurpose,
        destination: Option<&str>,
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
        check_binding(
            name,
            sealed.purpose.as_deref(),
            sealed.origin.as_deref(),
            purpose,
            destination,
        )?;
        let v = keys
            .open(project_id, name, &sealed)
            .map_err(|e| SecretError(e.to_string()))?;
        if destination.is_none()
            && let Some(o) = sealed.origin.as_deref()
            && origin_of(v.expose()).as_deref() != Some(o)
        {
            return Err(SecretError(format!(
                "the URL in the secret {name:?} is not on its origin {o}"
            )));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings() {
        let gl = Some("https://gitlab.example.com");
        assert!(
            check_binding(
                "t",
                Some("vcs"),
                gl,
                SecretPurpose::Vcs,
                Some("https://gitlab.example.com/api/v4/x")
            )
            .is_ok()
        );
        let moved = check_binding(
            "t",
            Some("vcs"),
            gl,
            SecretPurpose::Vcs,
            Some("https://evil.example.net/api/v4/x"),
        )
        .unwrap_err();
        assert!(moved.0.contains("may only be sent to"), "{moved}");
        assert!(
            check_binding(
                "t",
                Some("vcs"),
                gl,
                SecretPurpose::Fastmock,
                Some("https://gitlab.example.com")
            )
            .is_err()
        );
        assert!(check_binding("t", None, None, SecretPurpose::Webhook, None).is_err());
        assert!(
            check_binding(
                "t",
                Some("vcs"),
                None,
                SecretPurpose::Vcs,
                Some("https://gitlab.example.com")
            )
            .is_err()
        );
        assert!(
            check_binding(
                "k",
                Some("webhook"),
                None,
                SecretPurpose::Webhook,
                Some("https://anywhere.example.com/hook")
            )
            .is_ok()
        );
    }
}
