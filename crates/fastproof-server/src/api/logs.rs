//! Live logs of a launch (`31-streaming.md`): `POST .../logs` (NDJSON or a JSON array of
//! lines, idempotent by `(resultId, attempt, seq)`), `GET .../logs` (stored lines) and
//! `GET .../logs/stream` (Server-Sent Events, batched, drop-oldest with `gap` events).

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use fastproof_store::Page;
use fastproof_store::logs::{LogFilter, LogLevel, LogLine, contains_ci};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::results::Rejected;
use super::uploads::{identity_encoding, too_large};
use super::{ErrorBody, P, Q};
use crate::state::AppState;

/// Bytes of a result id in a log line, at most.
pub const MAX_RESULT_ID_BYTES: usize = 256;
/// Bytes of a step name in a log line, at most.
pub const MAX_STEP_BYTES: usize = 1024;
/// SSE events per second and subscriber, at most.
pub const EVENTS_PER_SECOND: u64 = 20;
/// Lines one subscriber buffers; older lines are dropped first.
pub const SUBSCRIBER_BUFFER: usize = 2000;
/// Lines in one `log` event, at most.
pub const LINES_PER_EVENT: usize = 500;
/// A stream ends after this long; clients reconnect and are authenticated again.
pub const MAX_STREAM: Duration = Duration::from_secs(3600);

/// Data of a `log` event of `GET .../logs/stream`: lines in order.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct LogStreamBatch {
    pub lines: Vec<LogLine>,
}

/// Data of a `gap` event of `GET .../logs/stream`: lines dropped because the client was slow.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct LogGap {
    pub skipped: u64,
}

/// Outcome of `POST .../logs`.
#[derive(Serialize, ToSchema, Default)]
pub struct LogIngestSummary {
    pub accepted: u64,
    /// Lines already stored (direct mode; in Kafka mode the writer drops them).
    pub duplicates: u64,
    pub rejected: Vec<Rejected>,
}

fn validate(l: &LogLine, max_line: usize) -> Result<(), String> {
    let id = l.result_id.trim();
    if id.is_empty() || id.len() > MAX_RESULT_ID_BYTES {
        return Err(format!(
            "resultId must have 1 to {MAX_RESULT_ID_BYTES} bytes"
        ));
    }
    if l.message.len() > max_line {
        return Err(format!("message exceeds {max_line} bytes"));
    }
    if l.step.as_ref().is_some_and(|s| s.len() > MAX_STEP_BYTES) {
        return Err(format!("step exceeds {MAX_STEP_BYTES} bytes"));
    }
    if l.seq > i64::MAX as u64 || l.attempt > 10_000 {
        return Err("seq or attempt out of range".into());
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/logs", tag = "logs",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    request_body(content = Vec<LogLine>, description = "A JSON array of lines or NDJSON (application/x-ndjson), one line per object"),
    responses((status = 202, body = LogIngestSummary), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 415, body = ErrorBody),
        (status = 503, body = ErrorBody)))]
