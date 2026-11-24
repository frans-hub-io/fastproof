//! Issue tracker APIs: create an issue (with screenshots), read its state, check the
//! connection. Jira Cloud (REST v3, ADF), Jira Server and Data Center (REST v2, wiki markup),
//! GitLab (REST v4), GitHub (REST, `2022-11-28`) and Azure DevOps Boards (REST 7.1).
//!
//! Calls go through the egress-checked client and never follow redirects (they carry the
//! token). The token is a project secret with purpose `tracker` bound to the origin of the
//! API URL. A `429`, a `503` with `Retry-After`, or GitHub's exhausted rate limit pauses every
//! call to that tracker from this replica until the time the tracker named (at most an hour).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use reqwest::Method;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ETAG, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use uuid::Uuid;

use super::egress::{MAX_RESPONSE_BYTES, OutResponse, OutboundError};
use super::issue_template::{self as tpl, Block};
use super::secrets::SecretPurpose;
use super::settings::{IssueTracker, TrackerApi};
use crate::state::AppState;
use fastproof_store::trackers::IssueState;

/// Screenshots attached to one new issue, at most.
pub const MAX_IMAGES: usize = 3;
/// Bytes of one screenshot, at most.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Issues one batch read asks for, at most.
pub const MAX_BATCH: usize = 25;
/// Jira fields every read asks for.
const JIRA_FIELDS: &str = "summary,status,resolution,project,updated";
/// The longest pause after a throttled answer.
const MAX_PAUSE: Duration = Duration::from_secs(3600);
/// Pause after a `429` without `Retry-After`.
const DEFAULT_PAUSE: Duration = Duration::from_secs(60);

/// Why a tracker call failed. The text never holds a URL or a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerError {
    /// The settings, the secret or the egress policy do not allow the call.
    Config(String),
    /// The tracker refused the request (`401`, `403`, `4xx`).
    Refused(u16, String),
    /// The issue or project does not exist (or the token cannot see it).
    NotFound,
    /// Throttled: try again after this long.
    Throttled(Duration),
    /// The tracker could not be reached or failed (`5xx`).
    Unavailable(String),
}

impl std::fmt::Display for TrackerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackerError::Config(m) => f.write_str(m),
            TrackerError::Refused(401, _) => f.write_str(
                "the tracker refused the credentials (HTTP 401): write the secret again",
            ),
            TrackerError::Refused(403, m) => write!(
                f,
                "the tracker refused access (HTTP 403): check the token's scopes{}",
                if m.is_empty() {
                    String::new()
                } else {
                    format!(": {m}")
                }
            ),
            TrackerError::Refused(s, m) => {
                write!(f, "the tracker refused the request (HTTP {s}): {m}")
            }
            TrackerError::NotFound => {
                f.write_str("not found in the tracker (or the token cannot see it)")
            }
            TrackerError::Throttled(d) => write!(
                f,
                "the tracker is rate limiting; try again in {}s",
                d.as_secs().max(1)
            ),
            TrackerError::Unavailable(m) => write!(f, "the tracker is not reachable: {m}"),
        }
    }
}

impl TrackerError {
    /// The text for callers without `settings:write` and for the stored sync error: settings,
    /// secret and egress problems are not detailed (they would name the redacted API origin
    /// and secrets); the detail goes to the log and the connection test.
    pub fn public(&self) -> String {
        match self {
            TrackerError::Config(_) => {
                "the tracker settings or secret are not usable; ask a project admin to test the connection".into()
            }
            other => other.to_string(),
        }
    }
}

/// Network failures or `5xx` in a row after which a tracker is paused.
const FAILURES_TO_PAUSE: u32 = 3;
/// First pause after failures; doubled for each further failure, at most an hour.
const FAILURE_PAUSE: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Default)]
struct Health {
    until: Option<Instant>,
    failures: u32,
}

static PAUSES: LazyLock<Mutex<HashMap<(Uuid, String), Health>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn paused(project: Uuid, tracker: &str) -> Option<Duration> {
    let m = PAUSES.lock().unwrap_or_else(|e| e.into_inner());
    m.get(&(project, tracker.to_string()))
        .and_then(|h| h.until)
        .and_then(|until| until.checked_duration_since(Instant::now()))
        .filter(|d| !d.is_zero())
}

fn with_health(project: Uuid, tracker: &str, f: impl FnOnce(&mut Health)) {
    let mut m = PAUSES.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if m.len() >= 10_000 {
        m.retain(|_, h| h.until.is_some_and(|u| u > now) || h.failures > 0);
        if m.len() >= 10_000 {
            m.clear();
        }
    }
    f(m.entry((project, tracker.to_string())).or_default());
}

fn pause(project: Uuid, tracker: &str, d: Duration) {
    with_health(project, tracker, |h| {
        h.until = Some(Instant::now() + d.min(MAX_PAUSE))
    });
}

