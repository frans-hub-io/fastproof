//! Direct uploads of attachments: pre-signed PUT URLs to S3 and their completion (see
//! `crate::direct_uploads`).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use fastproof_blob::{BlobKey, valid_sha256};
use fastproof_store::attachments::NewAttachment;
use fastproof_store::direct_uploads::{Issued, NewDirectUpload};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::attachments::{AttachmentCreated, clean_content_type};
use super::uploads::clean_file_name;
use super::{ErrorBody, P};
use crate::direct_uploads::available;
use crate::state::AppState;

/// Attachments per request.
pub const MAX_ITEMS: usize = 100;

/// One attachment the client wants to upload.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUploadItem {
    /// SHA-256 of the content, lowercase hex.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// `type/subtype`; anything else is stored as `application/octet-stream`.
    pub content_type: Option<String>,
    /// File name shown for the attachment.
    pub name: Option<String>,
}

/// Body of `POST .../launches/{id}/attachments/direct`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectUploadRequest {
    /// 1 to 100 attachments.
    pub items: Vec<DirectUploadItem>,
}

/// What to do with one attachment.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DirectUploadStatus {
    /// The project already stores this content: it is linked to the launch, use `id`.
    Exists,
    /// PUT the content to `url` with exactly `headers`, then complete `uploadId`.
    Upload,
    /// A limit refused it (`code`, `limit`): send it through `POST .../attachments` instead.
    Refused,
}

/// The answer for one attachment, in request order.
#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DirectUploadTarget {
    pub sha256: String,
    pub status: DirectUploadStatus,
    /// The attachment id to reference in results (`exists`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// `upload`: the id to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<Uuid>,
    /// `upload`: the pre-signed URL (a secret until it expires: do not log it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `upload`: always `PUT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// `upload`: headers the PUT must carry with exactly these values (they are signed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// `upload`: when the URL stops working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// `refused`: `too_large`, `launch_bytes`, `launch_open` or `project_open`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// `refused`: the limit that applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

impl DirectUploadTarget {
    fn new(sha256: &str, status: DirectUploadStatus) -> Self {
        Self {
            sha256: sha256.to_string(),
            status,
            id: None,
            upload_id: None,
            url: None,
            method: None,
            headers: None,
            expires_at: None,
            code: None,
            limit: None,
        }
    }

    fn refused(sha256: &str, code: &str, limit: u64) -> Self {
        Self {
            code: Some(code.to_string()),
            limit: Some(limit),
            ..Self::new(sha256, DirectUploadStatus::Refused)
        }
    }
}

