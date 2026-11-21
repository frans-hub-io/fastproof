//! Live log fan-out of one replica: lines reach [`LogHub`] from PostgreSQL `LISTEN` (direct
//! mode) or from a reader of `ingest.logs` that starts at the latest offset and never commits
//! (Kafka mode, `fastproof-live-<replica>`), and SSE subscribers filter them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fastproof_store::Store;
use fastproof_store::logs::{LOGS_CHANNEL, LogLine, LogNotice};
use fastproof_stream::Kafka;
use fastproof_stream::kafka::{Start, TOPIC_LOGS};
use fastproof_stream::messages::LogRecord;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Lines of one launch delivered together.
#[derive(Clone, Debug)]
pub struct LogBatch {
    pub project: String,
    pub launch_id: Uuid,
    pub lines: Vec<LogLine>,
}

/// The in-process side of live logs.
#[derive(Clone)]
pub struct LogHub {
    tx: broadcast::Sender<Arc<LogBatch>>,
}

impl LogHub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LogBatch>> {
        self.tx.subscribe()
    }

    pub fn publish(&self, b: LogBatch) {
        if !b.lines.is_empty() {
            let _ = self.tx.send(Arc::new(b));
        }
    }
}

impl Default for LogHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Direct mode: `LISTEN` on the logs channel; notices without inline lines are read back.
pub async fn listen_pg(pool: PgPool, store: Store, hub: LogHub, stop: CancellationToken) {
    let mut backoff = Duration::from_millis(250);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let mut listener = match PgListener::connect_with(&pool).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "connecting the log listener failed; retrying");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
                continue;
            }
        };
        if let Err(e) = listener.listen(LOGS_CHANNEL).await {
            tracing::warn!(error = %e, "LISTEN on the log channel failed; retrying");
            continue;
        }
        backoff = Duration::from_millis(250);
        loop {
            let n = tokio::select! {
                _ = stop.cancelled() => return,
                n = listener.try_recv() => n,
            };
            match n {
                Ok(Some(n)) => {
                    let Ok(notice) = serde_json::from_str::<LogNotice>(n.payload()) else {
                        continue;
                    };
                    let lines = match notice.lines {
                        Some(l) => l,
                        None => match store.log_chunks(&[notice.chunk_id]).await {
                            Ok(mut c) => c.pop().map(|c| c.lines).unwrap_or_default(),
                            Err(e) => {
                                tracing::warn!(error = %format!("{e:#}"), "reading a log chunk failed");
                                continue;
                            }
                        },
                    };
                    hub.publish(LogBatch {
                        project: notice.project,
                        launch_id: notice.launch_id,
                        lines,
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "log listener failed; reconnecting");
                    break;
                }
            }
        }
    }
}

/// Kafka mode: reads every partition of `ingest.logs` from the latest offset without a group or
/// commits, and publishes the lines of this replica's subscribers.
pub async fn listen_kafka(kafka: Arc<Kafka>, hub: LogHub, stop: CancellationToken) {
    let topic = kafka.topic(TOPIC_LOGS);
    let trusted = kafka.config().trusted_producers;
    let n = loop {
        match kafka.partition_count(&topic).await {
            Ok(n) => break n,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "live log reader: topic not ready");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        }
    };
    let mut tasks = Vec::new();
    for p in 0..n {
        let kafka = kafka.clone();
        let hub = hub.clone();
        let stop = stop.clone();
        let topic = topic.clone();
        tasks.push(tokio::spawn(async move {
            let mut offset: Option<i64> = None;
            loop {
                if stop.is_cancelled() {
                    return;
                }
                let o = match offset {
                    Some(o) => o,
                    None => match kafka.offset(&topic, p, Start::Latest).await {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::warn!(partition = p, error = %format!("{e:#}"), "live log reader: offset failed");
                            tokio::select! {
                                _ = stop.cancelled() => return,
                                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                            }
                            continue;
                        }
                    },
                };
                let fetched = tokio::select! {
                    _ = stop.cancelled() => return,
                    r = kafka.fetch(&topic, p, o, 4 * 1024 * 1024, 500) => r,
                };
                let (records, _hwm, from) = match fetched {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(partition = p, error = %format!("{e:#}"), "live log reader: fetch failed");
                        offset = None;
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                        continue;
                    }
                };
                let mut next = from;
                let mut by_launch: BTreeMap<(String, Uuid), Vec<LogLine>> = BTreeMap::new();
                for r in records {
                    if r.offset < from {
                        continue;
                    }
                    next = r.offset + 1;
                    if !trusted && !kafka.signer().is_some_and(|s| s.verify(&topic, &r.record)) {
                        continue;
                    }
                    let Some(Ok(rec)) = r.record.value.as_deref().map(serde_json::from_slice::<LogRecord>) else {
                        continue;
                    };
                    by_launch
                        .entry((rec.project.clone(), rec.launch_id))
                        .or_default()
                        .push(rec.into_line());
                }
                for ((project, launch_id), lines) in by_launch {
                    hub.publish(LogBatch {
                        project,
                        launch_id,
                        lines,
                    });
                }
                offset = Some(next);
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}
