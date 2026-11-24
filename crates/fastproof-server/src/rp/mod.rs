//! The ReportPortal reporting endpoint (`72-rp-compat-design.md`): `/rp/api/{v1,v2}/{project}/…`
//! for unchanged ReportPortal agents. Calls are translated into the native model: the item
//! tree is kept in `rp_items` ([`fastproof_store::rp`]); finished tests settle and go to the
//! native ingest as [`fastproof_model::ReportResult`]s, log lines to the native logs, files to
//! the native attachments, in the direct and the Kafka mode alike.

pub mod item;
pub mod launch;
pub mod log;
pub mod settle;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use fastproof_ingest::{JsonBounds, scan_json};
use fastproof_rp::classify::{Parent, Role};
use fastproof_store::rp::RpItem;
use fastproof_store::{Launch, Project};
use franshub_access::Permission;
use franshub_auth::{Auth, require};
use franshub_http::ApiError;
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, Semaphore, mpsc};
use uuid::Uuid;

use crate::config::Env;
use crate::state::AppState;

/// Bytes of a JSON body of an item or launch call, at most.
pub const MAX_JSON_BYTES: usize = 1024 * 1024;
/// Bytes of the `json_request_part` of a multipart log batch, at most.
pub const MAX_JSON_PART_BYTES: usize = 8 * 1024 * 1024;
/// Entries of a log batch, at most.
pub const MAX_BATCH_ENTRIES: usize = 1000;
/// Parts of a multipart log batch, at most.
pub const MAX_PARTS: usize = 1001;
/// Bytes of a multipart part header, at most.
pub const MAX_PART_HEADER_BYTES: usize = 8 * 1024;
/// A pending item whose parent does not come within this time becomes a root.
pub const PENDING_TTL: Duration = Duration::from_secs(120);
/// Lifetime of cached launches, items and project aliases on a replica.
pub const CACHE_TTL: Duration = Duration::from_secs(60);
/// The working set of a launch stays this long after it finished.
pub const KEEP_AFTER_DONE: Duration = Duration::from_secs(24 * 3600);
/// How long a request waits for a slot under `FASTPROOF_RP_MAX_INFLIGHT` before 503.
pub const INFLIGHT_WAIT: Duration = Duration::from_secs(2);
/// Version this endpoint reports in `GET /rp/api/info`: agents then send ISO-8601 times.
pub const RP_VERSION: &str = "5.14.0";

/// Settings of the endpoint (`FASTPROOF_RP_*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpConfig {
    /// `FASTPROOF_RP_ENABLED` (default on).
    pub enabled: bool,
    /// Quiet time after a test's finish or last log line before its result is emitted
    /// (`FASTPROOF_RP_SETTLE_MS`, 0 to 60000, default 2000).
    pub settle: Duration,
    /// How long a finishing launch waits for unfinished items, which then become broken
    /// `interrupted` (`FASTPROOF_RP_FINISH_GRACE_SECONDS`, 0 to 86400, default 120).
    pub finish_grace: Duration,
    /// A launch without calls for this long finishes as interrupted
    /// (`FASTPROOF_RP_LAUNCH_IDLE_TIMEOUT`, 60s to 7d, default 1h).
    pub idle_timeout: Duration,
    /// Body of one `/rp` request (`FASTPROOF_RP_MAX_REQUEST_BYTES`, default 64 MiB).
    pub max_request_bytes: u64,
    /// One attachment (`FASTPROOF_RP_MAX_ATTACHMENT_BYTES`, default 64 MiB).
    pub max_attachment_bytes: u64,
    /// Items of one launch (`FASTPROOF_RP_MAX_ITEMS_PER_LAUNCH`, default 1 000 000).
    pub max_items_per_launch: i64,
    /// Requests served at once per replica; more get 503 with `Retry-After`
    /// (`FASTPROOF_RP_MAX_INFLIGHT`, 1 to 100000, default 256).
    pub max_inflight: usize,
}

impl Default for RpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            settle: Duration::from_millis(2000),
            finish_grace: Duration::from_secs(120),
            idle_timeout: Duration::from_secs(3600),
            max_request_bytes: 64 * 1024 * 1024,
            max_attachment_bytes: 64 * 1024 * 1024,
            max_items_per_launch: 1_000_000,
            max_inflight: 256,
        }
    }
}

