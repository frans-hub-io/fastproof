//! Results: the streaming ingest of FastProof report JSON, lists, the tree, details and history.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use bytes::{Bytes, BytesMut};
use fastproof_ingest::{
    IngestError, JsonBounds, ReportOptions, name_warnings, report_to_parsed, scan_json,
};
use fastproof_model::{AttachmentKind, ReportResult};
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::results::{
    ATTACHMENT_FILTERS, Baseline, EnvFilter, EnvironmentCounts, GroupBy, LabelName, LaunchFacets,
    MAX_GROUP_LEVELS, MAX_LABEL_NAME_BYTES, ResultFilter, TreeFilter, TreeOptions,
    valid_label_name,
};
use fastproof_store::tm::ResultSource;
use fastproof_store::{
    DailyEntry, HistoryEntry, HistoryFilter, Launch, Page, ResultDetail, ResultSummary, TreeNode,
    TreeTest,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::uploads::{identity_encoding, too_large};
use super::{ErrorBody, P, Q, key_value, page_limit};
use crate::limits::heavy_error;
use crate::state::AppState;

/// Results persisted per transaction.
pub const CHUNK: usize = 500;
/// Rejections listed in a response, at most.
pub const MAX_REJECTED: usize = 1000;
/// Bytes of one NDJSON line, at most (and at most `max_document_bytes`).
pub const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
/// Lines up to this size are parsed on the request's task; longer ones on a blocking thread.
const INLINE_PARSE_BYTES: usize = 256 * 1024;
/// Bytes of a `q` filter, at most.
pub const MAX_Q_BYTES: usize = 256;

/// A result that was not accepted.
#[derive(Serialize, ToSchema)]
pub struct Rejected {
    /// Position in the array or line number (zero-based) in NDJSON.
    pub index: u64,
    pub message: String,
}

/// Outcome of `POST .../results`.
#[derive(Serialize, ToSchema, Default)]
pub struct IngestSummary {
    pub accepted: u64,
    pub duplicates: u64,
    pub rejected: Vec<Rejected>,
    /// Accepted results that were changed to fit a limit (a name cut to `max_name_bytes`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Rejected>,
}

struct ResultBatch<'a> {
    s: &'a AppState,
    launch: LaunchRef,
    env_key: Option<String>,
    pending: Vec<(u64, ReportResult)>,
    summary: IngestSummary,
    seen: u64,
    rejected_more: u64,
}

impl ResultBatch<'_> {
    fn reject(&mut self, index: u64, message: String) {
        if self.summary.rejected.len() < MAX_REJECTED {
            self.summary.rejected.push(Rejected { index, message });
        } else {
            self.rejected_more += 1;
        }
    }

    async fn push(&mut self, index: u64, r: Result<ReportResult, String>) -> ApiResult<()> {
        self.seen += 1;
        if self.seen > self.s.limits.max_results {
            return Err(too_large("max_results", self.s.limits.max_results));
        }
        match r {
            Ok(r) => self.pending.push((index, r)),
            Err(e) => self.reject(index, e),
        }
        if self.pending.len() >= CHUNK {
            self.flush().await?;
        }
        Ok(())
    }

    async fn flush(&mut self) -> ApiResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        let originals: Option<Vec<ReportResult>> = self
            .s
            .kafka_gateway()
            .then(|| pending.iter().map(|(_, r)| r.clone()).collect());
        let prep = prepare_reports(
            self.s,
            self.launch.project_id,
            self.env_key.as_deref(),
            pending,
        )
        .await?;
        for r in prep.rejected {
            self.reject(r.index, r.message);
        }
        for w in prep.warnings {
            if self.summary.warnings.len() < MAX_REJECTED {
                self.summary.warnings.push(w);
            }
        }
        if let Some(originals) = originals {
            let produced = crate::stream::produce_results(
                self.s,
                &self.launch,
                prep.accepted
                    .iter()
                    .map(|(pos, index, a)| {
                        (
                            *index,
                            originals[*pos].clone(),
                            a.result.identity.series_key.clone(),
                        )
                    })
                    .collect(),
            )
            .await?;
            for (index, message) in produced.rejected {
                self.reject(index, message);
            }
            self.summary.accepted += produced.accepted;
            return Ok(());
        }
        let attempts: Vec<NewAttempt> = prep.accepted.into_iter().map(|(_, _, a)| a).collect();
        self.s
            .store
            .link_attachments(&prep.used, self.launch.id)
            .await?;
        let out = self
            .s
            .store
            .persist_attempts(&self.launch, None, attempts)
            .await?;
        // A running line completed in the same batch was applied too: it is not a duplicate.
        self.summary.accepted += out.accepted + out.superseded;
        self.summary.duplicates += out.duplicates;
        metrics::counter!("fastproof_results_ingested_total", "source" => "api")
            .increment(out.accepted);
        Ok(())
    }
}

