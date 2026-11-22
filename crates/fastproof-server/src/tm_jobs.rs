//! Workers of test-management import and export jobs (`tm_jobs`). A worker claims a job
//! (lease), runs one step of it and hands it back: the dry run of an import (TestOps cases are
//! fetched first and kept as NDJSON), the apply (batches of 500 cases, each committed with the
//! job's progress, so a job resumes after a crash), or an export (zip with `cases.ndjson`,
//! `cases.csv` and the attachments). New jobs are started right away on the replica that took
//! the request; the `cases.jobs` scheduler job picks up the rest and removes expired blobs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fastproof_blob::BlobKey;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::import::{
    ClaimedJob, ExportParams, ImportParams, MAX_EXPORT_CASES, PlanOptions, Planner, TestOpsParams,
};
use fastproof_store::tm::{
    ImportFormat, ImportMode, ImportReport, ImportRowError, TmJobKind, TmJobProgress, TmJobStatus,
    UnknownFieldValues,
};
use fastproof_tm_import::{
    Format, ImportError, ImportedCase, Item, ParseLimits, ParseOptions, StepsLayout,
};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;

use crate::integrations::secrets::SecretPurpose;
use crate::state::AppState;
use crate::testops::{TestOps, check_base_url};

/// Cases applied per transaction.
pub const BATCH: usize = 500;
/// Workers per replica.
const WORKERS: usize = 2;

fn workers(instance: &str) -> Arc<Semaphore> {
    static ALL: Mutex<Option<HashMap<String, Arc<Semaphore>>>> = Mutex::new(None);
    let mut g = ALL.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .entry(instance.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(WORKERS)))
        .clone()
}

/// Starts a worker on this replica when one is free (it runs jobs until none is left).
pub fn kick(s: &AppState) {
    let Ok(permit) = workers(&s.instance).try_acquire_owned() else {
        return;
    };
    let s = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        loop {
            let owner = format!("{}-tm-{}", s.instance, Uuid::now_v7());
            match s.store.claim_tm_job(&owner).await {
                Ok(Some(job)) => run(&s, job, &owner).await,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "claiming a test-management job failed");
                    return;
                }
            }
        }
    });
}

/// The `cases.jobs` scheduler job: starts workers and removes what expired jobs left.
pub async fn tick(s: &AppState) -> anyhow::Result<()> {
    kick(s);
    for (id, keys) in s.store.expired_tm_job_blobs(200).await? {
        for k in keys {
            if let Ok(key) = BlobKey::parse(&k) {
                s.blobs.delete(&key).await?;
            }
        }
        s.store.forget_tm_job_blobs(id).await?;
    }
    for (project, name) in s.store.stale_import_secrets().await? {
        s.store.delete_secret(project, &name).await?;
    }
    Ok(())
}

fn fail_report(code: &str, message: &str) -> ImportReport {
    ImportReport {
        errors: vec![ImportRowError {
            row: 0,
            code: code.into(),
            message: message.chars().take(500).collect(),
        }],
        ..Default::default()
    }
}

async fn run(s: &AppState, job: ClaimedJob, owner: &str) {
    let r = match (job.kind, job.status) {
        (TmJobKind::Import, TmJobStatus::DryRun) => dry_run(s, &job, owner).await,
        (TmJobKind::Import, _) => apply(s, &job, owner).await,
        (TmJobKind::Export, _) => export(s, &job, owner).await,
        (TmJobKind::Backfill, _) => Ok(()),
    };
    if let Err(e) = r {
        let (code, message) = match e.downcast_ref::<ImportError>() {
            Some(ie) => (ie.code(), ie.to_string()),
            None => match e.downcast_ref::<fastproof_store::tm::TmError>() {
                Some(fastproof_store::tm::TmError::Conflict { code, message, .. })
                | Some(fastproof_store::tm::TmError::Invalid { code, message, .. }) => {
                    (*code, message.clone())
                }
                _ => ("job_failed", format!("{e:#}")),
            },
        };
        tracing::warn!(job = %job.id, code, "test-management job failed: {message}");
        let report = fail_report(code, &message);
        let _ = s
            .store
            .finish_tm_job(
                job.id,
                owner,
                &job.project_key,
                TmJobStatus::Failed,
                job.progress,
                Some(&report),
                None,
            )
            .await;
    }
}

