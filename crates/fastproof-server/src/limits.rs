//! Request guards against resource exhaustion: per-caller caps on event streams and heavy
//! reads, a global cap on heavy reads that keeps database connections free for ingest, a cap
//! on concurrent spools, the periodic re-check of streaming callers, a catch-panic layer and
//! an idle timeout on request bodies.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::bail;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use franshub_access::Permission;
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiResult};
use futures::{FutureExt, StreamExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::config::Env;
use crate::state::AppState;

/// How long a heavy read waits for a free slot before it answers 503.
pub const HEAVY_WAIT: Duration = Duration::from_secs(5);
/// Seconds a client is told to wait before it retries a refused heavy read.
const HEAVY_RETRY_AFTER: &str = "2";
/// How long an upload waits for a free spool slot before it answers 503.
pub const SPOOL_WAIT: Duration = Duration::from_secs(10);

/// Settings of the guards (`FASTPROOF_*`, see [`GuardSettings::from_env`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardSettings {
    /// Open event streams (`/events`, `/logs/stream`) per caller and replica.
    pub sse_per_caller: usize,
    /// How often an open event stream checks that its caller is still allowed.
    pub sse_recheck: Duration,
    /// Concurrent heavy reads (dashboard, log search, similar failures, rule dry runs, stored
    /// log reads) per caller and replica.
    pub heavy_per_caller: usize,
    /// Uploads spooled to the local disk at the same time, per replica.
    pub max_spools: usize,
    /// A request body that sends nothing for this long fails.
    pub body_idle_timeout: Duration,
}

impl Default for GuardSettings {
    fn default() -> Self {
        Self {
            sse_per_caller: 16,
            sse_recheck: Duration::from_secs(300),
            heavy_per_caller: 4,
            max_spools: 32,
            body_idle_timeout: Duration::from_secs(60),
        }
    }
}

fn number(env: &Env<'_>, k: &str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
    match env.var(k) {
        None => Ok(default),
        Some(v) => match v.trim().parse::<u64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(n),
            _ => bail!("{k}: a number {min}..={max}"),
        },
    }
}

impl GuardSettings {
    /// `FASTPROOF_SSE_MAX_PER_CALLER`, `FASTPROOF_SSE_RECHECK_SECONDS`,
    /// `FASTPROOF_HEAVY_QUERIES_PER_CALLER`, `FASTPROOF_MAX_CONCURRENT_SPOOLS`,
    /// `FASTPROOF_BODY_IDLE_TIMEOUT_SECONDS`.
    pub fn from_env(env: &Env<'_>) -> anyhow::Result<Self> {
        let d = Self::default();
        Ok(Self {
            sse_per_caller: number(
                env,
                "FASTPROOF_SSE_MAX_PER_CALLER",
                d.sse_per_caller as u64,
                1,
                1000,
            )? as usize,
            sse_recheck: Duration::from_secs(number(
                env,
                "FASTPROOF_SSE_RECHECK_SECONDS",
                d.sse_recheck.as_secs(),
                1,
                3600,
            )?),
            heavy_per_caller: number(
                env,
                "FASTPROOF_HEAVY_QUERIES_PER_CALLER",
                d.heavy_per_caller as u64,
                1,
                100,
            )? as usize,
            max_spools: number(
                env,
                "FASTPROOF_MAX_CONCURRENT_SPOOLS",
                d.max_spools as u64,
                1,
                10_000,
            )? as usize,
            body_idle_timeout: Duration::from_secs(number(
                env,
                "FASTPROOF_BODY_IDLE_TIMEOUT_SECONDS",
                d.body_idle_timeout.as_secs(),
                1,
                3600,
            )?),
        })
    }
}

/// Who a request counts against: the managed token, else the principal kind and name.
pub fn caller_key(auth: &Auth) -> String {
    match auth.principal.token_id {
        Some(id) => format!("token:{id}"),
        None => format!("{}:{}", auth.principal.kind.as_str(), auth.principal.name),
    }
}

type Counts = Arc<Mutex<HashMap<String, usize>>>;

/// A slot of a per-caller counter, released on drop.
#[derive(Debug)]
pub struct CallerSlot {
    counts: Counts,
    key: String,
}