fn number<T>(env: &Env<'_>, k: &str, default: T, min: T, max: T) -> anyhow::Result<T>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display + Copy,
{
    let Some(v) = env.var(k) else {
        return Ok(default);
    };
    let n: T = v
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{k}: invalid value {:?}", v.trim()))?;
    if n < min || n > max {
        anyhow::bail!("{k}: {min} to {max}");
    }
    Ok(n)
}

/// A duration `<n>` (seconds), `<n>s`, `<n>m`, `<n>h` or `<n>d`.
fn duration(
    env: &Env<'_>,
    k: &str,
    default: Duration,
    min: u64,
    max: u64,
) -> anyhow::Result<Duration> {
    let Some(v) = env.var(k) else {
        return Ok(default);
    };
    let t = v.trim().to_ascii_lowercase();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("{k}: invalid value {t:?}: use 3600, 30m or 1h"))?;
    let secs = match unit {
        "" | "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        _ => None,
    }
    .ok_or_else(|| anyhow::anyhow!("{k}: invalid value {t:?}: use 3600, 30m or 1h"))?;
    if secs < min || secs > max {
        anyhow::bail!("{k}: {min} to {max} seconds");
    }
    Ok(Duration::from_secs(secs))
}

impl RpConfig {
    pub fn from_env(env: &Env<'_>) -> anyhow::Result<Self> {
        let d = Self::default();
        const GIB: u64 = 1024 * 1024 * 1024;
        let enabled = match env
            .var("FASTPROOF_RP_ENABLED")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None => d.enabled,
            Some("true" | "1" | "yes" | "on") => true,
            Some("false" | "0" | "no" | "off") => false,
            Some(v) => anyhow::bail!("FASTPROOF_RP_ENABLED: expected true or false, got {v:?}"),
        };
        Ok(Self {
            enabled,
            settle: Duration::from_millis(number(
                env,
                "FASTPROOF_RP_SETTLE_MS",
                d.settle.as_millis() as u64,
                0,
                60_000,
            )?),
            finish_grace: Duration::from_secs(number(
                env,
                "FASTPROOF_RP_FINISH_GRACE_SECONDS",
                d.finish_grace.as_secs(),
                0,
                86_400,
            )?),
            idle_timeout: duration(
                env,
                "FASTPROOF_RP_LAUNCH_IDLE_TIMEOUT",
                d.idle_timeout,
                60,
                7 * 86_400,
            )?,
            max_request_bytes: number(
                env,
                "FASTPROOF_RP_MAX_REQUEST_BYTES",
                d.max_request_bytes,
                1024,
                4 * GIB,
            )?,
            max_attachment_bytes: number(
                env,
                "FASTPROOF_RP_MAX_ATTACHMENT_BYTES",
                d.max_attachment_bytes,
                1024,
                4 * GIB,
            )?,
            max_items_per_launch: number(
                env,
                "FASTPROOF_RP_MAX_ITEMS_PER_LAUNCH",
                d.max_items_per_launch,
                1,
                100_000_000,
            )?,
            max_inflight: number(env, "FASTPROOF_RP_MAX_INFLIGHT", d.max_inflight, 1, 100_000)?,
        })
    }
}

/// A map whose entries expire, bounded in size (when full, expired entries go, then all).
pub struct Cache<K, V> {
    map: Mutex<HashMap<K, (Instant, V)>>,
    ttl: Duration,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V: Clone> Cache<K, V> {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            ttl,
            cap,
        }
    }

    pub fn get(&self, k: &K) -> Option<V> {
        let m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.get(k)
            .filter(|(t, _)| t.elapsed() < self.ttl)
            .map(|(_, v)| v.clone())
    }

    pub fn put(&self, k: K, v: V) {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= self.cap && !m.contains_key(&k) {
            let ttl = self.ttl;
            m.retain(|_, (t, _)| t.elapsed() < ttl);
            if m.len() >= self.cap {
                m.clear();
            }
        }
        m.insert(k, (Instant::now(), v));
    }

    pub fn clear(&self) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn remove(&self, k: &K) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(k);
    }
}

/// A ReportPortal launch as handlers use it.
#[derive(Clone)]
pub struct LaunchInfo {
    pub uuid: String,
    pub launch: Launch,
    pub creator: String,
    /// Items started on this replica and not yet counted in `rp_launches`.
    pub uncounted: Arc<AtomicI64>,
    /// When this replica last told `rp_launches` about calls into the launch.
    pub touched: Arc<Mutex<Instant>>,
    /// Callers this replica checked as contributors of the launch.
    pub contributors: Arc<Mutex<HashSet<String>>>,
}

