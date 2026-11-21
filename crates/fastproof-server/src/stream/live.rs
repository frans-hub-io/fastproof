//! Live log fan-out of one replica: lines reach [`LogHub`] from PostgreSQL `LISTEN` (direct
//! mode) or from a reader of `ingest.logs` that starts at the latest offset and never commits
//! (Kafka mode, `fastproof-live-<replica>`; it fetches only while someone follows a launch).
//! Each SSE subscriber has its own queue, bounded in lines and bytes (oldest lines are dropped
//! first and reported as a gap); a subscriber that takes nothing for [`STALL_AFTER`] while its
//! queue overflows is dropped.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fastproof_store::Store;
use fastproof_store::logs::{LOGS_CHANNEL, LogLine, LogNotice};
use fastproof_stream::Kafka;
use fastproof_stream::kafka::{FetchWindow, Fetched, Start, TOPIC_LOGS};
use fastproof_stream::messages::LogRecord;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Lines one subscriber queues, at most.
pub const SUBSCRIBER_LINES: usize = 2000;
/// Bytes one subscriber queues, at most.
pub const SUBSCRIBER_BYTES: usize = 1024 * 1024;
/// A subscriber whose queue overflows and that took nothing for this long is dropped.
pub const STALL_AFTER: Duration = Duration::from_secs(60);

/// Lines of one launch delivered together.
#[derive(Clone, Debug)]
pub struct LogBatch {
    pub project: String,
    pub launch_id: Uuid,
    pub lines: Vec<LogLine>,
}

/// Bytes a queued line is counted for.
pub fn line_bytes(l: &LogLine) -> usize {
    l.message.len() + l.step.as_ref().map_or(0, |s| s.len()) + l.result_id.len() + 64
}

type Filter = Box<dyn Fn(&LogLine) -> bool + Send + Sync>;

struct Queue {
    lines: VecDeque<LogLine>,
    bytes: usize,
    dropped: u64,
    last_take: Instant,
}

/// One subscriber: the launch it follows, its filter and its bounded queue.
pub struct LogSub {
    id: u64,
    project: String,
    launch: Uuid,
    filter: Filter,
    queue: Mutex<Queue>,
    closed: AtomicBool,
    /// Released when the subscriber is dropped as stalled (stream slots and the like).
    held: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

impl LogSub {
    /// Keeps `v` until the subscription ends or is dropped as stalled.
    pub fn hold(&self, v: Box<dyn std::any::Any + Send>) {
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
    }

    /// True once the hub dropped this subscriber.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Lines dropped since the last call, then up to `max_lines` lines and about `max_bytes`
    /// bytes (at least one line) from the queue.
    pub fn take(&self, max_lines: usize, max_bytes: usize) -> (u64, Vec<LogLine>) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.last_take = Instant::now();
        let dropped = std::mem::take(&mut q.dropped);
        let mut out = Vec::new();
        let mut bytes = 0;
        while out.len() < max_lines {
            let Some(l) = q.lines.front() else { break };
            let b = line_bytes(l);
            if !out.is_empty() && bytes + b > max_bytes {
                break;
            }
            bytes += b;
            q.bytes -= b;
            if let Some(l) = q.lines.pop_front() {
                out.push(l);
            }
        }
        (dropped, out)
    }

    /// Queues the wanted lines; false when the subscriber stalled and must be dropped.
    fn push(&self, lines: &[LogLine]) -> bool {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let mut overflow = false;
        for l in lines {
            if !(self.filter)(l) {
                continue;
            }
            q.bytes += line_bytes(l);
            q.lines.push_back(l.clone());
            while q.lines.len() > SUBSCRIBER_LINES || q.bytes > SUBSCRIBER_BYTES {
                let Some(old) = q.lines.pop_front() else {
                    break;
                };
                q.bytes -= line_bytes(&old);
                q.dropped += 1;
                overflow = true;
            }
        }
        !(overflow && q.last_take.elapsed() > STALL_AFTER)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.lines.clear();
        q.bytes = 0;
        drop(q);
        self.held.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

#[derive(Default)]
struct Hub {
    subs: Mutex<HashMap<u64, Arc<LogSub>>>,
    next: AtomicU64,
}

/// The in-process side of live logs.
#[derive(Clone, Default)]
pub struct LogHub {
    hub: Arc<Hub>,
}

/// A registered subscriber; unregisters when dropped.
pub struct Subscription {
    pub sub: Arc<LogSub>,
    hub: Arc<Hub>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.sub.id);
    }
}

