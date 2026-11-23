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
use std::sync::{Arc, Mutex, RwLock};
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
use fastproof_store::{Store, StoredAttachment};
use franshub_access::{Binding, Principal, PrincipalKind, Scopes, Subject};
use franshub_auth::{Auth, AuthMethod, TokenStatus};
use franshub_http::{ApiError, ApiResult};
use franshub_oidc::session::{Lifetimes, SessionStore, digest as session_digest};
use ring::{digest, hkdf, hmac};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;
use uuid::Uuid;

use super::attachments::Cache;
use super::{ErrorBody, P};
use crate::state::AppState;

/// Name of the shared signing secret in `stream_keys`.
pub const KEY_NAME: &str = "media-url";
/// The row the previous signing secret moves to when the key is rotated.
pub const PREVIOUS_KEY_NAME: &str = "media-url-previous";
/// Default lifetime of a media URL (`FASTPROOF_MEDIA_URL_TTL_SECONDS`).
pub const DEFAULT_TTL_SECONDS: u64 = 300;
/// Longest allowed lifetime.
pub const MAX_TTL_SECONDS: u64 = 3600;
const EXPIRY_SKEW_MS: i64 = 60_000;
const PREFIX: &str = "/api/v1/media/";
const MAC_DOMAIN: &[u8] = b"fastproof-media-url-v2\0";
const HKDF_INFO: &[u8] = b"fastproof/media-url/v2";
const KID_LABEL: &[u8] = b"fastproof-media-kid\0";
/// The one limit on the length of a token, for issuing and for verifying.
pub const MAX_TOKEN_LEN: usize = 2048;
const MAX_GROUPS: usize = 32;
const CACHE_TTL: Duration = Duration::from_secs(5);
const CACHE_MAX: usize = 4096;

/// An attachment with the key of its project.
type ProjectAttachment = (String, StoredAttachment);

struct Len32;

impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

struct SigningKey {
    kid: String,
    key: hmac::Key,
}

/// HKDF-SHA256 with the shared database secret as salt and the secret held outside the
/// database as input keying material, so neither alone yields the key.
fn derive(db_secret: &[u8], outside_secret: &[u8]) -> SigningKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, db_secret).extract(outside_secret);
    let mut k = [0u8; 32];
    if let Ok(okm) = prk.expand(&[HKDF_INFO], Len32) {
        let _ = okm.fill(&mut k);
    }
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(KID_LABEL);
    ctx.update(&k);
    let kid = hex::encode(&ctx.finish().as_ref()[..4]);
    SigningKey {
        kid,
        key: hmac::Key::new(hmac::HMAC_SHA256, &k),
    }
}

struct Keys {
    current: SigningKey,
    previous: Option<SigningKey>,
}

/// Signing keys, limits and short-lived caches of media URLs.
pub struct Media {
    keys: RwLock<Keys>,
    outside_secret: Vec<u8>,
    ttl: Duration,
    sessions: Option<(Arc<dyn SessionStore>, Lifetimes)>,
    grants: Mutex<HashMap<String, Instant>>,
    attachments: Mutex<HashMap<(Uuid, String), (Instant, ProjectAttachment)>>,
    streams: Arc<Semaphore>,
    stream_timeout: Duration,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    x: Option<String>,
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

fn sign_with(k: &SigningKey, c: &Claims) -> anyhow::Result<String> {
    let payload = B64.encode(serde_json::to_vec(c)?);
    let tag = hmac::sign(
        &k.key,
        &[MAC_DOMAIN, k.kid.as_bytes(), b".", payload.as_bytes()].concat(),
    );
    Ok(format!("{}.{payload}.{}", k.kid, B64.encode(tag.as_ref())))
}

/// Builds a token the way the server does, from the database secret and the outside secret; for
/// tests and tools.
pub fn mint(
    db_secret: &[u8],
    outside_secret: &[u8],
    project: Uuid,
    sha256: &str,
    expires_ms: i64,
    kind: &str,
    name: &str,
) -> anyhow::Result<String> {
    sign_with(
        &derive(db_secret, outside_secret),
        &Claims {
            p: project,
            s: sha256.to_string(),
            e: expires_ms,
            k: kind.to_string(),
            n: name.to_string(),
            g: Vec::new(),
            x: None,
        },
    )
}

async fn read_keys(store: &Store, outside: &[u8]) -> anyhow::Result<Keys> {
    let current = store.stream_key(KEY_NAME).await?;
    let previous: Option<Vec<u8>> =
        sqlx::query_scalar("select secret from stream_keys where name = $1")
            .bind(PREVIOUS_KEY_NAME)
            .fetch_optional(store.pool())
            .await?;
    Ok(Keys {
        current: derive(&current, outside),
        previous: previous.map(|p| derive(&p, outside)),
    })
}

impl Media {
    /// Loads (or creates) the shared keys.
    pub async fn load(
        store: &Store,
        cfg: &crate::Config,
        sessions: Option<(Arc<dyn SessionStore>, Lifetimes)>,
    ) -> anyhow::Result<Self> {
        let outside = match &cfg.media_url_key {
            Some(k) => k.expose().as_bytes().to_vec(),
            None => {
                tracing::warn!(
                    "FASTPROOF_MEDIA_URL_KEY is not set: media URLs are signed with a key of this process only and work on the replica that issued them"
                );
                rand::random::<[u8; 32]>().to_vec()
            }
        };
        let keys = read_keys(store, &outside).await?;
        Ok(Self {
            keys: RwLock::new(keys),
            outside_secret: outside,
            ttl: cfg.media_url_ttl,
            sessions,
            grants: Mutex::new(HashMap::new()),
            attachments: Mutex::new(HashMap::new()),
            streams: Arc::new(Semaphore::new(cfg.media_max_streams)),
            stream_timeout: cfg.media_stream_timeout,
        })
    }

