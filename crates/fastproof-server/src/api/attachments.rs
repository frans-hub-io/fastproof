//! Attachments: upload for the streaming API, and content served with sandboxing headers (A5).

use axum::Json;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_blob::{BlobKey, spool_stream};
use fastproof_store::attachments::NewAttachment;
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::range::{self, RangeOutcome};
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

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/attachments", tag = "attachments", operation_id = "attachments_upload",
    params(("key" = String, Path), ("id" = Uuid, Path),
        ("X-FastProof-Attachment-Name" = Option<String>, Header, description = "file name")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = AttachmentCreated), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn upload(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    req: Request,
) -> ApiResult<(StatusCode, Json<AttachmentCreated>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let launch = s.launch_of(&project, id).await?;
    s.refuse_merged(&launch)?;
    let (limit, max) = if s.limits.max_entry_bytes < s.cfg.max_upload_bytes {
        ("max_entry_bytes", s.limits.max_entry_bytes)
    } else {
        ("max_upload_bytes", s.cfg.max_upload_bytes)
    };
    let created = store_attachment(&s, &project, launch.id, req, limit, max).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Spools a raw request body (at most `max` bytes, `limit` names the limit in the 413), stores
/// it content-addressed and links it to the launch.
pub async fn store_attachment(
    s: &AppState,
    project: &fastproof_store::Project,
    launch_id: Uuid,
    req: Request,
    limit: &str,
    max: u64,
) -> ApiResult<AttachmentCreated> {
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
    let _spool = s.guards.spool().await?;
    let dir = s.cfg.spool_dir.join("attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(Uuid::now_v7().to_string());
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
            s.store
                .cancel_blob_deletions(&[blob_key.to_string()])
                .await?;
            s.blobs.put_file(&blob_key, &spooled.path).await?;
        }
        let ids = s
            .store
            .insert_attachments(
                project.id,
                launch_id,
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
    Ok(AttachmentCreated {
        id,
        sha256: spooled.sha256,
        size: spooled.size,
    })
}

/// Query of the content route.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ContentQuery {
    /// `1` forces a download (of the original).
    pub download: Option<String>,
    /// `1` serves the original even when a seekable copy of a video is ready.
    pub original: Option<String>,
}

/// The blob a content request serves, with the validators of its bytes.
pub struct ServedBlob {
    pub key: BlobKey,
    pub sha256: String,
    pub size: u64,
    /// The bytes behind this address no longer change: false while a video still waits for its
    /// seekable copy (the response must then be revalidated).
    pub settled: bool,
}

/// Chooses what serves an attachment: the seekable copy of a video when one is ready (see
/// `crate::media_prepare`) unless `original`, else the stored original.
pub async fn blob_to_serve(
    s: &AppState,
    a: &fastproof_store::StoredAttachment,
    original: bool,
) -> ApiResult<ServedBlob> {
    let ct = clean_content_type(Some(&a.content_type));
    let mut settled = true;
    if !original && fastproof_store::media::VIDEO_TYPES.contains(&ct.as_str()) {
        match s.store.media_state(a.project_id, &a.sha256).await? {
            Some(m) if m.status == "ready" => {
                if let (Some(k), Some(sha), Some(n)) = (m.blob_key, m.sha256, m.size) {
                    return Ok(ServedBlob {
                        key: BlobKey::parse(&k)?,
                        sha256: sha,
                        size: n.max(0) as u64,
                        settled: true,
                    });
                }
            }
            Some(m) => settled = m.status != "running",
            None => settled = !s.cfg.media_prepare.enabled,
        }
    }
    Ok(ServedBlob {
        key: BlobKey::parse(&a.blob_key)?,
        sha256: a.sha256.clone(),
        size: a.size.max(0) as u64,
        settled,
    })
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

const RANGE_NOTE: &str = "Supports single byte ranges (206, 416), HEAD, a strong ETag (the SHA-256) with If-None-Match (304) and If-Range";

#[utoipa::path(get, path = "/api/v1/projects/{key}/attachments/{id}/content", tag = "attachments", operation_id = "attachments_content",
    params(("key" = String, Path), ("id" = Uuid, Path), ContentQuery),
    responses((status = 200, description = RANGE_NOTE),
        (status = 206, description = "the requested byte range, with Content-Range"),
        (status = 304, description = "If-None-Match matched the ETag"),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 416, description = "the range lies outside the content; Content-Range: bytes */size")))]