impl Drop for CallerSlot {
    fn drop(&mut self) {
        let mut m = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

fn take_slot(counts: &Counts, key: String, max: usize) -> Option<CallerSlot> {
    let mut m = counts.lock().unwrap_or_else(|e| e.into_inner());
    let n = m.entry(key.clone()).or_insert(0);
    if *n >= max {
        if *n == 0 {
            m.remove(&key);
        }
        return None;
    }
    *n += 1;
    Some(CallerSlot {
        counts: counts.clone(),
        key,
    })
}

/// A heavy read in progress: the caller's slot and a global slot.
#[derive(Debug)]
pub struct HeavyPermit {
    _slot: CallerSlot,
    _global: OwnedSemaphorePermit,
}

/// The guards of one replica.
#[derive(Debug)]
pub struct Guards {
    pub settings: GuardSettings,
    streams: Counts,
    heavy_callers: Counts,
    heavy: Arc<Semaphore>,
    spools: Arc<Semaphore>,
    /// Test-management writes that multiply work, per caller (`crate::tm_rate`).
    pub tm_writes: crate::tm_rate::WriteRate,
    /// When test-management users were last recorded (`crate::tm_people`).
    pub tm_people: crate::tm_people::Noted,
}

impl Guards {
    /// `heavy_slots` concurrent heavy reads per replica (the pool size minus the connections
    /// kept for ingest).
    pub fn new(settings: GuardSettings, heavy_slots: usize) -> Self {
        Self {
            settings,
            streams: Arc::default(),
            heavy_callers: Arc::default(),
            heavy: Arc::new(Semaphore::new(heavy_slots.max(1))),
            spools: Arc::new(Semaphore::new(settings.max_spools.max(1))),
            tm_writes: crate::tm_rate::WriteRate::default(),
            tm_people: crate::tm_people::Noted::default(),
        }
    }

    /// A slot for one more open event stream of the caller, or 429.
    pub fn stream_slot(&self, auth: &Auth) -> ApiResult<CallerSlot> {
        take_slot(
            &self.streams,
            caller_key(auth),
            self.settings.sse_per_caller,
        )
        .ok_or_else(|| {
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "sse_per_caller")
                .increment(1);
            ApiError::too_many_requests("too many open event streams for this caller")
        })
    }

    /// Admission of a heavy read: 429 when the caller already runs
    /// [`GuardSettings::heavy_per_caller`] of them, 503 when the replica's heavy slots stay
    /// taken for [`HEAVY_WAIT`].
    pub async fn heavy(&self, auth: &Auth) -> ApiResult<HeavyPermit> {
        let slot = take_slot(
            &self.heavy_callers,
            caller_key(auth),
            self.settings.heavy_per_caller,
        )
        .ok_or_else(|| {
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "heavy_per_caller")
                .increment(1);
            ApiError::too_many_requests("too many concurrent queries for this caller")
                .with_header(RETRY_AFTER, HeaderValue::from_static(HEAVY_RETRY_AFTER))
        })?;
        let global =
            match tokio::time::timeout(HEAVY_WAIT, self.heavy.clone().acquire_owned()).await {
                Ok(Ok(p)) => p,
                _ => {
                    metrics::counter!("fastproof_guard_rejections_total", "guard" => "heavy_global")
                    .increment(1);
                    return Err(ApiError::unavailable("the server is busy; retry later")
                        .with_header(RETRY_AFTER, HeaderValue::from_static(HEAVY_RETRY_AFTER)));
                }
            };
        Ok(HeavyPermit {
            _slot: slot,
            _global: global,
        })
    }

    /// A slot for spooling one upload to the local disk, or 503 after [`SPOOL_WAIT`].
    pub async fn spool(&self) -> ApiResult<OwnedSemaphorePermit> {
        match tokio::time::timeout(SPOOL_WAIT, self.spools.clone().acquire_owned()).await {
            Ok(Ok(p)) => Ok(p),
            _ => {
                metrics::counter!("fastproof_guard_rejections_total", "guard" => "spools")
                    .increment(1);
                Err(ApiError::unavailable(
                    "too many uploads in progress; retry later",
                ))
            }
        }
    }

