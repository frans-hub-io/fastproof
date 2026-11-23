//! Signed media URLs: a short-lived URL that serves one attachment without a header or cookie, so
//! that `<video>` and `<img>` can load content with an API token.
//!
//! The token is `<payload>.<mac>` (base64url, no padding). The payload names the project, the
//! SHA-256 of the attachment, the expiry and the principal that asked; the MAC is HMAC-SHA256 under
//! a key derived from the shared `media-url` row of `stream_keys` (created on first use, the same
//! way as the Kafka signing key) with its own domain separation. Every request checks the
//! signature, the expiry, that the attachment is still in the project and that the principal still
//! holds `launches:read` there. The token never reaches logs: [`redact_path`] removes it from the
//! request URI before anything else sees it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{SecondsFormat, TimeZone, Utc};
use fastproof_blob::valid_sha256;
use fastproof_store::Store;
use franshub_access::{Binding, Principal, PrincipalKind, Scopes, Subject};
use franshub_auth::{Auth, TokenStatus};
use franshub_http::{ApiError, ApiResult};
use ring::hmac;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{ErrorBody, P};
use crate::state::AppState;

/// Name of the shared signing secret in `stream_keys`.
pub const KEY_NAME: &str = "media-url";
/// Default lifetime of a media URL (`FASTPROOF_MEDIA_URL_TTL_SECONDS`).
pub const DEFAULT_TTL_SECONDS: u64 = 900;
/// Longest allowed lifetime.
pub const MAX_TTL_SECONDS: u64 = 3600;
const PREFIX: &str = "/api/v1/media/";
const MAC_DOMAIN: &[u8] = b"fastproof-media-url-v1\0";
const DERIVE_LABEL: &[u8] = b"fastproof/media-url/v1";
const MAX_TOKEN_LEN: usize = 2048;
const MAX_PAYLOAD: usize = 1200;
const MAX_GROUPS: usize = 32;
const GRANT_CACHE_TTL: Duration = Duration::from_secs(5);
const GRANT_CACHE_MAX: usize = 4096;

/// The signing key, the lifetime and a short-lived cache of principal checks.
pub struct Media {
    key: hmac::Key,
    ttl: Duration,
    grants: Mutex<HashMap<String, Instant>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    p: Uuid,
    s: String,
    e: i64,
    k: String,
    n: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    g: Vec<String>,
}

/// A media URL and when it stops working.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaUrl {
    /// Path of the URL, relative to the server: `/api/v1/media/<token>`.
    pub url: String,
    /// RFC 3339 expiry time.
    pub expires_at: String,
}

impl Media {
    /// Loads (or creates) the shared key.
    pub async fn load(store: &Store, ttl: Duration) -> anyhow::Result<Self> {
        let secret = store.stream_key(KEY_NAME).await?;
        Ok(Self::with_secret(&secret, ttl))
    }

