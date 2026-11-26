//! Requirement coverage and its exports (R, `74-tm-next-interfaces.md` §7.6, §9).

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_blob::BlobKey;
use fastproof_store::tm::coverage::CoverageRow;
use fastproof_store::tm::coverage::{CoverageExportParams, CoverageScope};
use fastproof_store::tm::import::ClaimedJob;
use fastproof_store::tm::requirements::number_of;
use fastproof_store::tm::{
    CoverageExportFormat, CoverageExportRequest, CoveragePage, TmJob, TmJobProgress, TmJobStatus,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use futures::StreamExt as _;
use rand::Rng as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(requirement_coverage))
        .routes(routes!(create_coverage_export))
        .routes(routes!(download_coverage_export))
}

/// Query of `GET /coverage`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct CoverageQuery {
    /// A test plan: its newest launch, or `launchId`.
    #[param(rename = "planId")]
    pub plan_id: Option<Uuid>,
    /// A launch of the plan (or alone: that launch).
    #[param(rename = "launchId")]
    pub launch_id: Option<Uuid>,
    /// Launch ids separated by commas, at most 50.
    #[param(rename = "launchIds")]
    pub launch_ids: Option<String>,
    /// Window scope: launches of this branch.
    pub branch: Option<String>,
    /// Window scope: launches of this environment key.
    pub environment: Option<String>,
    /// Window scope: launches of the last 1 to 365 days (30 with only a branch or environment).
    pub days: Option<i32>,
    /// A requirement number: its children; the top-level requirements without it.
    pub parent: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