/// Results converted for persisting: `(position in the input, caller's index, attempt)`.
pub struct PreparedReports {
    pub accepted: Vec<(usize, u64, NewAttempt)>,
    /// Attachment rows the accepted results reference.
    pub used: Vec<Uuid>,
    pub rejected: Vec<Rejected>,
    /// Accepted results changed to fit a limit.
    pub warnings: Vec<Rejected>,
}

/// Validates report results and resolves their attachment references (unknown attachments
/// reject the result). Used by the API and by the Kafka writer.
pub async fn prepare_reports(
    s: &AppState,
    project_id: Uuid,
    env_key: Option<&str>,
    pending: Vec<(u64, ReportResult)>,
) -> anyhow::Result<PreparedReports> {
    let ids: Vec<Uuid> = pending
        .iter()
        .flat_map(|(_, r)| {
            let mut v: Vec<Uuid> = r
                .attachments
                .iter()
                .filter_map(|a| a.id.parse().ok())
                .collect();
            fn walk(steps: &[fastproof_model::ReportStep], v: &mut Vec<Uuid>) {
                for s in steps {
                    v.extend(
                        s.attachments
                            .iter()
                            .filter_map(|a| a.id.parse::<Uuid>().ok()),
                    );
                    walk(&s.steps, v);
                }
            }
            walk(&r.steps, &mut v);
            v
        })
        .collect();
    let known = s.store.attachments_in_project(project_id, &ids).await?;
    let opts = ReportOptions {
        limits: &s.limits,
        environment_key: env_key,
    };
    let mut out = PreparedReports {
        accepted: Vec::with_capacity(pending.len()),
        used: Vec::new(),
        rejected: Vec::new(),
        warnings: Vec::new(),
    };
    for (pos, (index, r)) in pending.into_iter().enumerate() {
        let client = r.id.trim().to_string();
        let warnings = name_warnings(&r, &s.limits);
        let mut parsed = match report_to_parsed(r, &opts) {
            Ok(p) => p,
            Err(m) => {
                out.rejected.push(Rejected { index, message: m });
                continue;
            }
        };
        let mut missing = None;
        let mut used = Vec::new();
        parsed.visit_attachments_mut(&mut |a| {
            let id = a.id.as_ref().and_then(|i| i.0.parse::<Uuid>().ok());
            match id.and_then(|i| known.get(&i)) {
                Some((i, size, ct, name)) => {
                    a.size = Some(*size as u64);
                    if a.content_type.is_none() {
                        a.content_type = Some(ct.clone());
                    }
                    if a.name.is_empty() {
                        a.name = name.clone().unwrap_or_else(|| "attachment".into());
                    }
                    a.kind = AttachmentKind::infer(a.content_type.as_deref(), &a.name);
                    used.push(*i);
                }
                None => missing = Some(a.source.trim_start_matches("attachment:").to_string()),
            }
        });
        if let Some(m) = missing {
            out.rejected.push(Rejected {
                index,
                message: format!("unknown attachment id {m}"),
            });
            continue;
        }
        out.used.extend(used);
        out.warnings.extend(
            warnings
                .into_iter()
                .map(|message| Rejected { index, message }),
        );
        out.accepted.push((
            pos,
            index,
            NewAttempt {
                client_key: format!("r:{client}"),
                result: parsed,
            },
        ));
    }
    out.used.sort();
    out.used.dedup();
    Ok(out)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/results", tag = "results", operation_id = "results_ingest",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    description = "Adds results to a launch. A caller without `cases:sync` makes the launch untrusted for code sync, \
        and may add to a finished launch only if it created it (else 409 `launch_finished`).",
    request_body(content = ReportResult, description = "One ReportResult, a JSON array of them, or NDJSON (application/x-ndjson), see schemas/fastproof-report-v1.json"),
    responses((status = 202, body = IngestSummary), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody),
        (status = 415, body = ErrorBody)))]
