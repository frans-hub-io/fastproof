//! Ingest parsers of FastProof: allure-results, JUnit XML, CTRF and Playwright JSON.
//!
//! Entry point: [`ingest_path`]. The input is a filesystem path (a single report file, a
//! directory, a zip, a tar or a tar.gz archive). Reports are parsed into the
//! [`fastproof_model`] types; attachment files referenced by them are streamed to an
//! [`AttachmentSink`]. Every expansion is charged to one byte budget and all parsers are
//! bounded by [`Limits`]. The API is synchronous; call it from a blocking thread.

mod allure;
mod ctrf;
mod ctx;
mod error;
mod json;
mod junit;
mod limits;
mod playwright;
mod report;
mod resolve;
mod sink;
mod source;
mod text;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use fastproof_model::{
    AttachmentId, AttachmentRef, Category, Container, Globals, LaunchMeta, ParsedResult,
    RetryGroup, SourceFormat, group_retries,
};
use serde::{Deserialize, Serialize};

pub use allure::attach_fixtures;
pub use ctx::Warning;
pub use error::IngestError;
pub use json::{JsonBounds, JsonError, parse_json_bounded, scan_json};
pub use limits::Limits;
pub use report::{
    MAX_ATTACHMENTS_PER_RESULT, MAX_CLIENT_ID_BYTES, ReportOptions, report_to_parsed,
};
pub use sink::{AttachmentSink, MemoryAttachment, MemorySink, NullSink};
pub use source::UploadKind;
pub use text::sanitize_html;

use crate::ctx::Ctx;
use crate::limits::{Budget, read_bounded};
use crate::resolve::FileIndex;
use crate::source::Upload;

/// Options of one ingest call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestOptions {
    /// Forces the format instead of auto-detection.
    pub format: Option<SourceFormat>,
    pub limits: Limits,
    /// Original file name of a single-file upload (the spooled temp file may be unnamed).
    pub file_name: Option<String>,
    /// Named environment of the launch; part of every series key (Allure 3 environment).
    pub environment_key: Option<String>,
    /// Extra workspace prefixes stripped from source paths (e.g. the CI checkout dir).
    pub workspace_prefixes: Vec<String>,
    /// Drop masked/hidden parameter values after identity hashing (default true).
    pub redact_secret_parameters: bool,
    /// Also pass files that no report references to the sink (default false).
    pub store_unreferenced: bool,
    /// Attach container fixtures to results of the same upload (default true).
    pub attach_fixtures: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            format: None,
            limits: Limits::default(),
            file_name: None,
            environment_key: None,
            workspace_prefixes: Vec::new(),
            redact_secret_parameters: true,
            store_unreferenced: false,
            attach_fixtures: true,
        }
    }
}

impl IngestOptions {
    pub(crate) fn path_prefixes(&self) -> Vec<&str> {
        self.workspace_prefixes.iter().map(String::as_str).collect()
    }
}

/// An attachment body passed to the sink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAttachment {
    pub id: AttachmentId,
    /// Upload-relative path, or `inline/<n>/<name>` for inline bodies.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Declared size.
    pub size: u64,
    /// False for unreferenced files stored because of `store_unreferenced`.
    pub referenced: bool,
    pub inline: bool,
}

/// A file of the upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadFile {
    pub path: String,
    pub size: u64,
}

/// A file that was deliberately not parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoredFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub files: u64,
    pub documents: u64,
    pub results: u64,
    pub containers: u64,
    pub steps: u64,
    pub attachment_refs: u64,
    pub attachments_stored: u64,
    pub attachments_missing: u64,
    pub unreferenced_files: u64,
    pub ignored_files: u64,
    pub warnings: u64,
    /// Bytes charged to the expansion budget.
    pub bytes_charged: u64,
}

/// Everything parsed from one upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestBatch {
    pub upload_kind: UploadKind,
    /// Detected (or forced) format; `None` when the upload contained no report.
    pub format: Option<SourceFormat>,
    pub launch_meta: LaunchMeta,
    /// One entry per attempt, in ingest order. Group them with [`IngestBatch::retry_groups`].
    pub results: Vec<ParsedResult>,
    /// Allure containers, also when their fixtures were attached to results of this batch
    /// (their children may arrive in another upload).
    pub containers: Vec<Container>,
    pub globals: Globals,
    pub categories: Vec<Category>,
    pub attachments: Vec<StoredAttachment>,
    /// Files no report referenced (not passed to the sink unless `store_unreferenced`).
    pub unreferenced: Vec<UploadFile>,
    pub warnings: Vec<Warning>,
    pub ignored: Vec<IgnoredFile>,
    pub counts: Counts,
}