/// Answer of `POST .../launches/{id}/attachments/direct`, an item per requested attachment.
#[derive(Serialize, Deserialize, ToSchema, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DirectUploadResponse {
    pub items: Vec<DirectUploadTarget>,
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "direct_uploads_unavailable",
        "this server takes attachments only through POST .../attachments",
    )
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/attachments/direct", tag = "attachments",
    operation_id = "attachments_direct_create", request_body = DirectUploadRequest,
    description = "Pre-signed S3 PUT URLs for attachments of the launch. Content the project already stores is linked at once (`exists`, only for callers who may read the launch). URLs are bound to `content-type: application/octet-stream` and, unless turned off, to `x-amz-checksum-sha256`; the object is verified on completion. 501 when the server has no S3 store or direct uploads are off: use POST .../attachments.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = DirectUploadResponse), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 501, body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<DirectUploadRequest>,
) -> ApiResult<Json<DirectUploadResponse>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    if !available(&s) {
        return Err(unavailable());
    }
    let settings = &s.cfg.direct_uploads;
    if body.items.is_empty() || body.items.len() > MAX_ITEMS {
        return Err(ApiError::bad_request(format!(
            "items: 1 to {MAX_ITEMS} attachments"
        )));
    }
    for (i, it) in body.items.iter().enumerate() {
        if !valid_sha256(&it.sha256) {
            return Err(ApiError::bad_request(format!(
                "items[{i}].sha256: 64 lowercase hex characters"
            )));
        }
        if it.content_type.as_ref().is_some_and(|c| c.len() > 255)
            || it.name.as_ref().is_some_and(|n| n.len() > 1024)
        {
            return Err(ApiError::bad_request(format!(
                "items[{i}]: contentType has at most 255 bytes, name 1024"
            )));
        }
    }
    let meta: Vec<(String, Option<String>)> = body
        .items
        .iter()
        .map(|it| {
            (
                clean_content_type(it.content_type.as_deref()),
                it.name.as_deref().and_then(clean_file_name),
            )
        })
        .collect();

    // Content the project has is linked at once, for callers who may read it anyway.
    let may_read = auth
        .principal
        .denial(&s.perms.launches_read, Some(&project.key))
        .is_none();
    let mut existing = std::collections::HashMap::new();
    if may_read {
        let shas: Vec<String> = body.items.iter().map(|i| i.sha256.clone()).collect();
        let known = s.store.touch_attachments(project.id, &shas).await?;
        if !known.is_empty() {
            let mut rows = Vec::new();
            for (it, (ct, name)) in body.items.iter().zip(&meta) {
                if known.contains_key(&it.sha256) {
                    rows.push(NewAttachment {
                        sha256: it.sha256.clone(),
                        size: it.size.min(i64::MAX as u64) as i64,
                        content_type: ct.clone(),
                        original_name: name.clone(),
                        blob_key: BlobKey::attachment(&project.key, &it.sha256)?.to_string(),
                    });
                }
            }
            existing = s
                .store
                .insert_attachments(project.id, launch.id, &rows)
                .await?;
        }
    }

    let mut out: Vec<Option<DirectUploadTarget>> = Vec::with_capacity(body.items.len());
    let mut wanted = Vec::new();
    let mut wanted_at = Vec::new();
    for (i, (it, (ct, name))) in body.items.iter().zip(&meta).enumerate() {
        if let Some(aid) = existing.get(&it.sha256) {
            out.push(Some(DirectUploadTarget {
                id: Some(*aid),
                ..DirectUploadTarget::new(&it.sha256, DirectUploadStatus::Exists)
            }));
        } else if it.size > settings.max_bytes {
            out.push(Some(DirectUploadTarget::refused(
                &it.sha256,
                "too_large",
                settings.max_bytes,
            )));
        } else {
            out.push(None);
            wanted_at.push(i);
            wanted.push(NewDirectUpload {
                sha256: it.sha256.clone(),
                size: it.size as i64,
                content_type: ct.clone(),
                original_name: name.clone(),
            });
        }
    }
    let issued = s
        .store
        .create_direct_uploads(
            project.id,
            launch.id,
            &auth.principal.name,
            &wanted,
            settings.url_ttl,
            &settings.limits(),
        )
        .await?;
    let (mut n_issued, mut bytes, mut n_refused) = (0u64, 0i64, 0u64);
    for ((i, w), r) in wanted_at.into_iter().zip(&wanted).zip(issued) {
        out[i] = Some(match r {
            Issued::Upload {
                id: upload_id,
                expires_at,
            } => {
                let key = BlobKey::direct_incoming(&upload_id.to_string())?;
                let put = s
                    .blobs
                    .presign_direct_put(
                        &key,
                        settings.url_ttl,
                        settings.checksum.then_some(w.sha256.as_str()),
                    )
                    .await?
                    .ok_or_else(unavailable)?;
                n_issued += 1;
                bytes += w.size;
                DirectUploadTarget {
                    upload_id: Some(upload_id),
                    url: Some(put.url),
                    method: Some("PUT".into()),
                    headers: Some(put.headers.into_iter().collect()),
                    expires_at: Some(expires_at),
                    ..DirectUploadTarget::new(&w.sha256, DirectUploadStatus::Upload)
                }
            }
            Issued::Refused { code, limit } => {
                n_refused += 1;
                DirectUploadTarget::refused(&w.sha256, code, limit.max(0) as u64)
            }
        });
    }
    metrics::counter!("fastproof_direct_uploads_issued_total").increment(n_issued);
    if n_issued > 0 || n_refused > 0 {
        s.audit(
            &auth,
            "attachment.direct.create",
            Some(&project.key),
            serde_json::json!({"launchId": launch.id, "issued": n_issued, "bytes": bytes,
                "existing": existing.len(), "refused": n_refused}),
        )
        .await;
    }
    Ok(Json(DirectUploadResponse {
        items: out.into_iter().flatten().collect(),
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/attachments/direct/{uploadId}/complete", tag = "attachments",
    operation_id = "attachments_direct_complete",
    description = "Verifies the object PUT to the upload URL (size and SHA-256, read back from a copy the URL cannot change) and records it as an attachment of the launch. Idempotent: a completed upload answers 200 with the same attachment.",
    params(("key" = String, Path), ("id" = Uuid, Path), ("uploadId" = Uuid, Path)),
    responses((status = 201, body = AttachmentCreated), (status = 200, description = "completed before", body = AttachmentCreated),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, description = "`not_uploaded` (PUT first) or `verification_in_progress` (retry)", body = ErrorBody),
        (status = 410, description = "`upload_expired`", body = ErrorBody),
        (status = 422, description = "`verification_failed`: the object was deleted; upload through POST .../attachments", body = ErrorBody),
        (status = 501, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn complete(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id, upload_id)): P<(String, Uuid, Uuid)>,
) -> ApiResult<(StatusCode, Json<AttachmentCreated>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    if !available(&s) {
        return Err(unavailable());
    }
    let c = crate::direct_uploads::complete(&s, &auth, &project, launch.id, upload_id).await?;
    Ok((
        if c.new {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(AttachmentCreated {
            id: c.attachment_id,
            sha256: c.sha256,
            size: c.size,
        }),
    ))
}