pub async fn ingest(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<IngestSummary>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    identity_encoding(&req)?;
    s.note_plan_contributor(&auth, &project, &launch).await?;
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let ndjson = matches!(
        ct.as_str(),
        "application/x-ndjson" | "application/ndjson" | "application/jsonl"
    );
    if !ndjson && ct != "application/json" {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "expected Content-Type application/json or application/x-ndjson",
        ));
    }
    let mut batch = ResultBatch {
        s: &s,
        env_key: launch.environment.as_ref().and_then(|e| e.key.clone()),
        launch: launch_ref(&project.key, &launch),
        pending: Vec::new(),
        summary: IngestSummary::default(),
        seen: 0,
        rejected_more: 0,
    };
    let max_total = s.cfg.max_upload_bytes;
    let max_doc = s.limits.max_document_bytes;
    let bounds = JsonBounds::from(&s.limits);
    let mut values_left = s.limits.max_json_values;
    let mut body = req.into_body().into_data_stream();
    let mut total = 0u64;
    if ndjson {
        let max_line = max_doc.min(MAX_LINE_BYTES);
        let mut buf = BytesMut::new();
        let mut scanned = 0usize;
        let mut line_no = 0u64;
        loop {
            let chunk = body.next().await;
            let done = chunk.is_none();
            if let Some(c) = chunk {
                let c =
                    c.map_err(|e| ApiError::bad_request(format!("reading the body failed: {e}")))?;
                total += c.len() as u64;
                if total > max_total {
                    return Err(too_large("max_upload_bytes", max_total));
                }
                buf.extend_from_slice(&c);
            }
            // Only the bytes after the previous scan are searched for the next newline.
            while let Some(rel) = buf[scanned..].iter().position(|b| *b == b'\n') {
                let pos = scanned + rel;
                let mut line = buf.split_to(pos + 1).freeze();
                line.truncate(pos);
                scanned = 0;
                handle_line(&mut batch, line_no, line, &bounds, &mut values_left).await?;
                line_no += 1;
            }
            scanned = buf.len();
            if buf.len() as u64 > max_line {
                return Err(too_large("max_document_bytes", max_line));
            }
            if done {
                if !buf.is_empty() {
                    let line = buf.split().freeze();
                    handle_line(&mut batch, line_no, line, &bounds, &mut values_left).await?;
                }
                break;
            }
        }
    } else {
        let mut buf = BytesMut::new();
        while let Some(c) = body.next().await {
            let c =
                c.map_err(|e| ApiError::bad_request(format!("reading the body failed: {e}")))?;
            if (buf.len() + c.len()) as u64 > max_doc {
                return Err(too_large("max_document_bytes", max_doc));
            }
            buf.extend_from_slice(&c);
        }
        let bytes = buf.freeze();
        let items = tokio::task::spawn_blocking(move || parse_body(&bytes, &bounds))
            .await
            .map_err(|e| anyhow::anyhow!("the parse thread failed: {e}"))??;
        for (i, r) in items.into_iter().enumerate() {
            batch.push(i as u64, r).await?;
        }
    }
    batch.flush().await?;
    if batch.rejected_more > 0 {
        let more = batch.rejected_more;
        batch.summary.rejected.push(Rejected {
            index: u64::MAX,
            message: format!("{more} more results rejected"),
        });
    }
    Ok((StatusCode::ACCEPTED, Json(batch.summary)))
}

