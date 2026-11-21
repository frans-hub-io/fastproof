//! Ingest workers: claim queued uploads, parse them in a blocking thread, store attachments and
//! persist results in chunks. Any replica may process any upload.

use std::collections::HashMap;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fastproof_blob::{BlobKey, HashingWriter};
use fastproof_ingest::{AttachmentSink, IngestError, IngestOptions, ingest_path};
use fastproof_model::{AttachmentId, SourceFormat};
use fastproof_store::attachments::NewAttachment;
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::uploads::{ClaimedJob, UploadDone};
use fastproof_store::{UploadError, UploadItemCounts};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

/// Lease of a claimed job; heartbeats renew it every third of it.
pub const LEASE: Duration = Duration::from_secs(60);
/// Results persisted per transaction.
pub const CHUNK: usize = 1000;
/// Warnings kept per upload.
pub const MAX_WARNINGS: usize = 100;

/// An attachment body spooled by the sink.
#[derive(Debug, Clone)]
pub struct SpooledItem {
    pub path: PathBuf,
    pub size: u64,
    pub content_type: Option<String>,
    pub name: String,
}

/// Writes attachment bodies to spool files while hashing them; identical bodies are kept once.
pub struct SpoolSink {
    dir: PathBuf,
    n: u64,
    pub items: HashMap<String, SpooledItem>,
}

impl SpoolSink {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            n: 0,
            items: HashMap::new(),
        }
    }
}

impl AttachmentSink for SpoolSink {
    fn put(
        &mut self,
        source: &str,
        content_type: Option<&str>,
        _size: u64,
        body: &mut dyn Read,
    ) -> Result<AttachmentId, IngestError> {
        self.n += 1;
        let path = self.dir.join(format!("a{}", self.n));
        let f = std::fs::File::create(&path)?;
        let mut w = HashingWriter::new(BufWriter::with_capacity(64 * 1024, f));
        if let Err(e) = std::io::copy(body, &mut w) {
            let _ = std::fs::remove_file(&path);
            return Err(IngestError::Io(e));
        }
        let (sha, size, mut inner) = w.finish();
        inner.flush()?;
        drop(inner);
        if self.items.contains_key(&sha) {
            let _ = std::fs::remove_file(&path);
        } else {
            let name = source.rsplit('/').next().unwrap_or(source).to_string();
            self.items.insert(
                sha.clone(),
                SpooledItem {
                    path,
                    size,
                    content_type: content_type.map(str::to_string),
                    name,
                },
            );
        }
        Ok(AttachmentId(sha))
    }
}

/// Why a job did not complete.
pub(crate) enum JobFailure {
    /// The upload itself is bad: fail it without retrying.
    Permanent(UploadError),
    /// Something on our side: retry while attempts remain.
    Retry(anyhow::Error),
    /// Another worker took the job over.
    LeaseLost,
}

impl From<anyhow::Error> for JobFailure {
    fn from(e: anyhow::Error) -> Self {
        JobFailure::Retry(e)
    }
}

pub(crate) fn map_ingest_error(e: IngestError) -> JobFailure {
    if let Some(limit) = e.limit_name() {
        return JobFailure::Permanent(UploadError {
            code: "limit_exceeded".into(),
            message: e.to_string(),
            limit: Some(limit.to_string()),
        });
    }
    let code = match &e {
        IngestError::UnsafeEntry { .. } => "unsafe_archive",
        IngestError::Unsupported(_) => "unsupported",
        IngestError::Archive(_) => "invalid_archive",
        IngestError::Sink(_) | IngestError::Io(_) | IngestError::LimitExceeded { .. } => {
            return JobFailure::Retry(anyhow::Error::from(e));
        }
    };
    JobFailure::Permanent(UploadError {
        code: code.into(),
        message: e.to_string(),
        limit: None,
    })
}

/// The format as the upload API names it.
pub fn api_format(f: SourceFormat) -> &'static str {
    match f {
        SourceFormat::AllureResults => "allure",
        SourceFormat::JunitXml => "junit",
        SourceFormat::Ctrf => "ctrf",
        SourceFormat::PlaywrightJson => "playwright",
        SourceFormat::FastproofJson => "fastproof",
    }
}

fn format_of(f: &str) -> Option<SourceFormat> {
    match f {
        "auto" => None,
        other => SourceFormat::parse(other),
    }
}

