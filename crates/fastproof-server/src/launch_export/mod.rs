//! Launch export: a self-contained HTML report (print-optimised, so a browser prints it to
//! PDF) and an XLSX workbook of a launch's results. Results are read page by page and written
//! to a spool file, so memory stays bounded; launches above [`MAX_SYNC_RESULTS`] are exported
//! by a background job whose file goes to blob storage for 24 hours.

pub mod html;
pub mod xlsx;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use fastproof_blob::BlobKey;
use fastproof_store::results::ResultFilter;
use fastproof_store::{Launch, ResultSummary};
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::api::results::{ExplorerQuery, ResultQuery, result_filter};
use crate::state::AppState;

/// Results an inline export may hold.
pub const MAX_SYNC_RESULTS: i64 = 10_000;
/// Results any export may hold (a worksheet has 1 048 576 rows).
pub const MAX_EXPORT_RESULTS: i64 = 500_000;
/// Bytes an export file may have (uncompressed content), at most: `FASTPROOF_LAUNCH_EXPORT_MAX_BYTES`.
pub const DEFAULT_MAX_EXPORT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Seconds between heartbeats of a running job (file generation and upload).
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);
/// Results read per page.
pub const PAGE: i64 = 500;
/// Bytes of the filter text of a request, at most.
pub const MAX_QUERY_BYTES: usize = 4096;
/// Background exports a replica runs at once.
const WORKERS: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Html,
    Xlsx,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "html" => Some(Self::Html),
            "xlsx" => Some(Self::Xlsx),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Xlsx => "xlsx",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }
    }
}

/// Parses the filter part of a request (the query string of the result list) and checks it.
pub async fn parse_filter(
    s: &AppState,
    launch: &Launch,
    raw: &str,
) -> franshub_http::ApiResult<ResultFilter> {
    let (q, ex) = parse_queries(raw)?;
    let mut f = result_filter(s, launch, q, &ex).await?;
    f.before = None;
    f.limit = PAGE;
    Ok(f)
}

fn parse_queries(raw: &str) -> franshub_http::ApiResult<(ResultQuery, ExplorerQuery)> {
    use axum::extract::Query;
    let uri: axum::http::Uri = format!("/?{raw}")
        .parse()
        .map_err(|_| franshub_http::ApiError::bad_request("invalid filter"))?;
    let q = Query::<ResultQuery>::try_from_uri(&uri).map_err(|e| {
        franshub_http::ApiError::bad_request(format!("invalid query: {}", e.body_text()))
    })?;
    let ex = Query::<ExplorerQuery>::try_from_uri(&uri).map_err(|e| {
        franshub_http::ApiError::bad_request(format!("invalid query: {}", e.body_text()))
    })?;
    Ok((q.0, ex.0))
}

/// The `format` and the filter of a request's query string (`format` and the paging and
/// `inline` parameters are not part of the filter).
pub fn split_query(raw: &str) -> (Option<String>, bool, String) {
    let mut format = None;
    let mut inline = false;
    let mut rest = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
        match k.as_ref() {
            "format" => format = Some(v.into_owned()),
            "inline" => inline = v == "true",
            "before" | "limit" => {}
            _ => {
                rest.append_pair(&k, &v);
            }
        }
    }
    (format, inline, rest.finish())
}

pub struct TempFile(pub PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub struct Generated {
    pub file: TempFile,
    pub results: i64,
}

fn spool(s: &AppState, ext: &str) -> anyhow::Result<PathBuf> {
    let dir = s.cfg.spool_dir.join("launch-exports");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}.{ext}", Uuid::now_v7())))
}

/// The byte cap of one export file.
pub fn max_export_bytes() -> u64 {
    std::env::var("FASTPROOF_LAUNCH_EXPORT_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_EXPORT_BYTES)
}

/// Removes everything a crashed process left in the export spool (called at start, when no
/// export of this process can be running).
pub fn sweep_spool(spool_dir: &Path) {
    let dir = spool_dir.join("launch-exports");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

/// Why a generation stopped.
#[derive(Debug)]
pub enum Stop {
    TooMany(i64),
    TooBig(u64),
    Cancelled,
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::TooMany(n) => write!(f, "the export has more than {n} results"),
            Stop::TooBig(n) => write!(f, "the export file would exceed {n} bytes"),
            Stop::Cancelled => write!(f, "the export was cancelled"),
        }
    }
}