/// The 413 of a JSON bound (`max_json_values`, `max_json_depth` or `max_string_bytes`).
pub fn json_limit_error(e: &IngestError) -> ApiError {
    match e {
        IngestError::LimitExceeded { limit, max, .. } => too_large(limit, *max),
        other => ApiError::bad_request(other.to_string()),
    }
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

fn describe(e: &serde_json::Error) -> String {
    match e.classify() {
        serde_json::error::Category::Data => format!("invalid result: {e}"),
        _ => format!("invalid JSON: {e}"),
    }
}

/// A JSON body: one result or an array of them, after the bounds check. Arrays are read as
/// typed results; only when that fails are the elements looked at one by one to tell which
/// are invalid.
fn parse_body(bytes: &[u8], bounds: &JsonBounds) -> ApiResult<Vec<Result<ReportResult, String>>> {
    scan_json(bytes, bounds, "body").map_err(|e| json_limit_error(&e))?;
    let b = strip_bom(bytes);
    let not_json =
        |e: serde_json::Error| ApiError::bad_request(format!("the body is not valid JSON: {e}"));
    match b.iter().find(|c| !c.is_ascii_whitespace()) {
        Some(b'[') => match serde_json::from_slice::<Vec<ReportResult>>(b) {
            Ok(v) => Ok(v.into_iter().map(Ok).collect()),
            Err(_) => {
                let items: Vec<serde_json::Value> = serde_json::from_slice(b).map_err(not_json)?;
                Ok(items
                    .into_iter()
                    .map(|v| {
                        serde_json::from_value::<ReportResult>(v)
                            .map_err(|e| format!("invalid result: {e}"))
                    })
                    .collect())
            }
        },
        Some(b'{') => match serde_json::from_slice::<ReportResult>(b) {
            Ok(r) => Ok(vec![Ok(r)]),
            Err(e) if e.classify() == serde_json::error::Category::Data => {
                Ok(vec![Err(format!("invalid result: {e}"))])
            }
            Err(e) => Err(not_json(e)),
        },
        _ => {
            serde_json::from_slice::<serde::de::IgnoredAny>(b).map_err(not_json)?;
            Err(ApiError::bad_request("expected a JSON object or array"))
        }
    }
}

/// One NDJSON line: bounds check (charged to the request's value budget), then a typed parse.
fn parse_line(line: &[u8], bounds: &JsonBounds) -> (u64, Result<ReportResult, String>) {
    match scan_json(line, bounds, "line") {
        Err(e) => (0, Err(e.to_string())),
        Ok(n) => (
            n,
            serde_json::from_slice::<ReportResult>(strip_bom(line)).map_err(|e| describe(&e)),
        ),
    }
}

async fn handle_line(
    batch: &mut ResultBatch<'_>,
    line_no: u64,
    line: Bytes,
    bounds: &JsonBounds,
    values_left: &mut u64,
) -> ApiResult<()> {
    let line = match line.strip_suffix(b"\r") {
        Some(l) => line.slice(..l.len()),
        None => line,
    };
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(());
    }
    let (n, r) = if line.len() <= INLINE_PARSE_BYTES {
        parse_line(&line, bounds)
    } else {
        let bounds = *bounds;
        tokio::task::spawn_blocking(move || parse_line(&line, &bounds))
            .await
            .map_err(|e| anyhow::anyhow!("the parse thread failed: {e}"))?
    };
    if n > *values_left {
        return Err(too_large("max_json_values", batch.s.limits.max_json_values));
    }
    *values_left -= n;
    batch.push(line_no, r).await
}

pub fn launch_ref(project_key: &str, l: &Launch) -> LaunchRef {
    LaunchRef {
        id: l.id,
        project_id: l.project_id,
        project_key: project_key.to_string(),
        created_at: l.created_at,
    }
}

/// Query of the result list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ResultQuery {
    /// One status or a comma-separated list.
    pub status: Option<String>,
    /// Substring of the name, or words of the error message.
    pub q: Option<String>,
    /// A suite in the suite path.
    pub suite: Option<String>,
    /// `name:value` label.
    pub label: Option<String>,
    pub flaky: Option<bool>,
    pub fingerprint: Option<String>,
    /// `manual` (projected from run items) or `automated`.
    pub source: Option<ResultSource>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// The filter of a result list (or export) from its two query parts, validated.