/// Runs one worker until `stop`.
pub async fn worker(s: AppState, n: usize, stop: CancellationToken) {
    let worker_id = format!("{}-w{n}", s.instance);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let notified = s.wake.notified();
        match s.store.claim_job(&worker_id, LEASE).await {
            Ok(Some(job)) => process(&s, &worker_id, job, &stop).await,
            Ok(None) => {
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = notified => {}
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "claiming an ingest job failed");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        }
    }
}

fn process<'a>(
    s: &'a AppState,
    worker_id: &'a str,
    job: ClaimedJob,
    stop: &'a CancellationToken,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(process_impl(s, worker_id, job, stop))
}

async fn process_impl(s: &AppState, worker_id: &str, job: ClaimedJob, stop: &CancellationToken) {
    let started = Instant::now();
    metrics::gauge!("fastproof_ingest_jobs_processing").increment(1.0);
    let lost = Arc::new(AtomicBool::new(false));
    let hb = {
        let s = s.clone();
        let lost = lost.clone();
        let worker_id = worker_id.to_string();
        let job_id = job.job_id;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEASE / 3).await;
                match s.store.heartbeat_job(job_id, &worker_id, LEASE).await {
                    Ok(true) => {}
                    Ok(false) => {
                        lost.store(true, Ordering::SeqCst);
                        return;
                    }
                    Err(e) => tracing::warn!(error = %e, "ingest heartbeat failed"),
                }
            }
        })
    };
    let work = s
        .cfg
        .spool_dir
        .join("work")
        .join(format!("{}-{}", job.upload_id, job.attempt));
    let r = process_inner(s, &job, &work, &lost, stop, None).await;
    hb.abort();
    let _ = tokio::fs::remove_dir_all(&work).await;
    let completed = match &r {
        Ok(done) => Some(complete(s, &job, worker_id.to_string(), done.clone()).await),
        Err(_) => None,
    };
    let outcome = match r {
        Ok(done) => match completed.unwrap_or(Ok(false)) {
            Ok(true) => {
                if !s.cfg.retain_uploads
                    && let Some(k) = job.blob_key.as_deref().and_then(|k| BlobKey::parse(k).ok())
                    && let Err(e) = s.blobs.delete(&k).await
                {
                    tracing::warn!(error = %format!("{e:#}"), "deleting a processed upload failed");
                }
                metrics::counter!("fastproof_results_ingested_total", "source" => "upload")
                    .increment(done.counts.results as u64);
                tracing::info!(upload = %job.upload_id, results = done.counts.results,
                    attachments = done.counts.attachments, ms = started.elapsed().as_millis() as u64, "upload processed");
                "done"
            }
            Ok(false) => "lost",
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "recording a processed upload failed");
                "error"
            }
        },
        Err(JobFailure::Permanent(err)) => {
            tracing::info!(upload = %job.upload_id, code = %err.code, message = %err.message, "upload rejected");
            if let Err(e) = s.store.fail_job(&job, worker_id, &err, false).await {
                tracing::error!(error = %format!("{e:#}"), "recording a failed upload failed");
            }
            "failed"
        }
        Err(JobFailure::Retry(e)) => {
            tracing::warn!(upload = %job.upload_id, attempt = job.attempt, error = %format!("{e:#}"), "processing an upload failed");
            let err = UploadError {
                code: "internal".into(),
                message: "processing failed on the server".into(),
                limit: None,
            };
            if let Err(e) = s.store.fail_job(&job, worker_id, &err, true).await {
                tracing::error!(error = %format!("{e:#}"), "recording a failed upload failed");
            }
            "retry"
        }
        Err(JobFailure::LeaseLost) => {
            tracing::warn!(upload = %job.upload_id, "ingest lease lost; another worker continues");
            "lost"
        }
    };
    if matches!(outcome, "done" | "failed")
        && let Some(p) = s
            .local_uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job.upload_id)
    {
        let _ = std::fs::remove_file(p);
    }
    metrics::gauge!("fastproof_ingest_jobs_processing").decrement(1.0);
    metrics::counter!("fastproof_ingest_jobs_total", "outcome" => outcome).increment(1);
    metrics::histogram!("fastproof_ingest_duration_seconds")
        .record(started.elapsed().as_secs_f64());
    metrics::counter!("fastproof_ingest_bytes_total").increment(job.size_bytes.max(0) as u64);
}

fn complete<'a>(
    s: &'a AppState,
    job: &'a ClaimedJob,
    worker: String,
    done: UploadDone,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + 'a>> {
    Box::pin(async move { s.store.complete_job(job, &worker, &done).await })
}