fn spool(s: &AppState) -> anyhow::Result<PathBuf> {
    let dir = s.cfg.spool_dir.join("tm-jobs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(Uuid::now_v7().to_string()))
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.0.is_dir() {
            let _ = std::fs::remove_dir_all(&self.0);
        } else {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

async fn download(s: &AppState, key: &str) -> anyhow::Result<TempFile> {
    let path = spool(s)?;
    let k = BlobKey::parse(key)?;
    let t = TempFile(path.clone());
    s.blobs
        .get_to_file(&k, &path)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the file of the import is gone"))?;
    Ok(t)
}

fn format_of(p: &ImportParams) -> Format {
    if p.source == "testops" {
        return Format::FastproofNdjson;
    }
    p.format
        .and_then(|f| Format::parse(f.as_str()))
        .unwrap_or(Format::GenericCsv)
}

fn parse_options(s: &AppState, p: &ImportParams) -> ParseOptions {
    let l = &s.cfg.tm;
    ParseOptions {
        mapping: p.mapping.clone(),
        steps_layout: p.steps_layout.map(|x| match x {
            fastproof_store::tm::StepsLayout::Rows => StepsLayout::Rows,
            fastproof_store::tm::StepsLayout::Cells => StepsLayout::Cells,
        }),
        limits: ParseLimits {
            max_bytes: if p.source == "testops" {
                u64::MAX
            } else {
                l.max_import_bytes
            },
            max_rows: l.max_import_rows,
            max_cell_bytes: if p.source == "testops" {
                l.max_markdown_bytes.max(l.max_import_cell_bytes)
            } else {
                l.max_import_cell_bytes
            },
            max_columns: fastproof_tm_import::MAX_COLUMNS,
            max_steps: l.max_steps,
            max_step_depth: l.max_step_depth,
            max_tags: l.max_tags,
        },
    }
}

fn plan_options(p: &ImportParams) -> PlanOptions {
    let testops = p.source == "testops" || p.format == Some(ImportFormat::TestopsCsv);
    PlanOptions {
        mode: p.mode.unwrap_or(ImportMode::CreateOnly),
        unknown_values: p.unknown_field_values.unwrap_or(UnknownFieldValues::Skip),
        target_folder: p.target_folder_id,
        numbered_source: testops || p.format == Some(ImportFormat::FastproofNdjson),
        identities: testops,
    }
}

/// Parses a file on a blocking thread; items arrive on the channel.
fn spawn_parse(
    path: PathBuf,
    format: Format,
    opts: ParseOptions,
) -> (
    mpsc::Receiver<Item>,
    tokio::task::JoinHandle<Result<(), ImportError>>,
) {
    let (tx, rx) = mpsc::channel::<Item>(512);
    let h = tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path).map_err(ImportError::from)?;
        fastproof_tm_import::parse(format, std::io::BufReader::new(f), &opts, &mut |item| {
            tx.blocking_send(item).is_ok()
        })
    });
    (rx, h)
}

fn by_of(job: &ClaimedJob) -> String {
    job.created_by.clone().unwrap_or_else(|| "import".into())
}

