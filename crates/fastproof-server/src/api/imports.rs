//! Imports of test cases (T3): preview of a file, import jobs from a file (CSV, JSON, NDJSON,
//! with an optional zip of attachments) or from the Allure TestOps API (beta), always a dry
//! run first, then apply or cancel.

use axum::Json;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use fastproof_blob::{BlobKey, spool_stream};
use fastproof_store::tm::import::{ImportParams, TestOpsParams};
use fastproof_store::tm::{
    ImportFormat, ImportOptions, ImportPreview, StepsLayout, TestOpsImport, TmJob, TmJobKind,
};
use fastproof_tm_import::Format;
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::uploads::{clean_file_name, identity_encoding, spool_error};
use super::{ErrorBody, P, Q, tm_error};
use crate::integrations::secrets::{MAX_SECRET_BYTES, SecretPurpose, origin_of};
use crate::state::AppState;
use crate::testops::check_base_url;
use crate::tm_jobs;

/// Bytes of a file the preview reads.
pub const PREVIEW_BYTES: usize = 5 * 1024 * 1024;
/// Bytes of the `options` part.
const MAX_OPTIONS_BYTES: usize = 64 * 1024;
/// Entries of a column mapping.
const MAX_MAPPING: usize = 500;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    let uploads = OpenApiRouter::<AppState>::new()
        .routes(routes!(preview_import))
        .routes(routes!(create_import))
        .layer(DefaultBodyLimit::disable());
    OpenApiRouter::new()
        .merge(uploads)
        .routes(routes!(get_import, cancel_import))
        .routes(routes!(apply_import))
}

fn is_multipart(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"))
}

fn bad_multipart(e: axum::extract::multipart::MultipartError) -> ApiError {
    ApiError::bad_request(format!("invalid multipart body: {}", e.body_text()))
}

/// Query of `POST /imports/preview`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PreviewQuery {
    /// The format; detected when absent.
    pub format: Option<ImportFormat>,
}

fn to_format(f: ImportFormat) -> Format {
    Format::parse(f.as_str()).unwrap_or(Format::GenericCsv)
}