pub(crate) async fn result_filter(
    s: &AppState,
    launch: &Launch,
    q: ResultQuery,
    explorer: &ExplorerQuery,
) -> ApiResult<ResultFilter> {
    let statuses: Vec<String> = q
        .status
        .as_deref()
        .map(|v| {
            v.split(',')
                .map(|x| x.trim().to_ascii_lowercase())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if statuses.iter().any(|x| {
        !matches!(
            x.as_str(),
            "passed" | "failed" | "broken" | "skipped" | "unknown" | "running"
        )
    }) {
        return Err(ApiError::bad_request(
            "status must be passed, failed, broken, skipped, unknown or running",
        ));
    }
    for (name, v) in [
        ("q", &q.q),
        ("suite", &q.suite),
        ("label", &q.label),
        ("fingerprint", &q.fingerprint),
    ] {
        if v.as_ref().is_some_and(|v| v.len() > MAX_Q_BYTES) {
            return Err(ApiError::bad_request(format!(
                "{name} has at most {MAX_Q_BYTES} bytes"
            )));
        }
    }
    let f = ResultFilter {
        statuses,
        q: q.q.filter(|v| !v.trim().is_empty()),
        suite: q.suite,
        label: key_value(q.label.as_deref(), "label")?,
        flaky: q.flaky,
        fingerprint: q.fingerprint,
        source: q.source,
        extra: explorer.filter(s, launch).await?,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(f)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/results", tag = "results", operation_id = "results_list",
    params(("key" = String, Path), ("id" = Uuid, Path), ResultQuery, ExplorerQuery),
    responses((status = 200, body = Page<ResultSummary>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ResultQuery>,
    Q(explorer): Q<ExplorerQuery>,
) -> ApiResult<Json<Page<ResultSummary>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let f = result_filter(&s, &launch, q, &explorer).await?;
    Ok(Json(
        s.store.results(&launch, &f).await.map_err(heavy_error)?,
    ))
}

/// The explorer filters shared by the tree, its pages, the result list and the label list.
/// Lists are comma-separated, a comma inside a value is written `%2C` and a percent sign `%25`;
/// values within one filter are alternatives, filters combine.
#[derive(Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ExplorerQuery {
    /// Words of the error message (whole words, case-insensitive).
    pub message: Option<String>,
    /// `true`: only results with more than one attempt.
    pub retried: Option<String>,
    /// `true` or `false`.
    pub muted: Option<String>,
    /// `true`: failed or broken, not muted results whose series did not fail in the previous
    /// finished launch on the same branch and environment.
    pub new_failures: Option<String>,
    /// Severity label values; `%00` matches results without the label.
    pub severity: Option<String>,
    /// Owner label values; `%00` matches results without the label.
    pub owner: Option<String>,
    /// Tag label values; `%00` matches results without the label.
    pub tag: Option<String>,
    pub min_duration_ms: Option<String>,
    pub max_duration_ms: Option<String>,
    /// Defect type ids; `none` matches results without a defect.
    pub defect: Option<String>,
    /// `trace`, `video` or `screenshot`.
    pub attachment: Option<String>,
    /// An environment key of the results' series (`%2C` for a comma, `%25` for a percent sign); `%00` matches results without one.
    pub env: Option<String>,
}

/// Values of one list filter, at most.
pub const MAX_FILTER_VALUES: usize = 20;
/// The item of a label filter, or the `env`, that matches results without the label or without
/// an environment (`%00` in the URL). No stored value contains a NUL.
const NO_LABEL: &str = "\0";

/// Reads one item of a list filter: `%2C` is a comma, `%25` a percent sign, `%00` the marker
/// of a missing label; any other `%` stays as it is, so plain comma-separated lists work.
fn list_item(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let escape = tail.get(..3).map(str::to_ascii_uppercase);
        match escape.as_deref() {
            Some("%2C") => out.push(','),
            Some("%25") => out.push('%'),
            Some("%00") => out.push_str(NO_LABEL),
            _ => {
                out.push('%');
                rest = &tail[1..];
                continue;
            }
        }
        rest = &tail[3..];
    }
    out.push_str(rest);
    out
}

fn values(v: Option<&str>, name: &str) -> ApiResult<Vec<String>> {
    let Some(v) = v else { return Ok(Vec::new()) };
    if v.len() > MAX_Q_BYTES * 4 {
        return Err(ApiError::bad_request(format!("{name} is too long")));
    }
    let items: Vec<String> = v
        .split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(list_item)
        .collect();
    if items.len() > MAX_FILTER_VALUES {
        return Err(ApiError::bad_request(format!(
            "{name} has at most {MAX_FILTER_VALUES} values"
        )));
    }
    if items.iter().any(|x| x.len() > MAX_Q_BYTES) {
        return Err(ApiError::bad_request(format!(
            "values of {name} have at most {MAX_Q_BYTES} bytes"
        )));
    }
    Ok(items)
}

fn flag(v: Option<&str>, name: &str) -> ApiResult<Option<bool>> {
    match v {
        None | Some("") => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(ApiError::bad_request(format!(
            "{name} must be true or false"
        ))),
    }
}

fn millis(v: Option<&str>, name: &str) -> ApiResult<Option<i64>> {
    match v.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (0..=i64::MAX / 2).contains(n))
            .map(Some)
            .ok_or_else(|| {
                ApiError::bad_request(format!("{name} must be a number of milliseconds"))
            }),
    }
}

/// The `env` filter: an environment key (`%2C` for a comma, `%25` for a percent sign), or
/// `%00` for results without one.
pub(crate) fn env_filter(v: Option<&str>) -> ApiResult<Option<EnvFilter>> {
    match v.map(str::trim).filter(|e| !e.is_empty()) {
        None => Ok(None),
        Some(e) if e.len() > MAX_Q_BYTES => Err(ApiError::bad_request(format!(
            "env has at most {MAX_Q_BYTES} bytes"
        ))),
        Some(e) => Ok(Some(match list_item(e) {
            k if k == NO_LABEL => EnvFilter::None,
            k => EnvFilter::Key(k),
        })),
    }
}

