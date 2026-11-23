//! The writer (`fastproof-writer`): persists `ingest.results` and `ingest.logs` to PostgreSQL in
//! batches bounded in records and bytes by the consumer. The consumer offset is committed in
//! the same transaction as the last rows of a batch; rows are idempotent by client ids, which
//! absorbs a batch replayed after a stop half way and duplicate records from producer retries.
//! Transactions that hit a deadlock or serialization failure run again. Raw records are
//! dropped as they are decoded.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use fastproof_store::ingest::{
    NewAttempt, TX_ATTEMPTS, ensure_launch_partition, launch_maybe_gone, reserve_attempt_seq,
    retry_pause, retryable,
};
use fastproof_store::logs::LogLine;
use fastproof_store::stream::WriterLaunch;
use fastproof_stream::Kafka;
use fastproof_stream::consumer::BatchHandler;
use fastproof_stream::groups::PartitionCtx;
use fastproof_stream::kafka::RecordAndOffset;
use fastproof_stream::kafka::TOPIC_LOGS;
use fastproof_stream::messages::{LogRecord, ResultRecord};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::state::AppState;

/// Attempts of one launch and upload, with the attachments they reference.
type Work = (WriterLaunch, Option<Uuid>, Vec<NewAttempt>, Vec<Uuid>);

/// Writer of one process (shared by its partitions).
pub struct Writer {
    s: AppState,
    kafka: Arc<Kafka>,
    launches: Mutex<HashMap<(String, Uuid), Option<WriterLaunch>>>,
}

fn rejected(topic: &str, reason: &'static str) {
    metrics::counter!("fastproof_kafka_rejected_total", "topic" => topic.to_string(), "reason" => reason).increment(1);
}

impl Writer {
    pub fn new(s: AppState, kafka: Arc<Kafka>) -> Self {
        Self {
            s,
            kafka,
            launches: Mutex::new(HashMap::new()),
        }
    }