async fn raw_file(s: &AppState, job: &ClaimedJob, work: &Path) -> Result<PathBuf, JobFailure> {
    let local = s
        .local_uploads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&job.upload_id)
        .cloned();
    if let Some(p) = local
        && tokio::fs::metadata(&p).await.is_ok()
    {
        return Ok(p);
    }
    let key = job
        .blob_key
        .as_deref()
        .map(BlobKey::parse)
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("upload has no blob key"))?;
    let path = work.join("raw");
    match s.blobs.get_to_file(&key, &path).await? {
        Some(_) => Ok(path),
        None => Err(JobFailure::Permanent(UploadError {
            code: "missing".into(),
            message: "the uploaded file is no longer available".into(),
            limit: None,
        })),
    }
}

/// Persists (direct mode) or produces to `ingest.results` (Kafka mode) one chunk of parsed
/// results. A result too large for a Kafka record is persisted directly.
async fn emit_chunk(
    s: &AppState,
    kafka: Option<&std::sync::Arc<fastproof_stream::Kafka>>,
    launch: &LaunchRef,
    upload_id: uuid::Uuid,
    attempts: Vec<NewAttempt>,
) -> anyhow::Result<()> {
    let Some(kafka) = kafka else {
        s.store
            .persist_attempts(launch, Some(upload_id), attempts)
            .await?;
        return Ok(());
    };
    let max = kafka.config().max_record_bytes;
    let mut records = Vec::with_capacity(attempts.len());
    let mut direct = Vec::new();
    for a in attempts {
        let rec = fastproof_stream::messages::ResultRecord {
            v: fastproof_stream::messages::VERSION,
            project: launch.project_key.clone(),
            launch_id: launch.id,
            result: None,
            parsed: Some(Box::new(a.result)),
            client_key: Some(a.client_key),
            upload_id: Some(upload_id),
        };
        let value = serde_json::to_vec(&rec)?;
        if value.len() > max {
            direct.push(NewAttempt {
                client_key: rec.client_key.unwrap_or_default(),
                result: *rec.parsed.expect("set above"),
            });
            continue;
        }
        records.push(fastproof_stream::kafka::Outgoing {
            key: launch.id.to_string().into_bytes(),
            value,
        });
    }
    kafka
        .produce(
            &kafka.topic(fastproof_stream::kafka::TOPIC_RESULTS),
            records,
        )
        .await?;
    if !direct.is_empty() {
        metrics::counter!("fastproof_kafka_oversized_results_total").increment(direct.len() as u64);
        s.store
            .persist_attempts(launch, Some(upload_id), direct)
            .await?;
    }
    Ok(())
}

