//! Launch export: `GET .../launches/{id}/export` (HTML or XLSX of up to 10 000 results, streamed
//! from a spool file) and background jobs for larger launches
//! (`POST .../launches/{id}/exports`, status, download for 24 hours). The filters are those of
//! the result list. Permission: `launches:read`, as for reading the launch.

use crate::limits::caller_key;
use axum::Json;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use fastproof_blob::BlobKey;
use fastproof_store::launch_exports::{CreateDenied, LAUNCH_EXPORT_TTL_HOURS, LaunchExport};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P};
use crate::launch_export::{
    Format, MAX_EXPORT_RESULTS, MAX_QUERY_BYTES, MAX_SYNC_RESULTS, generate, parse_filter,
    spawn_job, split_query,
};
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(export_launch))
        .routes(routes!(create_launch_export))
        .routes(routes!(get_launch_export))
        .routes(routes!(download_launch_export))
}

/// Query of the export: the format plus every filter of `GET .../launches/{id}/results`
/// (`status`, `q`, `suite`, `label`, `flaky`, `fingerprint`, `source`, `message`, `retried`,
/// `muted`, `newFailures`, `severity`, `owner`, `tag`, `minDurationMs`, `maxDurationMs`,
/// `defect`, `attachment`, `env`). `before` and `limit` are ignored.
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct ExportQuery {
    /// `html` (print-optimised, a browser prints it to PDF) or `xlsx`.
    pub format: String,
    /// `true`: show the HTML in the browser (sandboxed) instead of downloading it.
    pub inline: Option<bool>,
}

fn parse_request(raw: Option<&str>) -> ApiResult<(Format, bool, String)> {
    let raw = raw.unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return Err(ApiError::bad_request("the query is too long"));
    }
    let (format, inline, rest) = split_query(raw);
    let format = format
        .as_deref()
        .and_then(Format::parse)
        .ok_or_else(|| ApiError::bad_request("format must be html or xlsx"))?;
    Ok((format, inline && format == Format::Html, rest))
}

fn file_name(project: &str, number: i64, f: Format) -> String {
    format!("launch-{project}-{number}.{}", f.as_str())
}