    /// The value of a record and whether FastProof signed it (the gateway or the upload
    /// workers); unsigned records only when producers are trusted. Unsigned records are
    /// validated and charged like gateway input.
    fn value<'a>(&self, topic: &str, r: &'a RecordAndOffset) -> Option<(&'a [u8], bool)> {
        let signed = self
            .kafka
            .signer()
            .is_some_and(|s| s.verify(topic, &r.record));
        if !signed && !self.kafka.config().trusted_producers {
            rejected(topic, "unsigned");
            return None;
        }
        r.record.value.as_deref().map(|v| (v, signed))
    }

    async fn launch(&self, project: &str, id: Uuid) -> anyhow::Result<Option<WriterLaunch>> {
        let key = (project.to_string(), id);
        if let Some(l) = self.launches.lock().await.get(&key) {
            return Ok(l.clone());
        }
        let l = self.s.store.writer_launch(project, id).await?;
        let mut cache = self.launches.lock().await;
        if cache.len() > 10_000 {
            cache.clear();
        }
        if l.is_some() {
            cache.insert(key, l.clone());
        }
        Ok(l)
    }

    /// After an error that may mean a launch was deleted: forgets the cached launches and
    /// returns those of `ids` that no longer exist. Their records are dropped (`unknown_launch`)
    /// instead of failing the batch again and again, which would stop the partition.
    async fn gone(&self, topic: &str, ids: &[(String, Uuid)]) -> anyhow::Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for (project, id) in ids {
            self.launches.lock().await.remove(&(project.clone(), *id));
            if self.s.store.writer_launch(project, *id).await?.is_none() {
                tracing::info!(launch = %id, "launch deleted while its records were written; dropping them");
                rejected(topic, "unknown_launch");
                out.push(*id);
            }
        }
        Ok(out)
    }

    async fn results(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        let mut by_launch: BTreeMap<(String, Uuid), Vec<ResultRecord>> = BTreeMap::new();
        for r in records {
            let Some((v, signed)) = self.value(&ctx.topic, &r) else {
                continue;
            };
            match serde_json::from_slice::<ResultRecord>(v) {
                Ok(rec) if rec.v == 1 && !signed && rec.result.is_none() => {
                    rejected(&ctx.topic, "unsigned_parsed")
                }
                Ok(rec) if rec.v == 1 => by_launch
                    .entry((rec.project.clone(), rec.launch_id))
                    .or_default()
                    .push(rec),
                _ => rejected(&ctx.topic, "invalid"),
            }
        }
        let mut work: Vec<Work> = Vec::new();
        for ((project, launch_id), recs) in by_launch {
            let Some(wl) = self.launch(&project, launch_id).await? else {
                rejected(&ctx.topic, "unknown_launch");
                continue;
            };
            let mut reports = Vec::new();
            let mut by_upload: BTreeMap<Option<Uuid>, Vec<NewAttempt>> = BTreeMap::new();
            for (i, rec) in recs.into_iter().enumerate() {
                match (rec.result, rec.parsed, rec.client_key) {
                    (Some(r), _, _) => reports.push((i as u64, r)),
                    (None, Some(p), Some(k)) if !k.is_empty() => by_upload
                        .entry(rec.upload_id)
                        .or_default()
                        .push(NewAttempt {
                            client_key: k,
                            result: *p,
                        }),
                    _ => rejected(&ctx.topic, "invalid"),
                }
            }
            let mut used = Vec::new();
            if !reports.is_empty() {
                let prep = crate::api::results::prepare_reports(
                    &self.s,
                    wl.launch.project_id,
                    wl.env_key.as_deref(),
                    reports,
                )
                .await?;
                for r in &prep.rejected {
                    tracing::debug!(launch = %launch_id, message = %r.message, "result rejected by the writer");
                    rejected(&ctx.topic, "invalid_result");
                }
                used = prep.used;
                by_upload
                    .entry(None)
                    .or_default()
                    .extend(prep.accepted.into_iter().map(|(_, _, a)| a));
            }
            for (upload, attempts) in by_upload {
                work.push((wl.clone(), upload, attempts, std::mem::take(&mut used)));
            }
        }
        let mut dropped: Vec<Uuid> = Vec::new();
        for (wl, _, _, used) in &work {
            if !used.is_empty()
                && let Err(e) = self.s.store.link_attachments(used, wl.launch.id).await
            {
                let key = (wl.launch.project_key.clone(), wl.launch.id);
                if !launch_maybe_gone(&e) || self.gone(&ctx.topic, &[key]).await?.is_empty() {
                    return Err(e);
                }
                dropped.push(wl.launch.id);
            }
        }
        work.retain(|w| !dropped.contains(&w.0.launch.id));
        // One transaction per launch (and upload), the last one with the offset: a transaction
        // that touched several launches would take the shared test case and series locks of
        // one launch after the per-series locks of another and could deadlock with other
        // partitions. A batch replayed after a crash between them is absorbed by client keys.
        let mut accepted = 0;
        let last = work.len();
        let mut committed = false;
        for (i, (wl, upload, attempts, _)) in work.into_iter().enumerate() {
            let offset = (i + 1 == last).then_some((next, hwm));
            match self.persist(ctx, &wl, upload, attempts, offset).await {
                Ok(n) => {
                    accepted += n;
                    committed |= offset.is_some();
                }
                Err(e) if launch_maybe_gone(&e) => {
                    let key = (wl.launch.project_key.clone(), wl.launch.id);
                    if self.gone(&ctx.topic, &[key]).await?.is_empty() {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if !committed {
            ctx.commit(next, Some(hwm)).await?;
        }
        metrics::counter!("fastproof_results_ingested_total", "source" => "kafka")
            .increment(accepted);
        Ok(())
    }

    /// Persists the attempts of one launch, committing the offset too when given; retried as a
    /// whole after a deadlock or serialization failure.
    async fn persist(
        &self,
        ctx: &PartitionCtx,
        wl: &WriterLaunch,
        upload: Option<Uuid>,
        attempts: Vec<NewAttempt>,
        offset: Option<(i64, i64)>,
    ) -> anyhow::Result<u64> {
        let mut conn = self.s.store.pool().acquire().await?;
        ensure_launch_partition(&mut conn, wl.launch.created_at).await?;
        let seq0 = reserve_attempt_seq(&mut conn, wl.launch.id, attempts.len()).await?;
        let mut n = 0;
        loop {
            n += 1;
            let r = async {
                let mut tx = sqlx::Connection::begin(&mut *conn).await?;
                let out = fastproof_store::ingest::persist_attempts_in(
                    &mut tx,
                    &wl.launch,
                    upload,
                    attempts.clone(),
                    seq0,
                )
                .await?;
                if let Some((next, hwm)) = offset {
                    ctx.commit_in(&mut tx, next, Some(hwm)).await?;
                }
                tx.commit().await?;
                Ok::<_, anyhow::Error>(out.accepted)
            }
            .await;
            if let Ok(n) = &r
                && *n > 0
            {
                // After the commit, outside the transaction: see `persist_attempts_in`.
                if let Err(e) = fastproof_store::events::notify_launch(
                    &mut conn,
                    &wl.launch.project_key,
                    wl.launch.id,
                )
                .await
                {
                    tracing::debug!(error = %format!("{e:#}"), "live notification failed");
                }
            }
            match r {
                Err(e) if n < TX_ATTEMPTS && retryable(&e).is_some() => {
                    retry_pause("writer", &e, n).await
                }
                r => return r,
            }
        }
    }

    async fn logs(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        let mut by_launch: BTreeMap<(String, Uuid), (Vec<LogLine>, Vec<LogLine>)> = BTreeMap::new();
        let max_line = self.s.cfg.log_limits.max_line_bytes;
        for r in records {
            let Some((v, signed)) = self.value(&ctx.topic, &r) else {
                continue;
            };
            match serde_json::from_slice::<LogRecord>(v) {
                Ok(rec) if rec.v == 1 => {
                    let e = by_launch
                        .entry((rec.project.clone(), rec.launch_id))
                        .or_default();
                    let mut line = rec.into_line();
                    if signed {
                        e.0.push(line);
                    } else if crate::api::logs::validate(&mut line, max_line).is_ok() {
                        e.1.push(line);
                    } else {
                        rejected(&ctx.topic, "invalid_line");
                    }
                }
                _ => rejected(&ctx.topic, "invalid"),
            }
        }
        let mut resolved = Vec::new();
        for ((project, launch_id), (mut lines, unsigned)) in by_launch {
            let Some(wl) = self.launch(&project, launch_id).await? else {
                rejected(&ctx.topic, "unknown_launch");
                continue;
            };
            if !unsigned.is_empty() {
                let bytes: i64 = unsigned
                    .iter()
                    .map(|l| {
                        (l.message.len()
                            + l.step.as_ref().map_or(0, |s| s.len())
                            + l.result_id.len()) as i64
                    })
                    .sum();
                if self
                    .s
                    .store
                    .charge_log_bytes(
                        wl.launch.id,
                        bytes,
                        self.s.cfg.log_limits.max_bytes_per_launch,
                    )
                    .await?
                {
                    lines.extend(unsigned);
                } else {
                    metrics::counter!("fastproof_kafka_rejected_total", "topic" => ctx.topic.clone(), "reason" => "log_budget")
                        .increment(unsigned.len() as u64);
                }
            }
            if !lines.is_empty() {
                resolved.push((wl, lines));
            }
        }
        let mut attempt = 0;
        let n = loop {
            attempt += 1;
            let r = async {
                let mut tx = self.s.store.pool().begin().await?;
                let mut n = 0;
                for (wl, lines) in &resolved {
                    let chunks = fastproof_store::logs::insert_log_lines(
                        &mut tx,
                        wl.launch.project_id,
                        &wl.launch.project_key,
                        wl.launch.id,
                        lines.clone(),
                        self.s.store.log_chunk_events(),
                    )
                    .await?;
                    n += chunks.iter().map(|c| c.lines.len() as u64).sum::<u64>();
                }
                ctx.commit_in(&mut tx, next, Some(hwm)).await?;
                tx.commit().await?;
                Ok::<_, anyhow::Error>(n)
            }
            .await;
            match r {
                Err(e) if attempt < TX_ATTEMPTS && retryable(&e).is_some() => {
                    retry_pause("writer_logs", &e, attempt).await
                }
                Err(e) if launch_maybe_gone(&e) => {
                    let keys: Vec<(String, Uuid)> = resolved
                        .iter()
                        .map(|(wl, _)| (wl.launch.project_key.clone(), wl.launch.id))
                        .collect();
                    let gone = self.gone(&ctx.topic, &keys).await?;
                    if gone.is_empty() {
                        return Err(e);
                    }
                    resolved.retain(|(wl, _)| !gone.contains(&wl.launch.id));
                }
                r => break r?,
            }
        };
        metrics::counter!("fastproof_log_lines_stored_total", "source" => "kafka").increment(n);
        Ok(())
    }
}

#[async_trait]
impl BatchHandler for Writer {
    async fn handle(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next_offset: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        if ctx.topic == self.kafka.topic(TOPIC_LOGS) {
            self.logs(ctx, records, next_offset, hwm).await
        } else {
            self.results(ctx, records, next_offset, hwm).await
        }
    }
}
