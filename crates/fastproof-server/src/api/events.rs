//! `GET /api/v1/projects/{key}/events`: Server-Sent Events of one project.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::{Stream, StreamExt, stream};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use super::{ErrorBody, P};
use crate::state::AppState;

/// A stream ends after this long; clients reconnect and are authenticated again.
pub const MAX_STREAM: Duration = Duration::from_secs(3600);

#[utoipa::path(get, path = "/api/v1/projects/{key}/events", tag = "events", operation_id = "events_stream",
    params(("key" = String, Path)),
    responses((status = 200, description = "text/event-stream: `launch.updated` {id}, `upload.updated` {launchId, id}; `ready` first, `resync` after missed events", content_type = "text/event-stream"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn stream(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let permit = s
        .sse
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("too many open event streams"))?;
    metrics::gauge!("fastproof_sse_streams").increment(1.0);
    let rx = s.live.subscribe();
    let key = project.key;
    let ready = stream::once(async { Ok(Event::default().event("ready").data("{}")) });
    let events = BroadcastStream::new(rx).filter_map(move |ev| {
        let key = key.clone();
        async move {
            match ev {
                Ok(ev) if ev.project == key => Some(Ok(Event::default()
                    .event(ev.kind.clone())
                    .data(ev.data.to_string()))),
                Ok(_) => None,
                Err(BroadcastStreamRecvError::Lagged(_)) => {
                    Some(Ok(Event::default().event("resync").data("{}")))
                }
            }
        }
    });
    let guard = StreamGuard(Some(permit));
    let body = ready
        .chain(events)
        .take_until(tokio::time::sleep(MAX_STREAM))
        .map(move |e| {
            let _ = &guard;
            e
        });
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