impl ExplorerQuery {
    /// The filters, after validation; `newFailures` resolves the baseline launch.
    async fn filter(&self, s: &AppState, launch: &Launch) -> ApiResult<TreeFilter> {
        if self.message.as_ref().is_some_and(|v| v.len() > MAX_Q_BYTES) {
            return Err(ApiError::bad_request(format!(
                "message has at most {MAX_Q_BYTES} bytes"
            )));
        }
        let mut labels = Vec::new();
        for (name, v) in [
            ("severity", &self.severity),
            ("owner", &self.owner),
            ("tag", &self.tag),
        ] {
            let values: Vec<String> = values(v.as_deref(), name)?
                .into_iter()
                .map(|x| if x == NO_LABEL { String::new() } else { x })
                .collect();
            if !values.is_empty() {
                labels.push((name.to_string(), values));
            }
        }
        let defects = values(self.defect.as_deref(), "defect")?;
        if defects
            .iter()
            .any(|d| d != "none" && d.parse::<Uuid>().is_err())
        {
            return Err(ApiError::bad_request(
                "defect must list defect type ids or none",
            ));
        }
        let mut attachments = 0;
        for a in values(self.attachment.as_deref(), "attachment")? {
            let bit = ATTACHMENT_FILTERS
                .iter()
                .find(|(n, _)| *n == a)
                .map(|(_, b)| *b)
                .ok_or_else(|| {
                    ApiError::bad_request("attachment must be trace, video or screenshot")
                })?;
            attachments |= bit;
        }
        let env = env_filter(self.env.as_deref())?;
        let min = millis(self.min_duration_ms.as_deref(), "minDurationMs")?;
        let max = millis(self.max_duration_ms.as_deref(), "maxDurationMs")?;
        let new_failures = if flag(self.new_failures.as_deref(), "newFailures")? == Some(true) {
            Some(baseline(s, launch).await?)
        } else {
            None
        };
        Ok(TreeFilter {
            message: self.message.clone().filter(|m| !m.trim().is_empty()),
            retried: flag(self.retried.as_deref(), "retried")? == Some(true),
            muted: flag(self.muted.as_deref(), "muted")?,
            new_failures,
            labels,
            min_duration_ms: min,
            max_duration_ms: max,
            defects,
            attachments,
            env,
            ..TreeFilter::default()
        })
    }
}

/// The launch "new failures" compares with: the previous finished launch on the same branch
/// and environment (none without a branch).
async fn baseline(s: &AppState, launch: &Launch) -> ApiResult<Baseline> {
    let Some(branch) = crate::integrations::gates::branch_of(launch) else {
        return Ok(Baseline { launch: None });
    };
    let b = s
        .store
        .baseline_launch(
            launch.project_id,
            branch,
            crate::integrations::gates::env_of(launch),
            launch.id,
            launch.created_at,
        )
        .await?;
    Ok(Baseline {
        launch: b.map(|l| (l.id, l.created_at)),
    })
}

/// Query of the tree.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TreeQuery {
    /// `suite` (default), `feature` or `package`, or up to five label names separated by
    /// commas (`epic,feature,story`, `owner`); `label:<name>` names a label that is also a
    /// preset (`label:feature`).
    pub group_by: Option<String>,
    /// Leaves (`tests`, `testCount`) instead of `resultIds`; groups that do not fit the
    /// response are read from `.../tree/tests`.
    pub include_tests: Option<bool>,
    /// One status or a comma-separated list; counts are those of the matching results.
    pub status: Option<String>,
    /// Substring of the test name, full name or suite path.
    pub q: Option<String>,
    /// Only results flaky in this run.
    pub flaky: Option<bool>,
    /// `manual` or `automated`.
    pub source: Option<ResultSource>,
    /// A result whose group is inlined first (to reveal a deep-linked result).
    pub reveal: Option<Uuid>,
}

/// Query of the tests of one tree group.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TreeTestsQuery {
    /// As in the tree.
    pub group_by: Option<String>,
    /// The group: its `path` as a JSON array of names (`[]` for the root).
    pub path: String,
    pub status: Option<String>,
    pub q: Option<String>,
    pub flaky: Option<bool>,
    /// `manual` or `automated`.
    pub source: Option<ResultSource>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// Bytes of the `path` of a tree group, at most.
pub const MAX_TREE_PATH_BYTES: usize = 8 * 1024;
/// Names in the `path` of a tree group, at most.
pub const MAX_TREE_PATH_NAMES: usize = 64;