/// A cached item: its place in the tree.
#[derive(Clone, Debug)]
pub struct ItemRoute {
    pub launch_id: Uuid,
    pub rp_launch: String,
    pub parent: Parent,
    pub num_id: i64,
}

impl ItemRoute {
    pub fn of(r: &RpItem) -> Self {
        Self {
            launch_id: r.launch_id,
            rp_launch: r.rp_launch.clone(),
            num_id: r.num_id,
            parent: parent_of(r),
        }
    }
}

/// The classification view of a stored item.
pub fn parent_of(r: &RpItem) -> Parent {
    Parent {
        uuid: r.uuid.clone(),
        role: Role::parse(&r.role).unwrap_or(Role::Container),
        item_type: r.item_type.clone(),
        name: r.name.clone(),
        depth: r.depth,
        owner_uuid: r.owner_uuid.clone(),
        log_key: r.log_key.clone(),
        log_step: r.log_step.clone(),
        seq_uuid: r.seq_uuid.clone(),
        error_uuid: r.error_uuid.clone(),
        suite_path: serde_json::from_value(r.suite_path.clone()).unwrap_or_default(),
    }
}

/// Per-replica state of the endpoint.
pub struct RpState {
    pub inflight: Arc<Semaphore>,
    pub launches: Cache<(Uuid, String), LaunchInfo>,
    pub items: Cache<(Uuid, String), ItemRoute>,
    pub aliases: Cache<String, Option<String>>,
    pub logs: mpsc::Sender<log::LogJob>,
    pub running: mpsc::Sender<settle::Running>,
    /// Wakes the settle watcher (a launch asked to finish).
    pub wake: Arc<Notify>,
}

/// Receivers of the replica's background tasks ([`start`]).
pub struct RpTasks {
    logs: mpsc::Receiver<log::LogJob>,
    running: mpsc::Receiver<settle::Running>,
}

impl RpState {
    pub fn new(cfg: &RpConfig) -> (Self, RpTasks) {
        let (logs, logs_rx) = mpsc::channel(4096);
        let (running, running_rx) = mpsc::channel(8192);
        (
            Self {
                inflight: Arc::new(Semaphore::new(cfg.max_inflight)),
                launches: Cache::new(CACHE_TTL, 10_000),
                items: Cache::new(CACHE_TTL, 200_000),
                aliases: Cache::new(CACHE_TTL, 10_000),
                logs,
                running,
                wake: Arc::new(Notify::new()),
            },
            RpTasks {
                logs: logs_rx,
                running: running_rx,
            },
        )
    }
}

/// Starts the replica's background work: the log writer and the writer of `running`
/// results (API role), and the settle watcher (API role).
pub fn start(
    s: &AppState,
    tasks: RpTasks,
    stop: &tokio_util::sync::CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    if !s.cfg.rp.enabled || !s.cfg.roles.api {
        return Vec::new();
    }
    vec![
        tokio::spawn(log::writer(s.clone(), tasks.logs, stop.clone())),
        tokio::spawn(settle::running_writer(
            s.clone(),
            tasks.running,
            stop.clone(),
        )),
        tokio::spawn(settle::watcher(s.clone(), stop.clone())),
    ]
}

/// An error in the ReportPortal shape `{"errorCode": n, "message": "..."}`.
#[derive(Debug)]
pub struct RpError {
    pub status: StatusCode,
    pub code: i32,
    pub message: String,
}

pub type RpResult<T> = Result<T, RpError>;

/// The ReportPortal error code of a status.
pub fn error_code(status: StatusCode) -> i32 {
    match status.as_u16() {
        400 | 413 | 415 => 4001,
        401 | 403 => 4003,
        404 => 4040,
        409 => 4091,
        _ => 5000,
    }
}

impl RpError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code: error_code(status),
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl From<ApiError> for RpError {
    fn from(e: ApiError) -> Self {
        Self::new(e.status(), e.body().message.clone())
    }
}

