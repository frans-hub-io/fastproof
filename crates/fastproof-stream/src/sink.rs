//! Analytics sinks: outbox events (`result.updated`, `attempt.stored`, `log.chunk`,
//! `log.chunks` and, for sinks that keep the triage history, `triage.decided`) are read in
//! batches from PostgreSQL (direct mode) or the `events` topic (Kafka mode), the rows they name
//! are loaded from PostgreSQL and written to ClickHouse or OpenSearch with retries, and the
//! position is committed after each successful batch (at least once; the writes are
//! idempotent by natural keys).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fastproof_model::Step;
use fastproof_store::Store;
use fastproof_store::logs::{LOG_CHUNK_EVENT, LOG_CHUNKS_EVENT, LogChunk, LogHits, LogSearch};
use fastproof_store::stream::{
    SimilarResult, SimilarSource, SinkAttempt, SinkResult, SinkTriage, message_hash,
};
use rskafka::record::RecordAndOffset;
use sha2::Digest;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::SinkTuning;
use crate::consumer::{BatchHandler, OutboxReader};
use crate::groups::{PartitionCtx, PartitionRunner};
use crate::kafka::{Kafka, Outgoing, Signer, TOPIC_EVENTS};
use crate::messages::EventRecord;

/// Log chunk events per batch, at most (each holds up to 1000 lines).
pub const MAX_CHUNKS_PER_BATCH: usize = 200;
/// Write attempts of one batch before the worker restarts from the committed position.
const WRITE_ATTEMPTS: u32 = 6;

/// Rows of one batch.
#[derive(Clone, Debug, Default)]
pub struct SinkBatch {
    pub results: Vec<SinkResult>,
    pub attempts: Vec<SinkAttempt>,
    pub logs: Vec<LogChunk>,
    /// Triage decisions (sinks whose [`Sink::wants_triage`] is true).
    pub triage: Vec<SinkTriage>,
}

impl SinkBatch {
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
            && self.attempts.is_empty()
            && self.logs.is_empty()
            && self.triage.is_empty()
    }

    /// A content hash used as the idempotency token of the batch.
    pub fn token(&self) -> String {
        let mut h = sha2::Sha256::new();
        for r in &self.results {
            h.update(r.id.as_bytes());
            h.update(r.updated_at.timestamp_micros().to_be_bytes());
        }
        for a in &self.attempts {
            h.update(a.id.as_bytes());
            h.update(a.status.as_bytes());
        }
        for c in &self.logs {
            h.update(c.id.to_be_bytes());
        }
        for t in &self.triage {
            h.update(t.decision_id.to_be_bytes());
        }
        hex::encode(&h.finalize()[..16])
    }
}

/// Rows to drop from a sink: deleted launches and deleted projects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deletions {
    /// `(project key, launch id)`.
    pub launches: Vec<(String, Uuid)>,
    /// `(project key, deletion time)`: rows of launches created up to that time (a later
    /// project with the same key keeps its rows).
    pub projects: Vec<(String, chrono::DateTime<chrono::Utc>)>,
}

impl Deletions {
    pub fn is_empty(&self) -> bool {
        self.launches.is_empty() && self.projects.is_empty()
    }
}

/// A destination of the outbox.
#[async_trait]
pub trait Sink: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Creates tables, templates and policies (idempotent).
    async fn prepare(&self) -> anyhow::Result<()>;
    async fn write(&self, batch: &SinkBatch, token: &str) -> anyhow::Result<()>;
    /// Drops the rows of deleted launches and projects (idempotent).
    async fn delete(&self, _d: &Deletions) -> anyhow::Result<()> {
        Ok(())
    }
    /// Whether the sink keeps the triage history: `triage.decided` events then load the
    /// batch's decisions and write its results again with their new defect.
    fn wants_triage(&self) -> bool {
        false
    }
}

/// Log search and similar failures served by an analytics backend.
#[async_trait]
pub trait SearchBackend: Send + Sync + 'static {
    async fn search_logs(&self, project_key: &str, s: &LogSearch) -> anyhow::Result<LogHits>;
    async fn similar(
        &self,
        project_key: &str,
        src: &SimilarSource,
        limit: i64,
    ) -> anyhow::Result<Vec<SimilarResult>>;
    async fn ping(&self) -> anyhow::Result<()>;
}

