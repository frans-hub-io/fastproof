//! Results: the streaming ingest of FastProof report JSON, lists, the tree, details and history.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use bytes::{Buf, BytesMut};
use fastproof_ingest::{ReportOptions, report_to_parsed};
use fastproof_model::{AttachmentKind, ReportResult};
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::results::{GroupBy, ResultFilter};
use fastproof_store::{
    HistoryEntry, HistoryFilter, Launch, Page, ResultDetail, ResultSummary, TreeNode,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::uploads::{identity_encoding, too_large};
use super::{ErrorBody, P, Q, key_value, page_limit};
use crate::state::AppState;

/// Results persisted per transaction.
pub const CHUNK: usize = 500;
/// Rejections listed in a response, at most.
pub const MAX_REJECTED: usize = 1000;

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

    async fn push_value(&mut self, index: u64, v: serde_json::Value) -> ApiResult<()> {
        self.seen += 1;
        if self.seen > self.s.limits.max_results {
            return Err(too_large("max_results", self.s.limits.max_results));
        }
        match serde_json::from_value::<ReportResult>(v) {
            Ok(r) => self.pending.push((index, r)),
            Err(e) => self.reject(index, format!("invalid result: {e}")),
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
        if let Some(originals) = originals {
            let produced = crate::stream::produce_results(
                self.s,
                &self.launch,
                prep.accepted
                    .iter()
                    .map(|(pos, index, _)| (*index, originals[*pos].clone()))
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
        let n = attempts.len() as u64;
        let out = self
            .s
            .store
            .persist_attempts(&self.launch, None, attempts)
            .await?;
        self.summary.accepted += out.accepted;
        self.summary.duplicates += n - out.accepted;
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
    };
    for (pos, (index, r)) in pending.into_iter().enumerate() {
        let client = r.id.trim().to_string();
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
            match id.and_then(|i| known.get(&i).map(|k| (i, k))) {
                Some((i, (size, ct, name))) => {
                    a.size = Some(*size as u64);
                    if a.content_type.is_none() {
                        a.content_type = Some(ct.clone());
                    }
                    if a.name.is_empty() {
                        a.name = name.clone().unwrap_or_else(|| "attachment".into());
                    }
                    a.kind = AttachmentKind::infer(a.content_type.as_deref(), &a.name);
                    used.push(i);
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

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/results", tag = "results",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    request_body(content = ReportResult, description = "One ReportResult, a JSON array of them, or NDJSON (application/x-ndjson), see schemas/fastproof-report-v1.json"),
    responses((status = 202, body = IngestSummary), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 415, body = ErrorBody)))]
pub async fn ingest(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<IngestSummary>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    identity_encoding(&req)?;
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
    let mut body = req.into_body().into_data_stream();
    let mut total = 0u64;
    if ndjson {
        let mut buf = BytesMut::new();
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
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.split_to(pos + 1);
                handle_line(&mut batch, line_no, &line[..pos]).await?;
                line_no += 1;
            }
            if buf.len() as u64 > max_doc {
                return Err(too_large("max_document_bytes", max_doc));
            }
            if done {
                if !buf.is_empty() {
                    let line = buf.split_to(buf.len());
                    handle_line(&mut batch, line_no, &line).await?;
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
        let v: serde_json::Value = serde_json::from_slice(buf.chunk())
            .map_err(|e| ApiError::bad_request(format!("the body is not valid JSON: {e}")))?;
        drop(buf);
        match v {
            serde_json::Value::Array(items) => {
                for (i, v) in items.into_iter().enumerate() {
                    batch.push_value(i as u64, v).await?;
                }
            }
            v @ serde_json::Value::Object(_) => batch.push_value(0, v).await?,
            _ => return Err(ApiError::bad_request("expected a JSON object or array")),
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

async fn handle_line(batch: &mut ResultBatch<'_>, line_no: u64, line: &[u8]) -> ApiResult<()> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(());
    }
    match serde_json::from_slice::<serde_json::Value>(line) {
        Ok(v) => batch.push_value(line_no, v).await,
        Err(e) => {
            batch.seen += 1;
            batch.reject(line_no, format!("invalid JSON: {e}"));
            Ok(())
        }
    }
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
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/results", tag = "results",
    params(("key" = String, Path), ("id" = Uuid, Path), ResultQuery),
    responses((status = 200, body = Page<ResultSummary>), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ResultQuery>,
) -> ApiResult<Json<Page<ResultSummary>>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
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
    let f = ResultFilter {
        statuses,
        q: q.q.filter(|v| !v.trim().is_empty()),
        suite: q.suite,
        label: key_value(q.label.as_deref(), "label")?,
        flaky: q.flaky,
        fingerprint: q.fingerprint,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(s.store.results(&launch, &f).await?))
}

/// Query of the tree.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TreeQuery {
    /// `suite` (default), `feature` or `package`.
    pub group_by: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/tree", tag = "results",
    params(("key" = String, Path), ("id" = Uuid, Path), TreeQuery),
    responses((status = 200, body = TreeNode), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn tree(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<TreeQuery>,
) -> ApiResult<Json<TreeNode>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let by = match q.group_by.as_deref().unwrap_or("suite") {
        "suite" => GroupBy::Suite,
        "feature" => GroupBy::Feature,
        "package" => GroupBy::Package,
        _ => {
            return Err(ApiError::bad_request(
                "groupBy must be suite, feature or package",
            ));
        }
    };
    Ok(Json(s.store.tree(&launch, by).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}", tag = "results",
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
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-cases/{testCaseId}/history", tag = "results",
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
    Ok(Json(HistoryList { test_case, items }))
}