impl From<anyhow::Error> for RpError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{e:#}"), "ReportPortal request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for RpError {
    fn into_response(self) -> Response {
        let mut r = (
            self.status,
            axum::Json(serde_json::json!({"errorCode": self.code, "message": self.message})),
        )
            .into_response();
        let h = r.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if self.status == StatusCode::UNAUTHORIZED {
            h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            h.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        r
    }
}

/// A JSON answer with a status.
pub fn json(status: StatusCode, v: serde_json::Value) -> Response {
    let mut r = (status, axum::Json(v)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// A JSON request body: within the request's byte and value bounds, then parsed leniently.
pub fn parse<T: DeserializeOwned + Default>(
    s: &AppState,
    body: Result<Bytes, BytesRejection>,
    allow_empty: bool,
) -> RpResult<T> {
    let body = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            RpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the body exceeds {MAX_JSON_BYTES} bytes"),
            )
        } else {
            RpError::bad_request("the body could not be read")
        }
    })?;
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return if allow_empty {
            Ok(T::default())
        } else {
            Err(RpError::bad_request("a JSON body is required"))
        };
    }
    parse_bytes(s, &body)
}

/// [`parse`] of bytes already read.
pub fn parse_bytes<T: DeserializeOwned>(s: &AppState, body: &[u8]) -> RpResult<T> {
    let bounds = JsonBounds::from(&s.limits);
    scan_json(body, &bounds, "body").map_err(|e| {
        let api = crate::api::results::json_limit_error(&e);
        RpError::new(api.status(), api.body().message.clone())
    })?;
    serde_json::from_slice(body)
        .map_err(|e| RpError::bad_request(format!("invalid JSON body: {e}")))
}

/// `v1` or `v2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

pub fn version(v: &str) -> RpResult<Version> {
    match v {
        "v1" => Ok(Version::V1),
        "v2" => Ok(Version::V2),
        _ => Err(RpError::not_found("resource")),
    }
}

impl AppState {
    /// The project of a `{project}` path segment (a project key, or a name listed in a
    /// project's `rp.aliases`) after checking `perm` in its scope. A segment that names no
    /// project answers as the native API does for an unknown key.
    pub async fn rp_project(
        &self,
        auth: Result<Auth, ApiError>,
        segment: &str,
        perm: &Permission,
    ) -> RpResult<(Auth, Project)> {
        let auth = auth?;
        let key = if fastproof_blob::valid_project_key(segment)
            && self.store.project(segment).await?.is_some()
        {
            Some(segment.to_string())
        } else if fastproof_rp::settings::valid_alias(segment) {
            let lower = segment.to_ascii_lowercase();
            match self.rp.aliases.get(&lower) {
                Some(k) => k,
                None => {
                    let k = self.store.rp_project_by_alias(segment).await?;
                    self.rp.aliases.put(lower, k.clone());
                    k
                }
            }
        } else {
            None
        };
        let Some(key) = key else {
            require(&auth, perm, Some(segment))?;
            return Err(RpError::not_found("project"));
        };
        let project = self.project_for(&auth, &key, perm).await?;
        Ok((auth, project))
    }