/// The key similar failures are matched on: the fingerprint, or the message hash while results
/// carry none.
pub fn match_key(fingerprint: Option<&str>, message: &str) -> String {
    match fingerprint.filter(|f| !f.is_empty()) {
        Some(f) => format!("f:{f}"),
        None if message.trim().is_empty() => String::new(),
        None => format!("h:{}", message_hash(message)),
    }
}

/// A step of a flattened step tree.
#[derive(Clone, Debug)]
pub struct FlatStep {
    pub idx: u32,
    pub parent: i32,
    pub depth: u16,
    pub name: String,
    pub status: &'static str,
    pub duration_ms: Option<i64>,
    pub message: String,
}

/// Steps in depth-first order with parent indexes (at most 10 000 per attempt).
pub fn flatten_steps(steps: &[Step]) -> Vec<FlatStep> {
    fn walk(steps: &[Step], parent: i32, depth: u16, out: &mut Vec<FlatStep>) {
        for s in steps {
            if out.len() >= 10_000 {
                return;
            }
            let idx = out.len() as u32;
            out.push(FlatStep {
                idx,
                parent,
                depth,
                name: s.name.clone(),
                status: s.status.as_str(),
                duration_ms: s.duration_ms.or(match (s.start, s.stop) {
                    (Some(a), Some(b)) if b >= a => Some(b - a),
                    _ => None,
                }),
                message: s.status_details.message.clone().unwrap_or_default(),
            });
            walk(&s.steps, idx as i32, depth.saturating_add(1), out);
        }
    }
    let mut out = Vec::new();
    walk(steps, -1, 0, &mut out);
    out
}

/// Log chunks an event names: one for a `log.chunk` event (format 1), those listed in a
/// `log.chunks` event (format 2).
pub fn chunk_count(e: &EventRecord) -> usize {
    match e.kind.as_str() {
        LOG_CHUNK_EVENT => 1,
        LOG_CHUNKS_EVENT => e
            .payload
            .get("chunks")
            .and_then(|c| c.as_array())
            .map_or(0, |c| c.len()),
        _ => 0,
    }
}

/// What a batch of events names: result ids, attempt ids, log chunk ids and triage batches
/// (`(project key, batch id)` of `triage.decided`).
struct Named {
    results: Vec<Uuid>,
    attempts: Vec<Uuid>,
    chunks: Vec<i64>,
    triage: Vec<(String, Uuid)>,
}

/// Decisions loaded and written at once, at most (with the results they name).
pub const SINK_TRIAGE: i64 = 1000;

fn ids_of(events: &[EventRecord]) -> Named {
    let mut results = BTreeSet::new();
    let mut attempts = BTreeSet::new();
    let mut chunks = BTreeSet::new();
    let mut triage = BTreeSet::new();
    for e in events {
        let id = e.payload.get("id");
        match e.kind.as_str() {
            "result.updated" | "result.stored" => {
                if let Some(u) = id.and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
                    results.insert(u);
                }
            }
            "attempt.stored" => {
                if let Some(u) = id.and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
                    attempts.insert(u);
                }
            }
            LOG_CHUNK_EVENT => {
                if let Some(c) = id.and_then(|v| v.as_i64()) {
                    chunks.insert(c);
                }
            }
            LOG_CHUNKS_EVENT => {
                if let Some(list) = e.payload.get("chunks").and_then(|c| c.as_array()) {
                    chunks.extend(list.iter().filter_map(|c| c.get("id")?.as_i64()));
                }
            }
            "triage.decided" => {
                if let (Some(p), Some(b)) = (
                    e.project.clone(),
                    e.payload
                        .get("batchId")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Uuid>().ok()),
                ) {
                    triage.insert((p, b));
                }
            }
            _ => {}
        }
    }
    Named {
        results: results.into_iter().collect(),
        attempts: attempts.into_iter().collect(),
        chunks: chunks.into_iter().collect(),
        triage: triage.into_iter().collect(),
    }
}