async fn dry_run(s: &AppState, job: &ClaimedJob, owner: &str) -> anyhow::Result<()> {
    let mut params: ImportParams = serde_json::from_value(job.params.clone())?;
    let by = by_of(job);
    let ctx = CaseCtx {
        project_id: job.project_id,
        project_key: &job.project_key,
        by: &by,
        limits: &s.cfg.tm,
    };
    let mut patch = json!({});
    if params.source == "testops" && params.blob.is_none() {
        let Some(key) = fetch_testops(s, job, owner, &params).await? else {
            return Ok(());
        };
        patch["blob"] = json!(key);
        params.blob = Some(key);
    }
    let blob = params
        .blob
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the import has no file"))?;
    let file = download(s, &blob).await?;
    let mut planner = s
        .store
        .import_planner(job.project_id, plan_options(&params))
        .await?;
    let (mut rx, h) = spawn_parse(
        file.0.clone(),
        format_of(&params),
        parse_options(s, &params),
    );
    let mut n: i64 = 0;
    let mut last = Instant::now();
    while let Some(item) = rx.recv().await {
        match item {
            Item::Case(c) => {
                s.store.dry_import_case(&mut planner, &ctx, &c).await?;
                n += 1;
            }
            Item::Error(e) => planner.error(e),
            Item::FieldDefs(d) => {
                planner
                    .field_defs_from_source(&s.store, &ctx, &d, false)
                    .await?
            }
        }
        if last.elapsed() > Duration::from_secs(5) {
            last = Instant::now();
            if !s
                .store
                .heartbeat_tm_job(
                    job.id,
                    owner,
                    TmJobProgress {
                        done: n,
                        total: None,
                    },
                )
                .await?
            {
                return Ok(());
            }
        }
    }
    h.await??;
    let report = planner.finish_report();
    patch["keepNumbers"] = json!(planner.keep_numbers);
    patch["maxNumber"] = json!(planner.max_number);
    patch["unknownValues"] = json!(report.unknown_values);
    s.store
        .finish_tm_job(
            job.id,
            owner,
            &job.project_key,
            TmJobStatus::Ready,
            TmJobProgress {
                done: n,
                total: Some(n),
            },
            Some(&report),
            Some(patch),
        )
        .await?;
    Ok(())
}

async fn testops_client<'a>(
    s: &'a AppState,
    job: &ClaimedJob,
    t: &TestOpsParams,
) -> anyhow::Result<TestOps<'a>> {
    let base = check_base_url(&t.base_url, t.allow_http).map_err(ImportError::invalid)?;
    let origin = crate::integrations::secrets::origin_of(base.as_str());
    let token = s
        .project_secret(
            job.project_id,
            &t.secret,
            SecretPurpose::Testops,
            origin.as_deref(),
        )
        .await
        .map_err(|e| ImportError::invalid(e.to_string()))?;
    Ok(TestOps::connect(&s.egress, base, token.expose())
        .await
        .map_err(|e| ImportError::invalid(e.0))?)
}

/// Fetches the cases of a TestOps project into an NDJSON blob; `None` when cancelled.
async fn fetch_testops(
    s: &AppState,
    job: &ClaimedJob,
    owner: &str,
    p: &ImportParams,
) -> anyhow::Result<Option<String>> {
    let t = p
        .testops
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no TestOps source"))?;
    let mut c = testops_client(s, job, &t).await?;
    let file = TempFile(spool(s)?);
    let mut out = std::io::BufWriter::new(std::fs::File::create(&file.0)?);
    out.write_all(
        fastproof_tm_import::export::ndjson_header(
            "testops",
            &chrono::Utc::now().to_rfc3339(),
            Vec::new(),
        )
        .as_bytes(),
    )?;
    let limits = &s.cfg.tm;
    let mut page = 0usize;
    let mut count = 0usize;
    let mut seen = HashSet::new();
    let mut last = Instant::now();
    loop {
        let (ids, more) = c
            .search(t.remote_project_id, page)
            .await
            .map_err(|e| ImportError::invalid(e.0))?;
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            count += 1;
            if count > limits.max_import_rows {
                return Err(ImportError::limit("maxImportRows", limits.max_import_rows).into());
            }
            if let Some(case) = c.case(id).await.map_err(|e| ImportError::invalid(e.0))? {
                out.write_all(fastproof_tm_import::export::ndjson_case(&case).as_bytes())?;
            }
            if c.received > limits.max_import_bytes {
                return Err(ImportError::limit("maxImportBytes", limits.max_import_bytes).into());
            }
            if last.elapsed() > Duration::from_secs(10) {
                last = Instant::now();
                if !s
                    .store
                    .heartbeat_tm_job(
                        job.id,
                        owner,
                        TmJobProgress {
                            done: count as i64,
                            total: None,
                        },
                    )
                    .await?
                {
                    return Ok(None);
                }
            }
        }
        if !s
            .store
            .heartbeat_tm_job(
                job.id,
                owner,
                TmJobProgress {
                    done: count as i64,
                    total: None,
                },
            )
            .await?
        {
            return Ok(None);
        }
        if !more {
            break;
        }
        page += 1;
    }
    out.flush()?;
    drop(out);
    let key = BlobKey::tm_job(&job.id.to_string(), "testops.ndjson")?;
    s.blobs.put_file(&key, &file.0).await?;
    Ok(Some(key.to_string()))
}