    /// Re-reads the current and previous keys from the database.
    pub async fn reload(&self, store: &Store) {
        match read_keys(store, &self.outside_secret).await {
            Ok(k) => {
                *self.keys.write().unwrap_or_else(|e| e.into_inner()) = k;
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "reloading the media signing keys failed")
            }
        }
    }

    /// The semaphore of concurrent attachment body streams.
    pub fn streams(&self) -> &Arc<Semaphore> {
        &self.streams
    }

    pub fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    /// Whether the session a URL is tied to still exists and has not run out.
    async fn session_alive(&self, key: &str) -> anyhow::Result<bool> {
        let Some((store, life)) = &self.sessions else {
            return Ok(false);
        };
        Ok(store
            .session(key)
            .await?
            .is_some_and(|r| life.expires_at(&r) > Utc::now()))
    }

    fn sign(&self, c: &Claims) -> anyhow::Result<String> {
        sign_with(
            &self.keys.read().unwrap_or_else(|e| e.into_inner()).current,
            c,
        )
    }

    fn verify(&self, token: &str) -> Option<Claims> {
        if token.len() > MAX_TOKEN_LEN {
            return None;
        }
        let mut parts = token.splitn(3, '.');
        let (kid, payload, mac) = (parts.next()?, parts.next()?, parts.next()?);
        let mac = B64.decode(mac).ok()?;
        {
            let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
            let key = [Some(&keys.current), keys.previous.as_ref()]
                .into_iter()
                .flatten()
                .find(|k| k.kid == kid)?;
            hmac::verify(
                &key.key,
                &[MAC_DOMAIN, kid.as_bytes(), b".", payload.as_bytes()].concat(),
                &mac,
            )
            .ok()?;
        }
        let c: Claims = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
        valid_sha256(&c.s).then_some(c)
    }

    fn granted(&self, key: &str) -> bool {
        let g = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        g.get(key).is_some_and(|t| t.elapsed() < CACHE_TTL)
    }

    fn remember(&self, key: String) {
        let mut g = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= CACHE_MAX {
            g.retain(|_, t| t.elapsed() < CACHE_TTL);
            if g.len() >= CACHE_MAX {
                g.clear();
            }
        }
        g.insert(key, Instant::now());
    }

    async fn attachment(
        &self,
        store: &Store,
        project: Uuid,
        sha: &str,
    ) -> anyhow::Result<Option<(String, StoredAttachment)>> {
        let k = (project, sha.to_string());
        {
            let c = self.attachments.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((t, v)) = c.get(&k)
                && t.elapsed() < CACHE_TTL
            {
                return Ok(Some(v.clone()));
            }
        }
        let found = store.attachment_by_sha(project, sha).await?;
        if let Some(v) = &found {
            let mut c = self.attachments.lock().unwrap_or_else(|e| e.into_inner());
            if c.len() >= CACHE_MAX {
                c.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
                if c.len() >= CACHE_MAX {
                    c.clear();
                }
            }
            c.insert(k, (Instant::now(), v.clone()));
        }
        Ok(found)
    }
}

/// Re-reads the signing keys periodically so that replicas follow a rotation without restarting.
pub async fn reload_keys(s: AppState, stop: CancellationToken) {
    let every = s.cfg.media_key_reload;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(every) => {}
        }
        s.media.reload(&s.store).await;
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