pub async fn content(
    State(s): State<AppState>,
    auth: Auth,
    method: Method,
    headers: HeaderMap,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ContentQuery>,
) -> ApiResult<Response> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let a = s
        .store
        .attachment(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("attachment"))?;
    let flag = |v: &Option<String>| v.as_deref().is_some_and(|v| v == "1" || v == "true");
    let force = flag(&q.download);
    serve(
        &s,
        &method,
        &headers,
        &a,
        force,
        force || flag(&q.original),
        Cache::Immutable,
    )
    .await
}

#[utoipa::path(head, path = "/api/v1/projects/{key}/attachments/{id}/content", tag = "attachments", operation_id = "attachments_content_head",
    params(("key" = String, Path), ("id" = Uuid, Path), ContentQuery),
    responses((status = 200, description = "the headers of the content"), (status = 206), (status = 304),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 416)))]
pub async fn content_head(
    state: State<AppState>,
    auth: Auth,
    method: Method,
    headers: HeaderMap,
    path: P<(String, Uuid)>,
    q: Q<ContentQuery>,
) -> ApiResult<Response> {
    content(state, auth, method, headers, path, q).await
}

/// How long a response may be cached.
#[derive(Clone, Copy, Debug)]
pub enum Cache {
    /// The content never changes behind its address.
    Immutable,
    /// Until this time (unix milliseconds): the lifetime of a media URL.
    Until(i64),
    /// Cached only with revalidation: the bytes behind the address may still change (a video
    /// whose seekable copy is being made).
    Revalidate,
}

impl Cache {
    fn header(self) -> HeaderValue {
        match self {
            Cache::Immutable => HeaderValue::from_static("private, max-age=31536000, immutable"),
            Cache::Revalidate => HeaderValue::from_static("private, no-cache"),
            Cache::Until(ms) => {
                let left = ((ms - chrono::Utc::now().timestamp_millis()) / 1000).max(0);
                HeaderValue::from_str(&format!("private, max-age={left}"))
                    .unwrap_or(HeaderValue::from_static("no-store"))
            }
        }
    }
}

fn sandbox(h: &mut HeaderMap, ct: &str, etag: &str, disposition: HeaderValue, cache: Cache) {
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(ct).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    h.insert(header::CACHE_CONTROL, cache.header());
    h.insert(header::CONTENT_DISPOSITION, disposition);
    h.insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("\"\"")),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
}

fn header_str(h: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn stream_permit(s: &AppState) -> ApiResult<tokio::sync::OwnedSemaphorePermit> {
    take_permit(s.media.streams())
}

fn take_permit(
    sem: &std::sync::Arc<tokio::sync::Semaphore>,
) -> ApiResult<tokio::sync::OwnedSemaphorePermit> {
    sem.clone().try_acquire_owned().map_err(|_| {
        metrics::counter!("attachment_streams_refused_total").increment(1);
        ApiError::unavailable("too many downloads in progress, retry shortly")
            .with_header(header::RETRY_AFTER, HeaderValue::from_static("1"))
    })
}

/// A body fed by a task that holds the stream permit and gives up when the store or the client
/// makes no progress for `timeout`.
fn paced(
    stream: futures::stream::BoxStream<'static, anyhow::Result<bytes::Bytes>>,
    permit: tokio::sync::OwnedSemaphorePermit,
    timeout: std::time::Duration,
) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(2);
    tokio::spawn(async move {
        let _permit = permit;
        let mut stream = stream;
        loop {
            match tokio::time::timeout(timeout, stream.next()).await {
                Ok(Some(Ok(b))) => {
                    if !matches!(
                        tokio::time::timeout(timeout, tx.send(Ok(b))).await,
                        Ok(Ok(()))
                    ) {
                        return;
                    }
                }
                Ok(Some(Err(e))) => {
                    let _ = tx.send(Err(std::io::Error::other(e))).await;
                    return;
                }
                Ok(None) => return,
                Err(_) => {
                    let _ = tx.try_send(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "no progress",
                    )));
                    return;
                }
            }
        }
    });
    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// Serves a stored attachment: 200, 206 for one byte range (a ranged read of the store), 304,
