//! Attachments: upload for the streaming API, and content served with sandboxing headers (A5).

use axum::Json;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_blob::{BlobKey, spool_stream};
use fastproof_store::attachments::NewAttachment;
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::uploads::{clean_file_name, identity_encoding, spool_error};
use super::{ErrorBody, P, Q};
use crate::state::AppState;

/// Header with the attachment's file name.
pub const NAME_HEADER: &str = "x-fastproof-attachment-name";

/// A stored attachment.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct AttachmentCreated {
    pub id: Uuid,
    pub sha256: String,
    pub size: u64,
}

/// `type/subtype` in lower case, or `application/octet-stream`.
pub fn clean_content_type(v: Option<&str>) -> String {
    let Some(v) = v else {
        return "application/octet-stream".into();
    };
    let base = v
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_'))
    };
    match base.split_once('/') {
        Some((t, st)) if ok(t) && ok(st) => base,
        _ => "application/octet-stream".into(),
    }
}

/// Types a browser may render inline under the sandbox; everything else is downloaded.
pub fn inline_safe(ct: &str) -> bool {
    matches!(
        ct,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
            | "video/mp4"
            | "video/webm"
            | "video/ogg"
            | "text/plain"
            | "text/csv"
            | "text/x-log"
            | "application/json"
    )
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/attachments", tag = "attachments",
    params(("key" = String, Path), ("id" = Uuid, Path),
        ("X-FastProof-Attachment-Name" = Option<String>, Header, description = "file name")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = AttachmentCreated), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 413, body = ErrorBody)))]
pub async fn upload(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<AttachmentCreated>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    identity_encoding(&req)?;
    let ct = clean_content_type(
        req.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    );
    let name = req
        .headers()
        .get(NAME_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(clean_file_name);
    let dir = s.cfg.spool_dir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(Uuid::now_v7().to_string());
    let (limit, max) = if s.limits.max_entry_bytes < s.cfg.max_upload_bytes {
        ("max_entry_bytes", s.limits.max_entry_bytes)
    } else {
        ("max_upload_bytes", s.cfg.max_upload_bytes)
    };
    let spooled = spool_stream(&path, req.into_body().into_data_stream().boxed(), max)
        .await
        .map_err(|e| spool_error(e, limit))?;
    let result = async {
        let existing = s
            .store
            .touch_attachments(project.id, std::slice::from_ref(&spooled.sha256))
            .await?;
        let blob_key = BlobKey::attachment(&project.key, &spooled.sha256)?;
        if !existing.contains_key(&spooled.sha256) {
            s.blobs.put_file(&blob_key, &spooled.path).await?;
        }
        let ids = s
            .store
            .insert_attachments(
                project.id,
                launch.id,
                &[NewAttachment {
                    sha256: spooled.sha256.clone(),
                    size: spooled.size as i64,
                    content_type: ct,
                    original_name: name,
                    blob_key: blob_key.to_string(),
                }],
            )
            .await?;
        ids.get(&spooled.sha256)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("attachment row missing"))
    }
    .await;
    spooled.remove();
    let id = result?;
    Ok((
        StatusCode::CREATED,
        Json(AttachmentCreated {
            id,
            sha256: spooled.sha256,
            size: spooled.size,
        }),
    ))
}

/// Query of the content route.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContentQuery {
    /// `1` forces a download.
    pub download: Option<String>,
}

fn disposition(kind: &str, name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/attachments/{id}/content", tag = "attachments",
    params(("key" = String, Path), ("id" = Uuid, Path), ContentQuery),
    responses((status = 200, description = "the bytes, with X-Content-Type-Options nosniff and a sandbox CSP"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn content(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ContentQuery>,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let a = s
        .store
        .attachment(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    let key = BlobKey::parse(&a.blob_key)?;
    let Some(body) = s.blobs.get(&key).await? else {
        tracing::warn!(attachment = %a.id, key = %key, "attachment blob is missing");
        return Err(ApiError::not_found("attachment"));
    };
    let ct = clean_content_type(Some(&a.content_type));
    let download = q
        .download
        .as_deref()
        .is_some_and(|v| v == "1" || v == "true")
        || !inline_safe(&ct);
    let name = a
        .original_name
        .clone()
        .unwrap_or_else(|| format!("attachment-{}", &a.sha256[..12]));
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&ct).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(body.size));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        disposition(if download { "attachment" } else { "inline" }, &name),
    );
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", a.sha256))
            .unwrap_or(HeaderValue::from_static("\"\"")),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    let stream = body.stream.map(|r| r.map_err(std::io::Error::other));
    Ok((StatusCode::OK, h, Body::from_stream(stream)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(clean_content_type(Some("Image/PNG; q=1")), "image/png");
        assert_eq!(
            clean_content_type(Some("text/html\r\nx: y")),
            "application/octet-stream"
        );
        assert_eq!(clean_content_type(None), "application/octet-stream");
        assert!(!inline_safe("text/html"));
        assert!(!inline_safe("image/svg+xml"));
        assert!(inline_safe("image/png"));
        let d = disposition("attachment", "a\"b\r\nc.html");
        assert!(
            d.to_str()
                .unwrap()
                .starts_with("attachment; filename=\"a_b__c.html\"")
        );
    }
}