/// The deletions the events name, checked against PostgreSQL: a launch must be gone and a
/// project marked deleted, so a forged or replayed event cannot drop live data.
pub async fn load_deletions(store: &Store, events: &[EventRecord]) -> anyhow::Result<Deletions> {
    let mut launches = Vec::new();
    let mut projects = Vec::new();
    for e in events {
        let Some(project) = e.project.clone() else {
            continue;
        };
        match e.kind.as_str() {
            "launch.deleted" => {
                if let Some(id) = e
                    .payload
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<Uuid>().ok())
                {
                    launches.push((project, id));
                }
            }
            "project.deleted" => {
                let id = e
                    .payload
                    .get("projectId")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<Uuid>().ok());
                let at = e.payload.get("deletedAt").and_then(|v| {
                    serde_json::from_value::<chrono::DateTime<chrono::Utc>>(v.clone()).ok()
                });
                if let (Some(id), Some(at)) = (id, at) {
                    projects.push((project, id, at));
                }
            }
            _ => {}
        }
    }
    let mut d = Deletions::default();
    if !launches.is_empty() {
        let ids: Vec<Uuid> = launches.iter().map(|l| l.1).collect();
        let live: Vec<Uuid> = sqlx::query_scalar("select id from launches where id = any($1)")
            .bind(&ids)
            .fetch_all(store.pool())
            .await?;
        d.launches = launches
            .into_iter()
            .filter(|l| !live.contains(&l.1))
            .collect();
    }
    for (key, id, at) in projects {
        let live: bool = sqlx::query_scalar(
            "select exists (select 1 from projects where id = $1 and deleted_at is null) \
             or exists (select 1 from projects where key = $2 and deleted_at is null and created_at <= $3)",
        )
        .bind(id)
        .bind(&key)
        .bind(at)
        .fetch_one(store.pool())
        .await?;
        if !live {
            d.projects.push((key, at));
        }
    }
    Ok(d)
}

/// Loads the rows the events name (rows deleted since are skipped).
pub async fn load_batch(store: &Store, events: &[EventRecord]) -> anyhow::Result<SinkBatch> {
    let n = ids_of(events);
    Ok(SinkBatch {
        results: store.sink_results(&n.results).await?,
        attempts: store.sink_attempts(&n.attempts).await?,
        logs: store.log_chunks(&n.chunks).await?,
        triage: store.sink_triage_rows(&n.triage, 0, 10_000).await?,
    })
}

/// Results loaded and written at once, at most.
pub const SINK_RESULTS: usize = 1000;
/// Attempts (each with its steps) loaded and written at once, at most.
pub const SINK_ATTEMPTS: usize = 250;
/// Stored log bytes loaded and written at once, about (a larger chunk goes alone).
pub const SINK_LOG_BYTES: i64 = 4 * 1024 * 1024;
/// Batches one sink writes at once in a process.
pub const SINK_WRITERS: usize = 2;

/// Groups chunk ids into runs of about `max` stored bytes, keeping their order.
pub fn chunk_groups(sizes: &[(i64, i64)], max: i64) -> Vec<Vec<i64>> {
    let mut out: Vec<Vec<i64>> = Vec::new();
    let mut bytes = 0;
    for (id, b) in sizes {
        let b = (*b).max(1);
        match out.last_mut() {
            Some(g) if bytes + b <= max => g.push(*id),
            _ => {
                out.push(vec![*id]);
                bytes = 0;
            }
        }
        bytes += b;
    }
    out
}

#[derive(Default)]
struct Written {
    results: u64,
    attempts: u64,
    lines: u64,
    decisions: u64,
}

/// Loads and writes the rows of `events` in parts bounded in rows and bytes, then applies the
/// deletions they name.
async fn write_parts(
    store: &Store,
    sink: &dyn Sink,
    events: &[EventRecord],
) -> anyhow::Result<Written> {
    let Named {
        results,
        attempts,
        chunks,
        triage,
    } = ids_of(events);
    let mut w = Written::default();
    for ids in results.chunks(SINK_RESULTS) {
        let b = SinkBatch {
            results: store.sink_results(ids).await?,
            ..SinkBatch::default()
        };
        if !b.is_empty() {
            sink.write(&b, &b.token()).await?;
            w.results += b.results.len() as u64;
        }
    }
    for ids in attempts.chunks(SINK_ATTEMPTS) {
        let b = SinkBatch {
            attempts: store.sink_attempts(ids).await?,
            ..SinkBatch::default()
        };
        if !b.is_empty() {
            sink.write(&b, &b.token()).await?;
            w.attempts += b.attempts.len() as u64;
        }
    }
    let sizes = store.log_chunk_sizes(&chunks).await?;
    for ids in chunk_groups(&sizes, SINK_LOG_BYTES) {
        let b = SinkBatch {
            logs: store.log_chunks(&ids).await?,
            ..SinkBatch::default()
        };
        if !b.is_empty() {
            sink.write(&b, &b.token()).await?;
            w.lines += b.logs.iter().map(|c| c.lines.len() as u64).sum::<u64>();
        }
    }
    if sink.wants_triage() {
        for batches in triage.chunks(100) {
            let mut after = 0;
            loop {
                let rows = store.sink_triage_rows(batches, after, SINK_TRIAGE).await?;
                let Some(last) = rows.last().map(|r| r.decision_id) else {
                    break;
                };
                let full = rows.len() as i64 == SINK_TRIAGE;
                after = last;
                w.decisions += write_triage(store, sink, rows).await?;
                if !full {
                    break;
                }
            }
        }
    }
    let deletions = load_deletions(store, events).await?;
    if !deletions.is_empty() {
        sink.delete(&deletions).await?;
        metrics::counter!("fastproof_sink_deletions_total", "sink" => sink.name())
            .increment((deletions.launches.len() + deletions.projects.len()) as u64);
    }
    Ok(w)
}