fn jwt_expiry_ms(headers: &HeaderMap) -> Option<i64> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let jwt = v
        .strip_prefix("Bearer ")
        .or_else(|| v.strip_prefix("bearer "))?;
    let payload = jwt.split('.').nth(1)?;
    let json: serde_json::Value = serde_json::from_slice(&B64.decode(payload).ok()?).ok()?;
    json.get("exp")?.as_i64()?.checked_mul(1000)
}

/// Where the lifetime of the credential that asked ends, and the session a URL is tied to.
async fn credential_end(
    s: &AppState,
    auth: &Auth,
    headers: &HeaderMap,
) -> ApiResult<(Option<i64>, Option<String>)> {
    match auth.method {
        AuthMethod::Session => {
            let rt = s.platform.oidc().ok_or_else(ApiError::unauthorized)?;
            let id = rt
                .oidc
                .session_id(headers)
                .ok_or_else(ApiError::unauthorized)?;
            let (store, life) = s
                .media
                .sessions
                .as_ref()
                .ok_or_else(ApiError::unauthorized)?;
            let key = session_digest(id);
            let rec = store
                .session(&key)
                .await?
                .ok_or_else(ApiError::unauthorized)?;
            Ok((Some(life.expires_at(&rec).timestamp_millis()), Some(key)))
        }
        AuthMethod::Bearer => {
            let exp = jwt_expiry_ms(headers).ok_or_else(ApiError::unauthorized)?;
            Ok((Some(exp), None))
        }
        _ => Ok((None, None)),
    }
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/attachments/{attachment}/media-url", tag = "attachments", operation_id = "attachments_media_url",
    params(("key" = String, Path), ("attachment" = String, Path, description = "the attachment id (UUID) or its lowercase hex SHA-256")),
    responses((status = 200, body = MediaUrl), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, sha)): P<(String, String)>,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let sha = if sha.len() == 64 {
        if !valid_sha256(&sha) {
            return Err(ApiError::not_found("attachment"));
        }
        s.store
            .attachment_by_sha(project.id, &sha)
            .await?
            .map(|(_, a)| a.sha256)
    } else {
        let id = Uuid::parse_str(&sha).map_err(|_| ApiError::not_found("attachment"))?;
        s.store.attachment(project.id, id).await?.map(|a| a.sha256)
    }
    .ok_or_else(|| ApiError::not_found("attachment"))?;
    let (k, n, g) = principal_claims(&s, &auth, &project.key)?;
    let (end, x) = credential_end(&s, &auth, &headers).await?;
    let mut expires = now_ms() + s.media.ttl.as_millis() as i64;
    if let Some(end) = end {
        expires = expires.min(end);
        if expires <= now_ms() {
            return Err(ApiError::unauthorized());
        }
    }
    let claims = Claims {
        p: project.id,
        s: sha,
        e: expires,
        k,
        n,
        g,
        x,
    };
    let token = s.media.sign(&claims)?;
    if token.len() > MAX_TOKEN_LEN {
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
            req.extensions_mut()
                .insert(axum::extract::OriginalUri(u.clone()));
            *req.uri_mut() = u;
        }
        req.extensions_mut().insert(MediaToken(token));
    }
    next.run(req).await
}