impl IngestBatch {
    /// Retry groups of the results of this batch (by series key).
    pub fn retry_groups(&self) -> Vec<RetryGroup> {
        group_retries(&self.results)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FileClass {
    AllureResult,
    AllureContainer,
    AllureGlobals,
    AllureExecutor,
    AllureCategories,
    AllureEnvironment,
    Ignored(&'static str),
    JsonCandidate,
    XmlCandidate,
    Other,
}

fn classify(path: &str) -> FileClass {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let parent = path.rsplit('/').nth(1).unwrap_or("");
    if lower.starts_with(".allure-write-") || (lower.starts_with('.') && lower.ends_with(".tmp")) {
        return FileClass::Ignored("allure temporary file");
    }
    if parent == "history" && lower.ends_with(".json") {
        return FileClass::Ignored("allure history (import is a separate feature)");
    }
    if lower.ends_with("-result.json") {
        return FileClass::AllureResult;
    }
    if lower.ends_with("-container.json") {
        return FileClass::AllureContainer;
    }
    if lower.ends_with("-globals.json") {
        return FileClass::AllureGlobals;
    }
    if lower.ends_with("-check.json")
        || lower.ends_with("-perf.json")
        || lower.ends_with("-metric.json")
        || lower == "performance.json"
    {
        return FileClass::Ignored("allure 3 extension file not supported");
    }
    match lower.as_str() {
        "executor.json" => return FileClass::AllureExecutor,
        "categories.json" => return FileClass::AllureCategories,
        "environment.properties" => return FileClass::AllureEnvironment,
        "environment.xml" => {
            return FileClass::Ignored("legacy Allure 1 environment.xml not supported");
        }
        _ => {}
    }
    if lower.contains("-attachment") {
        return FileClass::Other;
    }
    if lower.ends_with(".json") {
        return FileClass::JsonCandidate;
    }
    if lower.ends_with(".xml") {
        return FileClass::XmlCandidate;
    }
    FileClass::Other
}

#[derive(Default)]
struct Collected {
    allure_results: Vec<ParsedResult>,
    containers: Vec<Container>,
    globals: Globals,
    executor: Option<fastproof_model::Executor>,
    categories: Vec<Category>,
    environment: Vec<fastproof_model::KeyValue>,
    allure_files: Vec<String>,
    junit: Vec<(String, junit::JunitDoc)>,
    ctrf: Vec<(String, ctrf::CtrfDoc)>,
    playwright: Vec<(String, playwright::PlaywrightDoc)>,
    report_files: HashSet<String>,
    /// Results collected so far, kept up to date instead of recounted per entry.
    result_count: u64,
}

fn read_doc(r: &mut dyn Read, limits: &Limits, path: &str) -> Result<Vec<u8>, IngestError> {
    read_bounded(r, limits.max_document_bytes, "max_document_bytes", path)
}

/// Reads a JSON/XML candidate; a document larger than `max_document_bytes` is a limit
/// error only when its head looks like a report, otherwise it is treated as a plain file.
fn read_candidate(
    r: &mut dyn Read,
    limits: &Limits,
    path: &str,
    xml: bool,
) -> Result<Option<Vec<u8>>, IngestError> {
    let mut buf = Vec::new();
    let n = r
        .take(limits.max_document_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(IngestError::from_io)?;
    if n as u64 <= limits.max_document_bytes {
        return Ok(Some(buf));
    }
    let head = String::from_utf8_lossy(&buf[..buf.len().min(64 * 1024)]);
    let report = if xml {
        junit::is_junit(&head)
    } else {
        head.contains("\"reportFormat\"")
            || (head.contains("\"config\"") && head.contains("\"rootDir\""))
    };
    if report {
        Err(IngestError::limit_at(
            "max_document_bytes",
            limits.max_document_bytes,
            path.to_string(),
        ))
    } else {
        Ok(None)
    }
}

fn parse_entry(
    path: &str,
    class: FileClass,
    r: &mut dyn Read,
    ctx: &mut Ctx<'_>,
    col: &mut Collected,
) -> Result<(), IngestError> {
    let limits = ctx.limits;
    match class {
        FileClass::AllureResult
        | FileClass::AllureContainer
        | FileClass::AllureGlobals
        | FileClass::AllureExecutor
        | FileClass::AllureCategories => {
            let bytes = read_doc(r, limits, path)?;
            col.report_files.insert(path.to_string());
            col.allure_files.push(path.to_string());
            let v = match json::parse(&bytes, limits, path)? {
                Ok(v) => v,
                Err(e) => {
                    ctx.warn(Some(path), format!("invalid JSON: {e}"));
                    return Ok(());
                }
            };
            match class {
                FileClass::AllureResult => {
                    if let Some(res) = allure::parse_result(&v, path, ctx)? {
                        col.allure_results.push(res);
                        col.result_count += 1;
                    }
                }
                FileClass::AllureContainer => {
                    if let Some(c) = allure::parse_container(&v, path, ctx)? {
                        col.containers.push(c);
                    }
                }
                FileClass::AllureGlobals => {
                    let g = allure::parse_globals(&v, path, ctx)?;
                    col.globals.errors.extend(g.errors);
                    col.globals.attachments.extend(g.attachments);
                }
                FileClass::AllureExecutor => col.executor = Some(allure::parse_executor(&v)),
                FileClass::AllureCategories => {
                    col.categories = allure::parse_categories(&v, path, ctx)
                }
                _ => {}
            }
        }
        FileClass::AllureEnvironment => {
            let bytes = read_doc(r, limits, path)?;
            col.report_files.insert(path.to_string());
            col.allure_files.push(path.to_string());
            col.environment
                .extend(allure::parse_properties(&String::from_utf8_lossy(&bytes)));
        }
        FileClass::JsonCandidate => {
            let Some(bytes) = read_candidate(r, limits, path, false)? else {
                return Ok(());
            };
            let trimmed = bytes
                .iter()
                .position(|b| !b.is_ascii_whitespace())
                .map(|i| bytes[i]);
            if !matches!(trimmed, Some(b'{') | Some(0xEF)) {
                return Ok(());
            }
            let v = match json::parse(&bytes, limits, path)? {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            if ctrf::is_ctrf(&v) {
                col.report_files.insert(path.to_string());
                let d = ctrf::parse(&v, path, ctx)?;
                col.result_count += d.results.len() as u64;
                col.ctrf.push((path.to_string(), d));
            } else if playwright::is_playwright(&v) {
                col.report_files.insert(path.to_string());
                let d = playwright::parse(&v, path, ctx)?;
                col.result_count += d.results.len() as u64;
                col.playwright.push((path.to_string(), d));
            }
        }
        FileClass::XmlCandidate => {
            let Some(bytes) = read_candidate(r, limits, path, true)? else {
                return Ok(());
            };
            let text = String::from_utf8_lossy(&bytes);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            if !junit::is_junit(head_of(text, 64 * 1024)) {
                return Ok(());
            }
            col.report_files.insert(path.to_string());
            match junit::parse(text, path, ctx)? {
                Ok(doc) => {
                    col.result_count += doc.results.len() as u64;
                    col.junit.push((path.to_string(), doc));
                }
                Err(e) => ctx.warn(Some(path), e),
            }
        }
        FileClass::Ignored(_) | FileClass::Other => {}
    }
    if col.result_count > limits.max_results {
        return Err(IngestError::limit_at(
            "max_results",
            limits.max_results,
            path.to_string(),
        ));
    }
    Ok(())
}

/// Keys already present in a merged [`LaunchMeta`], so merging many documents stays linear.
#[derive(Default)]
struct MetaKeys {
    environment: HashSet<String>,
    attributes: HashSet<String>,
}

fn merge_meta(into: &mut LaunchMeta, keys: &mut MetaKeys, from: LaunchMeta) {
    if into.name.is_none() {
        into.name = from.name;
    }
    if into.ci.is_empty() {
        into.ci = from.ci;
    }
    if into.vcs.is_empty() {
        into.vcs = from.vcs;
    }
    if into.environment_key.is_none() {
        into.environment_key = from.environment_key;
    }
    if keys.environment.len() != into.environment.len() {
        keys.environment = into.environment.iter().map(|e| e.key.clone()).collect();
    }
    if keys.attributes.len() != into.attributes.len() {
        keys.attributes = into.attributes.iter().map(|e| e.key.clone()).collect();
    }
    for kv in from.environment {
        if keys.environment.insert(kv.key.clone()) {
            into.environment.push(kv);
        }
    }
    for kv in from.attributes {
        if keys.attributes.insert(kv.key.clone()) {
            into.attributes.push(kv);
        }
    }
    into.start = match (into.start, from.start) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    into.stop = match (into.stop, from.stop) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn for_each_ref(
    results: &mut [ParsedResult],
    containers: &mut [Container],
    globals: &mut Globals,
    f: &mut dyn FnMut(&mut AttachmentRef, &str),
) {
    for r in results.iter_mut() {
        let dir = r
            .source_file
            .as_deref()
            .map(dir_of)
            .unwrap_or("")
            .to_string();
        r.visit_attachments_mut(&mut |a| f(a, &dir));
    }
    for c in containers.iter_mut() {
        let dir = c
            .source_file
            .as_deref()
            .map(dir_of)
            .unwrap_or("")
            .to_string();
        for s in c.befores.iter_mut().chain(c.afters.iter_mut()) {
            s.visit_attachments_mut(&mut |a| f(a, &dir));
        }
    }
    for a in globals.attachments.iter_mut() {
        let dir = a
            .resolved_path
            .as_deref()
            .map(dir_of)
            .unwrap_or("")
            .to_string();
        f(a, &dir);
    }
}

/// Parses an upload and streams the referenced attachments to `sink`.
pub fn ingest_path(
    path: &Path,
    options: &IngestOptions,
    sink: &mut dyn AttachmentSink,
) -> Result<IngestBatch, IngestError> {
    let limits = &options.limits;
    let upload = Upload::open(path, options.file_name.as_deref(), limits)?;
    let budget = Budget::new(limits.max_total_uncompressed_bytes);
    let mut ctx = Ctx::new(options, budget.clone());
    let mut col = Collected::default();
    let mut index = FileIndex::default();
    let mut files: Vec<UploadFile> = Vec::new();
    let mut ignored: Vec<IgnoredFile> = Vec::new();
    let mut documents = 0u64;

    // Pass 1: index every file and parse the reports.
    {
        let mut skipped = |p: &str, reason: &'static str| {
            ignored.push(IgnoredFile {
                path: p.to_string(),
                reason: reason.to_string(),
            })
        };
        let mut ignored_classes: Vec<IgnoredFile> = Vec::new();
        upload.for_each(
            limits,
            &budget,
            &|_| true,
            &mut skipped,
            &mut |p, size, r| {
                index.insert(p);
                files.push(UploadFile {
                    path: p.to_string(),
                    size,
                });
                let class = match (options.format, upload.kind, classify(p)) {
                    (_, UploadKind::File, FileClass::Other) => {
                        let lower = p.to_ascii_lowercase();
                        if lower.ends_with(".xml") || options.format == Some(SourceFormat::JunitXml)
                        {
                            FileClass::XmlCandidate
                        } else {
                            FileClass::JsonCandidate
                        }
                    }
                    (_, _, c) => c,
                };
                if let FileClass::Ignored(reason) = class {
                    ignored_classes.push(IgnoredFile {
                        path: p.to_string(),
                        reason: reason.to_string(),
                    });
                    return Ok(());
                }
                if !matches!(class, FileClass::Other) {
                    documents += 1;
                }
                parse_entry(p, class, r, &mut ctx, &mut col)
            },
        )?;
        ignored.extend(ignored_classes);
    }

    // Choose the format.
    let has_allure =
        !col.allure_results.is_empty() || !col.containers.is_empty() || !col.globals.is_empty();
    let detected = if has_allure {
        Some(SourceFormat::AllureResults)
    } else if !col.playwright.is_empty() {
        Some(SourceFormat::PlaywrightJson)
    } else if !col.ctrf.is_empty() {
        Some(SourceFormat::Ctrf)
    } else if !col.junit.is_empty() {
        Some(SourceFormat::JunitXml)
    } else {
        None
    };
    let format = options.format.or(detected);
    let mut meta = LaunchMeta::default();
    let mut results: Vec<ParsedResult> = Vec::new();
    let mut containers: Vec<Container> = Vec::new();
    let mut globals = Globals::default();
    let mut categories = Vec::new();
    let ignore_docs = |paths: Vec<String>, what: &str, ignored: &mut Vec<IgnoredFile>| {
        for p in paths {
            ignored.push(IgnoredFile {
                path: p,
                reason: format!(
                    "{what} report ignored: upload parsed as {}",
                    format.map(SourceFormat::as_str).unwrap_or("none")
                ),
            });
        }
    };
    let Collected {
        allure_results,
        containers: allure_containers,
        globals: allure_globals,
        executor,
        categories: allure_categories,
        environment,
        allure_files,
        junit,
        ctrf,
        playwright,
        report_files,
        result_count: _,
    } = col;
    let mut meta_keys = MetaKeys::default();
    if format == Some(SourceFormat::AllureResults) {
        results = allure_results;
        containers = allure_containers;
        globals = allure_globals;
        categories = allure_categories;
        meta.environment = environment;
        if let Some(e) = executor {
            meta.ci.provider = e.executor_type.clone();
            meta.ci.url = e.build_url.clone().or(e.url.clone());
            meta.ci.build_order = e.build_order;
            meta.ci.pipeline = e.build_name.clone();
            meta.name = e.report_name.clone().or(e.build_name.clone());
            meta.executor = Some(e);
        }
    } else {
        ignore_docs(allure_files, "allure", &mut ignored);
    }
    if format == Some(SourceFormat::JunitXml) {
        for (_, d) in junit {
            if meta.name.is_none() {
                meta.name = d.name;
            }
            merge_meta(
                &mut meta,
                &mut meta_keys,
                LaunchMeta {
                    attributes: d.attributes,
                    start: d.start,
                    ..LaunchMeta::default()
                },
            );
            results.extend(d.results);
        }
    } else {
        ignore_docs(
            junit.into_iter().map(|d| d.0).collect(),
            "junit",
            &mut ignored,
        );
    }
    if format == Some(SourceFormat::Ctrf) {
        for (_, d) in ctrf {
            merge_meta(&mut meta, &mut meta_keys, d.meta);
            results.extend(d.results);
        }
    } else {
        ignore_docs(
            ctrf.into_iter().map(|d| d.0).collect(),
            "ctrf",
            &mut ignored,
        );
    }
    let keep_inline = format == Some(SourceFormat::PlaywrightJson);
    if keep_inline {
        for (_, d) in playwright {
            merge_meta(&mut meta, &mut meta_keys, d.meta);
            globals.errors.extend(d.errors);
            results.extend(d.results);
        }
    } else {
        ignore_docs(
            playwright.into_iter().map(|d| d.0).collect(),
            "playwright",
            &mut ignored,
        );
        ctx.inline.clear();
    }
    if format.is_none() {
        ctx.warn(None, "no supported test report found in the upload");
    }
    if let Some(env) = options.environment_key.as_deref() {
        meta.environment_key.get_or_insert_with(|| env.to_string());
    }

    // Pass 2: resolve and stream referenced attachments.
    let mut wanted: HashMap<String, Option<String>> = HashMap::new();
    let mut refs = 0u64;
    for_each_ref(
        &mut results,
        &mut containers,
        &mut globals,
        &mut |a, dir| {
            refs += 1;
            if a.resolved_path
                .as_deref()
                .is_some_and(|p| p.starts_with("inline/"))
                && a.id.is_none()
                && a.source.starts_with("inline/")
            {
                return;
            }
            match index.resolve(&a.source, a.resolved_path.as_deref(), dir) {
                Some(p) => {
                    wanted
                        .entry(p.clone())
                        .or_insert_with(|| a.content_type.clone());
                    a.resolved_path = Some(p);
                }
                None => a.resolved_path = None,
            }
        },
    );
    let referenced: HashSet<String> = wanted.keys().cloned().collect();
    let ignored_paths: HashSet<&str> = ignored.iter().map(|i| i.path.as_str()).collect();
    if options.store_unreferenced {
        for f in &files {
            if !report_files.contains(&f.path) && !ignored_paths.contains(f.path.as_str()) {
                wanted.entry(f.path.clone()).or_insert(None);
            }
        }
    }
    if (wanted.len() + ctx.inline.len()) as u64 > limits.max_attachments {
        return Err(IngestError::limit(
            "max_attachments",
            limits.max_attachments,
        ));
    }
    let mut stored: Vec<StoredAttachment> = Vec::new();
    let mut ids: HashMap<String, (AttachmentId, u64)> = HashMap::new();
    if !wanted.is_empty() {
        let mut none = |_: &str, _: &'static str| {};
        upload.for_each(
            limits,
            &budget,
            &|p| wanted.contains_key(p),
            &mut none,
            &mut |p, size, r| {
                if ids.contains_key(p) {
                    return Ok(());
                }
                let ct = wanted
                    .get(p)
                    .cloned()
                    .flatten()
                    .or_else(|| text::content_type_for(p).map(str::to_string));
                let id = sink.put(p, ct.as_deref(), size, r)?;
                stored.push(StoredAttachment {
                    id: id.clone(),
                    path: p.to_string(),
                    content_type: ct,
                    size,
                    referenced: referenced.contains(p),
                    inline: false,
                });
                ids.insert(p.to_string(), (id, size));
                Ok(())
            },
        )?;
    }
    for inl in std::mem::take(&mut ctx.inline) {
        let size = inl.bytes.len() as u64;
        let id = sink.put(
            &inl.key,
            inl.content_type.as_deref(),
            size,
            &mut inl.bytes.as_slice(),
        )?;
        stored.push(StoredAttachment {
            id: id.clone(),
            path: inl.key.clone(),
            content_type: inl.content_type,
            size,
            referenced: true,
            inline: true,
        });
        ids.insert(inl.key, (id, size));
    }
    let mut missing = 0u64;
    for_each_ref(
        &mut results,
        &mut containers,
        &mut globals,
        &mut |a, _| match a.resolved_path.as_ref().and_then(|p| ids.get(p)) {
            Some((id, size)) => {
                a.id = Some(id.clone());
                a.size = a.size.or(Some(*size));
                a.missing = false;
            }
            None => {
                a.missing = true;
                missing += 1;
            }
        },
    );

    if options.attach_fixtures && format == Some(SourceFormat::AllureResults) {
        let b = budget.clone();
        allure::attach_fixtures_with(
            &mut results,
            &containers,
            limits.max_steps_per_result,
            &mut |n, c| b.charge(n, c),
        )?;
    }

    let unreferenced: Vec<UploadFile> = files
        .iter()
        .filter(|f| {
            !report_files.contains(&f.path)
                && !referenced.contains(&f.path)
                && !ignored_paths.contains(f.path.as_str())
        })
        .cloned()
        .collect();
    if ctx.suppressed_warnings > 0 {
        let n = ctx.suppressed_warnings;
        ctx.warnings.push(Warning {
            file: None,
            message: format!("{n} more warnings suppressed"),
        });
    }
    let counts = Counts {
        files: files.len() as u64,
        documents,
        results: results.len() as u64,
        containers: containers.len() as u64,
        steps: results.iter().map(|r| r.step_count() as u64).sum(),
        attachment_refs: refs,
        attachments_stored: stored.len() as u64,
        attachments_missing: missing,
        unreferenced_files: unreferenced.len() as u64,
        ignored_files: ignored.len() as u64,
        warnings: ctx.warnings.len() as u64,
        bytes_charged: budget.used(),
    };
    Ok(IngestBatch {
        upload_kind: upload.kind,
        format,
        launch_meta: meta,
        results,
        containers,
        globals,
        categories,
        attachments: stored,
        unreferenced,
        warnings: ctx.warnings,
        ignored,
        counts,
    })
}

/// Longest prefix of at most `max` bytes that ends on a char boundary.
fn head_of(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The visitor of [`for_each_entry`]: normalized path, declared size, bounded reader.
pub type EntryVisit<'v> = dyn FnMut(&str, u64, &mut dyn Read) -> Result<(), IngestError> + 'v;

/// Streams every regular file of an archive (zip, tar, tar.gz), a directory or a single file
/// with the upload rules: entry paths are normalized and rejected when absolute, with `..`,
/// a drive letter or a NUL byte; symbolic links are refused; entry counts and sizes and the
/// uncompressed byte budget (`max_total_uncompressed_bytes`, charged for every byte read) are
/// enforced. `visit` gets the normalized path, the declared size and a bounded reader.
pub fn for_each_entry(
    path: &Path,
    file_name: Option<&str>,
    limits: &Limits,
    visit: &mut EntryVisit<'_>,
) -> Result<UploadKind, IngestError> {
    let upload = Upload::open(path, file_name, limits)?;
    let budget = Budget::new(limits.max_total_uncompressed_bytes);
    let mut skipped = |_: &str, _: &'static str| {};
    upload.for_each(limits, &budget, &|_| true, &mut skipped, visit)?;
    Ok(upload.kind)
}
