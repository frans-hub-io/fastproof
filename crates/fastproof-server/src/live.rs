//! Live updates: PostgreSQL `LISTEN` fan-out to the event streams of this replica, and wake-ups
//! of the ingest workers when a job is queued anywhere.

use std::sync::Arc;
use std::time::Duration;

use fastproof_store::events::LiveEvent;
use fastproof_store::{JOBS_CHANNEL, LIVE_CHANNEL};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

/// The in-process side of live updates.
#[derive(Clone)]
pub struct LiveHub {
    tx: broadcast::Sender<Arc<LiveEvent>>,
}

impl LiveHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>> {
        self.tx.subscribe()
    }

    pub fn publish(&self, ev: LiveEvent) {
        let _ = self.tx.send(Arc::new(ev));
    }
}

impl Default for LiveHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Channels of [`listen`].
pub const CHANNELS: &[&str] = &[LIVE_CHANNEL, JOBS_CHANNEL];

/// A dedicated connection that `LISTEN`s on `channels`.
pub async fn subscribe(pool: &PgPool, channels: &[&str]) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen_all(channels.iter().copied()).await?;
    Ok(listener)
}

/// The first subscription of a listener task, made before the server reports that it started,
/// so that notifications committed from then on reach this replica. `None` when it failed: the
/// task then keeps retrying.
pub async fn first_subscription(pool: &PgPool, channels: &[&str]) -> Option<PgListener> {
    match subscribe(pool, channels).await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!(error = %e, ?channels, "LISTEN failed at start; retrying in the background");
            None
        }
    }
}

/// Listens on the live and jobs channels until `stop`, reconnecting with backoff; `first` is a
/// subscription made at start (see [`first_subscription`]).
pub async fn listen(
    pool: PgPool,
    hub: LiveHub,
    wake: Arc<Notify>,
    stop: CancellationToken,
    first: Option<PgListener>,
) {
    let mut first = first;
    let mut backoff = Duration::from_millis(250);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let mut listener = match first.take() {
            Some(l) => l,
            None => match subscribe(&pool, CHANNELS).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(error = %e, "subscribing the notification listener failed; retrying");
                    tokio::select! {
                        _ = stop.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                    continue;
                }
            },
        };
        backoff = Duration::from_millis(250);
        wake.notify_waiters();
        loop {
            let n = tokio::select! {
                _ = stop.cancelled() => return,
                n = listener.try_recv() => n,
            };
            match n {
                Ok(Some(n)) => {
                    if n.channel() == JOBS_CHANNEL {
                        wake.notify_waiters();
                    } else if let Ok(ev) = serde_json::from_str::<LiveEvent>(n.payload()) {
                        hub.publish(ev);
                    }
                }
                Ok(None) => {
                    tracing::warn!("notification listener lost its connection; reconnecting");
                    wake.notify_waiters();
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "notification listener failed; reconnecting");
                    break;
                }
            }
        }
    }
}
