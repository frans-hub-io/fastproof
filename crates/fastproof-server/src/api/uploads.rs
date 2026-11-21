//! `POST|GET /api/v1/projects/{key}/launches/{id}/uploads`: the body is spooled to disk with the
//! size limit, stored as `uploads/<id>` and queued; any replica's worker processes it.

use axum::Json;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use fastproof_blob::{BlobKey, SpoolError, spool_stream};
use fastproof_store::Upload;
use fastproof_store::uploads::NewUpload;
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{ErrorBody, P, Q};
use crate::state::AppState;

/// Query of `POST .../uploads`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct UploadQuery {
    /// `auto` (default), `allure`, `junit`, `ctrf` or `playwright`.
    pub format: Option<String>,
    /// Original file name of a single-file upload (helps detection).
    pub file_name: Option<String>,
}

/// Uploads of a launch.
#[derive(Serialize, ToSchema)]
pub struct UploadList {
    pub items: Vec<Upload>,
}

/// The `payload_too_large` error naming the limit.
pub fn too_large(limit: &str, max: u64) -> ApiError {
    let unit = if limit.contains("bytes") {
        "bytes"
    } else if limit.contains("lines") {
        "lines"
    } else if limit.contains("results") {
        "results"
    } else {
        "items"
    };
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        format!("the body exceeds {limit} (max {max} {unit})"),
    )
    .with_details(serde_json::json!({"limit": limit, "max": max}))
}

/// Maps a spooling failure to an API error.
pub fn spool_error(e: SpoolError, limit: &str) -> ApiError {
    match e {
        SpoolError::TooLarge { max } => too_large(limit, max),
        SpoolError::Body(e) => ApiError::bad_request(format!("reading the body failed: {e:#}")),
        SpoolError::Io(e) => ApiError::from(anyhow::Error::from(e).context("spooling an upload")),
    }
}

/// A file name reduced to its last path component, bounded and printable.
pub fn clean_file_name(n: &str) -> Option<String> {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(255).collect();
    (!clean.is_empty() && clean != "." && clean != "..").then_some(clean)
}

fn normalize_format(f: Option<&str>) -> ApiResult<String> {
    let f = f
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "auto".into());
    match f.as_str() {
        "auto" | "allure" | "junit" | "ctrf" | "playwright" => Ok(f),
        _ => Err(ApiError::bad_request(
            "format must be auto, allure, junit, ctrf or playwright",
        )),
    }
}