    /// Open event streams counted for a caller key (tests).
    pub fn open_streams(&self, key: &str) -> usize {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

/// Checks an open stream's caller again: the credentials it opened with must still
/// authenticate, hold the permission in the project, and the project must be the same one.
pub struct Recheck {
    s: AppState,
    headers: HeaderMap,
    perm: Permission,
    key: String,
    project_id: Uuid,
}

impl Recheck {
    pub fn new(
        s: &AppState,
        headers: &HeaderMap,
        perm: &Permission,
        key: &str,
        project_id: Uuid,
    ) -> Self {
        let mut kept = HeaderMap::new();
        for (k, v) in headers {
            if matches!(
                k.as_str(),
                "authorization" | "cookie" | "origin" | "referer" | "sec-fetch-site"
            ) {
                kept.append(k.clone(), v.clone());
            }
        }
        Self {
            s: s.clone(),
            headers: kept,
            perm: perm.clone(),
            key: key.to_string(),
            project_id,
        }
    }

    /// How often streams check.
    pub fn interval(&self) -> Duration {
        self.s.guards.settings.sse_recheck
    }

    pub async fn allowed(&self) -> bool {
        let ok = match self
            .s
            .platform
            .authenticate(&self.headers, &Method::GET)
            .await
        {
            Ok(a) => require(&a, &self.perm, Some(&self.key)).is_ok(),
            Err(_) => false,
        };
        let ok = ok
            && matches!(self.s.store.project(&self.key).await, Ok(Some(p)) if p.id == self.project_id);
        if !ok {
            metrics::counter!("fastproof_sse_revoked_total").increment(1);
        }
        ok
    }
}

/// Answers a panicking handler with the uniform 500 instead of dropping the connection.
pub async fn catch_panic(req: Request, next: Next) -> Response {
    match AssertUnwindSafe(next.run(req)).catch_unwind().await {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            metrics::counter!("fastproof_http_panics_total").increment(1);
            tracing::error!(panic = %msg.chars().take(500).collect::<String>(), "a request handler panicked");
            ApiError::internal().into_response()
        }
    }
}

/// Fails a request body that sends nothing for the configured time (slow-body clients would
/// otherwise hold a spool slot, a file and a connection).
pub async fn body_idle_timeout(State(idle): State<Duration>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let data = body.into_data_stream();
    let timed = futures::stream::unfold(Some(data), move |state| async move {
        let mut data = state?;
        match tokio::time::timeout(idle, data.next()).await {
            Ok(Some(item)) => Some((item, Some(data))),
            Ok(None) => None,
            Err(_) => {
                metrics::counter!("fastproof_body_idle_timeouts_total").increment(1);
                Some((
                    Err(axum::Error::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("no data for {} s", idle.as_secs()),
                    ))),
                    None,
                ))
            }
        }
    });
    next.run(Request::from_parts(parts, Body::from_stream(timed)))
        .await
}

/// The uniform 503 of a heavy read that hit the database statement timeout.
pub fn is_statement_timeout(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<sqlx::Error>()
            .and_then(|e| e.as_database_error())
            .and_then(|d| d.code())
            .is_some_and(|c| c == "57014")
    })
}

/// Maps a store error of a heavy read: a cancelled statement is a 503, the rest a 500.
pub fn heavy_error(e: anyhow::Error) -> ApiError {
    if is_statement_timeout(&e) {
        metrics::counter!("fastproof_statement_timeouts_total").increment(1);
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "timeout",
            "the query took too long; narrow it down",
        );
    }
    ApiError::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::{get, post};

    #[test]
    fn caller_slots_are_counted_and_released() {
        let counts: Counts = Arc::default();
        let a = take_slot(&counts, "k".into(), 2).unwrap();
        let b = take_slot(&counts, "k".into(), 2).unwrap();
        assert!(take_slot(&counts, "k".into(), 2).is_none());
        assert!(take_slot(&counts, "other".into(), 2).is_some());
        drop(a);
        let c = take_slot(&counts, "k".into(), 2).unwrap();
        drop((b, c));
        assert!(counts.lock().unwrap().is_empty());
    }

    #[test]
    fn settings_from_env() {
        let none = |_: &str| None;
        assert_eq!(
            GuardSettings::from_env(&Env::new(&none)).unwrap(),
            GuardSettings::default()
        );
        let bad = |k: &str| (k == "FASTPROOF_SSE_MAX_PER_CALLER").then(|| "0".to_string());
        let e = GuardSettings::from_env(&Env::new(&bad))
            .unwrap_err()
            .to_string();
        assert!(e.contains("FASTPROOF_SSE_MAX_PER_CALLER"), "{e}");
    }

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn panics_become_500() {
        let app = Router::new()
            .route(
                "/boom",
                get(|| async {
                    let v: Vec<u8> = Vec::new();
                    v[usize::MAX / 2].to_string()
                }),
            )
            .layer(axum::middleware::from_fn(catch_panic));
        let base = serve(app).await;
        let r = reqwest::get(format!("{base}/boom")).await.unwrap();
        assert_eq!(r.status().as_u16(), 500);
    }

    #[tokio::test]
    async fn stalled_bodies_fail() {
        let app = Router::new()
            .route(
                "/echo",
                post(|b: axum::body::Bytes| async move { b.len().to_string() }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Duration::from_millis(300),
                body_idle_timeout,
            ));
        let base = serve(app).await;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
        tx.send(Ok(bytes::Bytes::from_static(b"abc")))
            .await
            .unwrap();
        let body = reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
        let started = std::time::Instant::now();
        let r = reqwest::Client::new()
            .post(format!("{base}/echo"))
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(!r.status().is_success(), "{}", r.status());
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(tx);
        let r = reqwest::Client::new()
            .post(format!("{base}/echo"))
            .body("abcd")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        assert_eq!(r.text().await.unwrap(), "4");
    }
}