impl std::error::Error for Stop {}

struct Pager<'a> {
    s: &'a AppState,
    launch: &'a Launch,
    filter: ResultFilter,
    max: i64,
    job: Option<Uuid>,
    n: i64,
    last: bool,
}

impl<'a> Pager<'a> {
    fn new(
        s: &'a AppState,
        launch: &'a Launch,
        filter: ResultFilter,
        max: i64,
        job: Option<Uuid>,
    ) -> Self {
        Self {
            s,
            launch,
            filter,
            max,
            job,
            n: 0,
            last: false,
        }
    }

    /// The next page and the number of results read so far; `None` after the last one.
    async fn next(&mut self) -> anyhow::Result<Option<(Vec<ResultSummary>, i64)>> {
        if self.last {
            return Ok(None);
        }
        let page = self.s.store.results(self.launch, &self.filter).await?;
        self.n += page.items.len() as i64;
        if self.n > self.max {
            return Err(Stop::TooMany(self.max).into());
        }
        if let Some(id) = self.job
            && !self.s.store.progress_launch_export(id, self.n).await?
        {
            return Err(Stop::Cancelled.into());
        }
        match page.next {
            Some(c) => self.filter.before = Some(c),
            None => self.last = true,
        }
        Ok(Some((page.items, self.n)))
    }
}

/// Writes the export of `launch` to a spool file.
#[allow(clippy::too_many_arguments)]
pub async fn generate(
    s: &AppState,
    project_key: &str,
    launch: &Launch,
    filter: ResultFilter,
    filter_text: &str,
    format: Format,
    max: i64,
    job: Option<Uuid>,
) -> anyhow::Result<Generated> {
    let path = spool(s, format.as_str())?;
    let file = TempFile(path.clone());
    let results = match format {
        Format::Html => {
            write_html(s, project_key, launch, filter, filter_text, &path, max, job).await?
        }
        Format::Xlsx => {
            write_xlsx(s, project_key, launch, filter, filter_text, &path, max, job).await?
        }
    };
    Ok(Generated { file, results })
}

#[allow(clippy::too_many_arguments)]
async fn write_html(
    s: &AppState,
    project_key: &str,
    launch: &Launch,
    filter: ResultFilter,
    filter_text: &str,
    path: &Path,
    max: i64,
    job: Option<Uuid>,
) -> anyhow::Result<i64> {
    let mut out = tokio::io::BufWriter::new(tokio::fs::File::create(path).await?);
    let head = html::head(launch, project_key, filter_text);
    let cap = max_export_bytes();
    let mut bytes = head.len() as u64;
    out.write_all(head.as_bytes()).await?;
    let mut pager = Pager::new(s, launch, filter, max, job);
    let mut n = 0;
    while let Some((items, done)) = pager.next().await? {
        n = done;
        let mut chunk = String::with_capacity(items.len() * 400);
        for r in &items {
            chunk.push_str(&html::row(r));
        }
        bytes += chunk.len() as u64;
        if bytes > cap {
            return Err(Stop::TooBig(cap).into());
        }
        out.write_all(chunk.as_bytes()).await?;
    }
    let generated = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    out.write_all(html::tail(n, crate::VERSION, &generated).as_bytes())
        .await?;
    out.flush().await?;
    Ok(n)
}

const RESULT_HEADERS: [&str; 16] = [
    "Status",
    "Test",
    "Suite",
    "Duration (ms)",
    "Started at",
    "Attempts",
    "Flaky in run",
    "Flaky",
    "Muted",
    "Transition",
    "Defect",
    "Defect group",
    "Issue",
    "Labels",
    "Message",
    "Result id",
];