pub(crate) async fn process_inner(
    s: &AppState,
    job: &ClaimedJob,
    work: &Path,
    lost: &AtomicBool,
    stop: &CancellationToken,
    kafka: Option<&std::sync::Arc<fastproof_stream::Kafka>>,
) -> Result<UploadDone, JobFailure> {
    tokio::fs::create_dir_all(work.join("att"))
        .await
        .map_err(anyhow::Error::from)?;
    let raw = raw_file(s, job, work).await?;
    let options = IngestOptions {
        format: format_of(&job.format),
        limits: s.limits.clone(),
        file_name: job.file_name.clone(),
        environment_key: job.env_key.clone(),
        attach_fixtures: false,
        ..IngestOptions::default()
    };
    let att_dir = work.join("att");
    let (batch, sink) = tokio::task::spawn_blocking(move || {
        let mut sink = SpoolSink::new(att_dir);
        let r = ingest_path(&raw, &options, &mut sink);
        (r, sink)
    })
    .await
    .map_err(|e| anyhow::anyhow!("the ingest thread failed: {e}"))?;
    let mut batch = batch.map_err(map_ingest_error)?;
    if lost.load(Ordering::SeqCst) || stop.is_cancelled() {
        return Err(JobFailure::LeaseLost);
    }

    // Attachments: upload the bodies the project does not have yet, then record them.
    let shas: Vec<String> = sink.items.keys().cloned().collect();
    let existing = s.store.touch_attachments(job.project_id, &shas).await?;
    let missing: Vec<(String, SpooledItem)> = sink
        .items
        .iter()
        .filter(|(sha, _)| !existing.contains_key(*sha))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let project_key = job.project_key.clone();
    let mut missing_keys = Vec::with_capacity(missing.len());
    for (sha, _) in &missing {
        missing_keys.push(BlobKey::attachment(&project_key, sha)?.to_string());
    }
    for keys in missing_keys.chunks(1000) {
        s.store.cancel_blob_deletions(keys).await?;
    }
    let uploads = futures::stream::iter(missing.into_iter().map(|(sha, item)| {
        let s = s.clone();
        let project_key = project_key.clone();
        async move {
            let key = BlobKey::attachment(&project_key, &sha)?;
            s.blobs.put_file(&key, &item.path).await?;
            Ok::<_, anyhow::Error>(())
        }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    for u in uploads {
        u?;
    }
    let mut rows = Vec::with_capacity(sink.items.len());
    for (sha, item) in &sink.items {
        rows.push(NewAttachment {
            sha256: sha.clone(),
            size: item.size as i64,
            content_type: crate::api::attachments::clean_content_type(item.content_type.as_deref()),
            original_name: crate::api::uploads::clean_file_name(&item.name),
            blob_key: BlobKey::attachment(&job.project_key, sha)?.to_string(),
        });
    }
    let mut ids: HashMap<String, uuid::Uuid> = HashMap::new();
    for chunk in rows.chunks(5000) {
        ids.extend(
            s.store
                .insert_attachments(job.project_id, job.launch_id, chunk)
                .await?,
        );
    }
    let mut rewrite = |a: &mut fastproof_model::AttachmentRef| {
        if let Some(id) = a.id.as_ref().and_then(|i| ids.get(&i.0)) {
            a.id = Some(AttachmentId(id.to_string()));
        } else {
            a.id = None;
        }
    };
    for r in &mut batch.results {
        r.visit_attachments_mut(&mut rewrite);
    }
    for c in &mut batch.containers {
        for st in c.befores.iter_mut().chain(c.afters.iter_mut()) {
            st.visit_attachments_mut(&mut rewrite);
        }
    }
    for a in &mut batch.globals.attachments {
        rewrite(a);
    }

    let launch = LaunchRef {
        id: job.launch_id,
        project_id: job.project_id,
        project_key: job.project_key.clone(),
        created_at: job.launch_created_at,
    };
    s.store
        .upsert_containers(&launch, &batch.containers)
        .await?;

    let format = batch.format;
    let total = batch.results.len();
    let results = std::mem::take(&mut batch.results);
    let mut attempts: Vec<NewAttempt> = Vec::with_capacity(CHUNK);
    for (i, r) in results.into_iter().enumerate() {
        let client_key = match (&r.source_uuid, format) {
            (Some(u), Some(f)) => format!("u:{}:{u}", f.as_str()),
            _ => format!("x:{}:{i}", job.upload_id),
        };
        attempts.push(NewAttempt {
            client_key,
            result: r,
        });
        if attempts.len() >= CHUNK {
            if lost.load(Ordering::SeqCst) || stop.is_cancelled() {
                return Err(JobFailure::LeaseLost);
            }
            emit_chunk(
                s,
                kafka,
                &launch,
                job.upload_id,
                std::mem::take(&mut attempts),
            )
            .await?;
        }
    }
    if !attempts.is_empty() {
        emit_chunk(s, kafka, &launch, job.upload_id, attempts).await?;
    }

    let mut warnings: Vec<String> = batch
        .warnings
        .iter()
        .take(MAX_WARNINGS)
        .map(|w| match &w.file {
            Some(f) => format!("{f}: {}", w.message),
            None => w.message.clone(),
        })
        .collect();
    if batch.warnings.len() > MAX_WARNINGS {
        warnings.push(format!(
            "{} more warnings",
            batch.warnings.len() - MAX_WARNINGS
        ));
    }
    if batch.counts.attachments_missing > 0 {
        warnings.push(format!(
            "{} attachment references point to files that are not in the upload",
            batch.counts.attachments_missing
        ));
    }
    let globals = if batch.globals.is_empty() {
        None
    } else {
        Some(serde_json::to_value(&batch.globals).map_err(anyhow::Error::from)?)
    };
    Ok(UploadDone {
        format: format.map(|f| api_format(f).to_string()),
        counts: UploadItemCounts {
            results: total as i64,
            attachments: sink.items.len() as i64,
            ignored: batch.ignored.len() as i64,
        },
        warnings,
        globals,
    })
}

/// Deletes spool files of this replica older than `age` (crashed requests and workers).
pub async fn clean_local_spool(dir: &Path, age: Duration) {
    for sub in ["incoming", "work", "attachments"] {
        let Ok(mut rd) = tokio::fs::read_dir(dir.join(sub)).await else {
            continue;
        };
        while let Ok(Some(e)) = rd.next_entry().await {
            let old = e
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|el| el > age);
            if old {
                let p = e.path();
                if p.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&p).await;
                } else {
                    let _ = tokio::fs::remove_file(&p).await;
                }
            }
        }
    }
}