/// Rejects compressed transfer encodings (the body itself may be an archive).
pub fn identity_encoding(req: &Request) -> ApiResult<()> {
    if let Some(v) = req.headers().get(header::CONTENT_ENCODING)
        && !v.as_bytes().eq_ignore_ascii_case(b"identity")
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Encoding is not supported: send the archive as the body",
        ));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/uploads", tag = "uploads", operation_id = "uploads_create",
    params(("key" = String, Path), ("id" = Uuid, Path), UploadQuery),
    request_body(content = Vec<u8>, description = "zip, tar, tar.gz, a JSON or XML report, or multipart with a `file` field",
        content_type = "application/octet-stream"),
    responses((status = 202, body = Upload), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 413, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<UploadQuery>,
    req: Request,
) -> ApiResult<(StatusCode, Json<Upload>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    let format = normalize_format(q.format.as_deref())?;
    identity_encoding(&req)?;
    let _spool = s.guards.spool().await?;
    let mut file_name = q.file_name.as_deref().and_then(clean_file_name);
    let upload_id = Uuid::now_v7();
    let dir = s.cfg.spool_dir.join("incoming");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(upload_id.to_string());
    let max = s.cfg.max_upload_bytes;
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("multipart/form-data"));
    let spooled = if is_multipart {
        let mut mp = Multipart::from_request(req, &s).await.map_err(|e| {
            ApiError::bad_request(format!("invalid multipart body: {}", e.body_text()))
        })?;
        let mut found = None;
        while let Some(field) = mp.next_field().await.map_err(|e| {
            ApiError::bad_request(format!("invalid multipart body: {}", e.body_text()))
        })? {
            if field.name() != Some("file") {
                continue;
            }
            if file_name.is_none() {
                file_name = field.file_name().and_then(clean_file_name);
            }
            found = Some(
                spool_stream(&path, Box::pin(field), max)
                    .await
                    .map_err(|e| spool_error(e, "max_upload_bytes"))?,
            );
            break;
        }
        found.ok_or_else(|| ApiError::bad_request("the multipart body has no `file` field"))?
    } else {
        let stream = req.into_body().into_data_stream().boxed();
        spool_stream(&path, stream, max)
            .await
            .map_err(|e| spool_error(e, "max_upload_bytes"))?
    };
    if spooled.size == 0 {
        spooled.remove();
        return Err(ApiError::bad_request("the upload is empty"));
    }
    let blob_key = BlobKey::upload(&upload_id.to_string())?;
    if let Err(e) = s.blobs.put_file(&blob_key, &spooled.path).await {
        spooled.remove();
        return Err(e.into());
    }
    s.local_uploads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(upload_id, spooled.path.clone());
    metrics::counter!("fastproof_upload_bytes_total").increment(spooled.size);
    let new_upload = NewUpload {
        id: upload_id,
        project_id: project.id,
        project_key: project.key.clone(),
        launch_id: launch.id,
        format,
        file_name,
        size_bytes: spooled.size as i64,
        sha256: spooled.sha256.clone(),
        blob_key: blob_key.to_string(),
        created_by: auth.principal.name.clone(),
        max_attempts: 3,
    };
    if s.kafka_gateway() {
        let upload = s.store.record_upload(&new_upload).await?;
        let rec = fastproof_stream::messages::UploadRecord {
            v: fastproof_stream::messages::VERSION,
            project: project.key.clone(),
            launch_id: launch.id,
            upload_id,
            blob_key: new_upload.blob_key.clone(),
            format: new_upload.format.clone(),
            file_name: new_upload.file_name.clone(),
        };
        let kafka = s.kafka()?;
        let produced = kafka
            .produce(
                &kafka.topic(fastproof_stream::kafka::TOPIC_UPLOADS),
                vec![fastproof_stream::kafka::Outgoing {
                    key: launch.id.to_string().into_bytes(),
                    value: serde_json::to_vec(&rec).map_err(anyhow::Error::from)?,
                }],
            )
            .await;
        if let Err(e) = produced {
            tracing::warn!(error = %format!("{e:#}"), "producing an upload to Kafka failed");
            if let Ok(Some(job)) = s.store.start_upload(upload_id, 1).await {
                let _ = s
                    .store
                    .fail_upload(
                        &job,
                        &fastproof_store::UploadError {
                            code: "unavailable".into(),
                            message: "the ingest queue was not available".into(),
                            limit: None,
                        },
                    )
                    .await;
            }
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "the ingest queue is not available; retry later",
            ));
        }
        audit_upload(&s, &auth, &project.key, &upload).await;
        return Ok((StatusCode::ACCEPTED, Json(upload)));
    }
    let upload = s.store.enqueue_upload(&new_upload).await?;
    s.wake.notify_one();
    audit_upload(&s, &auth, &project.key, &upload).await;
    Ok((StatusCode::ACCEPTED, Json(upload)))
}

async fn audit_upload(s: &AppState, auth: &Auth, project: &str, u: &Upload) {
    s.audit(
        auth,
        "upload.create",
        Some(project),
        serde_json::json!({"uploadId": u.id, "launchId": u.launch_id, "format": u.format,
            "sizeBytes": u.size_bytes}),
    )
    .await;
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/uploads", tag = "uploads", operation_id = "uploads_list",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = UploadList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<UploadList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    Ok(Json(UploadList {
        items: s.store.uploads(launch.id).await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(
            clean_file_name("../../etc/passwd").as_deref(),
            Some("passwd")
        );
        assert_eq!(
            clean_file_name("C:\\x\\report.xml").as_deref(),
            Some("report.xml")
        );
        assert_eq!(clean_file_name(".."), None);
        assert_eq!(clean_file_name(""), None);
    }
}