fn result_row(n: usize, r: &ResultSummary) -> String {
    use xlsx::Cell::{Empty, Num, Text};
    let suite = r.suite_path.join(" / ");
    let started = r
        .started_at
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default();
    let yes = |b: bool| if b { "yes" } else { "no" };
    let labels = r
        .labels
        .iter()
        .take(50)
        .map(|l| format!("{}={}", l.name, l.value))
        .collect::<Vec<_>>()
        .join("; ");
    let id = r.id.to_string();
    let (dname, dgroup, dissue) = match &r.defect {
        Some(d) => (
            d.name.as_str(),
            d.group.as_str(),
            d.issue_key
                .as_deref()
                .or(d.issue_url.as_deref())
                .unwrap_or(""),
        ),
        None => ("", "", ""),
    };
    xlsx::row_xml(
        n,
        &[
            Text(&r.status),
            Text(&r.full_name),
            Text(&suite),
            r.duration_ms.map(Num).unwrap_or(Empty),
            Text(&started),
            Num(r.attempt_count as i64),
            Text(yes(r.flaky_in_run)),
            Text(yes(r.flaky)),
            Text(yes(r.muted)),
            Text(r.transition.as_deref().unwrap_or("")),
            Text(dname),
            Text(dgroup),
            Text(dissue),
            Text(&labels),
            Text(r.message.as_deref().unwrap_or("")),
            Text(&id),
        ],
        false,
    )
}

#[allow(clippy::too_many_arguments)]
async fn write_xlsx(
    s: &AppState,
    project_key: &str,
    launch: &Launch,
    filter: ResultFilter,
    filter_text: &str,
    path: &Path,
    max: i64,
    job: Option<Uuid>,
) -> anyhow::Result<i64> {
    use xlsx::Cell::{Num, Text};
    let file = std::fs::File::create(path)?;
    let mut wb = xlsx::Workbook::create(file, &["Summary", "Results"])?;
    let c = &launch.counts;
    let started = launch.started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let finished = launch
        .finished_at
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default();
    let number = format!("#{}", launch.number);
    let duration = html::duration(launch.duration_ms);
    let vcs = |f: fn(&fastproof_store::LaunchVcs) -> Option<&String>| {
        launch.vcs.as_ref().and_then(f).cloned().unwrap_or_default()
    };
    let branch = vcs(|v| v.branch.as_ref());
    let commit = vcs(|v| v.commit.as_ref());
    let env = launch
        .environment
        .as_ref()
        .and_then(|e| e.key.clone())
        .unwrap_or_default();
    let generated = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut summary = String::new();
    let mut row = 1usize;
    let mut kv = |k: &str, v: xlsx::Cell<'_>, summary: &mut String| {
        summary.push_str(&xlsx::row_xml(row, &[Text(k), v], k.is_empty()));
        row += 1;
    };
    for (k, v) in [
        ("Project", project_key),
        ("Launch", number.as_str()),
        ("Name", launch.name.as_str()),
        ("Status", launch.status.as_str()),
        ("Started", started.as_str()),
        ("Finished", finished.as_str()),
        ("Duration", duration.as_str()),
        ("Environment", env.as_str()),
        ("Branch", branch.as_str()),
        ("Commit", commit.as_str()),
        ("Filter", filter_text),
        ("Generated", generated.as_str()),
    ] {
        kv(k, Text(v), &mut summary);
    }
    for (k, v) in [
        ("Total", c.total),
        ("Passed", c.passed),
        ("Failed", c.failed),
        ("Broken", c.broken),
        ("Skipped", c.skipped),
        ("Unknown", c.unknown),
        ("Flaky", c.flaky),
        ("Muted", c.muted),
    ] {
        kv(k, Num(v), &mut summary);
    }
    wb.start_sheet(1, &[16.0, 60.0], false)?;
    wb.rows(&summary)?;
    wb.end_sheet(None)?;
    wb.start_sheet(
        2,
        &[
            10.0, 60.0, 30.0, 14.0, 22.0, 9.0, 12.0, 8.0, 8.0, 14.0, 20.0, 16.0, 16.0, 40.0, 80.0,
            38.0,
        ],
        true,
    )?;
    let headers: Vec<xlsx::Cell<'_>> = RESULT_HEADERS.iter().map(|h| Text(h)).collect();
    wb.rows(&xlsx::row_xml(1, &headers, true))?;
    let wb = Arc::new(Mutex::new(Some(wb)));
    let mut pager = Pager::new(s, launch, filter, max, job);
    let cap = max_export_bytes();
    let mut bytes = 0u64;
    let mut n = 0i64;
    while let Some((items, done)) = pager.next().await? {
        n = done;
        let first = done as usize - items.len() + 2;
        let mut xml = String::with_capacity(items.len() * 600);
        for (i, r) in items.iter().enumerate() {
            xml.push_str(&result_row(first + i, r));
        }
        bytes += xml.len() as u64;
        if bytes > cap {
            return Err(Stop::TooBig(cap).into());
        }
        let wb = wb.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut g = wb.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_mut() {
                Some(w) => w.rows(&xml),
                None => Ok(()),
            }
        })
        .await??;
    }
    let wb = wb.clone();
    let last = (n as usize + 1).max(2);
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut g = wb.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut w) = g.take() {
            w.end_sheet(Some(&format!(
                "A1:{}{last}",
                xlsx::column(RESULT_HEADERS.len() - 1)
            )))?;
            w.finish()?;
        }
        Ok(())
    })
    .await??;
    Ok(n)
}

