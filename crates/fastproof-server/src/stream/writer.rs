//! The writer (`fastproof-writer`): persists `ingest.results` and `ingest.logs` to PostgreSQL.
//! One transaction per fetched batch holds the rows and the consumer offset, so a batch is
//! applied exactly once even when a writer stops half way; rows are also idempotent by client
//! ids, which absorbs duplicate records from producer retries.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use fastproof_store::ingest::NewAttempt;
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

    async fn results(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        let mut by_launch: BTreeMap<(String, Uuid), Vec<ResultRecord>> = BTreeMap::new();
        for r in &records {
            let Some((v, signed)) = self.value(&ctx.topic, r) else {
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
        for (wl, _, _, used) in &work {
            if !used.is_empty() {
                self.s.store.link_attachments(used, wl.launch.id).await?;
            }
        }
        let mut tx = self.s.store.pool().begin().await?;
        let mut accepted = 0;
        for (wl, upload, attempts, _) in work {
            let out =
                fastproof_store::ingest::persist_attempts_in(&mut tx, &wl.launch, upload, attempts)
                    .await?;
            accepted += out.accepted;
        }
        ctx.commit_in(&mut tx, next, Some(hwm)).await?;
        tx.commit().await?;
        metrics::counter!("fastproof_results_ingested_total", "source" => "kafka")
            .increment(accepted);
        Ok(())
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
        for r in &records {
            let Some((v, signed)) = self.value(&ctx.topic, r) else {
                continue;
            };
            match serde_json::from_slice::<LogRecord>(v) {
                Ok(rec) if rec.v == 1 => {
                    let e = by_launch
                        .entry((rec.project.clone(), rec.launch_id))
                        .or_default();
                    let line = rec.into_line();
                    if signed {
                        e.0.push(line);
                    } else if crate::api::logs::validate(&line, max_line).is_ok() {
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
        let mut tx = self.s.store.pool().begin().await?;
        let mut n = 0;
        for (wl, lines) in resolved {
            let chunks = fastproof_store::logs::insert_log_lines(
                &mut tx,
                wl.launch.project_id,
                &wl.launch.project_key,
                wl.launch.id,
                lines,
            )
            .await?;
            n += chunks.iter().map(|c| c.lines.len() as u64).sum::<u64>();
        }
        ctx.commit_in(&mut tx, next, Some(hwm)).await?;
        tx.commit().await?;
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