fn launch_ids(raw: Option<&str>) -> ApiResult<Option<Vec<Uuid>>> {
    let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if raw.len() > 50 * 37 {
        return Err(ApiError::bad_request("launchIds names at most 50 launches"));
    }
    raw.split(',')
        .map(|p| {
            Uuid::parse_str(p.trim())
                .map_err(|_| ApiError::bad_request("launchIds holds launch ids"))
        })
        .collect::<ApiResult<Vec<_>>>()
        .map(Some)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/coverage", tag = "requirements", operation_id = "requirement_coverage",
    description = "Coverage of the children of `parent` (the top-level requirements without it), each with everything \
        under it. Scope: none (each case's last result), `planId` (its newest launch or `launchId`), `launchIds` (at most \
        50), or a window (`days`, `branch`, `environment`); kinds cannot be mixed. A case counts with its newest result \
        per parameter set in the scope, and of several parameter sets the worst. A scope other than the default is a \
        heavy read (429 or 503 when the caller or the server runs too many).",
    params(("key" = String, Path), CoverageQuery),
    responses((status = 200, body = CoveragePage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn requirement_coverage(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<CoverageQuery>,
) -> ApiResult<Json<CoveragePage>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let scope = CoverageScope {
        plan_id: q.plan_id,
        launch_id: q.launch_id,
        launch_ids: launch_ids(q.launch_ids.as_deref())?,
        branch: q.branch,
        environment: q.environment,
        days: q.days,
    };
    let parent = match q.parent.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(
            number_of(v)
                .ok_or_else(|| ApiError::bad_request("parent must be a requirement number"))?,
        ),
    };
    let limit = page_limit(q.limit)?;
    let _permit = match scope == CoverageScope::default() {
        true => None,
        false => Some(s.guards.heavy(&auth).await?),
    };
    Ok(Json(
        s.store
            .requirement_coverage(p.id, &scope, parent, q.before.as_deref(), limit)
            .await
            .map_err(tm_error)?,
    ))
}

fn token_hash(t: &str) -> String {
    hex::encode(Sha256::digest(t.as_bytes()))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/coverage/exports", tag = "requirements", operation_id = "create_coverage_export",
    description = "Starts a CSV or XLSX export of the coverage of every requirement (each with everything under it) in the \
        scope. The answer's `downloadUrl` (shown only here) is a one-time link for the caller, valid for 24 hours once \
        the job is `done`. A caller has at most 2 and a project at most 4 coverage exports queued or running \
        (409 `export_busy`).",
    request_body = CoverageExportRequest,
    params(("key" = String, Path)),
    responses((status = 202, body = TmJob), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_coverage_export(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CoverageExportRequest>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let _permit = s.guards.heavy(&auth).await?;
    if body.launch_ids.as_ref().is_some_and(|l| l.len() > 50) {
        return Err(ApiError::bad_request("launchIds names at most 50 launches"));
    }
    let token: String = {
        let mut b = [0u8; 32];
        rand::rng().fill(&mut b);
        hex::encode(b)
    };
    let params = CoverageExportParams {
        format: body.format,
        scope: CoverageScope {
            plan_id: body.plan_id,
            launch_id: None,
            launch_ids: body.launch_ids.clone(),
            branch: body.branch.clone(),
            environment: body.environment.clone(),
            days: body.days,
        },
        token_sha256: token_hash(&token),
        blob: None,
        downloaded: false,
    };
    let id = Uuid::now_v7();
    let mut job = s
        .store
        .start_coverage_export(
            &super::requirements::ctx(&s, &p, &auth),
            &crate::limits::caller_key(&auth),
            id,
            &params,
        )
        .await
        .map_err(tm_error)?;
    job.download_url = Some(format!(
        "/api/v1/projects/{}/coverage/exports/{id}/download?token={token}",
        p.key
    ));
    s.audit(
        &auth,
        "export.create",
        Some(&p.key),
        serde_json::json!({"jobId": id, "kind": "coverage", "format": body.format.as_str(),
            "scope": params.scope}),
    )
    .await;
    crate::tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// Query of the download.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CoverageDownloadQuery {
    pub token: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/coverage/exports/{id}/download", tag = "requirements", operation_id = "download_coverage_export",
    description = "Downloads a finished coverage export once: only its creator, with the token of `downloadUrl`, within 24 hours.",
    params(("key" = String, Path), ("id" = Uuid, Path), CoverageDownloadQuery),
    responses((status = 200, description = "the CSV or XLSX file"), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody, description = "unknown, not done, used, expired, another caller or a wrong token")))]
pub async fn download_coverage_export(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<CoverageDownloadQuery>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    if q.token.len() != 64 || !q.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::not_found("export"));
    }
    let (blob, format) = s
        .store
        .take_coverage_download(
            p.id,
            id,
            &crate::limits::caller_key(&auth),
            &token_hash(&q.token.to_ascii_lowercase()),
        )
        .await?
        .ok_or_else(|| ApiError::not_found("export"))?;
    let key = BlobKey::parse(&blob)?;
    let body = s
        .blobs
        .get(&key)
        .await?
        .ok_or_else(|| ApiError::not_found("export"))?;
    s.audit(
        &auth,
        "export.download",
        Some(&p.key),
        serde_json::json!({"jobId": id, "kind": "coverage"}),
    )
    .await;
    let (ct, ext) = export_type(format);
    let mut r = Response::new(Body::from_stream(
        body.stream.map(|c| c.map_err(std::io::Error::other)),
    ));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"coverage-{}.{ext}\"",
        p.key
    )) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&body.size.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(r.into_response())
}

/// Content type and file extension of an export format.
pub fn export_type(f: CoverageExportFormat) -> (&'static str, &'static str) {
    match f {
        CoverageExportFormat::Csv => ("text/csv; charset=utf-8", "csv"),
        CoverageExportFormat::Xlsx => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        ),
    }
}

const HEADERS: [&str; 15] = [
    "Key",
    "Title",
    "Type",
    "Status",
    "Depth",
    "Parent",
    "Coverage",
    "Cases",
    "Passed",
    "Failed",
    "Not run",
    "Skipped",
    "Changed after run",
    "Issue",
    "External state",
];