/// 416, `HEAD`, always with the sandboxing headers. Used by the content route and by media URLs.
/// The bytes come from [`blob_to_serve`]; the ETag is the SHA-256 of those bytes.
pub async fn serve(
    s: &AppState,
    method: &Method,
    req: &HeaderMap,
    a: &fastproof_store::StoredAttachment,
    force_download: bool,
    original: bool,
    cache: Cache,
) -> ApiResult<Response> {
    let blob = blob_to_serve(s, a, original).await?;
    let cache = if blob.settled {
        cache
    } else {
        Cache::Revalidate
    };
    let key = blob.key;
    let ct = clean_content_type(Some(&a.content_type));
    let download = force_download || !inline_safe(&ct);
    let name = a
        .original_name
        .clone()
        .unwrap_or_else(|| format!("attachment-{}", &a.sha256[..12]));
    let etag = format!("\"{}\"", blob.sha256);
    let mut h = HeaderMap::new();
    sandbox(
        &mut h,
        &ct,
        &etag,
        disposition(if download { "attachment" } else { "inline" }, &name),
        cache,
    );
    let head = method == Method::HEAD;
    let size = blob.size;

    if header_str(req, header::IF_NONE_MATCH).is_some_and(|v| range::none_match(v, &etag)) {
        return Ok((StatusCode::NOT_MODIFIED, h).into_response());
    }
    let outcome = match header_str(req, header::RANGE) {
        Some(v)
            if header_str(req, header::IF_RANGE).is_none_or(|ir| range::if_range_ok(ir, &etag)) =>
        {
            range::parse(v, size)
        }
        _ => RangeOutcome::Full,
    };
    let missing = || {
        tracing::warn!(attachment = %a.id, key = %key, "attachment blob is missing");
        ApiError::not_found("attachment")
    };
    match outcome {
        RangeOutcome::Unsatisfiable => {
            h.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}"))
                    .unwrap_or(HeaderValue::from_static("bytes */*")),
            );
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(0u64));
            Ok((StatusCode::RANGE_NOT_SATISFIABLE, h).into_response())
        }
        RangeOutcome::Partial { start, end } => {
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start));
            h.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1))
                    .unwrap_or(HeaderValue::from_static("bytes */*")),
            );
            if head {
                if s.blobs.head(&key).await?.is_none() {
                    return Err(missing());
                }
                return Ok((StatusCode::PARTIAL_CONTENT, h).into_response());
            }
            let Some(body) = s.blobs.get_range(&key, start..end).await? else {
                return Err(missing());
            };
            metrics::counter!("attachment_bytes_served_total").increment(end - start);
            let stream = body.stream.map(|r| r.map_err(std::io::Error::other));
            Ok((StatusCode::PARTIAL_CONTENT, h, Body::from_stream(stream)).into_response())
        }
        RangeOutcome::Full => {
            if head {
                let Some(n) = s.blobs.head(&key).await? else {
                    return Err(missing());
                };
                h.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
                return Ok((StatusCode::OK, h).into_response());
            }
            let permit = stream_permit(s)?;
            let Some(body) = s.blobs.get(&key).await? else {
                return Err(missing());
            };
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(body.size));
            metrics::counter!("attachment_bytes_served_total").increment(body.size);
            Ok((
                StatusCode::OK,
                h,
                paced(body.stream, permit, s.media.stream_timeout()),
            )
                .into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streams_are_limited_and_stalled_ones_let_go() {
        use std::sync::Arc;
        use std::time::Duration;
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let p = take_permit(&sem).unwrap();
        let busy = take_permit(&sem).unwrap_err();
        assert_eq!(
            busy.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let endless =
            futures::stream::repeat_with(|| Ok(bytes::Bytes::from_static(&[0u8; 1024]))).boxed();
        let body = paced(endless, p, Duration::from_millis(200));
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(
            sem.available_permits(),
            1,
            "the unread stream gave its slot back"
        );
        drop(body);
        let ended = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(b"abc"))]).boxed();
        let p = take_permit(&sem).unwrap();
        let body = paced(ended, p, Duration::from_secs(5));
        let got = axum::body::to_bytes(body, 1024).await.unwrap();
        assert_eq!(got.as_ref(), b"abc");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            sem.available_permits(),
            1,
            "a finished stream released its slot"
        );
    }

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