    /// The ReportPortal launch `uuid` of a project: a launch started through this endpoint, a
    /// FastProof launch id (the CI recipe: `fastproof launch start` prints it and agents join
    /// it), or a launch started with that client uuid.
    pub async fn rp_launch_info(&self, project: &Project, uuid: &str) -> RpResult<LaunchInfo> {
        if !fastproof_rp::ids::valid_uuid(uuid) {
            return Err(RpError::not_found("launch"));
        }
        let key = (project.id, uuid.to_string());
        if let Some(l) = self.rp.launches.get(&key) {
            return Ok(l);
        }
        let launch_id = match self.store.rp_launch(project.id, uuid).await? {
            Some(rl) => Some(rl.launch_id),
            None => {
                let by_id = match Uuid::parse_str(uuid) {
                    Ok(id) => self.store.launch(project.id, id).await?.map(|l| l.id),
                    Err(_) => None,
                };
                match by_id {
                    Some(id) => Some(id),
                    None => self
                        .store
                        .launch_by_external(project.id, &format!("rp:{uuid}"))
                        .await?
                        .map(|l| l.id),
                }
            }
        };
        let Some(launch_id) = launch_id else {
            return Err(RpError::not_found("launch"));
        };
        let launch = self.launch_of(project, launch_id).await?;
        let creator = self
            .store
            .launch_creator(launch.id)
            .await?
            .unwrap_or_default();
        let rl = self
            .store
            .rp_launch_insert(project.id, uuid, launch.id, &creator)
            .await?;
        let info = LaunchInfo {
            uuid: uuid.to_string(),
            launch,
            creator: rl.creator,
            uncounted: Arc::new(AtomicI64::new(0)),
            touched: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(3600))),
            contributors: Arc::new(Mutex::new(HashSet::new())),
        };
        self.rp.launches.put(key, info.clone());
        Ok(info)
    }

    /// A caller writes into a launch: the native contributor rules (a merged launch refuses,
    /// a finished one takes data only from its creator or a caller with `cases:sync`), checked
    /// once per caller and launch on a replica.
    pub async fn rp_contributor(
        &self,
        auth: &Auth,
        project: &Project,
        info: &LaunchInfo,
    ) -> RpResult<()> {
        let who = crate::limits::caller_key(auth);
        let known = info
            .contributors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&who);
        if known && info.launch.finished_at.is_none() {
            return Ok(());
        }
        let current = self.launch_of(project, info.launch.id).await?;
        self.note_plan_contributor(auth, project, &current).await?;
        info.contributors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(who);
        Ok(())
    }

    /// Counts a started item against the launch's item limit and notes the call for the
    /// idle timeout; `rp_launches` is told every 100 items or 5 seconds.
    pub async fn rp_count_items(
        &self,
        project: &Project,
        info: &LaunchInfo,
        items: i64,
    ) -> RpResult<()> {
        let pending = info.uncounted.fetch_add(items, Ordering::Relaxed) + items;
        let due = {
            let t = info.touched.lock().unwrap_or_else(|e| e.into_inner());
            t.elapsed() >= Duration::from_secs(5)
        };
        if pending < 100 && !due {
            return Ok(());
        }
        let n = info.uncounted.swap(0, Ordering::Relaxed);
        *info.touched.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        let max = self.cfg.rp.max_items_per_launch;
        if self
            .store
            .rp_launch_touch(project.id, &info.uuid, n, max)
            .await?
            .is_none()
        {
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "rp_items")
                .increment(1);
            return Err(RpError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the launch has more than FASTPROOF_RP_MAX_ITEMS_PER_LAUNCH ({max}) items"),
            ));
        }
        Ok(())
    }

    /// The item `uuid` of a project, from the cache or the store.
    pub async fn rp_route(
        &self,
        project_id: Uuid,
        uuid: &str,
    ) -> anyhow::Result<Option<ItemRoute>> {
        if !fastproof_rp::ids::valid_uuid(uuid) {
            return Ok(None);
        }
        let key = (project_id, uuid.to_string());
        if let Some(r) = self.rp.items.get(&key) {
            return Ok(Some(r));
        }
        let Some(row) = self.store.rp_item(project_id, uuid).await? else {
            return Ok(None);
        };
        let r = ItemRoute::of(&row);
        if row.role != "pending" {
            self.rp.items.put(key, r.clone());
        }
        Ok(Some(r))
    }
}

/// Checks the `rp` area of a project settings document: its shape, and that no other project
/// uses one of its aliases (as a key or an alias).
pub async fn validate_settings(
    s: &AppState,
    project_id: Uuid,
    doc: &serde_json::Value,
) -> franshub_http::ApiResult<()> {
    let rp = fastproof_rp::settings::RpSettings::from_doc(doc).map_err(ApiError::bad_request)?;
    let taken = s.store.rp_alias_conflicts(project_id, &rp.aliases).await?;
    if !taken.is_empty() {
        return Err(ApiError::bad_request(
            "rp.aliases: an alias is already the key or an alias of another project",
        ));
    }
    s.rp.aliases.clear();
    Ok(())
}

async fn not_supported() -> RpError {
    RpError::new(StatusCode::NOT_IMPLEMENTED, "not supported by FastProof")
}

/// The template of a matched route for metrics (`/rp/api/{v}/{project}/item`).
fn endpoint(req: &Request) -> String {
    req.extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "other".into())
}

/// Every `/rp` request: compressed bodies are refused, at most `FASTPROOF_RP_MAX_INFLIGHT`
/// are served at once (503 with `Retry-After` beyond), and each is counted by route template
/// and status.
async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let ep = endpoint(&req);
    let resp = if !s.cfg.rp.enabled {
        not_supported().await.into_response()
    } else if req
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        RpError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Encoding is not supported",
        )
        .into_response()
    } else {
        // A short wait before refusing: some agents (pytest) do not retry a refused start.
        match tokio::time::timeout(INFLIGHT_WAIT, s.rp.inflight.clone().acquire_owned()).await {
            Ok(Ok(permit)) => {
                let r = next.run(req).await;
                drop(permit);
                r
            }
            _ => {
                metrics::counter!("fastproof_guard_rejections_total", "guard" => "rp_inflight")
                    .increment(1);
                RpError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many requests in progress; retry later",
                )
                .into_response()
            }
        }
    };
    metrics::counter!("fastproof_rp_requests_total", "endpoint" => ep, "code" => resp.status().as_u16().to_string())
        .increment(1);
    resp
}