pub async fn ingest(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<LogIngestSummary>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    identity_encoding(&req)?;
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let ndjson = matches!(
        ct.as_str(),
        "application/x-ndjson" | "application/ndjson" | "application/jsonl"
    );
    if !ndjson && ct != "application/json" {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "expected Content-Type application/json or application/x-ndjson",
        ));
    }
    let limits = s.cfg.log_limits;
    let mut body = req.into_body().into_data_stream();
    let mut buf = Vec::new();
    while let Some(c) = body.next().await {
        let c = c.map_err(|e| ApiError::bad_request(format!("reading the body failed: {e}")))?;
        if (buf.len() + c.len()) as u64 > limits.max_request_bytes {
            return Err(too_large("max_request_bytes", limits.max_request_bytes));
        }
        buf.extend_from_slice(&c);
    }
    let mut summary = LogIngestSummary::default();
    let mut lines = Vec::new();
    let mut push =
        |index: u64, v: Result<LogLine, String>, summary: &mut LogIngestSummary| -> ApiResult<()> {
            if (lines.len() + summary.rejected.len()) >= limits.max_lines_per_request {
                return Err(too_large(
                    "max_lines_per_request",
                    limits.max_lines_per_request as u64,
                ));
            }
            match v.and_then(|l| validate(&l, limits.max_line_bytes).map(|_| l)) {
                Ok(l) => lines.push(l),
                Err(message) => summary.rejected.push(Rejected { index, message }),
            }
            Ok(())
        };
    if ndjson {
        for (i, line) in buf.split(|b| *b == b'\n').enumerate() {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let v =
                serde_json::from_slice::<LogLine>(line).map_err(|e| format!("invalid line: {e}"));
            push(i as u64, v, &mut summary)?;
        }
    } else {
        let items: Vec<serde_json::Value> = if buf.iter().all(|b| b.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&buf).map_err(|e| {
                ApiError::bad_request(format!("expected a JSON array of lines: {e}"))
            })?
        };
        if items.len() > limits.max_lines_per_request {
            return Err(too_large(
                "max_lines_per_request",
                limits.max_lines_per_request as u64,
            ));
        }
        for (i, v) in items.into_iter().enumerate() {
            let v = serde_json::from_value::<LogLine>(v).map_err(|e| format!("invalid line: {e}"));
            push(i as u64, v, &mut summary)?;
        }
    }
    drop(buf);
    if lines.is_empty() {
        return Ok((StatusCode::ACCEPTED, Json(summary)));
    }
    let bytes: i64 = lines
        .iter()
        .map(|l| {
            (l.message.len() + l.step.as_ref().map_or(0, |s| s.len()) + l.result_id.len()) as i64
        })
        .sum();
    if !s
        .store
        .charge_log_bytes(launch.id, bytes, limits.max_bytes_per_launch)
        .await?
    {
        return Err(too_large(
            "max_bytes_per_launch",
            limits.max_bytes_per_launch as u64,
        ));
    }
    let n = lines.len() as u64;
    if s.kafka_gateway() {
        crate::stream::produce_logs(&s, &project.key, launch.id, lines).await?;
        summary.accepted = n;
    } else {
        let chunks = s
            .store
            .write_log_lines(project.id, &project.key, launch.id, lines)
            .await?;
        let stored: u64 = chunks.iter().map(|c| c.lines.len() as u64).sum();
        summary.accepted = stored;
        summary.duplicates = n - stored;
    }
    metrics::counter!("fastproof_log_lines_received_total").increment(n);
    Ok((StatusCode::ACCEPTED, Json(summary)))
}

/// Query of stored logs.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LogQuery {
    /// Client id of a result.
    pub result_id: Option<String>,
    pub attempt: Option<u32>,
    /// Lowest level shown: `trace`, `debug`, `info`, `warn` or `error`.
    pub level: Option<String>,
    /// Case-insensitive text in the message.
    pub q: Option<String>,
    /// Cursor from `next`: older lines.
    pub before: Option<String>,
    /// 1..=5000, default 500.
    pub limit: Option<i64>,
}

fn level_of(v: Option<&str>) -> ApiResult<Option<LogLevel>> {
    match v.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => LogLevel::parse(v).map(Some).ok_or_else(|| {
            ApiError::bad_request("level must be trace, debug, info, warn or error")
        }),
    }
}

fn text_filter(q: Option<String>) -> ApiResult<Option<String>> {
    match q.filter(|q| !q.trim().is_empty()) {
        Some(q) if q.len() > 256 => Err(ApiError::bad_request("q has at most 256 bytes")),
        other => Ok(other),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/logs", tag = "logs",
    params(("key" = String, Path), ("id" = Uuid, Path), LogQuery),
    responses((status = 200, body = Page<LogLine>, description = "The newest matching lines before the cursor, oldest first; `next` pages to older lines"),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<LogQuery>,
) -> ApiResult<Json<Page<LogLine>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let limit = match q.limit {
        None => 500,
        Some(n) if (1..=5000).contains(&n) => n,
        Some(_) => return Err(ApiError::bad_request("limit must be 1..=5000")),
    };
    if let Some(b) = &q.before
        && (b.len() > 64
            || b.split_once(':')
                .is_none_or(|(a, c)| a.parse::<i64>().is_err() || c.parse::<i64>().is_err()))
    {
        return Err(ApiError::bad_request("invalid cursor"));
    }
    let f = LogFilter {
        result_id: q.result_id.filter(|r| !r.is_empty()),
        attempt: q.attempt,
        level: level_of(q.level.as_deref())?,
        q: text_filter(q.q)?,
        before: q.before,
        limit,
    };
    Ok(Json(s.store.launch_logs(launch.id, &f).await?))
}

/// Query of the live stream.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct StreamQuery {
    pub result_id: Option<String>,
    pub level: Option<String>,
    pub q: Option<String>,
}

