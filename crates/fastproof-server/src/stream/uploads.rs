//! Kafka mode uploads (`fastproof-ingest`): the gateway stores the archive in blob storage and
//! produces `ingest.uploads`; an ingest worker parses it, stores attachments and produces the
//! results to `ingest.results`, then marks the upload done and commits its offset. A record
//! seen again after a crash parses the upload again; the writer drops the duplicates by
//! client key.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use fastproof_blob::BlobKey;
use fastproof_store::UploadError;
use fastproof_stream::consumer::BatchHandler;
use fastproof_stream::groups::PartitionCtx;
use fastproof_stream::kafka::RecordAndOffset;
use fastproof_stream::messages::UploadRecord;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::ingest::{JobFailure, process_inner};
use crate::state::AppState;

/// Processing attempts of one upload before it fails.
const ATTEMPTS: i32 = 3;

/// Processes uploads; the partitions of one process share `FASTPROOF_INGEST_WORKERS` slots,
/// so a backlog never parses more archives at once than the direct mode workers would.
pub struct UploadHandler {
    pub s: AppState,
    pub stop: CancellationToken,
    slots: Arc<Semaphore>,
}

impl UploadHandler {
    pub fn new(s: AppState, stop: CancellationToken) -> Self {
        let slots = Arc::new(Semaphore::new(s.cfg.ingest_workers.max(1)));
        Self { s, stop, slots }
    }

    /// The launch of the upload was deleted while it was processed: the upload went with it,
    /// so there is no status left to record; the record is done (its offset is committed).
    fn launch_gone(&self, job: &fastproof_store::uploads::ClaimedJob) {
        if let Some(p) = self
            .s
            .local_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job.upload_id)
        {
            let _ = std::fs::remove_file(p);
        }
        metrics::counter!("fastproof_ingest_jobs_total", "outcome" => "launch_deleted")
            .increment(1);
        tracing::info!(upload = %job.upload_id, launch = %job.launch_id, "launch deleted while its upload was processed; stopped");
    }

    async fn process(&self, rec: UploadRecord) -> anyhow::Result<()> {
        let s = &self.s;
        let kafka = s
            .kafka
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Kafka is not connected"))?;
        for attempt in 1..=ATTEMPTS {
            let Some(job) = s.store.start_upload(rec.upload_id, attempt).await? else {
                return Ok(());
            };
            if job.project_key != rec.project || job.launch_id != rec.launch_id {
                metrics::counter!("fastproof_kafka_rejected_total", "topic" => "ingest.uploads", "reason" => "mismatch")
                    .increment(1);
                return Ok(());
            }
            let work = s
                .cfg
                .spool_dir
                .join("work")
                .join(format!("{}-k{attempt}", job.upload_id));
            let lost = AtomicBool::new(false);
            let started = std::time::Instant::now();
            let r = process_inner(s, &job, &work, &lost, &self.stop, Some(kafka)).await;
            let _ = tokio::fs::remove_dir_all(&work).await;
            match r {
                Ok(done) => {
                    if !s.store.finish_upload(&job, &done).await? {
                        self.launch_gone(&job);
                        return Ok(());
                    }
                    if !s.cfg.retain_uploads
                        && let Some(k) =
                            job.blob_key.as_deref().and_then(|k| BlobKey::parse(k).ok())
                        && let Err(e) = s.blobs.delete(&k).await
                    {
                        tracing::warn!(error = %format!("{e:#}"), "deleting a processed upload failed");
                    }
                    if let Some(p) = s
                        .local_uploads
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&job.upload_id)
                    {
                        let _ = std::fs::remove_file(p);
                    }
                    metrics::counter!("fastproof_ingest_jobs_total", "outcome" => "done")
                        .increment(1);
                    metrics::histogram!("fastproof_ingest_duration_seconds")
                        .record(started.elapsed().as_secs_f64());
                    tracing::info!(upload = %job.upload_id, results = done.counts.results, "upload processed into Kafka");
                    return Ok(());
                }
                Err(JobFailure::Permanent(err)) => {
                    s.store.fail_upload(&job, &err).await?;
                    metrics::counter!("fastproof_ingest_jobs_total", "outcome" => "failed")
                        .increment(1);
                    return Ok(());
                }
                Err(JobFailure::LeaseLost) => anyhow::bail!("stopping"),
                Err(JobFailure::LaunchGone) => {
                    self.launch_gone(&job);
                    return Ok(());
                }
                Err(JobFailure::Retry(e)) => {
                    tracing::warn!(upload = %job.upload_id, attempt, error = %format!("{e:#}"), "processing an upload failed");
                    if attempt == ATTEMPTS {
                        s.store
                            .fail_upload(
                                &job,
                                &UploadError {
                                    code: "internal".into(),
                                    message: "processing failed on the server".into(),
                                    limit: None,
                                },
                            )
                            .await?;
                        metrics::counter!("fastproof_ingest_jobs_total", "outcome" => "failed")
                            .increment(1);
                        return Ok(());
                    }
                    tokio::select! {
                        _ = self.stop.cancelled() => anyhow::bail!("stopping"),
                        _ = tokio::time::sleep(Duration::from_secs(5 * attempt as u64)) => {}
                    }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl BatchHandler for UploadHandler {
    async fn handle(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next_offset: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        let kafka = self
            .s
            .kafka
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Kafka is not connected"))?;
        let trusted = kafka.config().trusted_producers;
        for r in records {
            if !trusted
                && !kafka
                    .signer()
                    .is_some_and(|s| s.verify(&ctx.topic, &r.record))
            {
                metrics::counter!("fastproof_kafka_rejected_total", "topic" => ctx.topic.clone(), "reason" => "unsigned")
                    .increment(1);
                continue;
            }
            let Some(Ok(rec)) = r
                .record
                .value
                .as_deref()
                .map(serde_json::from_slice::<UploadRecord>)
            else {
                metrics::counter!("fastproof_kafka_rejected_total", "topic" => ctx.topic.clone(), "reason" => "invalid")
                    .increment(1);
                continue;
            };
            {
                let _slot = tokio::select! {
                    _ = self.stop.cancelled() => anyhow::bail!("stopping"),
                    s = self.slots.acquire() => s?,
                };
                self.process(rec).await?;
            }
            ctx.commit(r.offset + 1, Some(hwm)).await?;
        }
        ctx.commit(next_offset, Some(hwm)).await
    }
}