    pub fn with_secret(secret: &[u8], ttl: Duration) -> Self {
        let root = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let derived = hmac::sign(&root, DERIVE_LABEL);
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()),
            ttl,
            grants: Mutex::new(HashMap::new()),
        }
    }

    fn sign(&self, c: &Claims) -> anyhow::Result<String> {
        let payload = B64.encode(serde_json::to_vec(c)?);
        let tag = hmac::sign(&self.key, &[MAC_DOMAIN, payload.as_bytes()].concat());
        Ok(format!("{payload}.{}", B64.encode(tag.as_ref())))
    }

    fn verify(&self, token: &str) -> Option<Claims> {
        if token.len() > MAX_TOKEN_LEN {
            return None;
        }
        let (payload, mac) = token.split_once('.')?;
        let mac = B64.decode(mac).ok()?;
        hmac::verify(&self.key, &[MAC_DOMAIN, payload.as_bytes()].concat(), &mac).ok()?;
        let c: Claims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
        valid_sha256(&c.s).then_some(c)
    }

    fn granted(&self, key: &str) -> bool {
        let g = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        g.get(key).is_some_and(|t| t.elapsed() < GRANT_CACHE_TTL)
    }

    fn remember(&self, key: String) {
        let mut g = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= GRANT_CACHE_MAX {
            g.retain(|_, t| t.elapsed() < GRANT_CACHE_TTL);
            if g.len() >= GRANT_CACHE_MAX {
                g.clear();
            }
        }
        g.insert(key, Instant::now());
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn rejected(reason: &'static str) -> ApiError {
    metrics::counter!("media_url_rejections_total", "reason" => reason).increment(1);
    ApiError::not_found("resource")
}

fn principal_claims(
    s: &AppState,
    auth: &Auth,
    project_key: &str,
) -> ApiResult<(String, String, Vec<String>)> {
    let p = &auth.principal;
    match p.kind {
        PrincipalKind::ApiToken => {
            let id = p
                .token_id
                .ok_or_else(|| ApiError::bad_request("the token has no id"))?;
            Ok(("a".into(), id.to_string(), Vec::new()))
        }
        PrincipalKind::Token => Ok(("t".into(), p.name.clone(), Vec::new())),
        PrincipalKind::User => {
            let policy = s.platform.policy();
            let mut groups: Vec<String> = p
                .groups
                .iter()
                .filter(|g| {
                    !policy
                        .bindings_for_user("", std::slice::from_ref(g))
                        .is_empty()
                })
                .cloned()
                .collect();
            groups.sort();
            groups.dedup();
            if groups.len() > MAX_GROUPS {
                tracing::warn!(
                    project = project_key,
                    "too many bound groups for a media URL"
                );
                return Err(ApiError::bad_request(
                    "too many groups to issue a media URL; download the content instead",
                ));
            }
            Ok(("u".into(), p.name.clone(), groups))
        }
    }
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/attachments/{sha256}/media-url", tag = "attachments", operation_id = "attachments_media_url",
    params(("key" = String, Path), ("sha256" = String, Path, description = "lowercase hex SHA-256 of the attachment")),
    responses((status = 200, body = MediaUrl), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P((key, sha)): P<(String, String)>,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if !valid_sha256(&sha) {
        return Err(ApiError::not_found("attachment"));
    }
    s.store
        .attachment_by_sha(project.id, &sha)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    let (k, n, g) = principal_claims(&s, &auth, &project.key)?;
    let expires = now_ms() + s.media.ttl.as_millis() as i64;
    let claims = Claims {
        p: project.id,
        s: sha,
        e: expires,
        k,
        n,
        g,
    };
    let token = s.media.sign(&claims)?;
    if token.len() > MAX_PAYLOAD * 2 {
        return Err(ApiError::bad_request("the media URL would be too long"));
    }
    metrics::counter!("media_urls_issued_total").increment(1);
    let at = Utc
        .timestamp_millis_opt(expires)
        .single()
        .unwrap_or_else(Utc::now);
    let body = MediaUrl {
        url: format!("{PREFIX}{token}"),
        expires_at: at.to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(body),
    )
        .into_response())
}

/// The token of a request to `/api/v1/media/<token>`, moved out of the URI by [`redact_path`].
#[derive(Clone)]
pub struct MediaToken(pub String);

/// Middleware, outermost: replaces the path of `/api/v1/media/<token>` with `/api/v1/media/_`
/// (and drops the query) before routing, logging and tracing see it, and passes the token on as
/// a [`MediaToken`] extension.
pub async fn redact_path(mut req: Request, next: Next) -> Response {
    if let Some(rest) = req.uri().path().strip_prefix(PREFIX) {
        let token = rest.to_string();
        let mut parts = req.uri().clone().into_parts();
        parts.path_and_query = Some(
            "/api/v1/media/_"
                .parse()
                .unwrap_or_else(|_| axum::http::uri::PathAndQuery::from_static("/")),
        );
        if let Ok(u) = Uri::from_parts(parts) {
            *req.uri_mut() = u;
        }
        req.extensions_mut().insert(MediaToken(token));
    }
    next.run(req).await
}

async fn principal_allowed(s: &AppState, c: &Claims, project_key: &str) -> anyhow::Result<bool> {
    let cache_key = format!("{}\0{}\0{}\0{}", c.k, c.n, c.g.join("\u{1}"), c.p);
    if s.media.granted(&cache_key) {
        return Ok(true);
    }
    let policy = s.platform.policy();
    let principal: Option<Principal> = match c.k.as_str() {
        "a" => {
            let Some(svc) = s.platform.tokens() else {
                return Ok(false);
            };
            let Ok(id) = c.n.parse::<Uuid>() else {
                return Ok(false);
            };
            match svc.store().get(id).await? {
                Some(t) if t.status(Utc::now()) == TokenStatus::Active => {
                    s.platform.token_principal(&t)
                }
                _ => None,
            }
        }
        "t" => s
            .platform
            .static_tokens()
            .roles()
            .find(|(name, _)| *name == c.n)
            .map(|(name, role)| {
                let b = Binding {
                    subject: Subject::User(name.to_string()),
                    role: role.to_string(),
                    scopes: Scopes::Global,
                };
                policy.principal(PrincipalKind::Token, name, &[b])
            }),
        "u" => policy.user_principal(&c.n, &c.g),
        _ => None,
    };
    let ok = principal.is_some_and(|p| p.allowed(&s.perms.launches_read, Some(project_key)));
    if ok {
        s.media.remember(cache_key);
    }
    Ok(ok)
}

#[utoipa::path(get, path = "/api/v1/media/{token}", tag = "attachments", operation_id = "media_get",
    security(()),
    params(("token" = String, Path, description = "the token of a media URL")),
    responses((status = 200, description = "the bytes, like the content route; no credentials needed"),
        (status = 206), (status = 304), (status = 404, body = ErrorBody), (status = 416)))]