struct Subscriber {
    rx: tokio::sync::broadcast::Receiver<std::sync::Arc<crate::stream::live::LogBatch>>,
    project: String,
    launch: Uuid,
    result_id: Option<String>,
    level: Option<LogLevel>,
    q: Option<String>,
    buf: VecDeque<LogLine>,
    dropped: u64,
    tick: tokio::time::Interval,
    _guard: StreamGuard,
}

impl Subscriber {
    fn wants(&self, l: &LogLine) -> bool {
        if self.result_id.as_ref().is_some_and(|r| *r != l.result_id) {
            return false;
        }
        if self.level.is_some_and(|min| l.level < min) {
            return false;
        }
        self.q.as_deref().is_none_or(|q| contains_ci(&l.message, q))
    }

    fn take(&mut self, l: LogLine) {
        if self.buf.len() >= SUBSCRIBER_BUFFER {
            self.buf.pop_front();
            self.dropped += 1;
        }
        self.buf.push_back(l);
    }

    /// The next event: at most one per tick, a `gap` first when lines were dropped.
    async fn next_event(&mut self) -> Option<Event> {
        loop {
            tokio::select! {
                biased;
                _ = self.tick.tick() => {
                    if self.dropped > 0 {
                        let n = std::mem::take(&mut self.dropped);
                        metrics::counter!("fastproof_log_stream_dropped_total").increment(n);
                        let data = serde_json::to_string(&LogGap { skipped: n }).unwrap_or_default();
                        return Some(Event::default().event("gap").data(data));
                    }
                    if !self.buf.is_empty() {
                        let n = self.buf.len().min(LINES_PER_EVENT);
                        let lines: Vec<LogLine> = self.buf.drain(..n).collect();
                        let data = serde_json::to_string(&LogStreamBatch { lines })
                            .unwrap_or_else(|_| "{\"lines\":[]}".into());
                        return Some(Event::default().event("log").data(data));
                    }
                }
                r = self.rx.recv() => match r {
                    Ok(b) => {
                        if b.launch_id != self.launch || b.project != self.project {
                            continue;
                        }
                        for l in b.lines.iter() {
                            if self.wants(l) {
                                self.take(l.clone());
                            }
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        self.dropped += n.max(1);
                    }
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/logs/stream", tag = "logs",
    params(("key" = String, Path), ("id" = Uuid, Path), StreamQuery),
    responses((status = 200, description = "text/event-stream: `ready` first; `log` events carry `LogStreamBatch` {lines} (at most 20 events per second); `gap` events carry `LogGap` {skipped} when the oldest buffered lines were dropped because the client was slow", content_type = "text/event-stream"),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn stream(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<StreamQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let level = level_of(q.level.as_deref())?;
    let text = text_filter(q.q)?.map(|t| t.to_lowercase());
    let permit = s
        .sse
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("too many open event streams"))?;
    metrics::gauge!("fastproof_sse_streams").increment(1.0);
    let mut tick = tokio::time::interval(Duration::from_millis(1000 / EVENTS_PER_SECOND));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let sub = Subscriber {
        rx: s.logs.subscribe(),
        project: project.key,
        launch: launch.id,
        result_id: q.result_id.filter(|r| !r.is_empty()),
        level,
        q: text,
        buf: VecDeque::new(),
        dropped: 0,
        tick,
        _guard: StreamGuard(Some(permit)),
    };
    let ready = futures::stream::once(async { Ok(Event::default().event("ready").data("{}")) });
    let events = futures::stream::unfold(sub, |mut sub| async move {
        sub.next_event().await.map(|e| (Ok(e), sub))
    });
    let body = ready
        .chain(events)
        .take_until(tokio::time::sleep(MAX_STREAM));
    Ok(Sse::new(body).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

struct StreamGuard(Option<tokio::sync::OwnedSemaphorePermit>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if self.0.take().is_some() {
            metrics::gauge!("fastproof_sse_streams").decrement(1.0);
        }
    }
}