/// Counts a network failure or `5xx`; the third in a row pauses the tracker. Returns the pause.
fn failed(project: Uuid, tracker: &str) -> Option<Duration> {
    let mut out = None;
    with_health(project, tracker, |h| {
        h.failures = h.failures.saturating_add(1);
        if h.failures >= FAILURES_TO_PAUSE {
            let d = FAILURE_PAUSE
                .saturating_mul(1 << (h.failures - FAILURES_TO_PAUSE).min(5))
                .min(MAX_PAUSE);
            h.until = Some(Instant::now() + d);
            out = Some(d);
        }
    });
    out
}

fn succeeded(project: Uuid, tracker: &str) {
    let mut m = PAUSES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = m.get_mut(&(project, tracker.to_string())) {
        h.failures = 0;
    }
}

/// How long a throttled answer asks to wait: `Retry-After` (seconds or a date), GitHub's and
/// GitLab's reset time, or [`DEFAULT_PAUSE`].
pub fn throttle_delay(status: u16, h: &HeaderMap) -> Option<Duration> {
    let text = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).map(str::trim);
    let reset = || {
        let at: i64 = text("x-ratelimit-reset")
            .or_else(|| text("ratelimit-reset"))?
            .parse()
            .ok()?;
        Some(Duration::from_secs(
            (at - chrono::Utc::now().timestamp()).clamp(1, 3600) as u64,
        ))
    };
    let after = text("retry-after").and_then(super::notify::retry_after);
    match status {
        429 => Some(after.or_else(reset).unwrap_or(DEFAULT_PAUSE)),
        503 => after,
        403 if text("x-ratelimit-remaining") == Some("0") || after.is_some() => {
            Some(after.or_else(reset).unwrap_or(DEFAULT_PAUSE))
        }
        _ => None,
    }
    .map(|d| d.clamp(Duration::from_secs(1), MAX_PAUSE))
}

/// The tracker's error message from a JSON error body (Jira `errorMessages`/`errors`, GitLab,
/// GitHub and Azure DevOps `message`), cut to 300 characters.
fn error_message(body: &[u8]) -> String {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(a) = v.get("errorMessages").and_then(Value::as_array) {
        parts.extend(a.iter().filter_map(Value::as_str).map(str::to_string));
    }
    if let Some(o) = v.get("errors").and_then(Value::as_object) {
        parts.extend(
            o.iter()
                .map(|(k, x)| format!("{k}: {}", x.as_str().unwrap_or_default())),
        );
    }
    match v.get("message") {
        Some(Value::String(s)) => parts.push(s.clone()),
        Some(Value::Object(o)) => {
            parts.extend(o.iter().map(|(k, x)| format!("{k}: {x}")));
        }
        Some(Value::Array(a)) => parts.extend(a.iter().map(|x| x.to_string())),
        _ => {}
    }
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        parts.push(e.to_string());
    }
    parts
        .join("; ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect()
}

fn jira_updated(v: &Value) -> Option<String> {
    v.pointer("/fields/updated")
        .and_then(Value::as_str)
        .filter(|e| e.len() <= 200 && e.is_ascii())
        .map(str::to_string)
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A screenshot to attach.
#[derive(Clone, Debug)]
pub struct Image {
    pub name: String,
    pub content_type: String,
    pub bytes: Bytes,
}

/// Whether the bytes are a PNG, JPEG, GIF or WebP image of the declared type.
pub fn image_matches(content_type: &str, b: &[u8]) -> bool {
    match content_type {
        "image/png" => b.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"),
        "image/webp" => b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP",
        _ => false,
    }
}

/// A file name safe in a multipart header and a URL: letters, digits, `.`, `-`, `_`.
pub fn safe_file_name(name: &str, content_type: &str) -> String {
    let base: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let base = base.trim_matches('.').to_string();
    let ext = match content_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        _ => "webp",
    };
    if base.is_empty() {
        format!("screenshot.{ext}")
    } else if base.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// A key as the user typed it, normalised: `SHOP-12` (Jira), `#12` (GitLab, GitHub), `12`
/// (Azure DevOps).
pub fn parse_key(tracker: &str, input: &str) -> Option<String> {
    let k = input.trim();
    match tracker {
        "jira" => {
            let (p, n) = k.rsplit_once('-')?;
            let ok = !p.is_empty()
                && p.len() <= 32
                && p.as_bytes()[0].is_ascii_uppercase()
                && p.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                && !n.is_empty()
                && n.len() <= 12
                && n.bytes().all(|b| b.is_ascii_digit());
            ok.then(|| k.to_string())
        }
        _ => {
            let n = k.trim_start_matches('#');
            let ok = !n.is_empty()
                && n.len() <= 12
                && n.bytes().all(|b| b.is_ascii_digit())
                && !n.starts_with('0');
            ok.then(|| {
                if tracker == "azure" {
                    n.to_string()
                } else {
                    format!("#{n}")
                }
            })
        }
    }
}

enum Payload {
    Empty,
    Json(Value),
    JsonPatch(Value),
    Raw(String, Bytes),
}

/// A connection to one configured tracker.
pub struct Client<'a> {
    st: &'a AppState,
    project_id: Uuid,
    tracker: &'a IssueTracker,
    api: &'a TrackerApi,
    auth: HeaderValue,
}