fn headers(r: &mut Response, f: Format, name: &str, inline: bool, len: Option<u64>) {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(f.content_type()),
    );
    let disp = if inline { "inline" } else { "attachment" };
    if let Ok(v) = HeaderValue::from_str(&format!("{disp}; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(v) = len.and_then(|l| HeaderValue::from_str(&l.to_string()).ok()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&format!("sandbox; {}", crate::launch_export::html::POLICY))
            .unwrap_or_else(|_| HeaderValue::from_static("sandbox; default-src 'none'")),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/export", tag = "launches", operation_id = "launches_export",
    description = "Exports the results of a launch (the filters of the result list apply) as one self-contained HTML file \
        (no scripts, no external fetches; print-optimised, so a browser prints it to PDF) or as an XLSX workbook with a summary \
        sheet. Only for launches of at most 10 000 results (else 409 `export_too_large`: use `POST .../exports`). \
        Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path), ExportQuery),
    responses((status = 200, description = "the file"), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn export_launch(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let (format, inline, text) = parse_request(raw.as_deref())?;
    if launch.counts.total > MAX_SYNC_RESULTS {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "export_too_large",
            format!(
                "the launch has more than {MAX_SYNC_RESULTS} results: start an export job with POST .../exports"
            ),
        ));
    }
    let filter = parse_filter(&s, &launch, &text).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let g = generate(
        &s,
        &project.key,
        &launch,
        filter,
        &text,
        format,
        MAX_SYNC_RESULTS,
        None,
    )
    .await
    .map_err(|e| match e.downcast_ref::<crate::launch_export::Stop>() {
        Some(stop) => ApiError::new(StatusCode::CONFLICT, "export_too_large", stop.to_string()),
        None => ApiError::from(e),
    })?;
    s.audit(
        &auth,
        "launch.export",
        Some(&project.key),
        serde_json::json!({"launchId": id, "number": launch.number, "format": format.as_str(),
            "results": g.results, "filter": text}),
    )
    .await;
    let file = tokio::fs::File::open(&g.file.0)
        .await
        .map_err(anyhow::Error::from)?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    drop(g);
    let mut r = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    headers(
        &mut r,
        format,
        &file_name(&project.key, launch.number, format),
        inline,
        Some(len),
    );
    Ok(r)
}

fn download_url(project: &str, launch: Uuid, id: Uuid) -> String {
    format!("/api/v1/projects/{project}/launches/{launch}/exports/{id}/download")
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/exports", tag = "launches", operation_id = "launches_export_create",
    description = "Starts a background export of a launch of any size (at most 500 000 results) with the same `format` and filters \
        as `GET .../export`. The file is downloadable by its creator for 24 hours once the job is `done`. A caller has at most 2 \
        and a project at most 4 exports queued or running (else 409 `export_busy`). Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path), ExportQuery),
    responses((status = 202, body = LaunchExport), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_launch_export(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    RawQuery(raw): RawQuery,
) -> ApiResult<(StatusCode, Json<LaunchExport>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let (format, _, text) = parse_request(raw.as_deref())?;
    parse_filter(&s, &launch, &text).await?;
    if text.is_empty() && launch.counts.total > MAX_EXPORT_RESULTS {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "export_too_large",
            format!(
                "the launch has more than {MAX_EXPORT_RESULTS} results: narrow the export with a filter"
            ),
        ));
    }
    let job_id = Uuid::now_v7();
    let job = s
        .store
        .create_launch_export(
            job_id,
            project.id,
            launch.id,
            format.as_str(),
            &text,
            launch.counts.total.min(MAX_EXPORT_RESULTS),
            &auth.principal.name,
            &caller_key(&auth),
        )
        .await?
        .map_err(|d| match d {
            CreateDenied::Busy => ApiError::new(
                StatusCode::CONFLICT,
                "export_busy",
                "too many exports are queued or running; retry when one is done",
            ),
            CreateDenied::Quota => ApiError::new(
                StatusCode::CONFLICT,
                "export_quota",
                "the exports of the last 24 hours are used up; retry later",
            ),
        })?;
    s.audit(
        &auth,
        "launch.export",
        Some(&project.key),
        serde_json::json!({"launchId": id, "number": launch.number, "format": format.as_str(),
            "jobId": job_id, "filter": text}),
    )
    .await;
    spawn_job(&s, job_id);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/exports/{export_id}", tag = "launches", operation_id = "launches_export_get",
    description = "Status and progress of an export job. Visible only to its creator (404 for anyone else).",
    params(("key" = String, Path), ("id" = Uuid, Path), ("export_id" = Uuid, Path)),
    responses((status = 200, body = LaunchExport), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_launch_export(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id, export_id)): P<(String, Uuid, Uuid)>,
) -> ApiResult<Json<LaunchExport>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let (mut job, _) = own_export(&s, &auth, &project, id, export_id).await?;
    if job.status == "done"
        && job.finished_at.is_some_and(|f| {
            chrono::Utc::now() - f < chrono::Duration::hours(LAUNCH_EXPORT_TTL_HOURS)
        })
    {
        job.download_url = Some(download_url(&project.key, id, export_id));
    }
    Ok(Json(job))
}

async fn own_export(
    s: &AppState,
    auth: &Auth,
    project: &fastproof_store::Project,
    launch: Uuid,
    id: Uuid,
) -> ApiResult<(LaunchExport, Option<String>)> {
    let (job, blob, owner) = s
        .store
        .launch_export(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("export"))?;
    if job.launch_id != launch || owner != caller_key(auth) {
        return Err(ApiError::not_found("export"));
    }
    Ok((job, blob))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/exports/{export_id}/download", tag = "launches", operation_id = "launches_export_download",
    description = "Downloads a finished export: only its creator, within 24 hours.",
    params(("key" = String, Path), ("id" = Uuid, Path), ("export_id" = Uuid, Path)),
    responses((status = 200, description = "the file"), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody, description = "unknown, not done, expired or another caller's")))]
pub async fn download_launch_export(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id, export_id)): P<(String, Uuid, Uuid)>,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let (job, blob) = own_export(&s, &auth, &project, id, export_id).await?;
    let fresh = job
        .finished_at
        .is_some_and(|f| chrono::Utc::now() - f < chrono::Duration::hours(LAUNCH_EXPORT_TTL_HOURS));
    let (true, true, Some(blob)) = (job.status == "done", fresh, blob) else {
        return Err(ApiError::not_found("export"));
    };
    let format = Format::parse(&job.format).ok_or_else(|| ApiError::not_found("export"))?;
    let body = s
        .blobs
        .get(&BlobKey::parse(&blob)?)
        .await?
        .ok_or_else(|| ApiError::not_found("export"))?;
    let launch = s.launch_of(&project, id).await?;
    s.audit(
        &auth,
        "launch.export.download",
        Some(&project.key),
        serde_json::json!({"launchId": id, "jobId": export_id}),
    )
    .await;
    let mut r = Response::new(Body::from_stream(
        body.stream.map(|c| c.map_err(std::io::Error::other)),
    ));
    headers(
        &mut r,
        format,
        &file_name(&project.key, launch.number, format),
        false,
        Some(body.size),
    );
    Ok(r)
}