fn from_format(f: Format) -> ImportFormat {
    ImportFormat::parse(f.as_str()).unwrap_or(ImportFormat::GenericCsv)
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/imports/preview", tag = "imports", operation_id = "preview_import",
    description = "Reads the first 5 MB of a file (multipart field `file`) and answers the detected format, the header, \
        the first 20 rows, the suggested column mapping and the steps layout. Nothing is stored.",
    params(("key" = String, Path), PreviewQuery),
    request_body(content = Vec<u8>, content_type = "multipart/form-data", description = "field `file`"),
    responses((status = 200, body = ImportPreview), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn preview_import(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<PreviewQuery>,
    req: Request,
) -> ApiResult<Json<ImportPreview>> {
    let _p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    identity_encoding(&req)?;
    let _permit = s.guards.heavy(&auth).await?;
    if !is_multipart(&req) {
        return Err(ApiError::bad_request(
            "send multipart/form-data with a `file` field",
        ));
    }
    let mut mp = Multipart::from_request(req, &s)
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart body: {}", e.body_text())))?;
    let mut head: Option<Vec<u8>> = None;
    while let Some(mut field) = mp.next_field().await.map_err(bad_multipart)? {
        if field.name() != Some("file") {
            continue;
        }
        let mut buf = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(bad_multipart)? {
            let room = PREVIEW_BYTES - buf.len();
            buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if buf.len() >= PREVIEW_BYTES {
                break;
            }
        }
        head = Some(buf);
        break;
    }
    let head =
        head.ok_or_else(|| ApiError::bad_request("the multipart body has no `file` field"))?;
    if head.is_empty() {
        return Err(ApiError::bad_request("the file is empty"));
    }
    let p = tokio::task::spawn_blocking(move || {
        fastproof_tm_import::preview(&head, q.format.map(to_format))
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(ImportPreview {
        format: from_format(p.format),
        headers: p.headers,
        rows: p.rows,
        suggested_mapping: p.suggested_mapping,
        steps_layout: match p.steps_layout {
            fastproof_tm_import::StepsLayout::Rows => StepsLayout::Rows,
            fastproof_tm_import::StepsLayout::Cells => StepsLayout::Cells,
        },
    }))
}

fn check_options(o: &ImportOptions) -> ApiResult<()> {
    if let Some(m) = &o.mapping {
        if m.len() > MAX_MAPPING {
            return Err(ApiError::bad_request(format!(
                "mapping has at most {MAX_MAPPING} columns"
            )));
        }
        for (k, v) in m {
            if k.len() > 1000 || v.len() > 210 {
                return Err(ApiError::bad_request("mapping entries are too long"));
            }
            if fastproof_tm_import::generic::Target::parse(v).is_none() {
                return Err(ApiError::bad_request(format!(
                    "mapping: unknown target {v:?}"
                )));
            }
        }
    }
    Ok(())
}

async fn check_folder(s: &AppState, project_id: Uuid, f: Option<Uuid>) -> ApiResult<()> {
    if let Some(f) = f
        && s.store.case_folder(project_id, f).await?.is_none()
    {
        return Err(ApiError::not_found("folder"));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/imports", tag = "imports", operation_id = "create_import",
    description = "Starts an import job, always as a dry run (`ready` with an `ImportReport`; apply it with \
        `POST /imports/{id}/apply`). Either multipart with `file` (CSV, JSON or NDJSON, at most `maxImportBytes`), \
        `options` (`ImportOptions` JSON) and optionally `attachments` (a zip whose files cases reference by name, at most \
        `maxImportZipBytes`), or JSON `TestOpsImport` (beta: the token is stored as a project secret at once and never \
        returned, logged or audited; the host must be allowed by `FASTPROOF_EGRESS_ALLOW`). One import per project at a time.",
    params(("key" = String, Path)),
    request_body(content = TestOpsImport, description = "or multipart/form-data: `file`, `options`, `attachments`"),
    responses((status = 202, body = TmJob), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody, description = "target folder"), (status = 409, body = ErrorBody, description = "`import_busy`"),
        (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn create_import(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    req: Request,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    identity_encoding(&req)?;
    let id = Uuid::now_v7();
    let (params, detail) = if is_multipart(&req) {
        file_import(&s, &p.id, id, req).await?
    } else {
        testops_import(&s, &p, id, req, &auth).await?
    };
    let job = s
        .store
        .create_tm_job(
            id,
            p.id,
            &p.key,
            TmJobKind::Import,
            &params,
            &auth.principal.name,
        )
        .await
        .map_err(tm_error);
    let job = match job {
        Ok(j) => j,
        Err(e) => {
            discard(&s, &params, p.id).await;
            return Err(e);
        }
    };
    s.audit(
        &auth,
        "import.start",
        Some(&p.key),
        serde_json::json!({"jobId": job.id, "import": detail}),
    )
    .await;
    tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn discard(s: &AppState, params: &serde_json::Value, project_id: Uuid) {
    for k in ["blob", "zip"] {
        if let Some(b) = params.get(k).and_then(|v| v.as_str())
            && let Ok(key) = BlobKey::parse(b)
        {
            let _ = s.blobs.delete(&key).await;
        }
    }
    if let Some(sec) = params.pointer("/testops/secret").and_then(|v| v.as_str()) {
        let _ = s.store.delete_secret(project_id, sec).await;
    }
}

async fn file_import(
    s: &AppState,
    project_id: &Uuid,
    id: Uuid,
    req: Request,
) -> ApiResult<(serde_json::Value, serde_json::Value)> {
    let _spool = s.guards.spool().await?;
    let dir = s.cfg.spool_dir.join("tm-imports");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let mut mp = Multipart::from_request(req, s)
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart body: {}", e.body_text())))?;
    let mut options: Option<ImportOptions> = None;
    let mut file: Option<(fastproof_blob::Spooled, Option<String>)> = None;
    let mut zip: Option<fastproof_blob::Spooled> = None;
    let l = &s.cfg.tm;
    let result = async {
        while let Some(mut field) = mp.next_field().await.map_err(bad_multipart)? {
            match field.name() {
                Some("options") => {
                    let mut buf = Vec::new();
                    while let Some(c) = field.chunk().await.map_err(bad_multipart)? {
                        if buf.len() + c.len() > MAX_OPTIONS_BYTES {
                            return Err(ApiError::bad_request("options has at most 64 KiB"));
                        }
                        buf.extend_from_slice(&c);
                    }
                    let o: ImportOptions = serde_json::from_slice(&buf)
                        .map_err(|e| ApiError::bad_request(format!("options: {e}")))?;
                    options = Some(o);
                }
                Some("file") if file.is_none() => {
                    let name = field.file_name().and_then(clean_file_name);
                    let path = dir.join(format!("{}-file", Uuid::now_v7()));
                    let sp = spool_stream(&path, Box::pin(field), l.max_import_bytes)
                        .await
                        .map_err(|e| spool_error(e, "maxImportBytes"))?;
                    file = Some((sp, name));
                }
                Some("attachments") if zip.is_none() => {
                    let path = dir.join(format!("{}-zip", Uuid::now_v7()));
                    let sp = spool_stream(&path, Box::pin(field), l.max_import_zip_bytes)
                        .await
                        .map_err(|e| spool_error(e, "maxImportZipBytes"))?;
                    zip = Some(sp);
                }
                other => {
                    return Err(ApiError::bad_request(format!(
                        "unexpected multipart field {:?} (send options, file and optionally attachments once each)",
                        other.unwrap_or("").chars().take(64).collect::<String>()
                    )));
                }
            }
        }
        let opts = options.clone().ok_or_else(|| ApiError::bad_request("the multipart body has no `options` field"))?;
        check_options(&opts)?;
        check_folder(s, *project_id, opts.target_folder_id).await?;
        let (f, name) = file.as_ref().ok_or_else(|| ApiError::bad_request("the multipart body has no `file` field"))?;
        if f.size == 0 {
            return Err(ApiError::bad_request("the file is empty"));
        }
        let fkey = BlobKey::tm_job(&id.to_string(), "source")?;
        s.blobs.put_file(&fkey, &f.path).await?;
        let mut zkey = None;
        if let Some(z) = &zip
            && z.size > 0
        {
            let k = BlobKey::tm_job(&id.to_string(), "attachments.zip")?;
            s.blobs.put_file(&k, &z.path).await?;
            zkey = Some(k.to_string());
        }
        let params = tm_jobs::file_params(&opts, name.clone(), fkey.to_string(), zkey.clone());
        let detail = serde_json::json!({"source": "file", "format": opts.format, "mode": opts.mode, "fileName": name,
            "bytes": f.size, "attachments": zkey.is_some()});
        Ok((params, detail))
    }
    .await;
    if let Some((f, _)) = file {
        f.remove();
    }
    if let Some(z) = zip {
        z.remove();
    }
    result
}

async fn testops_import(
    s: &AppState,
    p: &fastproof_store::Project,
    id: Uuid,
    req: Request,
    auth: &Auth,
) -> ApiResult<(serde_json::Value, serde_json::Value)> {
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map_err(|_| ApiError::bad_request("the body is larger than 64 KiB"))?;
    let t: TestOpsImport = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::bad_request(format!("invalid body: {}", redact(&e.to_string()))))?;
    let base =
        check_base_url(&t.base_url, s.cfg.testops_allow_http).map_err(ApiError::bad_request)?;
    s.egress.policy().check_url(&base).map_err(|_| {
        ApiError::bad_request("the host of baseUrl is not allowed by FASTPROOF_EGRESS_ALLOW")
    })?;
    if t.remote_project_id <= 0 {
        return Err(ApiError::bad_request(
            "remoteProjectId must be a positive number",
        ));
    }
    if t.token.is_empty()
        || t.token.len() > MAX_SECRET_BYTES
        || t.token.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(format!(
            "token must have 1 to {MAX_SECRET_BYTES} bytes"
        )));
    }
    check_folder(s, p.id, t.target_folder_id).await?;
    let Some(keys) = &s.cfg.integrations.secrets else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "imports from Allure TestOps need FASTPROOF_SECRETS_KEY_FILE",
        ));
    };
    let origin =
        origin_of(base.as_str()).ok_or_else(|| ApiError::bad_request("baseUrl has no origin"))?;
    let name = format!("tm-import-{id}");
    let sealed = keys.seal(
        p.id,
        &name,
        Some(SecretPurpose::Testops.as_str()),
        Some(&origin),
        &t.token,
    )?;
    s.store
        .put_secret(p.id, &name, &sealed, &auth.principal.name)
        .await?;
    let params = ImportParams {
        source: "testops".into(),
        format: Some(ImportFormat::FastproofNdjson),
        mode: Some(t.mode),
        target_folder_id: t.target_folder_id,
        unknown_field_values: Some(fastproof_store::tm::UnknownFieldValues::Skip),
        testops: Some(TestOpsParams {
            base_url: base.to_string(),
            remote_project_id: t.remote_project_id,
            secret: name,
            allow_http: s.cfg.testops_allow_http,
        }),
        ..Default::default()
    };
    let detail = serde_json::json!({"source": "allure-testops", "baseUrl": base.as_str(), "remoteProjectId": t.remote_project_id,
        "mode": t.mode});
    Ok((
        serde_json::to_value(params).map_err(anyhow::Error::from)?,
        detail,
    ))
}

/// A JSON error message without quoted input (it may contain the token).
fn redact(m: &str) -> String {
    match m.find('`') {
        Some(_) => m
            .split('`')
            .enumerate()
            .map(|(i, p)| if i % 2 == 1 { "…" } else { p })
            .collect(),
        None => m.to_string(),
    }
}

async fn import_job(s: &AppState, project_id: Uuid, id: Uuid) -> ApiResult<TmJob> {
    match s.store.tm_job_with_params(project_id, id).await? {
        Some((j, _)) if j.kind == TmJobKind::Import => Ok(j),
        _ => Err(ApiError::not_found("import")),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/imports/{id}", tag = "imports", operation_id = "get_import",
    description = "Status, progress and report (the dry run's while `ready`, the applied one when `done`) of an import.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_import(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<TmJob>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    Ok(Json(import_job(&s, p.id, id).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/imports/{id}/apply", tag = "imports", operation_id = "apply_import",
    description = "Applies a ready import (its dry run): cases are created and, with `upsert`, matching ones updated with a \
        version `source = import`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 202, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`job_not_ready` or `import_busy`")))]