/// Stores one attachment file (content-addressed) and returns its id.
async fn store_attachment(
    s: &AppState,
    job: &ClaimedJob,
    path: &Path,
    name: &str,
) -> anyhow::Result<Uuid> {
    let (sha, size) = fastproof_blob::sha256_file(path)?;
    let key = BlobKey::attachment(&job.project_key, &sha)?;
    let existing = s
        .store
        .touch_attachments(job.project_id, std::slice::from_ref(&sha))
        .await?;
    if !existing.contains_key(&sha) {
        s.store.cancel_blob_deletions(&[key.to_string()]).await?;
        s.blobs.put_file(&key, path).await?;
    }
    let ct = franshub_http::content_type(name);
    let ct = if ct.starts_with("text/html") || ct == "image/svg+xml" {
        "application/octet-stream"
    } else {
        ct
    };
    let name: String = name.chars().filter(|c| !c.is_control()).take(255).collect();
    s.store
        .import_attachment(job.project_id, &sha, size as i64, ct, &name, key.as_str())
        .await
}

/// The attachment sources the cases of the import reference.
async fn needed_attachments(
    s: &AppState,
    file: &Path,
    params: &ImportParams,
) -> anyhow::Result<BTreeMap<String, String>> {
    let (mut rx, h) = spawn_parse(
        file.to_path_buf(),
        format_of(params),
        parse_options(s, params),
    );
    let mut out = BTreeMap::new();
    while let Some(item) = rx.recv().await {
        if let Item::Case(c) = item {
            for a in &c.attachments {
                if out.len() < 100_000 {
                    out.insert(a.source.clone(), a.name.clone());
                }
            }
        }
    }
    let _ = h.await?;
    Ok(out)
}

async fn resolve_attachments(
    s: &AppState,
    job: &ClaimedJob,
    file: &Path,
    params: &ImportParams,
) -> anyhow::Result<HashMap<String, Uuid>> {
    let mut map = HashMap::new();
    if params.zip.is_none() && params.testops.is_none() {
        return Ok(map);
    }
    let needed = needed_attachments(s, file, params).await?;
    if needed.is_empty() {
        return Ok(map);
    }
    let l = &s.cfg.tm;
    if let Some(zip) = &params.zip {
        let z = download(s, zip).await?;
        let dir = TempFile(spool(s)?);
        std::fs::create_dir_all(&dir.0)?;
        let limits = fastproof_ingest::Limits {
            max_upload_bytes: l.max_import_zip_bytes,
            max_total_uncompressed_bytes: l.max_import_zip_bytes,
            max_entry_bytes: l.max_attachment_bytes,
            ..Default::default()
        };
        let wanted: HashSet<String> = needed.keys().cloned().collect();
        let zpath = z.0.clone();
        let dpath = dir.0.clone();
        let found: Vec<(String, PathBuf, String)> = tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            let mut i = 0usize;
            fastproof_ingest::for_each_entry(
                &zpath,
                Some("attachments.zip"),
                &limits,
                &mut |path, _, r| {
                    let file_name = path.rsplit('/').next().unwrap_or(path).to_string();
                    let source = if wanted.contains(path) {
                        path.to_string()
                    } else if wanted.contains(&file_name) {
                        file_name.clone()
                    } else {
                        return Ok(());
                    };
                    i += 1;
                    let out = dpath.join(i.to_string());
                    let mut f = std::fs::File::create(&out)?;
                    std::io::copy(r, &mut f)?;
                    found.push((source, out, file_name));
                    Ok(())
                },
            )
            .map_err(|e| ImportError::invalid(format!("the attachment zip: {e}")))?;
            Ok::<_, ImportError>(found)
        })
        .await??;
        for (source, path, file_name) in found {
            if map.contains_key(&source) {
                continue;
            }
            let id = store_attachment(s, job, &path, &file_name).await?;
            map.insert(source, id);
        }
    }
    if let Some(t) = &params.testops {
        let ids: Vec<(String, String)> = needed
            .iter()
            .filter_map(|(src, name)| {
                src.strip_prefix("testops:").map(|id| {
                    (
                        src.clone(),
                        if name.is_empty() {
                            id.to_string()
                        } else {
                            name.clone()
                        },
                    )
                })
            })
            .collect();
        if !ids.is_empty() {
            let mut c = testops_client(s, job, t).await?;
            let mut total: u64 = 0;
            for (source, name) in ids {
                let id = source.trim_start_matches("testops:");
                let Some(bytes) = c
                    .attachment(id, l.max_attachment_bytes.min(usize::MAX as u64) as usize)
                    .await
                    .map_err(|e| ImportError::invalid(e.0))?
                else {
                    continue;
                };
                total += bytes.len() as u64;
                if total > l.max_import_zip_bytes {
                    return Err(
                        ImportError::limit("maxImportZipBytes", l.max_import_zip_bytes).into(),
                    );
                }
                let tmp = TempFile(spool(s)?);
                tokio::fs::write(&tmp.0, &bytes).await?;
                let aid = store_attachment(s, job, &tmp.0, &name).await?;
                map.insert(source, aid);
            }
        }
    }
    Ok(map)
}