/// Writes decisions with the current rows of the results they name (their defect columns
/// changed); returns the decisions written.
pub async fn write_triage(
    store: &Store,
    sink: &dyn Sink,
    rows: Vec<SinkTriage>,
) -> anyhow::Result<u64> {
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|r| r.result_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let n = rows.len() as u64;
    let b = SinkBatch {
        results: store.sink_results(&ids).await?,
        triage: rows,
        ..SinkBatch::default()
    };
    if !b.is_empty() {
        sink.write(&b, &b.token()).await?;
    }
    Ok(n)
}

/// Loads and writes a batch, retrying with backoff. Rows are loaded and written in parts of
/// at most [`SINK_RESULTS`] results, [`SINK_ATTEMPTS`] attempts or about [`SINK_LOG_BYTES`] of
/// log lines, so the memory of a write does not depend on the size of the batch.
pub async fn write_batch(
    store: &Store,
    sink: &dyn Sink,
    events: &[EventRecord],
) -> anyhow::Result<()> {
    let mut backoff = Duration::from_millis(500);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let started = Instant::now();
        match write_parts(store, sink, events).await {
            Ok(w) => {
                let name = sink.name();
                metrics::counter!("fastproof_sink_rows_total", "sink" => name, "kind" => "results")
                    .increment(w.results);
                metrics::counter!("fastproof_sink_rows_total", "sink" => name, "kind" => "attempts")
                    .increment(w.attempts);
                metrics::counter!("fastproof_sink_rows_total", "sink" => name, "kind" => "log_lines")
                    .increment(w.lines);
                metrics::counter!("fastproof_sink_rows_total", "sink" => name, "kind" => "triage")
                    .increment(w.decisions);
                metrics::counter!("fastproof_sink_events_total", "sink" => name)
                    .increment(events.len() as u64);
                metrics::histogram!("fastproof_sink_batch_seconds", "sink" => name)
                    .record(started.elapsed().as_secs_f64());
                if let Some(last) = events.iter().map(|e| e.created_at).max() {
                    metrics::gauge!("fastproof_sink_lag_seconds", "sink" => name)
                        .set((chrono::Utc::now() - last).num_milliseconds().max(0) as f64 / 1000.0);
                }
                return Ok(());
            }
            Err(e) => {
                metrics::counter!("fastproof_sink_errors_total", "sink" => sink.name())
                    .increment(1);
                if attempt >= WRITE_ATTEMPTS {
                    return Err(e);
                }
                tracing::warn!(sink = sink.name(), attempt, error = %format!("{e:#}"), "sink write failed; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// Direct mode: a sink reading the PostgreSQL outbox (group member on partition 0 of
/// [`crate::consumer::OUTBOX_TOPIC`]).
pub struct OutboxSink {
    pub store: Store,
    pub sink: Arc<dyn Sink>,
    pub tuning: SinkTuning,
}

#[async_trait]
impl PartitionRunner for OutboxSink {
    async fn run(
        self: Arc<Self>,
        ctx: PartitionCtx,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        let start = ctx.stored_offset().await?.unwrap_or(1);
        let mut reader = OutboxReader::new(self.store.clone(), start - 1);
        let name = self.sink.name();
        loop {
            let mut deadline: Option<Instant> = None;
            let mut events: Vec<EventRecord> = Vec::new();
            let mut chunks = 0;
            loop {
                let want = (self.tuning.batch_size - events.len()).min(1000) as i64;
                let got = reader.next(want).await?;
                let full = got.len() as i64 == want;
                for e in got {
                    reader.advance(e.id);
                    let e = EventRecord::from(e);
                    chunks += chunk_count(&e);
                    events.push(e);
                }
                if !events.is_empty() && deadline.is_none() {
                    deadline = Some(Instant::now() + self.tuning.flush_interval);
                }
                if events.len() >= self.tuning.batch_size
                    || chunks >= MAX_CHUNKS_PER_BATCH
                    || stop.is_cancelled()
                    || deadline.is_some_and(|d| Instant::now() >= d)
                {
                    break;
                }
                if full {
                    continue;
                }
                let nap = deadline
                    .map(|d| d.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::from_millis(200))
                    .min(Duration::from_millis(200));
                tokio::select! {
                    _ = stop.cancelled() => {}
                    _ = tokio::time::sleep(nap) => {}
                }
            }
            if !events.is_empty() {
                write_batch(&self.store, self.sink.as_ref(), &events).await?;
                ctx.commit(reader.last() + 1, None).await?;
            }
            if let Ok(Some(max)) =
                sqlx::query_scalar::<_, Option<i64>>("select max(id) from events")
                    .fetch_one(self.store.pool())
                    .await
            {
                metrics::gauge!("fastproof_sink_lag_events", "sink" => name)
                    .set((max - reader.last()).max(0) as f64);
            }
            if stop.is_cancelled() {
                return Ok(());
            }
        }
    }
}

/// Kafka mode: a sink consuming the `events` topic. Only records the relay signed are used.
/// The partitions of one process share [`SINK_WRITERS`] write slots.
pub struct KafkaSink {
    pub store: Store,
    pub sink: Arc<dyn Sink>,
    pub signer: Signer,
    pub slots: Arc<tokio::sync::Semaphore>,
}

impl KafkaSink {
    pub fn new(store: Store, sink: Arc<dyn Sink>, signer: Signer) -> Self {
        Self {
            store,
            sink,
            signer,
            slots: Arc::new(tokio::sync::Semaphore::new(SINK_WRITERS)),
        }
    }
}

/// Splits events into parts of at most `max_events` events and [`MAX_CHUNKS_PER_BATCH`] log
/// chunk events (each chunk loads up to a thousand lines).
pub fn sink_parts(events: &[EventRecord], max_events: usize) -> Vec<&[EventRecord]> {
    let mut out = Vec::new();
    let (mut start, mut chunks) = (0, 0);
    for (i, e) in events.iter().enumerate() {
        let c = chunk_count(e);
        if i > start && (i - start >= max_events || chunks + c > MAX_CHUNKS_PER_BATCH) {
            out.push(&events[start..i]);
            start = i;
            chunks = 0;
        }
        chunks += c;
    }
    if start < events.len() {
        out.push(&events[start..]);
    }
    out
}

#[async_trait]
impl BatchHandler for KafkaSink {
    async fn handle(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next_offset: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        let mut events = Vec::with_capacity(records.len());
        for r in records {
            if !self.signer.verify(&ctx.topic, &r.record) {
                metrics::counter!("fastproof_kafka_rejected_total", "topic" => ctx.topic.clone(), "reason" => "unsigned")
                    .increment(1);
                continue;
            }
            match r
                .record
                .value
                .as_deref()
                .map(serde_json::from_slice::<EventRecord>)
            {
                Some(Ok(e)) => events.push(e),
                _ => {
                    metrics::counter!("fastproof_kafka_rejected_total", "topic" => ctx.topic.clone(), "reason" => "invalid")
                        .increment(1);
                }
            }
        }
        for part in sink_parts(&events, 1000) {
            let _slot = self.slots.acquire().await?;
            write_batch(&self.store, self.sink.as_ref(), part).await?;
        }
        metrics::gauge!("fastproof_sink_lag_events", "sink" => self.sink.name())
            .set((hwm - next_offset).max(0) as f64);
        ctx.commit(next_offset, Some(hwm)).await
    }
}

/// Kafka mode: copies the PostgreSQL outbox to the `events` topic (at least once; events carry
/// their outbox id).
pub struct OutboxRelay {
    pub store: Store,
    pub kafka: Arc<Kafka>,
}

#[async_trait]
impl PartitionRunner for OutboxRelay {
    async fn run(
        self: Arc<Self>,
        ctx: PartitionCtx,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        let start = ctx.stored_offset().await?.unwrap_or(1);
        let mut reader = OutboxReader::new(self.store.clone(), start - 1);
        let topic = self.kafka.topic(TOPIC_EVENTS);
        loop {
            let got = reader.next(1000).await?;
            if got.is_empty() {
                tokio::select! {
                    _ = stop.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
                continue;
            }
            let last = got.last().map(|e| e.id).unwrap_or(reader.last());
            let mut out = Vec::with_capacity(got.len());
            for e in got {
                let rec = EventRecord::from(e);
                out.push(Outgoing {
                    key: rec.key(),
                    value: serde_json::to_vec(&rec)?,
                });
            }
            self.kafka.produce(&topic, out).await?;
            reader.advance(last);
            ctx.commit(reader.last() + 1, None).await?;
            if stop.is_cancelled() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastproof_model::Status;

    #[test]
    fn every_triage_batch_of_an_event_batch_is_written() {
        let events: Vec<EventRecord> = (0..2500)
            .map(|i| EventRecord {
                v: 1,
                id: i,
                kind: "triage.decided".into(),
                project: Some(if i % 2 == 0 { "a" } else { "b" }.into()),
                payload: serde_json::json!({"batchId": Uuid::now_v7(), "via": "undo"}),
                created_at: chrono::Utc::now(),
            })
            .collect();
        let n = ids_of(&events);
        assert_eq!(n.triage.len(), 2500);
        assert_eq!(n.triage.iter().filter(|t| t.0 == "b").count(), 1250);
    }

    #[test]
    fn steps_flatten_depth_first() {
        let leaf = Step {
            name: "leaf".into(),
            status: Status::Failed,
            duration_ms: Some(5),
            ..Default::default()
        };
        let root = Step {
            name: "root".into(),
            status: Status::Passed,
            start: Some(10),
            stop: Some(30),
            steps: vec![leaf.clone(), leaf],
            ..Default::default()
        };
        let f = flatten_steps(&[root]);
        assert_eq!(f.len(), 3);
        assert_eq!(
            (f[0].parent, f[0].depth, f[0].duration_ms),
            (-1, 0, Some(20))
        );
        assert_eq!((f[2].parent, f[2].depth, f[2].status), (0, 1, "failed"));
        assert_eq!(match_key(None, " "), "");
        assert!(match_key(Some("abc"), "m").starts_with("f:"));
        assert_eq!(match_key(None, "Timeout 5s"), match_key(None, "timeout 7s"));
    }

    fn event(kind: &str) -> EventRecord {
        EventRecord {
            v: 1,
            id: 1,
            kind: kind.into(),
            project: Some("p".into()),
            payload: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn kafka_batches_honour_the_chunk_cap() {
        let events: Vec<EventRecord> = (0..MAX_CHUNKS_PER_BATCH * 10)
            .map(|i| {
                event(if i % 2 == 0 {
                    "log.chunk"
                } else {
                    "result.updated"
                })
            })
            .collect();
        let parts = sink_parts(&events, 1000);
        assert!(parts.iter().all(|p| p.iter().filter(|e| e.kind == "log.chunk").count()
            <= MAX_CHUNKS_PER_BATCH));
        assert!(parts.iter().all(|p| p.len() <= 1000));
        assert_eq!(parts.iter().map(|p| p.len()).sum::<usize>(), events.len());
        assert!(sink_parts(&[], 1000).is_empty());
    }

    #[test]
    fn log_chunks_are_written_in_groups_bounded_by_bytes() {
        let sizes = vec![(1, 100), (2, 300), (3, 50), (4, 1000), (5, 10), (6, 0)];
        assert_eq!(
            chunk_groups(&sizes, 400),
            vec![vec![1, 2], vec![3], vec![4], vec![5, 6]]
        );
        assert!(chunk_groups(&[], 400).is_empty());
        let many: Vec<(i64, i64)> = (0..1000).map(|i| (i, 64 * 1024)).collect();
        let groups = chunk_groups(&many, SINK_LOG_BYTES);
        assert!(
            groups
                .iter()
                .all(|g| g.len() as i64 * 64 * 1024 <= SINK_LOG_BYTES)
        );
        assert_eq!(groups.iter().map(|g| g.len()).sum::<usize>(), 1000);
    }
}