/// A CSV field: quoted, and a leading `=`, `+`, `-`, `@`, tab or carriage return neutralised
/// with `'` so spreadsheets never run it as a formula.
pub fn csv_field(s: &str) -> String {
    let mut v = String::with_capacity(s.len() + 3);
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        v.push('\'');
    }
    v.push_str(s);
    format!("\"{}\"", v.replace('"', "\"\""))
}

struct Line {
    key: String,
    title: String,
    req_type: String,
    status: String,
    depth: i64,
    parent: String,
    coverage: String,
    counts: [i64; 5],
    changed: String,
    issue: String,
    state: String,
}

fn line(r: &CoverageRow) -> Line {
    let c = &r.coverage.cases;
    Line {
        key: format!("REQ-{}", r.root.number),
        title: r.root.title.clone(),
        req_type: r.root.req_type.clone(),
        status: r.root.status.as_str().to_string(),
        depth: r.root.depth.into(),
        parent: r
            .root
            .parent_number
            .map(|n| format!("REQ-{n}"))
            .unwrap_or_default(),
        coverage: r.coverage.coverage.as_str().to_string(),
        counts: [c.total, c.passed, c.failed, c.not_run, c.skipped],
        changed: if r.coverage.changed_after_run {
            "yes"
        } else {
            "no"
        }
        .into(),
        issue: r.root.issue_key.clone().unwrap_or_default(),
        state: r.root.external_state.clone().unwrap_or_default(),
    }
}

fn write_csv(path: &std::path::Path, rows: &[CoverageRow]) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
    w.write_all(b"\xEF\xBB\xBF")?;
    let head: Vec<String> = HEADERS.iter().map(|h| csv_field(h)).collect();
    writeln!(w, "{}", head.join(","))?;
    for r in rows {
        let l = line(r);
        let mut f: Vec<String> = vec![
            csv_field(&l.key),
            csv_field(&l.title),
            csv_field(&l.req_type),
            csv_field(&l.status),
            l.depth.to_string(),
            csv_field(&l.parent),
            csv_field(&l.coverage),
        ];
        f.extend(l.counts.iter().map(i64::to_string));
        f.extend([
            csv_field(&l.changed),
            csv_field(&l.issue),
            csv_field(&l.state),
        ]);
        writeln!(w, "{}", f.join(","))?;
    }
    w.flush()?;
    Ok(())
}

fn write_xlsx(
    path: &std::path::Path,
    project_key: &str,
    scope: &str,
    rows: &[CoverageRow],
) -> anyhow::Result<()> {
    use crate::launch_export::xlsx::{self, Cell};
    let mut wb = xlsx::Workbook::create(std::fs::File::create(path)?, &["Summary", "Coverage"])?;
    let generated = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut by_status = std::collections::BTreeMap::<&str, i64>::new();
    for r in rows.iter().filter(|r| r.root.depth == 1) {
        *by_status.entry(r.coverage.coverage.as_str()).or_default() += 1;
    }
    let mut summary = String::new();
    let mut n = 1usize;
    for (k, v) in [
        ("Project", project_key),
        ("Scope", scope),
        ("Generated", generated.as_str()),
    ] {
        summary.push_str(&xlsx::row_xml(n, &[Cell::Text(k), Cell::Text(v)], false));
        n += 1;
    }
    summary.push_str(&xlsx::row_xml(
        n,
        &[Cell::Text("Requirements"), Cell::Num(rows.len() as i64)],
        false,
    ));
    n += 1;
    for s in ["uncovered", "failed", "notRun", "partial", "passed"] {
        let label = format!("Top level: {s}");
        summary.push_str(&xlsx::row_xml(
            n,
            &[
                Cell::Text(&label),
                Cell::Num(by_status.get(s).copied().unwrap_or(0)),
            ],
            false,
        ));
        n += 1;
    }
    wb.start_sheet(1, &[20.0, 40.0], false)?;
    wb.rows(&summary)?;
    wb.end_sheet(None)?;
    wb.start_sheet(
        2,
        &[
            10.0, 60.0, 14.0, 10.0, 7.0, 10.0, 11.0, 8.0, 8.0, 8.0, 8.0, 8.0, 10.0, 14.0, 16.0,
        ],
        true,
    )?;
    let head: Vec<Cell<'_>> = HEADERS.iter().map(|h| Cell::Text(h)).collect();
    wb.rows(&xlsx::row_xml(1, &head, true))?;
    let mut xml = String::with_capacity(rows.len().min(10_000) * 400);
    for (i, r) in rows.iter().enumerate() {
        let l = line(r);
        let mut cells = vec![
            Cell::Text(&l.key),
            Cell::Text(&l.title),
            Cell::Text(&l.req_type),
            Cell::Text(&l.status),
            Cell::Num(l.depth),
            Cell::Text(&l.parent),
            Cell::Text(&l.coverage),
        ];
        cells.extend(l.counts.iter().map(|c| Cell::Num(*c)));
        cells.extend([
            Cell::Text(&l.changed),
            Cell::Text(&l.issue),
            Cell::Text(&l.state),
        ]);
        xml.push_str(&xlsx::row_xml(i + 2, &cells, false));
        if xml.len() > 1 << 20 {
            wb.rows(&xml)?;
            xml.clear();
        }
    }
    wb.rows(&xml)?;
    wb.end_sheet(Some(&format!(
        "A1:{}{}",
        xlsx::column(HEADERS.len() - 1),
        rows.len() + 1
    )))?;
    wb.finish()
}