async fn apply(s: &AppState, job: &ClaimedJob, owner: &str) -> anyhow::Result<()> {
    let params: ImportParams = serde_json::from_value(job.params.clone())?;
    let by = by_of(job);
    let ctx = CaseCtx {
        project_id: job.project_id,
        project_key: &job.project_key,
        by: &by,
        limits: &s.cfg.tm,
    };
    let blob = params
        .blob
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the import has no file"))?;
    let file = download(s, &blob).await?;
    let mut planner: Planner = s
        .store
        .import_planner(job.project_id, plan_options(&params))
        .await?;
    planner.keep_numbers = params.keep_numbers;
    planner.max_number = params.max_number;
    let resume = job.progress.done.max(0) as usize;
    if resume > 0 {
        if let Some(r) = &job.report {
            planner.report = r.clone();
        }
    } else {
        planner.raise_seq(&s.store, job.project_id).await?;
        if params.unknown_field_values == Some(UnknownFieldValues::Create) {
            let unknown: BTreeMap<String, Vec<String>> = job
                .params
                .get("unknownValues")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            planner.create_options(&s.store, &ctx, &unknown).await?;
        }
    }
    let attachments = resolve_attachments(s, job, &file.0, &params).await?;
    let total = job.progress.total;
    let (mut rx, h) = spawn_parse(
        file.0.clone(),
        format_of(&params),
        parse_options(s, &params),
    );
    let mut seen = 0usize;
    let mut batch: Vec<ImportedCase> = Vec::with_capacity(BATCH);
    while let Some(item) = rx.recv().await {
        match item {
            Item::Case(c) => {
                seen += 1;
                if seen <= resume {
                    continue;
                }
                batch.push(*c);
            }
            Item::Error(e) => {
                if resume == 0 {
                    planner.error(e);
                }
            }
            Item::FieldDefs(d) => {
                planner
                    .field_defs_from_source(&s.store, &ctx, &d, true)
                    .await?
            }
        }
        if batch.len() >= BATCH {
            let ok = s
                .store
                .apply_import_batch(
                    &mut planner,
                    &ctx,
                    job.id,
                    owner,
                    &batch,
                    &attachments,
                    seen as i64,
                    total,
                )
                .await?;
            if !ok {
                return Ok(());
            }
            batch.clear();
        }
    }
    h.await??;
    if !batch.is_empty()
        && !s
            .store
            .apply_import_batch(
                &mut planner,
                &ctx,
                job.id,
                owner,
                &batch,
                &attachments,
                seen as i64,
                total,
            )
            .await?
    {
        return Ok(());
    }
    let report = planner.finish_report();
    s.store
        .finish_tm_job(
            job.id,
            owner,
            &job.project_key,
            TmJobStatus::Done,
            TmJobProgress {
                done: seen as i64,
                total: Some(seen as i64),
            },
            Some(&report),
            None,
        )
        .await?;
    if let Some(t) = &params.testops {
        let _ = s.store.delete_secret(job.project_id, &t.secret).await;
    }
    Ok(())
}

