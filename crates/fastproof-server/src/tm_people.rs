//! Remembers the people who use test management so the assignee picker (`GET /people`) can
//! list members of bound OIDC groups and show display names. Test-management pages call
//! [`note`]: for an OIDC user (session or bearer JWT) it records, at most every
//! [`NOTE_EVERY`] per user and replica and in the background, the groups the access policy
//! binds (only those) and the display name (the `name` claim of the session's ID token or of
//! the bearer JWT, which the platform already validated). A user in no bound group is
//! forgotten. Tokens are not recorded; they are listed from the token store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use base64::Engine as _;
use franshub_access::PrincipalKind;
use franshub_auth::{Auth, AuthMethod};
use tokio::sync::Semaphore;

use crate::state::AppState;

/// How often one user is recorded per replica.
pub const NOTE_EVERY: Duration = Duration::from_secs(600);
/// Users remembered by the throttle per replica.
const MAX_NOTED: usize = 10_000;
/// Recordings running at once per replica; more are skipped (and retried on a later request).
const MAX_RUNNING: usize = 4;

/// The throttle of one replica.
#[derive(Debug)]
pub struct Noted {
    at: Mutex<HashMap<String, Instant>>,
    running: Arc<Semaphore>,
}

impl Default for Noted {
    fn default() -> Self {
        Self {
            at: Mutex::default(),
            running: Arc::new(Semaphore::new(MAX_RUNNING)),
        }
    }
}

impl Noted {
    /// Whether `name` is due at `now` (and marks it noted).
    fn due(&self, name: &str, now: Instant) -> bool {
        let mut m = self.at.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = m.get(name)
            && now.duration_since(*t) < NOTE_EVERY
        {
            return false;
        }
        if m.len() >= MAX_NOTED {
            m.retain(|_, t| now.duration_since(*t) < NOTE_EVERY);
            if m.len() >= MAX_NOTED {
                m.clear();
            }
        }
        m.insert(name.to_string(), now);
        true
    }

    fn forget(&self, name: &str) {
        self.at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
    }
}

/// The `name` claim of a JWT's payload (not verified here: only for tokens the platform
/// validated or stored), trimmed, without control characters, at most 200 bytes.
pub fn display_of_jwt(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    if payload.len() > 64 * 1024 {
        return None;
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let name = v.get("name")?.as_str()?.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    let max = fastproof_store::tm::people::MAX_DISPLAY_BYTES;
    let mut end = name.len().min(max);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    Some(name[..end].to_string())
}

/// The groups of `groups` that a binding of the access policy names (only these are stored).
pub fn bound_groups(policy: &franshub_access::AccessPolicy, groups: &[String]) -> Vec<String> {
    let bound: std::collections::HashSet<&str> = policy
        .bindings()
        .iter()
        .filter_map(|b| match &b.subject {
            franshub_access::Subject::Group(g) => Some(g.as_str()),
            _ => None,
        })
        .collect();
    groups
        .iter()
        .filter(|g| bound.contains(g.as_str()))
        .cloned()
        .collect()
}

/// Records the caller when it is an OIDC user that is due; returns at once.
pub fn note(s: &AppState, auth: &Auth, headers: &HeaderMap) {
    if auth.principal.kind != PrincipalKind::User {
        return;
    }
    let name = auth.principal.name.clone();
    let noted = &s.guards.tm_people;
    if !noted.due(&name, Instant::now()) {
        return;
    }
    let Ok(permit) = noted.running.clone().try_acquire_owned() else {
        noted.forget(&name);
        return;
    };
    let bearer = match auth.method {
        AuthMethod::Bearer => headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::to_string),
        _ => None,
    };
    let session = match (auth.method, s.platform.oidc()) {
        (AuthMethod::Session, Some(rt)) => rt.oidc.session_id(headers).map(str::to_string),
        _ => None,
    };
    let groups = bound_groups(s.platform.policy(), &auth.principal.groups);
    let leave = groups.is_empty();
    let s = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        if leave {
            if let Err(e) = s.store.forget_person(&name).await {
                tracing::warn!(error = %e, "forgetting a person failed");
            }
            return;
        }
        let mut display = bearer.as_deref().and_then(display_of_jwt);
        if let (Some(id), Some(rt)) = (session, s.platform.oidc()) {
            match rt.sessions().get(&id).await {
                Ok(Some(r)) => display = r.id_token.as_deref().and_then(display_of_jwt),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "reading the session for the people list failed")
                }
            }
        }
        if let Err(e) = s
            .store
            .note_person(&name, display.as_deref(), &groups)
            .await
        {
            tracing::warn!(error = %e, "recording a person failed");
            s.guards.tm_people.forget(&name);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: serde_json::Value) -> String {
        let e = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        format!(
            "{}.{}.sig",
            e(br#"{"alg":"RS256"}"#),
            e(claims.to_string().as_bytes())
        )
    }

    #[test]
    fn display_comes_from_the_name_claim() {
        let t = jwt(serde_json::json!({"sub": "1", "name": " Ann Example "}));
        assert_eq!(display_of_jwt(&t).as_deref(), Some("Ann Example"));
        assert_eq!(display_of_jwt(&jwt(serde_json::json!({"sub": "1"}))), None);
        assert_eq!(
            display_of_jwt(&jwt(serde_json::json!({"name": "a\u{7}b"}))),
            None
        );
        assert_eq!(display_of_jwt("not-a-jwt"), None);
        let long = "é".repeat(150);
        let d = display_of_jwt(&jwt(serde_json::json!({ "name": long }))).unwrap();
        assert!(d.len() <= 200 && d.chars().all(|c| c == 'é'), "{d}");
    }

    #[test]
    fn only_bound_groups_are_kept() {
        let catalog = std::sync::Arc::new(crate::state::catalog().unwrap());
        let policy = franshub_access::AccessPolicy::parse(
            catalog,
            "bindings:\n  - subject: group:qa\n    role: tester\n    scopes: [web]\n  - subject: user:ann\n    role: viewer\n    scopes: [web]\n",
        )
        .unwrap();
        let groups = vec![
            "qa".to_string(),
            "hr-payroll".to_string(),
            "dev".to_string(),
        ];
        assert_eq!(bound_groups(&policy, &groups), ["qa"]);
        assert!(bound_groups(&policy, &["dev".to_string()]).is_empty());
    }

    #[test]
    fn a_user_is_noted_once_per_period() {
        let n = Noted::default();
        let t = Instant::now();
        assert!(n.due("ann", t));
        assert!(!n.due("ann", t + Duration::from_secs(5)));
        assert!(n.due("bob", t));
        assert!(n.due("ann", t + NOTE_EVERY));
    }
}