fn workers(instance: &str) -> Arc<Semaphore> {
    static ALL: Mutex<Option<std::collections::HashMap<String, Arc<Semaphore>>>> = Mutex::new(None);
    let mut g = ALL.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Default::default)
        .entry(instance.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(WORKERS)))
        .clone()
}

/// Runs a queued job on this replica (when a worker slot frees up).
pub fn spawn_job(s: &AppState, id: Uuid) {
    let s = s.clone();
    tokio::spawn(async move {
        let Ok(_permit) = workers(&s.instance).acquire_owned().await else {
            return;
        };
        if let Err(e) = run_job(&s, id).await {
            tracing::warn!(job = %id, error = %format!("{e:#}"), "launch export failed");
            let msg = match e.downcast_ref::<Stop>() {
                Some(stop) => stop.to_string(),
                None => "the export failed".to_string(),
            };
            let _ = s.store.fail_launch_export(id, &msg).await;
        }
    });
}

async fn run_job(s: &AppState, id: Uuid) -> anyhow::Result<()> {
    let Some(job) = s.store.start_launch_export(id).await? else {
        return Ok(());
    };
    let launch = s
        .store
        .launch(job.project_id, job.export.launch_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the launch is gone"))?;
    let format = Format::parse(&job.export.format).ok_or_else(|| anyhow::anyhow!("bad format"))?;
    let filter = parse_filter(s, &launch, &job.query)
        .await
        .map_err(|e| anyhow::anyhow!("invalid filter: {e:?}"))?;
    let _beat = Heartbeat::start(s, id);
    let g = generate(
        s,
        &job.project_key,
        &launch,
        filter,
        &job.query,
        format,
        MAX_EXPORT_RESULTS,
        Some(id),
    )
    .await?;
    let key = BlobKey::tm_job(
        &id.to_string(),
        &format!("launch-export.{}", format.as_str()),
    )?;
    s.store.set_launch_export_blob(id, key.as_str()).await?;
    let size = s.blobs.put_file(&key, &g.file.0).await?;
    complete(s, id, &key, g.results, size as i64).await
}

/// Marks the job done; when it is not running any more (it was failed while the file was
/// uploaded), the uploaded file is deleted so no object is left without a row.
pub async fn complete(
    s: &AppState,
    id: Uuid,
    key: &BlobKey,
    results: i64,
    size: i64,
) -> anyhow::Result<()> {
    if !s
        .store
        .finish_launch_export(id, results, key.as_str(), size)
        .await?
    {
        s.blobs.delete(key).await?;
        s.store.expire_launch_export(id).await?;
    }
    Ok(())
}

/// Keeps a running job alive while its file is built and uploaded; stops when dropped.
struct Heartbeat(tokio::task::JoinHandle<()>);

impl Heartbeat {
    fn start(s: &AppState, id: Uuid) -> Self {
        let s = s.clone();
        Self(tokio::spawn(async move {
            let mut t = tokio::time::interval(HEARTBEAT);
            t.tick().await;
            loop {
                t.tick().await;
                let _ = s.store.touch_launch_export(id).await;
            }
        }))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The scheduler job: fails lost jobs and removes expired files. A file that cannot be deleted
/// is logged and retried later; it never stops the others.
pub async fn tick(s: &AppState) -> anyhow::Result<()> {
    s.store.fail_stale_launch_exports().await?;
    for (id, key) in s.store.due_launch_exports(100).await? {
        if let Ok(k) = BlobKey::parse(&key)
            && let Err(e) = s.blobs.delete(&k).await
        {
            tracing::warn!(job = %id, error = %format!("{e:#}"), "deleting an expired launch export failed");
            metrics::counter!("fastproof_launch_export_delete_failures_total").increment(1);
            continue;
        }
        s.store.expire_launch_export(id).await?;
    }
    s.store.delete_old_launch_exports().await?;
    Ok(())
}
