//! Exports of test cases (T3): a streamed CSV or NDJSON of a filter (at most 50 000 cases), a
//! zip job with the attachments (a one-time link for its creator, 24 hours), and the status
//! of any test-management job.

use axum::Json;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_blob::BlobKey;
use fastproof_store::tm::import::{ExportParams, MAX_EXPORT_CASES};
use fastproof_store::tm::{CaseFilter, TmJob, TmJobKind};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use futures::StreamExt;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_stream::wrappers::ReceiverStream;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::cases::case_query;
use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;
use crate::tm_jobs;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(export_cases))
        .routes(routes!(create_case_export))
        .routes(routes!(download_case_export))
        .routes(routes!(get_tm_job))
}

/// Query of `GET /cases/export`: `format` plus the filters of `GET /cases` (`folder`,
/// `recursive`, `q`, `status`, `automation`, `field`, `tag`).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    /// `csv` or `ndjson` (the FastProof format the importer reads back).
    pub format: String,
    pub folder: Option<Uuid>,
    pub recursive: Option<bool>,
    pub q: Option<String>,
    pub status: Option<String>,
    pub field: Option<String>,
    pub tag: Option<String>,
}

fn format_of(raw: Option<&str>) -> ApiResult<&'static str> {
    let f = url::form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .find(|(k, _)| k == "format")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    match f.as_str() {
        "csv" => Ok("csv"),
        "ndjson" => Ok("ndjson"),
        _ => Err(ApiError::bad_request("format must be csv or ndjson")),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/export", tag = "imports", operation_id = "export_cases",
    description = "Streams the (not archived) cases of a filter as CSV (cells that a spreadsheet would run as a formula \
        start with `'`) or FastProof NDJSON, at most 50 000 cases (409 `limit_reached` above).",
    params(("key" = String, Path), ExportQuery),
    responses((status = 200, content_type = "text/csv", description = "CSV or NDJSON"), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn export_cases(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let format = format_of(raw.as_deref())?;
    let q = case_query(raw.as_deref())?;
    let permit = s.guards.heavy(&auth).await?;
    let src = s
        .store
        .export_source(p.id, &q.filter, MAX_EXPORT_CASES)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "export.create",
        Some(&p.key),
        serde_json::json!({"format": format, "cases": src.numbers.len(),
        "filter": q.filter}),
    )
    .await;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(8);
    let st = s.clone();
    let project = p.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let keys = src.field_keys();
        let head = if format == "csv" {
            fastproof_tm_import::export::csv_header(&keys)
        } else {
            fastproof_tm_import::export::ndjson_header(
                &project.key,
                &chrono::Utc::now().to_rfc3339(),
                src.field_defs(),
            )
        };
        if tx.send(Ok(head.into())).await.is_err() {
            return;
        }
        for chunk in src.numbers.chunks(500) {
            let page = match st.store.export_page(project.id, &src, chunk).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "case export failed");
                    let _ = tx.send(Err(std::io::Error::other("export failed"))).await;
                    return;
                }
            };
            let mut out = String::new();
            for (c, _) in page {
                if format == "csv" {
                    out.push_str(&fastproof_tm_import::export::csv_case(&c, &keys));
                } else {
                    out.push_str(&fastproof_tm_import::export::ndjson_case(&c));
                }
            }
            if tx.send(Ok(out.into())).await.is_err() {
                return;
            }
        }
    });
    let (ct, ext) = if format == "csv" {
        ("text/csv; charset=utf-8", "csv")
    } else {
        ("application/x-ndjson", "ndjson")
    };
    let mut r = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"cases-{}.{ext}\"", p.key))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

/// Body of `POST /cases/exports`.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseExportRequest {
    /// `zip`: `cases.ndjson`, `cases.csv` and `attachments/`.
    pub format: String,
    #[serde(default)]
    pub filter: Option<CaseFilter>,
    /// Put the attachments of the cases into the zip.
    #[serde(default)]
    pub attachments: bool,
}

fn token_hash(t: &str) -> String {
    hex::encode(Sha256::digest(t.as_bytes()))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/cases/exports", tag = "imports", operation_id = "create_case_export",
    description = "Starts a zip export job. The answer's `downloadUrl` (shown only here) is a one-time link for the caller, \
        valid for 24 hours once the job is `done`.",
    request_body = CaseExportRequest,
    params(("key" = String, Path)),
    responses((status = 202, body = TmJob), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_case_export(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<CaseExportRequest>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    if body.format != "zip" {
        return Err(ApiError::bad_request("format must be zip"));
    }
    let filter = body.filter.unwrap_or_default();
    s.store
        .export_source(p.id, &filter, MAX_EXPORT_CASES)
        .await
        .map_err(tm_error)?;
    let token: String = {
        let mut b = [0u8; 32];
        rand::rng().fill(&mut b);
        hex::encode(b)
    };
    let params = ExportParams {
        filter: filter.clone(),
        attachments: body.attachments,
        token_sha256: token_hash(&token),
        blob: None,
        downloaded: false,
    };
    let id = Uuid::now_v7();
    let mut job = s
        .store
        .create_tm_job(
            id,
            p.id,
            &p.key,
            TmJobKind::Export,
            &serde_json::to_value(&params).map_err(anyhow::Error::from)?,
            &auth.principal.name,
        )
        .await
        .map_err(tm_error)?;
    job.download_url = Some(format!(
        "/api/v1/projects/{}/cases/exports/{id}/download?token={token}",
        p.key
    ));
    s.audit(
        &auth,
        "export.create",
        Some(&p.key),
        serde_json::json!({"jobId": id, "format": "zip",
        "attachments": body.attachments, "filter": filter}),
    )
    .await;
    tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// Query of the download.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DownloadQuery {
    pub token: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/cases/exports/{id}/download", tag = "imports", operation_id = "download_case_export",
    description = "Downloads a finished zip export once: only its creator, with the token of `downloadUrl`, within 24 hours.",
    params(("key" = String, Path), ("id" = Uuid, Path), DownloadQuery),
    responses((status = 200, content_type = "application/zip", description = "the zip"), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody, description = "unknown, not done, used, expired, another caller or a wrong token")))]
pub async fn download_case_export(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<DownloadQuery>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    if q.token.len() != 64 || !q.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::not_found("export"));
    }
    let blob = s
        .store
        .take_export_download(
            p.id,
            id,
            &auth.principal.name,
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
        serde_json::json!({"jobId": id}),
    )
    .await;
    let mut r = Response::new(Body::from_stream(
        body.stream.map(|c| c.map_err(std::io::Error::other)),
    ));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"cases-{}.zip\"", p.key)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&body.size.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r.into_response())
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/tm-jobs/{id}", tag = "imports", operation_id = "get_tm_job",
    description = "A test-management job of the project (import, export, backfill): status, progress and report. Export jobs are visible only to their creator.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_tm_job(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<TmJob>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let (job, _) = s
        .store
        .tm_job_with_params(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    if job.kind == TmJobKind::Export
        && job.created_by.as_deref() != Some(auth.principal.name.as_str())
    {
        return Err(ApiError::not_found("job"));
    }
    Ok(Json(job))
}