/// The `/rp` routes.
pub fn router(s: &AppState) -> Router<AppState> {
    let p = "/rp/api/{v}/{project}";
    let json_routes = Router::new()
        .route(
            &format!("{p}/launch"),
            post(launch::start).get(launch::list),
        )
        .route(&format!("{p}/launch/merge"), post(not_supported))
        .route(&format!("{p}/launch/uuid/{{lid}}"), get(launch::by_uuid))
        .route(&format!("{p}/launch/{{lid}}/finish"), put(launch::finish))
        .route(&format!("{p}/launch/{{lid}}/update"), put(launch::update))
        .route(&format!("{p}/item"), post(item::start_root))
        .route(&format!("{p}/item/"), post(item::start_root))
        .route(&format!("{p}/item/uuid/{{iid}}"), get(item::by_uuid))
        .route(
            &format!("{p}/item/{{iid}}"),
            post(item::start_child).put(item::finish),
        )
        .route(&format!("{p}/item/{{iid}}/update"), put(item::update))
        .route(&format!("{p}/log/entry"), post(log::entry))
        .route(&format!("{p}/settings"), get(launch::settings))
        .route("/rp/api/info", get(launch::info))
        .route("/rp/api/{v}/info", get(launch::info))
        .layer(DefaultBodyLimit::max(MAX_JSON_BYTES));
    let batch = Router::new()
        .route(&format!("{p}/log"), post(log::batch))
        .layer(DefaultBodyLimit::disable());
    Router::new()
        .merge(json_routes)
        .merge(batch)
        .route("/rp", any(not_supported))
        .route("/rp/{*rest}", any(not_supported))
        .layer(middleware::from_fn_with_state(s.clone(), guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env() {
        let vars: HashMap<&str, &str> = [
            ("FASTPROOF_RP_SETTLE_MS", "500"),
            ("FASTPROOF_RP_LAUNCH_IDLE_TIMEOUT", "30m"),
            ("FASTPROOF_RP_MAX_INFLIGHT", "8"),
        ]
        .into_iter()
        .collect();
        let get = |k: &str| vars.get(k).map(|v| v.to_string());
        let c = RpConfig::from_env(&Env::new(&get)).unwrap();
        assert_eq!(c.settle, Duration::from_millis(500));
        assert_eq!(c.idle_timeout, Duration::from_secs(1800));
        assert_eq!(c.max_inflight, 8);
        assert_eq!(c.max_request_bytes, 64 * 1024 * 1024);
        for (k, v) in [
            ("FASTPROOF_RP_SETTLE_MS", "70000"),
            ("FASTPROOF_RP_LAUNCH_IDLE_TIMEOUT", "5s"),
            ("FASTPROOF_RP_MAX_INFLIGHT", "0"),
            ("FASTPROOF_RP_ENABLED", "maybe"),
        ] {
            let get = |x: &str| (x == k).then(|| v.to_string());
            assert!(RpConfig::from_env(&Env::new(&get)).is_err(), "{k}={v}");
        }
    }

    #[test]
    fn caches_expire_and_stay_bounded() {
        let c: Cache<u32, u32> = Cache::new(Duration::from_secs(60), 2);
        c.put(1, 1);
        c.put(2, 2);
        c.put(3, 3);
        assert_eq!(c.get(&3), Some(3));
        assert!(c.get(&1).is_none() || c.get(&2).is_none());
        c.remove(&3);
        assert_eq!(c.get(&3), None);
        let e: Cache<u32, u32> = Cache::new(Duration::from_millis(0), 2);
        e.put(1, 1);
        assert_eq!(e.get(&1), None);
    }

    #[test]
    fn errors_have_the_reportportal_shape() {
        assert_eq!(error_code(StatusCode::NOT_FOUND), 4040);
        assert_eq!(error_code(StatusCode::FORBIDDEN), 4003);
        assert_eq!(error_code(StatusCode::NOT_IMPLEMENTED), 5000);
        let r = RpError::new(StatusCode::SERVICE_UNAVAILABLE, "busy").into_response();
        assert_eq!(r.headers()[header::RETRY_AFTER], "1");
    }
}