async fn export(s: &AppState, job: &ClaimedJob, owner: &str) -> anyhow::Result<()> {
    let params: ExportParams = serde_json::from_value(job.params.clone())?;
    let src = s
        .store
        .export_source(job.project_id, &params.filter, MAX_EXPORT_CASES)
        .await?;
    let dir = TempFile(spool(s)?);
    std::fs::create_dir_all(dir.0.join("attachments"))?;
    let keys = src.field_keys();
    let mut nd = std::io::BufWriter::new(std::fs::File::create(dir.0.join("cases.ndjson"))?);
    let mut csv = std::io::BufWriter::new(std::fs::File::create(dir.0.join("cases.csv"))?);
    nd.write_all(
        fastproof_tm_import::export::ndjson_header(
            &job.project_key,
            &chrono::Utc::now().to_rfc3339(),
            src.field_defs(),
        )
        .as_bytes(),
    )?;
    csv.write_all(fastproof_tm_import::export::csv_header(&keys).as_bytes())?;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut budget: u64 = 0;
    let mut done = 0i64;
    let total = src.numbers.len() as i64;
    for chunk in src.numbers.chunks(500) {
        for (c, atts) in s.store.export_page(job.project_id, &src, chunk).await? {
            nd.write_all(fastproof_tm_import::export::ndjson_case(&c).as_bytes())?;
            csv.write_all(fastproof_tm_import::export::csv_case(&c, &keys).as_bytes())?;
            if params.attachments {
                for a in atts {
                    budget += a.size.max(0) as u64;
                    if budget > s.cfg.tm.max_import_zip_bytes {
                        return Err(ImportError::limit(
                            "maxImportZipBytes",
                            s.cfg.tm.max_import_zip_bytes,
                        )
                        .into());
                    }
                    let entry = fastproof_store::tm::export::attachment_entry(a.id, &a.name);
                    if files.iter().any(|(e, _)| *e == entry) {
                        continue;
                    }
                    let p = dir.0.join("attachments").join(files.len().to_string());
                    if let Ok(k) = BlobKey::parse(&a.blob_key)
                        && s.blobs.get_to_file(&k, &p).await?.is_some()
                    {
                        files.push((entry, p));
                    }
                }
            }
        }
        done += chunk.len() as i64;
        if !s
            .store
            .heartbeat_tm_job(
                job.id,
                owner,
                TmJobProgress {
                    done,
                    total: Some(total),
                },
            )
            .await?
        {
            return Ok(());
        }
    }
    nd.flush()?;
    csv.flush()?;
    drop((nd, csv));
    let zip_path = dir.0.join("export.zip");
    let (d, zp) = (dir.0.clone(), zip_path.clone());
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let f = std::fs::File::create(&zp)?;
        let mut z = zip::ZipWriter::new(std::io::BufWriter::new(f));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        for name in ["cases.ndjson", "cases.csv"] {
            z.start_file(name, opts)?;
            std::io::copy(&mut std::fs::File::open(d.join(name))?, &mut z)?;
        }
        for (entry, p) in files {
            z.start_file(format!("attachments/{entry}"), opts)?;
            std::io::copy(&mut std::fs::File::open(p)?, &mut z)?;
        }
        z.finish()?.flush()?;
        Ok(())
    })
    .await??;
    let key = BlobKey::tm_job(&job.id.to_string(), "export.zip")?;
    s.blobs.put_file(&key, &zip_path).await?;
    s.store
        .finish_tm_job(
            job.id,
            owner,
            &job.project_key,
            TmJobStatus::Done,
            TmJobProgress {
                done: total,
                total: Some(total),
            },
            None,
            Some(json!({"blob": key.to_string()})),
        )
        .await?;
    Ok(())
}

/// The parameters of a new file import as stored (for the API).
pub fn file_params(
    opts: &fastproof_store::tm::ImportOptions,
    file_name: Option<String>,
    blob: String,
    zip: Option<String>,
) -> Value {
    serde_json::to_value(ImportParams {
        source: "file".into(),
        format: Some(opts.format),
        mapping: opts.mapping.clone(),
        steps_layout: opts.steps_layout,
        mode: Some(opts.mode),
        target_folder_id: opts.target_folder_id,
        unknown_field_values: Some(opts.unknown_field_values),
        file_name,
        blob: Some(blob),
        zip,
        ..Default::default()
    })
    .unwrap_or_default()
}