pub async fn get(
    State(s): State<AppState>,
    method: Method,
    headers: HeaderMap,
    token: Option<axum::Extension<MediaToken>>,
    path: P<String>,
) -> ApiResult<Response> {
    let token = token.map(|t| t.0.0).unwrap_or(path.0);
    let Some(c) = s.media.verify(&token) else {
        return Err(rejected("invalid"));
    };
    if c.e <= now_ms() {
        return Err(rejected("expired"));
    }
    let Some((project_key, a)) = s.store.attachment_by_sha(c.p, &c.s).await? else {
        return Err(rejected("gone"));
    };
    if !principal_allowed(&s, &c, &project_key).await? {
        return Err(rejected("principal"));
    }
    super::attachments::serve(&s, &method, &headers, &a, false, false).await
}

#[utoipa::path(head, path = "/api/v1/media/{token}", tag = "attachments", operation_id = "media_head",
    security(()),
    params(("token" = String, Path)),
    responses((status = 200), (status = 206), (status = 304), (status = 404, body = ErrorBody), (status = 416)))]
pub async fn head(
    state: State<AppState>,
    method: Method,
    headers: HeaderMap,
    token: Option<axum::Extension<MediaToken>>,
    path: P<String>,
) -> ApiResult<Response> {
    get(state, method, headers, token, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> Media {
        Media::with_secret(&[7; 32], Duration::from_secs(60))
    }

    fn claims() -> Claims {
        Claims {
            p: Uuid::nil(),
            s: "ab".repeat(32),
            e: 1,
            k: "t".into(),
            n: "ci".into(),
            g: vec![],
        }
    }

    #[test]
    fn signed_round_trip_and_tampering() {
        let m = media();
        let t = m.sign(&claims()).unwrap();
        assert!(m.verify(&t).is_some());
        assert!(
            t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        );
        for i in 0..t.len() {
            let mut b = t.clone().into_bytes();
            b[i] = if b[i] == b'A' { b'B' } else { b'A' };
            let s = String::from_utf8(b).unwrap();
            if s != t {
                assert!(m.verify(&s).is_none(), "byte {i} changed");
            }
        }
        let other = Media::with_secret(&[8; 32], Duration::from_secs(60));
        assert!(other.verify(&t).is_none());
        assert!(m.verify("").is_none());
        assert!(m.verify("a.b").is_none());
        assert!(m.verify(&"x".repeat(5000)).is_none());
    }
}