impl LogHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Follows the lines of one launch that pass `filter`.
    pub fn subscribe(
        &self,
        project: &str,
        launch: Uuid,
        filter: impl Fn(&LogLine) -> bool + Send + Sync + 'static,
    ) -> Subscription {
        let sub = Arc::new(LogSub {
            id: self.hub.next.fetch_add(1, Ordering::Relaxed),
            project: project.to_string(),
            launch,
            filter: Box::new(filter),
            queue: Mutex::new(Queue {
                lines: VecDeque::new(),
                bytes: 0,
                dropped: 0,
                last_take: Instant::now(),
            }),
            closed: AtomicBool::new(false),
            held: Mutex::new(None),
        });
        self.hub
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sub.id, sub.clone());
        Subscription {
            sub,
            hub: self.hub.clone(),
        }
    }

    /// Registered subscribers.
    pub fn subscribers(&self) -> usize {
        self.hub
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Launches with at least one subscriber.
    pub fn launches(&self) -> HashSet<Uuid> {
        self.hub
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|s| s.launch)
            .collect()
    }

    pub fn publish(&self, b: LogBatch) {
        if b.lines.is_empty() {
            return;
        }
        let targets: Vec<Arc<LogSub>> = self
            .hub
            .subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|s| s.launch == b.launch_id && s.project == b.project)
            .cloned()
            .collect();
        for sub in targets {
            if !sub.push(&b.lines) {
                metrics::counter!("fastproof_log_stream_stalled_total").increment(1);
                self.hub
                    .subs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&sub.id);
                sub.close();
            }
        }
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

/// How often a partition reader without subscribers moves to the end of the log.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// The launch a record is keyed by (the gateway keys records by launch id).
fn record_launch(r: &fastproof_stream::kafka::RecordAndOffset) -> Option<Uuid> {
    std::str::from_utf8(r.record.key.as_deref()?)
        .ok()?
        .parse()
        .ok()
}

/// Kafka mode: reads every partition of `ingest.logs` from the latest offset without a group or
/// commits, and publishes the lines of this replica's subscribers. Without subscribers a reader
/// only follows the end of the log; with them it fetches within the process budget and decodes
/// only the records of followed launches.
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
            let mut window = FetchWindow::new(kafka.config().fetch_max_bytes);
            loop {
                if stop.is_cancelled() {
                    return;
                }
                let wanted = hub.launches();
                if wanted.is_empty() || offset.is_none() {
                    match kafka.offset(&topic, p, Start::Latest).await {
                        Ok(o) => offset = Some(o),
                        Err(e) => {
                            tracing::warn!(partition = p, error = %format!("{e:#}"), "live log reader: offset failed");
                            offset = None;
                            tokio::select! {
                                _ = stop.cancelled() => return,
                                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                            }
                            continue;
                        }
                    }
                    if wanted.is_empty() {
                        tokio::select! {
                            _ = stop.cancelled() => return,
                            _ = tokio::time::sleep(IDLE_POLL) => {}
                        }
                        continue;
                    }
                }
                let Some(o) = offset else { continue };
                let fetched = tokio::select! {
                    _ = stop.cancelled() => return,
                    r = kafka.fetch_bounded(&topic, p, o, &mut window, 500) => r,
                };
                let Fetched {
                    records,
                    from,
                    reservation,
                    ..
                } = match fetched {
                    Ok(f) => f,
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
                    next = r.offset + 1;
                    if record_launch(&r).is_some_and(|l| !wanted.contains(&l)) {
                        continue;
                    }
                    if !trusted && !kafka.signer().is_some_and(|s| s.verify(&topic, &r.record)) {
                        continue;
                    }
                    let Some(Ok(rec)) = r.record.value.as_deref().map(serde_json::from_slice::<LogRecord>) else {
                        continue;
                    };
                    if !wanted.contains(&rec.launch_id) {
                        continue;
                    }
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
                drop(reservation);
                offset = Some(next);
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastproof_store::logs::{LogLevel, LogStream};

    fn line(seq: u64, n: usize) -> LogLine {
        LogLine {
            result_id: "r".into(),
            attempt: 0,
            seq,
            ts: 0,
            level: LogLevel::Info,
            stream: LogStream::Stdout,
            message: "x".repeat(n),
            step: None,
        }
    }

    #[test]
    fn queues_are_bounded_and_stalled_subscribers_dropped() {
        let hub = LogHub::new();
        let launch = Uuid::now_v7();
        let s = hub.subscribe("p", launch, |l| l.seq % 2 == 0);
        let other = hub.subscribe("p", Uuid::now_v7(), |_| true);
        hub.publish(LogBatch {
            project: "p".into(),
            launch_id: launch,
            lines: (0..100).map(|i| line(i, 64 * 1024)).collect(),
        });
        let (dropped, lines) = s.sub.take(10_000, usize::MAX);
        assert!(lines.iter().all(|l| l.seq % 2 == 0));
        assert!(lines.iter().map(line_bytes).sum::<usize>() <= SUBSCRIBER_BYTES);
        assert_eq!(dropped + lines.len() as u64, 50);
        assert!(other.sub.take(10, 1000).1.is_empty());
        let (_, few) = {
            hub.publish(LogBatch {
                project: "p".into(),
                launch_id: launch,
                lines: (0..10).map(|i| line(i * 2, 100)).collect(),
            });
            s.sub.take(3, usize::MAX)
        };
        assert_eq!(few.len(), 3);
        s.sub.queue.lock().unwrap().last_take = Instant::now() - STALL_AFTER * 2;
        hub.publish(LogBatch {
            project: "p".into(),
            launch_id: launch,
            lines: (0..100).map(|i| line(i * 2, 64 * 1024)).collect(),
        });
        assert!(s.sub.is_closed());
        assert_eq!(hub.subscribers(), 1);
        drop(s);
        drop(other);
        assert_eq!(hub.subscribers(), 0);
    }
}