impl<'a> Client<'a> {
    /// Reads the token for the tracker (purpose `tracker`, bound to the API origin).
    pub async fn new(
        st: &'a AppState,
        project_id: Uuid,
        tracker: &'a IssueTracker,
    ) -> Result<Client<'a>, TrackerError> {
        let api = tracker
            .api
            .as_ref()
            .ok_or_else(|| TrackerError::Config("the tracker has no api settings".into()))?;
        let secret = st
            .project_secret(
                project_id,
                &api.token_secret_ref,
                SecretPurpose::Tracker,
                Some(&api.url),
            )
            .await
            .map_err(|e| TrackerError::Config(e.0))?;
        let basic = match api.auth.as_deref() {
            Some(a) => a == "basic",
            None => match tracker.tracker.as_str() {
                "jira" => !api.jira_server(),
                "azure" => true,
                _ => false,
            },
        };
        let value = if basic {
            let raw = if tracker.tracker == "azure" && !secret.expose().contains(':') {
                format!(":{}", secret.expose())
            } else {
                secret.expose().to_string()
            };
            if !raw.contains(':') {
                return Err(TrackerError::Config(
                    "basic authentication needs the secret as user:token".into(),
                ));
            }
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(raw)
            )
        } else {
            format!("Bearer {}", secret.expose().trim())
        };
        let mut auth = HeaderValue::from_str(&value)
            .map_err(|_| TrackerError::Config("the secret is not a valid token".into()))?;
        auth.set_sensitive(true);
        Ok(Client {
            st,
            project_id,
            tracker,
            api,
            auth,
        })
    }

    fn id(&self) -> &str {
        self.tracker.id.as_deref().unwrap_or(&self.tracker.tracker)
    }

    fn base(&self) -> &str {
        self.api.url.trim_end_matches('/')
    }

    fn jira(&self, path: &str) -> String {
        let v = if self.api.jira_server() { "2" } else { "3" };
        format!("{}/rest/api/{v}{path}", self.base())
    }

    fn gitlab(&self, path: &str) -> String {
        format!(
            "{}/api/v4/projects/{}{path}",
            self.base(),
            enc(&self.api.project)
        )
    }

    fn github(&self, path: &str) -> String {
        format!("{}/repos/{}{path}", self.base(), self.api.project)
    }

    fn azure(&self, path: &str) -> String {
        let sep = if path.contains('?') { '&' } else { '?' };
        format!(
            "{}/{}/_apis{path}{sep}api-version=7.1",
            self.base(),
            enc(&self.api.project)
        )
    }

    async fn call(
        &self,
        method: Method,
        url: &str,
        payload: Payload,
        etag: Option<&str>,
    ) -> Result<OutResponse, TrackerError> {
        if let Some(d) = paused(self.project_id, self.id()) {
            return Err(TrackerError::Throttled(d));
        }
        let mut rb = self
            .st
            .egress
            .request(method, url)
            .map_err(|e| match e {
                OutboundError::Denied(d) => TrackerError::Config(d.to_string()),
                other => TrackerError::Unavailable(other.to_string()),
            })?
            .header(AUTHORIZATION, self.auth.clone());
        rb = match self.tracker.tracker.as_str() {
            "github" => rb
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            _ => rb.header(ACCEPT, "application/json"),
        };
        if let Some(e) = etag {
            rb = rb.header(reqwest::header::IF_NONE_MATCH, e);
        }
        rb = match payload {
            Payload::Empty => rb,
            Payload::Json(v) => rb.json(&v),
            Payload::JsonPatch(v) => rb
                .header(CONTENT_TYPE, "application/json-patch+json")
                .body(v.to_string()),
            Payload::Raw(ct, b) => rb.header(CONTENT_TYPE, ct).body(b),
        };
        if self.tracker.tracker == "jira" {
            rb = rb.header("X-Atlassian-Token", "no-check");
        }
        let r = match self
            .st
            .egress
            .send_no_redirect(rb, MAX_RESPONSE_BYTES)
            .await
        {
            Ok(r) => r,
            Err(OutboundError::Denied(d)) => return Err(TrackerError::Config(d.to_string())),
            Err(other) => {
                failed(self.project_id, self.id());
                return Err(TrackerError::Unavailable(other.to_string()));
            }
        };
        metrics::counter!("fastproof_tracker_calls_total", "tracker" => self.tracker.tracker.clone(),
            "status" => (r.status / 100).to_string() + "xx")
        .increment(1);
        if let Some(d) = throttle_delay(r.status, &r.headers) {
            pause(self.project_id, self.id(), d);
            return Err(TrackerError::Throttled(d));
        }
        if r.status >= 500 {
            failed(self.project_id, self.id());
        } else {
            succeeded(self.project_id, self.id());
        }
        match r.status {
            200..=299 | 304 => Ok(r),
            404 | 410 => Err(TrackerError::NotFound),
            300..=399 => Err(TrackerError::Refused(
                r.status,
                "the API answered with a redirect; use the final API URL".into(),
            )),
            400..=499 => Err(TrackerError::Refused(r.status, error_message(&r.body))),
            s => Err(TrackerError::Unavailable(format!("HTTP {s}"))),
        }
    }

    fn json(r: &OutResponse) -> Result<Value, TrackerError> {
        r.json::<Value>()
            .map_err(|e| TrackerError::Unavailable(e.to_string()))
    }

    /// Reads the configured project; answers its name.
    pub async fn check(&self) -> Result<String, TrackerError> {
        let (url, field) = match self.tracker.tracker.as_str() {
            "jira" => (
                self.jira(&format!("/project/{}", enc(&self.api.project))),
                "name",
            ),
            "gitlab" => (self.gitlab(""), "path_with_namespace"),
            "github" => (self.github(""), "full_name"),
            _ => (
                format!(
                    "{}/_apis/projects/{}?api-version=7.1",
                    self.base(),
                    enc(&self.api.project)
                ),
                "name",
            ),
        };
        let r = self.call(Method::GET, &url, Payload::Empty, None).await?;
        let v = Self::json(&r)?;
        Ok(v.get(field)
            .and_then(Value::as_str)
            .unwrap_or(&self.api.project)
            .chars()
            .take(200)
            .collect())
    }

    fn browse_url(&self, key: &str) -> Option<String> {
        super::issues::issue_link(std::slice::from_ref(self.tracker), key).map(|l| l.url)
    }

    fn labels(&self) -> Vec<String> {
        self.api.labels.clone().unwrap_or_default()
    }

    fn issue_type(&self) -> &str {
        self.api.issue_type.as_deref().unwrap_or("Bug")
    }

    /// Creates the issue and attaches the screenshots. A create is never retried here (the
    /// tracker might have made it); screenshots that fail are reported, not fatal.
    pub async fn create(
        &self,
        title: &str,
        blocks: &[Block],
        images: &[Image],
    ) -> Result<(IssueState, usize, Vec<String>), TrackerError> {
        match self.tracker.tracker.as_str() {
            "jira" => self.create_jira(title, blocks, images).await,
            "gitlab" => self.create_gitlab(title, blocks, images).await,
            "github" => {
                let body = json!({"title": title, "body": tpl::markdown(blocks, false), "labels": self.labels()});
                let r = self
                    .call(
                        Method::POST,
                        &self.github("/issues"),
                        Payload::Json(body),
                        None,
                    )
                    .await?;
                let v = Self::json(&r)?;
                let notes = if images.is_empty() {
                    Vec::new()
                } else {
                    vec!["GitHub has no API for issue attachments; open the result for its screenshots".into()]
                };
                Ok((self.github_state(&v)?, 0, notes))
            }
            _ => self.create_azure(title, blocks, images).await,
        }
    }

    async fn create_jira(
        &self,
        title: &str,
        blocks: &[Block],
        images: &[Image],
    ) -> Result<(IssueState, usize, Vec<String>), TrackerError> {
        let description = if self.api.jira_server() {
            json!(tpl::jira_wiki(blocks))
        } else {
            tpl::adf(blocks)
        };
        let mut fields = json!({
            "project": {"key": self.api.project},
            "summary": title,
            "issuetype": {"name": self.issue_type()},
            "description": description,
        });
        let labels = self.labels();
        if !labels.is_empty() {
            fields["labels"] = json!(labels);
        }
        let r = self
            .call(
                Method::POST,
                &self.jira("/issue"),
                Payload::Json(json!({"fields": fields})),
                None,
            )
            .await?;
        let v = Self::json(&r)?;
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .and_then(|k| parse_key("jira", k))
            .ok_or_else(|| TrackerError::Unavailable("the answer has no issue key".into()))?;
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut attached = 0;
        let mut notes = Vec::new();
        for img in images {
            let (ct, body) = multipart(img);
            match self
                .call(
                    Method::POST,
                    &self.jira(&format!("/issue/{}/attachments", enc(&key))),
                    Payload::Raw(ct, body),
                    None,
                )
                .await
            {
                Ok(_) => attached += 1,
                Err(e) => notes.push(format!("{}: {}", img.name, e.public())),
            }
        }
        let state = match self.get(&key, Some(&id), None).await {
            Ok(Some((s, _))) => s,
            _ => IssueState {
                url: self
                    .browse_url(&key)
                    .unwrap_or_else(|| format!("{}/browse/{key}", self.base())),
                key,
                remote_id: id,
                title: title.to_string(),
                state: String::new(),
                closed: false,
            },
        };
        Ok((state, attached, notes))
    }

    async fn create_gitlab(
        &self,
        title: &str,
        blocks: &[Block],
        images: &[Image],
    ) -> Result<(IssueState, usize, Vec<String>), TrackerError> {
        let body = tpl::markdown(blocks, true);
        let r = self
            .call(
                Method::POST,
                &self.gitlab("/issues"),
                Payload::Json(json!({"title": title, "description": body,
                    "labels": self.labels().join(",")})),
                None,
            )
            .await?;
        let v = Self::json(&r)?;
        let state = self.gitlab_state(&v)?;
        let mut notes = Vec::new();
        let mut uploads = Vec::new();
        for img in images {
            let (ct, b) = multipart(img);
            match self
                .call(
                    Method::POST,
                    &self.gitlab("/uploads"),
                    Payload::Raw(ct, b),
                    None,
                )
                .await
                .and_then(|r| Self::json(&r))
            {
                Ok(v) => {
                    let url = v.get("url").and_then(Value::as_str).unwrap_or_default();
                    if url.starts_with("/uploads/")
                        && url.len() <= 512
                        && url
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                    {
                        uploads.push(format!(
                            "![{}]({url})",
                            safe_file_name(&img.name, &img.content_type)
                        ));
                    } else {
                        notes.push(format!("{}: unexpected upload answer", img.name));
                    }
                }
                Err(e) => notes.push(format!("{}: {}", img.name, e.public())),
            }
        }
        let mut attached = 0;
        if !uploads.is_empty() {
            let full = format!("{body}\n\n{}", uploads.join("\n"));
            match self
                .call(
                    Method::PUT,
                    &self.gitlab(&format!(
                        "/issues/{}",
                        enc(state.key.trim_start_matches('#'))
                    )),
                    Payload::Json(json!({"description": full})),
                    None,
                )
                .await
            {
                Ok(_) => attached = uploads.len(),
                Err(e) => notes.push(format!(
                    "the screenshots were uploaded but not added to the issue: {}",
                    e.public()
                )),
            }
        }
        Ok((state, attached, notes))
    }

    async fn create_azure(
        &self,
        title: &str,
        blocks: &[Block],
        images: &[Image],
    ) -> Result<(IssueState, usize, Vec<String>), TrackerError> {
        let description = tpl::html(blocks);
        let field = if self.issue_type().eq_ignore_ascii_case("bug") {
            "/fields/Microsoft.VSTS.TCM.ReproSteps"
        } else {
            "/fields/System.Description"
        };
        let mut ops = vec![
            json!({"op": "add", "path": "/fields/System.Title", "value": title}),
            json!({"op": "add", "path": field, "value": description}),
        ];
        let labels = self.labels();
        if !labels.is_empty() {
            ops.push(
                json!({"op": "add", "path": "/fields/System.Tags", "value": labels.join("; ")}),
            );
        }
        let mut attached = 0;
        let mut notes = Vec::new();
        for img in images {
            let name = safe_file_name(&img.name, &img.content_type);
            match self
                .call(
                    Method::POST,
                    &format!(
                        "{}/{}/_apis/wit/attachments?fileName={}&api-version=7.1",
                        self.base(),
                        enc(&self.api.project),
                        enc(&name)
                    ),
                    Payload::Raw("application/octet-stream".into(), img.bytes.clone()),
                    None,
                )
                .await
                .and_then(|r| Self::json(&r))
            {
                Ok(v) => match v.get("url").and_then(Value::as_str) {
                    Some(u)
                        if super::secrets::origin_of(u)
                            == super::secrets::origin_of(&self.api.url) =>
                    {
                        ops.push(json!({"op": "add", "path": "/relations/-",
                            "value": {"rel": "AttachedFile", "url": u, "attributes": {"comment": "Screenshot from FastProof"}}}));
                        attached += 1;
                    }
                    _ => notes.push(format!("{}: unexpected upload answer", img.name)),
                },
                Err(e) => notes.push(format!("{}: {}", img.name, e.public())),
            }
        }
        let r = self
            .call(
                Method::POST,
                &self.azure(&format!("/wit/workitems/${}", enc(self.issue_type()))),
                Payload::JsonPatch(Value::Array(ops)),
                None,
            )
            .await?;
        let v = Self::json(&r)?;
        Ok((self.azure_state(&v)?, attached, notes))
    }

    /// Reads an issue; `Ok(None)` when it did not change since `etag` (a `304` for GitLab and
    /// GitHub; for Jira the `updated` time stored as the etag is compared, because Jira has
    /// no conditional reads).
    pub async fn get(
        &self,
        key: &str,
        remote_id: Option<&str>,
        etag: Option<&str>,
    ) -> Result<Option<(IssueState, Option<String>)>, TrackerError> {
        let number = key.trim_start_matches('#');
        let url = match self.tracker.tracker.as_str() {
            "jira" => self.jira(&format!("/issue/{}?fields={JIRA_FIELDS}", enc(key))),
            "gitlab" => self.gitlab(&format!("/issues/{}", enc(number))),
            "github" => self.github(&format!("/issues/{}", enc(number))),
            _ => self.azure(&format!(
                "/wit/workitems/{}?fields=System.State,System.Title,System.TeamProject",
                enc(remote_id.unwrap_or(number))
            )),
        };
        let sent = etag.filter(|_| matches!(self.tracker.tracker.as_str(), "gitlab" | "github"));
        let r = self.call(Method::GET, &url, Payload::Empty, sent).await?;
        if r.status == 304 {
            return Ok(None);
        }
        let header_etag = r
            .headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|e| e.len() <= 200 && e.is_ascii())
            .map(str::to_string);
        let v = Self::json(&r)?;
        let (s, new_etag) = match self.tracker.tracker.as_str() {
            "jira" => {
                let state = self.jira_state(&v)?;
                let updated = jira_updated(&v);
                if updated.is_some() && updated.as_deref() == etag {
                    return Ok(None);
                }
                (state, updated)
            }
            "gitlab" => (self.gitlab_state(&v)?, header_etag),
            "github" => (self.github_state(&v)?, header_etag),
            _ => (self.azure_state(&v)?, header_etag),
        };
        Ok(Some((s, new_etag)))
    }

    /// Reads several issues of this tracker's project with one call: Jira `key in (...)`
    /// search, GitLab `iids[]`, GitHub GraphQL (one `issue(number:)` alias per issue) and
    /// Azure DevOps `workitemsbatch`. Answers the states by issue key; an issue the tracker
    /// did not return (deleted, no access, another project) is missing. At most
    /// [`MAX_BATCH`] issues are asked for; the rest are ignored. The second part of an
    /// answer is Jira's `updated` time (the etag of its next read).
    pub async fn get_many(
        &self,
        items: &[(String, Option<String>)],
    ) -> Result<HashMap<String, (IssueState, Option<String>)>, TrackerError> {
        let kind = self.tracker.tracker.as_str();
        let items: Vec<&(String, Option<String>)> = items
            .iter()
            .filter(|(k, _)| parse_key(kind, k).is_some())
            .take(MAX_BATCH)
            .collect();
        let mut out = HashMap::new();
        if items.is_empty() {
            return Ok(out);
        }
        let number = |k: &str| k.trim_start_matches('#').to_string();
        match kind {
            "jira" => {
                let keys: Vec<&str> = items.iter().map(|(k, _)| k.as_str()).collect();
                let url = if self.api.jira_server() {
                    self.jira("/search")
                } else {
                    self.jira("/search/jql")
                };
                let body = json!({"jql": format!("key in ({})", keys.join(",")),
                    "fields": JIRA_FIELDS.split(',').collect::<Vec<_>>(), "maxResults": keys.len()});
                let r = self
                    .call(Method::POST, &url, Payload::Json(body), None)
                    .await?;
                let v = Self::json(&r)?;
                for i in v
                    .get("issues")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(items.len())
                {
                    if let Ok(s) = self.jira_state(i) {
                        out.insert(s.key.clone(), (s, jira_updated(i)));
                    }
                }
            }
            "gitlab" => {
                let iids: Vec<String> = items
                    .iter()
                    .map(|(k, _)| format!("iids[]={}", number(k)))
                    .collect();
                let url = self.gitlab(&format!(
                    "/issues?scope=all&per_page={}&{}",
                    items.len(),
                    iids.join("&")
                ));
                let r = self.call(Method::GET, &url, Payload::Empty, None).await?;
                let v = Self::json(&r)?;
                for i in v.as_array().into_iter().flatten().take(items.len()) {
                    if let Ok(s) = self.gitlab_state(i) {
                        out.insert(s.key.clone(), (s, None));
                    }
                }
            }
            "github" => {
                let (owner, name) = self.api.project.split_once('/').ok_or_else(|| {
                    TrackerError::Config("the GitHub project must be owner/repository".into())
                })?;
                let aliases: String = items
                    .iter()
                    .map(|(k, _)| {
                        let n = number(k);
                        format!("i{n}:issue(number:{n}){{...F}}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let query = format!(
                    "query($owner:String!,$name:String!){{repository(owner:$owner,name:$name){{{aliases}}}}} \
                     fragment F on Issue{{databaseId number title state stateReason url}}"
                );
                let r = self
                    .call(
                        Method::POST,
                        &self.github_graphql(),
                        Payload::Json(
                            json!({"query": query, "variables": {"owner": owner, "name": name}}),
                        ),
                        None,
                    )
                    .await?;
                let v = Self::json(&r)?;
                let limited = v
                    .get("errors")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|e| e.get("type").and_then(Value::as_str) == Some("RATE_LIMITED"));
                if limited {
                    pause(self.project_id, self.id(), DEFAULT_PAUSE);
                    return Err(TrackerError::Throttled(DEFAULT_PAUSE));
                }
                let Some(repo) = v.pointer("/data/repository").filter(|r| r.is_object()) else {
                    return Ok(out);
                };
                for (k, _) in &items {
                    let n = number(k);
                    if let Some(i) = repo.get(format!("i{n}")).filter(|i| i.is_object())
                        && let Some(s) = self.github_graphql_state(i)
                    {
                        out.insert(s.key.clone(), (s, None));
                    }
                }
            }
            _ => {
                let ids: Vec<u64> = items
                    .iter()
                    .filter_map(|(k, remote)| remote.as_deref().unwrap_or(k).parse().ok())
                    .collect();
                let url = self.azure("/wit/workitemsbatch");
                let body = json!({"ids": ids, "errorPolicy": "Omit",
                    "fields": ["System.State", "System.Title", "System.TeamProject"]});
                let r = self
                    .call(Method::POST, &url, Payload::Json(body), None)
                    .await?;
                let v = Self::json(&r)?;
                for i in v
                    .get("value")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(items.len())
                {
                    if let Ok(s) = self.azure_state(i) {
                        out.insert(s.key.clone(), (s, None));
                    }
                }
            }
        }
        Ok(out)
    }

    fn github_graphql(&self) -> String {
        let base = self.base();
        match base.strip_suffix("/api/v3") {
            Some(root) => format!("{root}/api/graphql"),
            None => format!("{base}/graphql"),
        }
    }

    fn github_graphql_state(&self, v: &Value) -> Option<IssueState> {
        let n = v.get("number").and_then(Value::as_u64)?;
        let key = format!("#{n}");
        let state = v.get("state").and_then(Value::as_str)?.to_ascii_lowercase();
        let shown = match v.get("stateReason").and_then(Value::as_str) {
            Some(r) if state == "closed" && !r.is_empty() => {
                format!("closed ({})", r.to_ascii_lowercase())
            }
            _ => state.clone(),
        };
        Some(IssueState {
            url: self.web_url(v, "url", &key),
            remote_id: v
                .get("databaseId")
                .map(|x| x.to_string())
                .unwrap_or_default(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            closed: state == "closed",
            state: shown,
            key,
        })
    }

    fn jira_state(&self, v: &Value) -> Result<IssueState, TrackerError> {
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .and_then(|k| parse_key("jira", k))
            .ok_or_else(|| TrackerError::Unavailable("the answer has no issue key".into()))?;
        let f = v.get("fields").cloned().unwrap_or_default();
        let project = f.pointer("/project/key").and_then(Value::as_str);
        if !key.starts_with(&format!("{}-", self.api.project))
            || project != Some(self.api.project.as_str())
        {
            return Err(TrackerError::NotFound);
        }
        let status = f.get("status").cloned().unwrap_or_default();
        let category = status
            .pointer("/statusCategory/key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(IssueState {
            url: self
                .browse_url(&key)
                .unwrap_or_else(|| format!("{}/browse/{key}", self.base())),
            remote_id: v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: f
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            state: status
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            closed: category == "done",
            key,
        })
    }

    /// A link from the tracker's answer, kept only on the API's or the URL template's origin
    /// and at most 2048 bytes.
    fn own_url(&self, u: Option<&str>) -> Option<String> {
        let u = u?;
        let o = super::secrets::origin_of(u)?;
        let template = self.tracker.url_template.replace("{key}", "1");
        let ok = u.len() <= 2048
            && (Some(&o) == super::secrets::origin_of(&self.api.url).as_ref()
                || Some(&o) == super::secrets::origin_of(&template).as_ref());
        ok.then(|| u.to_string())
    }

    fn web_url(&self, v: &Value, field: &str, key: &str) -> String {
        self.own_url(v.get(field).and_then(Value::as_str))
            .or_else(|| self.browse_url(key))
            .unwrap_or_default()
    }

    fn gitlab_state(&self, v: &Value) -> Result<IssueState, TrackerError> {
        let iid = v
            .get("iid")
            .and_then(Value::as_u64)
            .ok_or_else(|| TrackerError::Unavailable("the answer has no iid".into()))?;
        let key = format!("#{iid}");
        let p = self.api.project.as_str();
        let same = if p.bytes().all(|b| b.is_ascii_digit()) {
            v.get("project_id").map(|x| x.to_string()).as_deref() == Some(p)
        } else {
            let full = v.pointer("/references/full").and_then(Value::as_str);
            let web = v
                .get("web_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            full.is_some_and(|f| f.eq_ignore_ascii_case(&format!("{p}#{iid}")))
                || web.contains(&format!("/{}/-/issues/", p.to_ascii_lowercase()))
        };
        if !same {
            return Err(TrackerError::NotFound);
        }
        let state = v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(IssueState {
            url: self.web_url(v, "web_url", &key),
            remote_id: v.get("id").map(|x| x.to_string()).unwrap_or_default(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            closed: state == "closed",
            state,
            key,
        })
    }

    fn github_state(&self, v: &Value) -> Result<IssueState, TrackerError> {
        let n = v
            .get("number")
            .and_then(Value::as_u64)
            .ok_or_else(|| TrackerError::Unavailable("the answer has no number".into()))?;
        let key = format!("#{n}");
        let repo = format!("/repos/{}", self.api.project.to_ascii_lowercase());
        let same = match v.get("repository_url").and_then(Value::as_str) {
            Some(r) => r.to_ascii_lowercase().ends_with(&repo),
            None => v
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|u| u.to_ascii_lowercase().contains(&format!("{repo}/issues/"))),
        };
        if !same {
            return Err(TrackerError::NotFound);
        }
        let state = v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let shown = match v.get("state_reason").and_then(Value::as_str) {
            Some(r) if state == "closed" && !r.is_empty() => format!("closed ({r})"),
            _ => state.clone(),
        };
        Ok(IssueState {
            url: self.web_url(v, "html_url", &key),
            remote_id: v.get("id").map(|x| x.to_string()).unwrap_or_default(),
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            closed: state == "closed",
            state: shown,
            key,
        })
    }

    fn azure_state(&self, v: &Value) -> Result<IssueState, TrackerError> {
        let id = v
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| TrackerError::Unavailable("the answer has no id".into()))?;
        let key = id.to_string();
        let team = v
            .pointer("/fields/System.TeamProject")
            .and_then(Value::as_str);
        if !team.is_some_and(|t| t.eq_ignore_ascii_case(&self.api.project)) {
            return Err(TrackerError::NotFound);
        }
        let state = v
            .pointer("/fields/System.State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let closed_states = self
            .api
            .closed_states
            .clone()
            .unwrap_or_else(|| vec!["Closed".into(), "Done".into(), "Removed".into()]);
        let url = match self.own_url(v.pointer("/_links/html/href").and_then(Value::as_str)) {
            Some(u) => u,
            None => self.browse_url(&key).unwrap_or_else(|| {
                format!(
                    "{}/{}/_workitems/edit/{id}",
                    self.base(),
                    enc(&self.api.project)
                )
            }),
        };
        Ok(IssueState {
            url,
            remote_id: key.clone(),
            title: v
                .pointer("/fields/System.Title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            closed: closed_states.iter().any(|c| c.eq_ignore_ascii_case(&state)),
            state,
            key,
        })
    }
}

/// A `multipart/form-data` body with one `file` part.
fn multipart(img: &Image) -> (String, Bytes) {
    let boundary = format!("fastproof-{}", Uuid::new_v4().simple());
    let name = safe_file_name(&img.name, &img.content_type);
    let mut b = Vec::with_capacity(img.bytes.len() + 256);
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {}\r\n\r\n",
            img.content_type
        )
        .as_bytes(),
    );
    b.extend_from_slice(&img.bytes);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (
        format!("multipart/form-data; boundary={boundary}"),
        Bytes::from(b),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(parse_key("jira", " SHOP-12 ").as_deref(), Some("SHOP-12"));
        assert_eq!(parse_key("jira", "shop-12"), None);
        assert_eq!(parse_key("jira", "SHOP-"), None);
        assert_eq!(parse_key("gitlab", "12").as_deref(), Some("#12"));
        assert_eq!(parse_key("github", "#7").as_deref(), Some("#7"));
        assert_eq!(parse_key("azure", "#7").as_deref(), Some("7"));
        assert_eq!(parse_key("github", "../1"), None);
        assert_eq!(parse_key("github", "007"), None);
    }

    #[test]
    fn throttling() {
        let mut h = HeaderMap::new();
        assert_eq!(throttle_delay(429, &h), Some(DEFAULT_PAUSE));
        h.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(throttle_delay(429, &h), Some(Duration::from_secs(7)));
        assert_eq!(throttle_delay(503, &h), Some(Duration::from_secs(7)));
        assert_eq!(throttle_delay(500, &h), None);
        let mut gh = HeaderMap::new();
        gh.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        let reset = (chrono::Utc::now().timestamp() + 30).to_string();
        gh.insert("x-ratelimit-reset", HeaderValue::from_str(&reset).unwrap());
        let d = throttle_delay(403, &gh).unwrap();
        assert!(d.as_secs() >= 28 && d.as_secs() <= 31, "{d:?}");
        assert_eq!(throttle_delay(403, &HeaderMap::new()), None);
        h.insert("retry-after", HeaderValue::from_static("999999"));
        assert_eq!(throttle_delay(429, &h), Some(MAX_PAUSE));
    }

    #[test]
    fn error_messages() {
        assert_eq!(
            error_message(br#"{"errorMessages":["x"],"errors":{"summary":"required"}}"#),
            "x; summary: required"
        );
        assert_eq!(
            error_message(br#"{"message":"Bad credentials"}"#),
            "Bad credentials"
        );
        assert_eq!(error_message(b"<html>"), "");
    }

    #[test]
    fn images_and_names() {
        assert!(image_matches("image/png", b"\x89PNG\r\n\x1a\nxxxx"));
        assert!(!image_matches("image/png", b"<svg"));
        assert!(!image_matches("image/svg+xml", b"<svg"));
        assert_eq!(safe_file_name("../a b\".png", "image/png"), "a_b_.png");
        assert_eq!(safe_file_name("", "image/jpeg"), "screenshot.jpg");
        assert_eq!(safe_file_name("shot", "image/webp"), "shot.webp");
        let (ct, b) = multipart(&Image {
            name: "x\r\n.png".into(),
            content_type: "image/png".into(),
            bytes: Bytes::from_static(b"PNG"),
        });
        let text = String::from_utf8_lossy(&b);
        assert!(ct.starts_with("multipart/form-data; boundary=fastproof-"));
        assert!(text.contains("filename=\"x__.png\""), "{text}");
    }

    #[test]
    fn network_failures_pause_a_tracker() {
        let p = Uuid::now_v7();
        assert_eq!(failed(p, "gl"), None);
        succeeded(p, "gl");
        assert_eq!(failed(p, "gl"), None);
        assert_eq!(failed(p, "gl"), None);
        assert_eq!(failed(p, "gl"), Some(FAILURE_PAUSE));
        assert!(paused(p, "gl").is_some());
        assert_eq!(failed(p, "gl"), Some(FAILURE_PAUSE * 2));
        assert!(paused(p, "gh").is_none());
        assert!(
            TrackerError::Config("may only be sent to https://jira.example.com".into())
                .public()
                .contains("ask a project admin")
        );
    }

    #[test]
    fn pauses_are_per_tracker() {
        let p = Uuid::now_v7();
        assert!(paused(p, "a").is_none());
        pause(p, "a", Duration::from_secs(5));
        assert!(paused(p, "a").is_some());
        assert!(paused(p, "b").is_none());
    }
}