pub async fn apply_import(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<(StatusCode, Json<TmJob>)> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    import_job(&s, p.id, id).await?;
    let job = s
        .store
        .apply_tm_job(p.id, &p.key, id)
        .await
        .map_err(tm_error)?;
    s.audit(&auth, "import.apply", Some(&p.key), serde_json::json!({"jobId": id, "report": job.report.as_ref().map(|r|
        serde_json::json!({"create": r.create, "update": r.update, "skip": r.skip, "folders": r.folders, "errors": r.errors.len()}))}))
        .await;
    tm_jobs::kick(&s);
    Ok((StatusCode::ACCEPTED, Json(job)))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/imports/{id}", tag = "imports", operation_id = "cancel_import",
    description = "Cancels an import that is not finished; applied batches stay.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TmJob), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`job_finished`")))]
pub async fn cancel_import(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<TmJob>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_import).await?;
    let (job, params) = s
        .store
        .cancel_tm_job(p.id, &p.key, id, TmJobKind::Import)
        .await
        .map_err(|e| match e {
            fastproof_store::tm::TmError::NotFound(_) => ApiError::not_found("import"),
            e => tm_error(e),
        })?;
    discard(&s, &params, p.id).await;
    s.audit(
        &auth,
        "import.cancel",
        Some(&p.key),
        serde_json::json!({"jobId": id}),
    )
    .await;
    Ok(Json(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_quoted_input() {
        assert_eq!(
            redact("unknown field `token123`, expected `x`"),
            "unknown field …, expected …"
        );
    }
}
