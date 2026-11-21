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

/// Listens on the live and jobs channels until `stop`, reconnecting with backoff.
pub async fn listen(pool: PgPool, hub: LiveHub, wake: Arc<Notify>, stop: CancellationToken) {
    let mut backoff = Duration::from_millis(250);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "connecting the notification listener failed; retrying");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
                continue;
            }
        };
        if let Err(e) = listener.listen_all([LIVE_CHANNEL, JOBS_CHANNEL]).await {
            tracing::warn!(error = %e, "LISTEN failed; retrying");
            continue;
        }
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