async fn principal_allowed(s: &AppState, c: &Claims, project_key: &str) -> anyhow::Result<bool> {
    let cache_key = format!(
        "{}\0{}\0{}\0{}\0{}",
        c.k,
        c.n,
        c.g.join("\u{1}"),
        c.p,
        c.x.as_deref().unwrap_or_default()
    );
    if s.media.granted(&cache_key) {
        return Ok(true);
    }
    if let Some(x) = &c.x
        && !s.media.session_alive(x).await?
    {
        return Ok(false);
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
    let now = now_ms();
    if c.e > now + MAX_TTL_SECONDS as i64 * 1000 + EXPIRY_SKEW_MS {
        return Err(rejected("range"));
    }
    if c.e <= now {
        return Err(rejected("expired"));
    }
    let Some((project_key, a)) = s.media.attachment(&s.store, c.p, &c.s).await? else {
        return Err(rejected("gone"));
    };
    if !principal_allowed(&s, &c, &project_key).await? {
        return Err(rejected("principal"));
    }
    super::attachments::serve(&s, &method, &headers, &a, false, false, Cache::Until(c.e)).await
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

    fn media(db: &[u8], out: &[u8], prev: Option<&[u8]>) -> Media {
        Media {
            keys: RwLock::new(Keys {
                current: derive(db, out),
                previous: prev.map(|p| derive(p, out)),
            }),
            outside_secret: out.to_vec(),
            ttl: Duration::from_secs(60),
            sessions: None,
            grants: Mutex::new(HashMap::new()),
            attachments: Mutex::new(HashMap::new()),
            streams: Arc::new(Semaphore::new(1)),
            stream_timeout: Duration::from_secs(1),
        }
    }

    fn claims() -> Claims {
        Claims {
            p: Uuid::nil(),
            s: "ab".repeat(32),
            e: 1,
            k: "t".into(),
            n: "ci".into(),
            g: vec![],
            x: None,
        }
    }

    #[test]
    fn signed_round_trip_and_tampering() {
        let m = media(&[7; 32], b"outside-secret-0123456789", None);
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
        let other = media(&[8; 32], b"outside-secret-0123456789", None);
        assert!(other.verify(&t).is_none());
        assert!(m.verify("").is_none());
        assert!(m.verify("a.b").is_none());
        assert!(m.verify(&"x".repeat(5000)).is_none());
    }

    #[test]
    fn the_database_secret_alone_is_not_enough() {
        let m = media(&[7; 32], b"outside-secret-0123456789", None);
        for guess in [&b""[..], b"outside-secret-0123456780", &[7u8; 32]] {
            let forged = sign_with(&derive(&[7; 32], guess), &claims()).unwrap();
            assert!(m.verify(&forged).is_none());
        }
        let right = sign_with(&derive(&[7; 32], b"outside-secret-0123456789"), &claims()).unwrap();
        assert!(m.verify(&right).is_some());
    }

    #[test]
    fn rotation_accepts_the_previous_key_until_it_is_dropped() {
        let out = b"outside-secret-0123456789";
        let old = media(&[1; 32], out, None);
        let token = old.sign(&claims()).unwrap();
        let rotated = media(&[2; 32], out, Some(&[1; 32]));
        assert!(
            rotated.verify(&token).is_some(),
            "previous key still accepted"
        );
        let fresh = rotated.sign(&claims()).unwrap();
        assert!(rotated.verify(&fresh).is_some());
        assert!(
            old.verify(&fresh).is_none(),
            "an unreloaded replica does not know the new key"
        );
        let dropped = media(&[2; 32], out, None);
        assert!(dropped.verify(&token).is_none());
        assert!(dropped.verify(&fresh).is_some());
    }

    #[tokio::test]
    async fn a_url_ends_with_its_session() {
        use franshub_oidc::session::{MemorySessionStore, SessionRecord};
        let store = Arc::new(MemorySessionStore::new(10, 10));
        let now = Utc::now();
        let rec = SessionRecord {
            name: "u".into(),
            groups: vec![],
            created_at: now,
            last_seen: now,
            id_token: None,
        };
        let life = Lifetimes {
            ttl: Duration::from_secs(3600),
            idle: Duration::from_secs(600),
        };
        store
            .create_session("k1", &rec, life.expires_at(&rec))
            .await
            .unwrap();
        let mut m = media(&[1; 32], b"outside-secret-0123456789", None);
        m.sessions = Some((store.clone(), life));
        assert!(m.session_alive("k1").await.unwrap());
        assert!(!m.session_alive("other").await.unwrap());
        store.delete_session("k1").await.unwrap();
        assert!(!m.session_alive("k1").await.unwrap(), "signed out");
        let old = SessionRecord {
            created_at: now - chrono::Duration::hours(2),
            last_seen: now - chrono::Duration::hours(2),
            ..rec
        };
        store
            .create_session("k2", &old, now + chrono::Duration::hours(1))
            .await
            .unwrap();
        assert!(!m.session_alive("k2").await.unwrap(), "idle for too long");
        m.sessions = None;
        assert!(!m.session_alive("k1").await.unwrap());
    }

    #[test]
    fn the_bearer_expiry_is_read_from_the_jwt() {
        let mut h = HeaderMap::new();
        let payload = B64.encode(br#"{"exp":1800000000,"sub":"u"}"#);
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer aGVhZA.{payload}.sig")).unwrap(),
        );
        assert_eq!(jwt_expiry_ms(&h), Some(1_800_000_000_000));
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer opaque"),
        );
        assert_eq!(jwt_expiry_ms(&h), None);
    }

    #[test]
    fn one_length_limit() {
        let m = media(&[7; 32], b"outside-secret-0123456789", None);
        let mut c = claims();
        c.n = "n".repeat(MAX_TOKEN_LEN);
        let t = m.sign(&c).unwrap();
        assert!(t.len() > MAX_TOKEN_LEN);
        assert!(
            m.verify(&t).is_none(),
            "verification applies the same limit"
        );
    }
}