fn group_by(v: Option<&str>) -> ApiResult<GroupBy> {
    match v.unwrap_or("suite").trim() {
        "suite" | "" => return Ok(GroupBy::Suite),
        "feature" => return Ok(GroupBy::Feature),
        "package" => return Ok(GroupBy::Package),
        _ => {}
    }
    let v = v.unwrap_or_default();
    let names: Vec<String> = v
        .split(',')
        .map(str::trim)
        .map(|n| n.strip_prefix("label:").unwrap_or(n).to_string())
        .collect();
    if names.is_empty() || names.len() > MAX_GROUP_LEVELS {
        return Err(ApiError::bad_request(format!(
            "groupBy has 1 to {MAX_GROUP_LEVELS} label names"
        )));
    }
    if !names.iter().all(|n| valid_label_name(n)) {
        return Err(ApiError::bad_request(format!(
            "groupBy must be suite, feature, package or label names of at most \
             {MAX_LABEL_NAME_BYTES} letters, digits and _ - . : @ /"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    if !names.iter().all(|n| seen.insert(n.as_str())) {
        return Err(ApiError::bad_request("groupBy names a label twice"));
    }
    Ok(GroupBy::Labels(names))
}

fn statuses(v: Option<&str>) -> ApiResult<Vec<String>> {
    let list: Vec<String> = v
        .map(|v| {
            v.split(',')
                .map(|x| x.trim().to_ascii_lowercase())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if list.iter().any(|x| {
        !matches!(
            x.as_str(),
            "passed" | "failed" | "broken" | "skipped" | "unknown" | "running"
        )
    }) {
        return Err(ApiError::bad_request(
            "status must be passed, failed, broken, skipped, unknown or running",
        ));
    }
    Ok(list)
}

async fn tree_filter(
    s: &AppState,
    launch: &Launch,
    status: Option<&str>,
    q: Option<String>,
    flaky: Option<bool>,
    source: Option<ResultSource>,
    explorer: &ExplorerQuery,
) -> ApiResult<TreeFilter> {
    if q.as_ref().is_some_and(|v| v.len() > MAX_Q_BYTES) {
        return Err(ApiError::bad_request(format!(
            "q has at most {MAX_Q_BYTES} bytes"
        )));
    }
    Ok(TreeFilter {
        statuses: statuses(status)?,
        q: q.filter(|v| !v.trim().is_empty()),
        flaky: flaky.filter(|f| *f),
        source,
        ..explorer.filter(s, launch).await?
    })
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/tree", tag = "results", operation_id = "results_tree",
    params(("key" = String, Path), ("id" = Uuid, Path), TreeQuery, ExplorerQuery),
    responses((status = 200, body = TreeNode), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn tree(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<TreeQuery>,
    Q(explorer): Q<ExplorerQuery>,
) -> ApiResult<Json<TreeNode>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let by = group_by(q.group_by.as_deref())?;
    let filter = tree_filter(
        &s,
        &launch,
        q.status.as_deref(),
        q.q,
        q.flaky,
        q.source,
        &explorer,
    )
    .await?;
    let o = TreeOptions {
        by,
        include_tests: q.include_tests.unwrap_or(false),
        filter,
        reveal: q.reveal,
    };
    Ok(Json(
        s.store.tree_with(&launch, &o).await.map_err(heavy_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/tree/tests", tag = "results", operation_id = "results_tree_tests",
    params(("key" = String, Path), ("id" = Uuid, Path), TreeTestsQuery, ExplorerQuery),
    responses((status = 200, body = Page<TreeTest>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn tree_tests(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<TreeTestsQuery>,
    Q(explorer): Q<ExplorerQuery>,
) -> ApiResult<Json<Page<TreeTest>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let by = group_by(q.group_by.as_deref())?;
    if q.path.len() > MAX_TREE_PATH_BYTES {
        return Err(ApiError::bad_request(format!(
            "path has at most {MAX_TREE_PATH_BYTES} bytes"
        )));
    }
    let path: Vec<String> = serde_json::from_str(&q.path)
        .map_err(|_| ApiError::bad_request("path must be a JSON array of strings"))?;
    if path.len() > MAX_TREE_PATH_NAMES {
        return Err(ApiError::bad_request(format!(
            "path has at most {MAX_TREE_PATH_NAMES} names"
        )));
    }
    if q.before
        .as_ref()
        .is_some_and(|v| v.len() > MAX_TREE_PATH_BYTES)
    {
        return Err(ApiError::bad_request("before is not a cursor"));
    }
    let f = tree_filter(
        &s,
        &launch,
        q.status.as_deref(),
        q.q,
        q.flaky,
        q.source,
        &explorer,
    )
    .await?;
    let limit = page_limit(q.limit)?;
    Ok(Json(
        s.store
            .tree_tests(&launch, &by, &path, &f, q.before.as_deref(), limit)
            .await
            .map_err(heavy_error)?,
    ))
}

/// Label names of a launch.
#[derive(Serialize, ToSchema)]
pub struct LabelNameList {
    /// Most used first, at most 200.
    pub items: Vec<LabelName>,
    /// Only the first results of a very large launch were read.
    pub partial: bool,
}

/// Query of the label names and the facets of a launch.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EnvQuery {
    /// An environment key (`%2C` for a comma, `%25` for a percent sign); `%00` for results without one.
    pub env: Option<String>,
}

impl EnvQuery {
    fn explorer(&self) -> ExplorerQuery {
        ExplorerQuery {
            env: self.env.clone(),
            ..ExplorerQuery::default()
        }
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/labels", tag = "results", operation_id = "results_labels",
    params(("key" = String, Path), ("id" = Uuid, Path), EnvQuery),
    description = "The label names of the results of a launch with how many results have each and how many distinct values it has: the choices of a custom tree grouping.",
    responses((status = 200, body = LabelNameList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn labels(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<EnvQuery>,
) -> ApiResult<Json<LabelNameList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let f = q.explorer().filter(&s, &launch).await?;
    let l = s
        .store
        .launch_labels(&launch, &f)
        .await
        .map_err(heavy_error)?;
    Ok(Json(LabelNameList {
        items: l.items,
        partial: l.partial,
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/facets", tag = "results", operation_id = "results_facets",
    params(("key" = String, Path), ("id" = Uuid, Path), EnvQuery),
    description = "How many results of a launch (in one environment) each explorer filter value keeps.",
    responses((status = 200, body = LaunchFacets), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn facets(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<EnvQuery>,
) -> ApiResult<Json<LaunchFacets>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let f = q.explorer().filter(&s, &launch).await?;
    let b = baseline(&s, &launch).await?;
    Ok(Json(
        s.store
            .launch_facets(&launch, &f, &b)
            .await
            .map_err(heavy_error)?,
    ))
}

/// The environments of a launch.
#[derive(Serialize, ToSchema)]
pub struct EnvironmentCountsList {
    /// Largest first, at most 100.
    pub items: Vec<EnvironmentCounts>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/environments", tag = "results", operation_id = "results_environments",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    description = "The environments the results of a launch ran in (the environment key of their series: the launch environment, and for some formats a project or target per result), with status counts.",
    responses((status = 200, body = EnvironmentCountsList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn environments(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<EnvironmentCountsList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    Ok(Json(EnvironmentCountsList {
        items: s
            .store
            .launch_environments(&launch)
            .await
            .map_err(heavy_error)?,
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}", tag = "results", operation_id = "results_detail",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = ResultDetail), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn detail(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<ResultDetail>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let d = s
        .store
        .result_detail(project.id, id, s.history.as_ref())
        .await?
        .ok_or_else(|| ApiError::not_found("result"))?;
    Ok(Json(d))
}

/// Query of a test case history.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub limit: Option<i64>,
}

/// History of a test case, newest first.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryList {
    pub test_case: fastproof_store::TestCaseView,
    pub items: Vec<HistoryEntry>,
    /// Daily aggregates before the oldest item, when the raw history is shorter than `limit`
    /// (launches beyond the raw window).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub daily: Vec<DailyEntry>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-cases/{testCaseId}/history", tag = "results", operation_id = "results_history",
    params(("key" = String, Path), ("testCaseId" = Uuid, Path), HistoryQuery),
    responses((status = 200, body = HistoryList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn history(
    State(s): State<AppState>,
    auth: Auth,
    P((key, test_case_id)): P<(String, Uuid)>,
    Q(q): Q<HistoryQuery>,
) -> ApiResult<Json<HistoryList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let test_case = s
        .store
        .test_case(project.id, test_case_id)
        .await?
        .ok_or_else(|| ApiError::not_found("test case"))?;
    let f = HistoryFilter {
        branch: q.branch,
        environment: q.environment,
        debug: false,
        limit: page_limit(q.limit)?,
    };
    let items = s
        .history
        .test_case_history(project.id, test_case_id, &f)
        .await?;
    let daily = if (items.len() as i64) < f.limit {
        let before = items
            .iter()
            .filter_map(|i| i.started_at)
            .min()
            .map(|t| t.date_naive());
        s.history
            .test_case_daily(project.id, test_case_id, &f, before)
            .await?
    } else {
        Vec::new()
    };
    Ok(Json(HistoryList {
        test_case,
        items,
        daily,
    }))
}
