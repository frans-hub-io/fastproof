//! Direct uploads of attachments to S3 through pre-signed PUT URLs.
//!
//! The client declares SHA-256, size and type per attachment and gets a URL for
//! `direct/<id>/in`, bound to an inert content type and (by default) to the declared checksum.
//! On completion the server copies the object to `direct/<id>/check`, which the URL cannot
//! write, hashes that copy, and only then copies it to the content-addressed attachment key
//! and records the attachment like any other upload (so dedup, media URLs, the seekable copies
//! of `media.prepare` and the garbage collector see no difference). Objects that do not match
//! are deleted; the `direct_uploads.sweep` job removes what expired or was left behind.

use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use axum::http::{HeaderValue, StatusCode, header};
use fastproof_blob::BlobKey;
use fastproof_store::Project;
use fastproof_store::attachments::NewAttachment;
use fastproof_store::direct_uploads::{Claim, DirectLimits, DirectUpload};
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::config::Env;
use crate::state::AppState;

/// The largest single PUT S3 accepts.
pub const MAX_OBJECT_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Settings (`FASTPROOF_DIRECT_UPLOAD*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectSettings {
    /// `FASTPROOF_DIRECT_UPLOADS` (default on; only the S3 backend issues URLs).
    pub enabled: bool,
    /// Lifetime of an upload URL (`FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS`, 60 to 21600,
    /// default 900).
    pub url_ttl: Duration,
    /// Largest attachment (`FASTPROOF_DIRECT_UPLOAD_MAX_BYTES`, at most 5 GiB, default 1 GiB).
    pub max_bytes: u64,
    /// `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_LAUNCH` (default 1000).
    pub max_open_per_launch: u64,
    /// `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_PROJECT` (default 10000).
    pub max_open_per_project: u64,
    /// Declared bytes per launch (`FASTPROOF_DIRECT_UPLOAD_MAX_BYTES_PER_LAUNCH`, default 50 GiB).
    pub max_bytes_per_launch: u64,
    /// Bind `x-amz-checksum-sha256` to the URL (`FASTPROOF_DIRECT_UPLOAD_CHECKSUM`, default on;
    /// turn off for an S3-compatible store that rejects the header).
    pub checksum: bool,
    /// Verifications at once per replica (`FASTPROOF_DIRECT_UPLOAD_VERIFY_CONCURRENCY`, 1 to 64,
    /// default 4).
    pub verify_concurrency: usize,
    /// Time allowed for one verification (`FASTPROOF_DIRECT_UPLOAD_VERIFY_TIMEOUT_SECONDS`, 10
    /// to 3600, default 600).
    pub verify_timeout: Duration,
}

impl Default for DirectSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            url_ttl: Duration::from_secs(900),
            max_bytes: 1 << 30,
            max_open_per_launch: 1000,
            max_open_per_project: 10_000,
            max_bytes_per_launch: 50 << 30,
            checksum: true,
            verify_concurrency: 4,
            verify_timeout: Duration::from_secs(600),
        }
    }
}

fn number(env: &Env<'_>, k: &str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
    match env.var(k) {
        None => Ok(default),
        Some(v) => match v.trim().parse::<u64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(n),
            _ => bail!("{k}: a number {min}..={max}"),
        },
    }
}

fn flag(env: &Env<'_>, k: &str, default: bool) -> anyhow::Result<bool> {
    match env.var(k).map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None => Ok(default),
        Some("true" | "1" | "yes" | "on") => Ok(true),
        Some("false" | "0" | "no" | "off") => Ok(false),
        Some(v) => bail!("{k}: expected true or false, got {v:?}"),
    }
}

impl DirectSettings {
    pub fn from_env(env: &Env<'_>) -> anyhow::Result<Self> {
        let d = Self::default();
        Ok(Self {
            enabled: flag(env, "FASTPROOF_DIRECT_UPLOADS", d.enabled)?,
            url_ttl: Duration::from_secs(number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS",
                d.url_ttl.as_secs(),
                60,
                21_600,
            )?),
            max_bytes: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_MAX_BYTES",
                d.max_bytes,
                1,
                MAX_OBJECT_BYTES,
            )?,
            max_open_per_launch: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_LAUNCH",
                d.max_open_per_launch,
                1,
                1_000_000,
            )?,
            max_open_per_project: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_PROJECT",
                d.max_open_per_project,
                1,
                10_000_000,
            )?,
            max_bytes_per_launch: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_MAX_BYTES_PER_LAUNCH",
                d.max_bytes_per_launch,
                1,
                i64::MAX as u64,
            )?,
            checksum: flag(env, "FASTPROOF_DIRECT_UPLOAD_CHECKSUM", d.checksum)?,
            verify_concurrency: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_VERIFY_CONCURRENCY",
                d.verify_concurrency as u64,
                1,
                64,
            )? as usize,
            verify_timeout: Duration::from_secs(number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_VERIFY_TIMEOUT_SECONDS",
                d.verify_timeout.as_secs(),
                10,
                3600,
            )?),
        })
    }

    pub fn limits(&self) -> DirectLimits {
        DirectLimits {
            max_open_per_launch: self.max_open_per_launch as i64,
            max_open_per_project: self.max_open_per_project as i64,
            max_bytes_per_launch: self.max_bytes_per_launch.min(i64::MAX as u64) as i64,
        }
    }

    /// A claim older than this belongs to a verification that died.
    pub fn lease(&self) -> Duration {
        self.verify_timeout + Duration::from_secs(60)
    }
}

/// Per-replica state.
#[derive(Clone, Debug)]
pub struct DirectState {
    verify: Arc<Semaphore>,
}

impl DirectState {
    pub fn new(s: &DirectSettings) -> Self {
        Self {
            verify: Arc::new(Semaphore::new(s.verify_concurrency.max(1))),
        }
    }
}

/// Direct uploads are offered: enabled and an S3 store.
pub fn available(s: &AppState) -> bool {
    s.cfg.direct_uploads.enabled && s.blobs.backend() == "s3"
}

/// A verified upload, as the attachment it became.
pub struct Completed {
    pub attachment_id: Uuid,
    pub sha256: String,
    pub size: u64,
    /// False when an earlier call completed it.
    pub new: bool,
}

/// Why a verification did not store the attachment.
enum Verdict {
    /// Neither the client's object nor a server copy exists.
    Missing,
    /// The object is not what was declared (deleted).
    Mismatch(String),
    Store(anyhow::Error),
}

impl From<anyhow::Error> for Verdict {
    fn from(e: anyhow::Error) -> Self {
        Verdict::Store(e)
    }
}

fn keys(id: Uuid) -> anyhow::Result<(BlobKey, BlobKey)> {
    let id = id.to_string();
    Ok((BlobKey::direct_incoming(&id)?, BlobKey::direct_check(&id)?))
}

async fn verify(s: &AppState, project: &Project, u: &DirectUpload) -> Result<Uuid, Verdict> {
    let (incoming, check) = keys(u.id)?;
    let declared = u.size.max(0) as u64;
    match s.blobs.head(&incoming).await? {
        Some(n) => {
            if n != declared {
                s.blobs.delete(&incoming).await?;
                return Err(Verdict::Mismatch(format!(
                    "the object has {n} bytes, {declared} were declared"
                )));
            }
            s.blobs.copy(&incoming, &check).await?;
            s.blobs.delete(&incoming).await?;
        }
        None => {
            if s.blobs.head(&check).await?.is_none() {
                return Err(Verdict::Missing);
            }
        }
    }
    let Some(mut body) = s.blobs.get(&check).await? else {
        return Err(Verdict::Missing);
    };
    let mut hasher = Sha256::new();
    let mut n = 0u64;
    let mut over = body.size != declared;
    while !over && let Some(chunk) = body.stream.next().await {
        let chunk = chunk?;
        n += chunk.len() as u64;
        if n > declared {
            over = true;
            break;
        }
        hasher.update(&chunk);
    }
    drop(body);
    if over || n != declared {
        s.blobs.delete(&check).await?;
        return Err(Verdict::Mismatch(format!(
            "the object does not have the {declared} declared bytes"
        )));
    }
    if hex::encode(hasher.finalize()) != u.sha256 {
        s.blobs.delete(&check).await?;
        return Err(Verdict::Mismatch(
            "the object does not match the declared sha256".into(),
        ));
    }
    let key = BlobKey::attachment(&project.key, &u.sha256)?;
    let existing = s
        .store
        .touch_attachments(project.id, std::slice::from_ref(&u.sha256))
        .await?;
    if !existing.contains_key(&u.sha256) {
        s.store.cancel_blob_deletions(&[key.to_string()]).await?;
        s.blobs.copy(&check, &key).await?;
    }
    s.blobs.delete(&check).await?;
    let ids = s
        .store
        .insert_attachments(
            project.id,
            u.launch_id,
            &[NewAttachment {
                sha256: u.sha256.clone(),
                size: u.size,
                content_type: u.content_type.clone(),
                original_name: u.original_name.clone(),
                blob_key: key.to_string(),
            }],
        )
        .await?;
    let id = ids
        .get(&u.sha256)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("attachment row missing"))?;
    Ok(id)
}

fn retry_after(e: ApiError, secs: &'static str) -> ApiError {
    e.with_header(header::RETRY_AFTER, HeaderValue::from_static(secs))
}

fn outcome(o: &'static str) {
    metrics::counter!("fastproof_direct_uploads_completed_total", "outcome" => o).increment(1);
}

/// Completes a direct upload of the launch: verifies the object and records the attachment
/// (idempotent: a completed upload answers its attachment again).
pub async fn complete(
    s: &AppState,
    auth: &franshub_auth::Auth,
    project: &Project,
    launch_id: Uuid,
    id: Uuid,
) -> ApiResult<Completed> {
    let settings = &s.cfg.direct_uploads;
    let u = match s
        .store
        .claim_direct_upload(project.id, launch_id, id, settings.lease())
        .await?
    {
        Claim::Verify(u) => u,
        Claim::Done(u) => {
            return Ok(Completed {
                attachment_id: u
                    .attachment_id
                    .ok_or_else(|| anyhow::anyhow!("completed upload without attachment"))?,
                sha256: u.sha256,
                size: u.size.max(0) as u64,
                new: false,
            });
        }
        Claim::Busy => {
            return Err(retry_after(
                ApiError::new(
                    StatusCode::CONFLICT,
                    "verification_in_progress",
                    "the upload is being verified; retry shortly",
                ),
                "2",
            ));
        }
        Claim::Expired => {
            return Err(ApiError::new(
                StatusCode::GONE,
                "upload_expired",
                "the upload URL expired before the upload was completed; request a new one",
            ));
        }
        Claim::Failed(reason) => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "verification_failed",
                format!("the uploaded object was rejected: {reason}"),
            ));
        }
        Claim::NotFound => return Err(ApiError::not_found("direct upload")),
    };
    let permit = match tokio::time::timeout(
        Duration::from_secs(10),
        s.direct.verify.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(p)) => p,
        _ => {
            s.store.release_direct_upload(u.id).await?;
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "direct_verify")
                .increment(1);
            return Err(retry_after(
                ApiError::unavailable("too many direct uploads are being verified; retry shortly"),
                "2",
            ));
        }
    };
    let result = tokio::time::timeout(settings.verify_timeout, verify(s, project, &u)).await;
    drop(permit);
    match result {
        Ok(Ok(attachment_id)) => {
            s.store.finish_direct_upload(u.id, attachment_id).await?;
            outcome("verified");
            Ok(Completed {
                attachment_id,
                sha256: u.sha256,
                size: u.size.max(0) as u64,
                new: true,
            })
        }
        Ok(Err(Verdict::Missing)) => {
            s.store.release_direct_upload(u.id).await?;
            outcome("missing");
            Err(ApiError::new(
                StatusCode::CONFLICT,
                "not_uploaded",
                "the object is not in the store yet: PUT it to the upload URL first",
            ))
        }
        Ok(Err(Verdict::Mismatch(reason))) => {
            s.store.fail_direct_upload(u.id, &reason).await?;
            outcome("rejected");
            tracing::warn!(upload = %u.id, project = %project.key, %reason, "direct upload rejected");
            s.audit(
                auth,
                "attachment.direct.rejected",
                Some(&project.key),
                serde_json::json!({"uploadId": u.id, "launchId": launch_id, "sha256": u.sha256,
                    "size": u.size, "reason": reason}),
            )
            .await;
            Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "verification_failed",
                format!("the uploaded object was rejected: {reason}"),
            ))
        }
        Ok(Err(Verdict::Store(e))) => {
            s.store.release_direct_upload(u.id).await?;
            outcome("store_error");
            tracing::error!(upload = %u.id, error = %format!("{e:#}"), "verifying a direct upload failed");
            Err(retry_after(
                ApiError::unavailable("the blob store failed while verifying; retry shortly"),
                "5",
            ))
        }
        Err(_) => {
            s.store.release_direct_upload(u.id).await?;
            outcome("timeout");
            tracing::warn!(upload = %u.id, "verifying a direct upload timed out");
            Err(retry_after(
                ApiError::unavailable("verifying the upload took too long; retry later"),
                "30",
            ))
        }
    }
}

/// A file in a report archive that the CLI uploaded directly is replaced by
/// `fastproof-attachment-ref:v1:<attachment id>\n`; ingest links that attachment when it belongs
/// to the upload's launch.
pub const STUB_PREFIX: &[u8] = b"fastproof-attachment-ref:v1:";

/// Longest stub body.
pub const STUB_MAX: usize = 80;

/// Prefix of the placeholder attachment id a stub gets while a report is parsed.
pub const STUB_ID: &str = "attachment-ref:";

/// The body of a stub for an attachment id.
pub fn stub(attachment_id: Uuid) -> Vec<u8> {
    let mut v = STUB_PREFIX.to_vec();
    v.extend_from_slice(attachment_id.to_string().as_bytes());
    v.push(b'\n');
    v
}

/// The attachment id of a stub body, `None` for any other content.
pub fn parse_stub(body: &[u8]) -> Option<Uuid> {
    if body.len() > STUB_MAX {
        return None;
    }
    let rest = body.strip_prefix(STUB_PREFIX)?;
    let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
    let s = std::str::from_utf8(rest).ok()?;
    if s.len() != 36 {
        return None;
    }
    s.parse().ok()
}

/// What a sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    pub expired: u64,
    pub orphans: u64,
    pub pruned: u64,
}

/// Rows kept after an upload finished (quotas count completed bytes while they exist).
pub const KEEP_FINISHED: Duration = Duration::from_secs(30 * 86_400);

/// Objects under `direct/` this old are deleted unless their upload can still be completed.
pub const ORPHAN_AGE: Duration = Duration::from_secs(900);

/// Expires uploads whose URL ran out, deletes their objects and objects nobody will complete,
/// and prunes old rows. Objects younger than `orphan_age` are kept ([`ORPHAN_AGE`] in the job).
pub async fn sweep(s: &AppState, orphan_age: Duration) -> anyhow::Result<SweepStats> {
    let mut st = SweepStats::default();
    let lease = s.cfg.direct_uploads.lease();
    loop {
        let ids = s.store.expire_direct_uploads(lease, 1000).await?;
        for id in &ids {
            let (incoming, check) = keys(*id)?;
            s.blobs.delete(&incoming).await?;
            s.blobs.delete(&check).await?;
        }
        st.expired += ids.len() as u64;
        if ids.len() < 1000 {
            break;
        }
    }
    let objects = s.blobs.list(&BlobKey::direct_prefix()).await?;
    let old: Vec<(String, Uuid)> = objects
        .into_iter()
        .filter(|(_, at)| (chrono::Utc::now() - *at).to_std().unwrap_or_default() >= orphan_age)
        .filter_map(|(k, _)| {
            let id = k.split('/').nth(1)?.parse::<Uuid>().ok()?;
            Some((k, id))
        })
        .collect();
    for chunk in old.chunks(1000) {
        let ids: Vec<Uuid> = chunk.iter().map(|(_, i)| *i).collect();
        let live = s.store.live_direct_uploads(&ids).await?;
        for (k, id) in chunk {
            if live.contains(id) {
                continue;
            }
            let Ok(key) = BlobKey::parse(k) else { continue };
            s.blobs.delete(&key).await?;
            st.orphans += 1;
        }
    }
    loop {
        let n = s.store.prune_direct_uploads(KEEP_FINISHED, 10_000).await?;
        st.pruned += n;
        if n < 10_000 {
            break;
        }
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn parse(vars: &[(&str, &str)]) -> anyhow::Result<DirectSettings> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let get = |k: &str| m.get(k).cloned();
        DirectSettings::from_env(&Env::new(&get))
    }

    #[test]
    fn stubs_round_trip_and_nothing_else_parses() {
        let id = Uuid::now_v7();
        let b = stub(id);
        assert!(b.len() <= STUB_MAX);
        assert_eq!(parse_stub(&b), Some(id));
        assert_eq!(parse_stub(&b[..b.len() - 1]), Some(id));
        assert_eq!(parse_stub(b"fastproof-attachment-ref:v1:not-a-uuid"), None);
        assert_eq!(parse_stub(b"hello"), None);
        let mut long = b.clone();
        long.extend_from_slice(&[b' '; 80]);
        assert_eq!(parse_stub(&long), None);
    }

    #[test]
    fn settings_have_bounds() {
        assert_eq!(parse(&[]).unwrap(), DirectSettings::default());
        let s = parse(&[
            ("FASTPROOF_DIRECT_UPLOADS", "false"),
            ("FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS", "120"),
            ("FASTPROOF_DIRECT_UPLOAD_MAX_BYTES", "5368709120"),
            ("FASTPROOF_DIRECT_UPLOAD_CHECKSUM", "off"),
        ])
        .unwrap();
        assert!(!s.enabled && !s.checksum);
        assert_eq!(s.url_ttl, Duration::from_secs(120));
        assert_eq!(s.max_bytes, MAX_OBJECT_BYTES);
        for (k, v) in [
            ("FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS", "59"),
            ("FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS", "86400"),
            ("FASTPROOF_DIRECT_UPLOAD_MAX_BYTES", "5368709121"),
            ("FASTPROOF_DIRECT_UPLOAD_VERIFY_CONCURRENCY", "0"),
            ("FASTPROOF_DIRECT_UPLOADS", "maybe"),
        ] {
            let e = parse(&[(k, v)]).unwrap_err().to_string();
            assert!(e.starts_with(k), "{e}");
        }
    }
}
