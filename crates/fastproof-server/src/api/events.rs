//! `GET /api/v1/projects/{key}/events`: Server-Sent Events of one project.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::{Stream, StreamExt, stream};
use tokio::sync::broadcast::error::RecvError;

use super::logs::StreamGuard;
use super::{ErrorBody, P};
use crate::limits::Recheck;
use crate::state::AppState;

/// A stream ends after this long; clients reconnect and are authenticated again.
pub const MAX_STREAM: Duration = Duration::from_secs(3600);

struct Follower {
    rx: tokio::sync::broadcast::Receiver<std::sync::Arc<fastproof_store::events::LiveEvent>>,
    key: String,
    recheck: Recheck,
    check: tokio::time::Interval,
    _guard: StreamGuard,
}

impl Follower {
    /// The next event of the project; `None` when the caller lost access or the hub closed.
    async fn next_event(&mut self) -> Option<Event> {
        loop {
            tokio::select! {
                _ = self.check.tick() => {
                    if !self.recheck.allowed().await {
                        return None;
                    }
                }
                ev = self.rx.recv() => match ev {
                    Ok(ev) if ev.project == self.key => {
                        return Some(Event::default().event(ev.kind.clone()).data(ev.data.to_string()));
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return Some(Event::default().event("resync").data("{}")),
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/events", tag = "events", operation_id = "events_stream",
    params(("key" = String, Path)),
    responses((status = 200, description = "text/event-stream: `launch.updated` {id}, `upload.updated` {launchId, id}; `ready` first, `resync` after missed events. The stream ends when the caller loses access (checked every few minutes)", content_type = "text/event-stream"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn stream(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P(key): P<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let slot = s.guards.stream_slot(&auth)?;
    let permit = s
        .sse
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("too many open event streams"))?;
    metrics::gauge!("fastproof_sse_streams").increment(1.0);
    let recheck = Recheck::new(
        &s,
        &headers,
        &s.perms.launches_read,
        &project.key,
        project.id,
    );
    let every = recheck.interval();
    let mut check = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let f = Follower {
        rx: s.live.subscribe(),
        key: project.key,
        recheck,
        check,
        _guard: StreamGuard(Some(permit), Some(slot)),
    };
    let ready = stream::once(async { Ok(Event::default().event("ready").data("{}")) });
    let events = stream::unfold(f, |mut f| async move {
        f.next_event().await.map(|e| (Ok(e), f))
    });
    let body = ready
        .chain(events)
        .take_until(tokio::time::sleep(MAX_STREAM));
    Ok(Sse::new(body).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