fn scope_text(v: &fastproof_store::tm::CoverageScopeView) -> String {
    match v.kind.as_str() {
        "plan" => format!(
            "plan {} ({} launch)",
            v.plan_id.map(|p| p.to_string()).unwrap_or_default(),
            v.launches
        ),
        "launches" => format!("{} launches", v.launches),
        "window" => format!(
            "last {} days{}{} ({} launches)",
            v.days.unwrap_or_default(),
            v.branch
                .as_ref()
                .map(|b| format!(", branch {b}"))
                .unwrap_or_default(),
            v.environment
                .as_ref()
                .map(|e| format!(", environment {e}"))
                .unwrap_or_default(),
            v.launches
        ),
        _ => "last result of each case".into(),
    }
}

/// Runs a `coverage_export` job: computes the coverage of every requirement in the scope,
/// writes the file and stores it as the job's blob for 24 hours.
pub async fn run_export(s: &AppState, job: &ClaimedJob, owner: &str) -> anyhow::Result<()> {
    let params: CoverageExportParams = serde_json::from_value(job.params.clone())?;
    let (view, rows) = s
        .store
        .coverage_rows(job.project_id, &params.scope, s.cfg.tm.max_requirements)
        .await?;
    let total = rows.len() as i64;
    let (_, ext) = export_type(params.format);
    let dir = s.cfg.spool_dir.join("tm-jobs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.{ext}", Uuid::now_v7()));
    struct Spool(std::path::PathBuf);
    impl Drop for Spool {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let spool = Spool(path.clone());
    let format = params.format;
    let key = job.project_key.clone();
    let scope = scope_text(&view);
    tokio::task::spawn_blocking(move || match format {
        CoverageExportFormat::Csv => write_csv(&path, &rows),
        CoverageExportFormat::Xlsx => write_xlsx(&path, &key, &scope, &rows),
    })
    .await??;
    let blob = BlobKey::tm_job(&job.id.to_string(), &format!("coverage.{ext}"))?;
    s.blobs.put_file(&blob, &spool.0).await?;
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
            Some(serde_json::json!({"blob": blob.to_string()})),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_fields_never_start_a_formula() {
        assert_eq!(csv_field("=1+1"), "\"'=1+1\"");
        assert_eq!(csv_field("@SUM(A1)"), "\"'@SUM(A1)\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("plain"), "\"plain\"");
    }
}
