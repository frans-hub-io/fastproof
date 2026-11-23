//! Direct uploads of attachments to S3 through pre-signed PUT URLs.
//!
//! The client declares SHA-256, size and type per attachment and gets a URL for
//! `direct/<installation>/<id>/in`, bound to an inert content type and (by default) to the
//! declared checksum. On completion the server copies the object to a key of its own for this
//! verification attempt (`.../check-<nonce>`), which the URL cannot write, hashes that copy, and
//! only then copies it to the content-addressed attachment key and records the attachment like
//! any other upload (so dedup, media URLs, the seekable copies of `media.prepare` and the garbage
//! collector see no difference). Objects that do not match are deleted; the
//! `direct_uploads.sweep` job removes what expired or was left behind.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::bail;
use axum::http::{HeaderValue, StatusCode, header};
use fastproof_blob::BlobKey;
use fastproof_store::Project;
use fastproof_store::attachments::NewAttachment;
use fastproof_store::direct_uploads::{Claim, DirectLimits, DirectUpload, Leftover};
use franshub_http::{ApiError, ApiResult};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OnceCell, Semaphore};
use uuid::Uuid;

use crate::config::Env;
use crate::state::AppState;

/// The largest single PUT S3 accepts.
pub const MAX_OBJECT_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Open uploads per launch and per project, and their bytes per project, when URLs are not
/// bound to the checksum (`FASTPROOF_DIRECT_UPLOAD_CHECKSUM=false`): lower bounds apply.
pub const UNCHECKED_OPEN_PER_LAUNCH: u64 = 100;
pub const UNCHECKED_OPEN_PER_PROJECT: u64 = 1000;
pub const UNCHECKED_OPEN_BYTES_PER_PROJECT: u64 = 10 << 30;

/// Settings (`FASTPROOF_DIRECT_UPLOAD*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectSettings {
    /// `FASTPROOF_DIRECT_UPLOADS` (default on; only the S3 backend issues URLs).
    pub enabled: bool,
    /// Lifetime of an upload URL (`FASTPROOF_DIRECT_UPLOAD_URL_TTL_SECONDS`, 60 to 21600,
    /// default 900).
    pub url_ttl: Duration,
    /// Largest attachment (`FASTPROOF_DIRECT_UPLOAD_MAX_BYTES`, at most 5 GiB, default 1 GiB),
    /// never above the limits of the server path ([`DirectSettings::clamp`]).
    pub max_bytes: u64,
    /// `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_LAUNCH` (default 1000).
    pub max_open_per_launch: u64,
    /// `FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_PER_PROJECT` (default 10000).
    pub max_open_per_project: u64,
    /// Declared bytes per launch (`FASTPROOF_DIRECT_UPLOAD_MAX_BYTES_PER_LAUNCH`, default 50 GiB).
    pub max_bytes_per_launch: u64,
    /// Declared bytes of open uploads per project
    /// (`FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_BYTES_PER_PROJECT`, default 100 GiB).
    pub max_open_bytes_per_project: u64,
    /// Bind `x-amz-checksum-sha256` to the URL (`FASTPROOF_DIRECT_UPLOAD_CHECKSUM`, default on;
    /// turn off for an S3-compatible store that rejects the header).
    pub checksum: bool,
    /// Hand out `http://` URLs (`FASTPROOF_DIRECT_UPLOAD_ALLOW_HTTP`, default off).
    pub allow_http: bool,
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
            max_open_bytes_per_project: 100 << 30,
            checksum: true,
            allow_http: false,
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

/// A boolean variable (`true|1|yes|on`, `false|0|no|off`).
pub fn flag(env: &Env<'_>, k: &str, default: bool) -> anyhow::Result<bool> {
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
            max_open_bytes_per_project: number(
                env,
                "FASTPROOF_DIRECT_UPLOAD_MAX_OPEN_BYTES_PER_PROJECT",
                d.max_open_bytes_per_project,
                1,
                i64::MAX as u64,
            )?,
            checksum: flag(env, "FASTPROOF_DIRECT_UPLOAD_CHECKSUM", d.checksum)?,
            allow_http: flag(env, "FASTPROOF_DIRECT_UPLOAD_ALLOW_HTTP", d.allow_http)?,
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

    /// Keeps the largest direct attachment within the limits of the server path
    /// (`FASTPROOF_MAX_UPLOAD_BYTES` and the per-entry limit).
    pub fn clamp(&mut self, max_upload_bytes: u64, max_entry_bytes: u64) {
        self.max_bytes = self.max_bytes.min(max_upload_bytes).min(max_entry_bytes);
    }

    pub fn limits(&self) -> DirectLimits {
        let i = |v: u64| v.min(i64::MAX as u64) as i64;
        let (launch, project, bytes) = if self.checksum {
            (
                self.max_open_per_launch,
                self.max_open_per_project,
                self.max_open_bytes_per_project,
            )
        } else {
            (
                self.max_open_per_launch.min(UNCHECKED_OPEN_PER_LAUNCH),
                self.max_open_per_project.min(UNCHECKED_OPEN_PER_PROJECT),
                self.max_open_bytes_per_project
                    .min(UNCHECKED_OPEN_BYTES_PER_PROJECT),
            )
        };
        DirectLimits {
            max_open_per_launch: i(launch),
            max_open_per_project: i(project),
            max_bytes_per_launch: i(self.max_bytes_per_launch),
            max_open_bytes_per_project: i(bytes),
        }
    }

    /// A claim older than this belongs to a verification that died.
    pub fn lease(&self) -> Duration {
        self.verify_timeout + Duration::from_secs(60)
    }
}

/// How often a failed check of the store is retried.
const PROBE_RETRY: Duration = Duration::from_secs(60);

/// Per-replica state.
#[derive(Clone, Debug)]
pub struct DirectState {
    verify: Arc<Semaphore>,
    installation: Arc<OnceCell<String>>,
    /// Whether the store refuses bodies that do not match the signed checksum, once known; or
    /// when the last check failed.
    probe: Arc<Mutex<Option<Result<bool, Instant>>>>,
    warned: Arc<AtomicBool>,
}

impl DirectState {
    pub fn new(s: &DirectSettings) -> Self {
        Self {
            verify: Arc::new(Semaphore::new(s.verify_concurrency.max(1))),
            installation: Arc::default(),
            probe: Arc::default(),
            warned: Arc::default(),
        }
    }
}

/// The id of this installation in `direct/` keys (shared by its replicas through the database).
pub async fn installation(s: &AppState) -> anyhow::Result<String> {
    Ok(s.direct
        .installation
        .get_or_try_init(|| s.store.direct_upload_installation())
        .await?
        .clone())
}

fn warn_once(s: &AppState, msg: &str) {
    if !s.direct.warned.swap(true, Ordering::Relaxed) {
        tracing::error!("direct uploads are off: {msg}");
    }
}

/// Whether the store refuses a body that does not match the signed checksum (checked once per
/// replica and remembered; a failed check is retried after a minute).
async fn checksum_enforced(s: &AppState, inst: &str) -> bool {
    let mut p = s.direct.probe.lock().await;
    match *p {
        Some(Ok(v)) => return v,
        Some(Err(at)) if at.elapsed() < PROBE_RETRY => return false,
        _ => {}
    }
    let id = format!("probe-{}", Uuid::new_v4().simple());
    let r = match BlobKey::direct_incoming(inst, &id) {
        Ok(k) => s.blobs.checksum_enforced(&k).await,
        Err(e) => Err(e),
    };
    match r {
        Ok(Some(v)) => {
            if !v {
                warn_once(
                    s,
                    "the S3 store accepts bodies that do not match a signed x-amz-checksum-sha256; \
                     set FASTPROOF_DIRECT_UPLOAD_CHECKSUM=false to use direct uploads with lower open quotas",
                );
            }
            *p = Some(Ok(v));
            v
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "checking the S3 store for direct uploads failed");
            *p = Some(Err(Instant::now()));
            false
        }
    }
}

/// Direct uploads are offered: enabled, an S3 store, https URLs (unless allowed otherwise) and
/// a store that checks the signed checksum (unless the binding is off). Returns the
/// installation id of the keys.
pub async fn ready(s: &AppState) -> anyhow::Result<Option<String>> {
    let c = &s.cfg.direct_uploads;
    if !c.enabled || s.blobs.backend() != "s3" {
        return Ok(None);
    }
    if s.blobs.client_urls_https() != Some(true) && !c.allow_http {
        warn_once(
            s,
            "upload URLs would use http; set FASTPROOF_S3_PUBLIC_ENDPOINT to an https endpoint or FASTPROOF_DIRECT_UPLOAD_ALLOW_HTTP=true",
        );
        return Ok(None);
    }
    let inst = installation(s).await?;
    if c.checksum && !checksum_enforced(s, &inst).await {
        return Ok(None);
    }
    Ok(Some(inst))
}

/// Checks the store once at start, so the first upload does not wait for it.
pub async fn warm(s: AppState) {
    if let Err(e) = ready(&s).await {
        tracing::warn!(error = %format!("{e:#}"), "preparing direct uploads failed");
    }
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

fn incoming(inst: &str, id: Uuid) -> anyhow::Result<BlobKey> {
    BlobKey::direct_incoming(inst, &id.to_string())
}

fn check(inst: &str, id: Uuid, claim: &str) -> anyhow::Result<BlobKey> {
    BlobKey::direct_check(inst, &id.to_string(), claim)
}

/// Verifies under the attempt `claim`; `previous` is the attempt it took over, whose copy is
/// used when the client's object is gone.
async fn verify(
    s: &AppState,
    project: &Project,
    u: &DirectUpload,
    inst: &str,
    claim: &str,
    previous: Option<&str>,
) -> Result<Uuid, Verdict> {
    let incoming = incoming(inst, u.id)?;
    let check = check(inst, u.id, claim)?;
    let previous = previous
        .filter(|p| *p != claim)
        .map(|p| self::check(inst, u.id, p))
        .transpose()?;
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
        None => match &previous {
            Some(p) if s.blobs.head(p).await?.is_some() => s.blobs.copy(p, &check).await?,
            _ => return Err(Verdict::Missing),
        },
    }
    if let Some(p) = &previous {
        s.blobs.delete(p).await?;
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
    inst: &str,
) -> ApiResult<Completed> {
    let settings = &s.cfg.direct_uploads;
    let claim = hex::encode(Uuid::new_v4().as_bytes())[..16].to_string();
    let (u, previous) = match s
        .store
        .claim_direct_upload(project.id, launch_id, id, settings.lease(), &claim)
        .await?
    {
        Claim::Verify { upload, previous } => (upload, previous),
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
            s.store.release_direct_upload(u.id, &claim).await?;
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "direct_verify")
                .increment(1);
            return Err(retry_after(
                ApiError::unavailable("too many direct uploads are being verified; retry shortly"),
                "2",
            ));
        }
    };
    let result = tokio::time::timeout(
        settings.verify_timeout,
        verify(s, project, &u, inst, &claim, previous.as_deref()),
    )
    .await;
    drop(permit);
    match result {
        Ok(Ok(attachment_id)) => {
            if !s
                .store
                .finish_direct_upload(u.id, &claim, attachment_id)
                .await?
            {
                tracing::warn!(upload = %u.id, "a direct upload was verified after another attempt took it over");
            }
            outcome("verified");
            Ok(Completed {
                attachment_id,
                sha256: u.sha256,
                size: u.size.max(0) as u64,
                new: true,
            })
        }
        Ok(Err(Verdict::Missing)) => {
            s.store.release_direct_upload(u.id, &claim).await?;
            outcome("missing");
            Err(ApiError::new(
                StatusCode::CONFLICT,
                "not_uploaded",
                "the object is not in the store yet: PUT it to the upload URL first",
            ))
        }
        Ok(Err(Verdict::Mismatch(reason))) => {
            s.store
                .fail_direct_upload(u.id, Some(&claim), &reason)
                .await?;
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
            s.store.release_direct_upload(u.id, &claim).await?;
            outcome("store_error");
            tracing::error!(upload = %u.id, error = %format!("{e:#}"), "verifying a direct upload failed");
            Err(retry_after(
                ApiError::unavailable("the blob store failed while verifying; retry shortly"),
                "5",
            ))
        }
        Err(_) => {
            s.store.release_direct_upload(u.id, &claim).await?;
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
    /// Completed or failed uploads whose leftover objects were deleted.
    pub finished: u64,
    /// Pending uploads whose object was larger than declared (deleted and failed).
    pub oversize: u64,
    pub pruned: u64,
}

/// Rows kept after an upload finished (quotas count completed bytes while they exist).
pub const KEEP_FINISHED: Duration = Duration::from_secs(30 * 86_400);

/// Objects under `direct/` this old are deleted by the orphan listing unless their upload can
/// still be completed.
pub const ORPHAN_AGE: Duration = Duration::from_secs(3600);

/// Pending uploads whose object size is checked per sweep.
pub const SIZE_CHECKS: i64 = 2000;

async fn delete_leftovers(s: &AppState, inst: &str, rows: &[Leftover]) -> anyhow::Result<()> {
    for r in rows {
        s.blobs.delete(&incoming(inst, r.id)?).await?;
        if let Some(c) = &r.claim {
            s.blobs.delete(&check(inst, r.id, c)?).await?;
        }
    }
    Ok(())
}

/// Works off the rows: expires uploads whose URL ran out and deletes their objects, deletes what
/// a PUT may have left after completion once the URL is dead, deletes and fails pending uploads
/// whose object is larger than declared, and prunes old rows.
pub async fn sweep(s: &AppState) -> anyhow::Result<SweepStats> {
    let mut st = SweepStats::default();
    let inst = installation(s).await?;
    let lease = s.cfg.direct_uploads.lease();
    loop {
        let rows = s.store.expire_direct_uploads(lease, 1000).await?;
        delete_leftovers(s, &inst, &rows).await?;
        st.expired += rows.len() as u64;
        if rows.len() < 1000 {
            break;
        }
    }
    loop {
        let rows = s.store.sweep_finished_direct_uploads(1000).await?;
        delete_leftovers(s, &inst, &rows).await?;
        st.finished += rows.len() as u64;
        if rows.len() < 1000 {
            break;
        }
    }
    for (id, declared) in s.store.direct_uploads_to_size_check(SIZE_CHECKS).await? {
        let key = incoming(&inst, id)?;
        if let Some(n) = s.blobs.head(&key).await?
            && n > declared.max(0) as u64
        {
            s.blobs.delete(&key).await?;
            if s.store
                .fail_direct_upload(
                    id,
                    None,
                    &format!("the object has {n} bytes, {declared} were declared"),
                )
                .await?
            {
                st.oversize += 1;
            }
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

/// Lists this installation's `direct/` prefix and deletes objects older than `age` whose upload
/// cannot be completed any more (rows pruned or purged with their project, interrupted
/// cleanups, checks of the store); returns how many. Runs rarely: the sweep works off rows.
pub async fn sweep_orphans(s: &AppState, age: Duration) -> anyhow::Result<u64> {
    let inst = installation(s).await?;
    let objects = s.blobs.list(&BlobKey::direct_prefix(&inst)?).await?;
    let old: Vec<(String, Option<Uuid>)> = objects
        .into_iter()
        .filter(|(_, at)| (chrono::Utc::now() - *at).to_std().unwrap_or_default() >= age)
        .map(|(k, _)| {
            let id = k.split('/').nth(2).and_then(|i| i.parse::<Uuid>().ok());
            (k, id)
        })
        .collect();
    let mut n = 0u64;
    for chunk in old.chunks(1000) {
        let ids: Vec<Uuid> = chunk.iter().filter_map(|(_, i)| *i).collect();
        let live = s.store.live_direct_uploads(&ids).await?;
        for (k, id) in chunk {
            if id.is_some_and(|i| live.contains(&i)) {
                continue;
            }
            let Ok(key) = BlobKey::parse(k) else { continue };
            s.blobs.delete(&key).await?;
            n += 1;
        }
    }
    Ok(n)
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
        let l = s.limits();
        assert_eq!(l.max_open_per_launch, UNCHECKED_OPEN_PER_LAUNCH as i64);
        assert_eq!(l.max_open_per_project, UNCHECKED_OPEN_PER_PROJECT as i64);
        assert_eq!(
            l.max_open_bytes_per_project,
            UNCHECKED_OPEN_BYTES_PER_PROJECT as i64
        );
        let mut c = s.clone();
        c.clamp(100 << 20, 512 << 20);
        assert_eq!(
            c.max_bytes,
            100 << 20,
            "never above FASTPROOF_MAX_UPLOAD_BYTES"
        );
        c.clamp(1 << 30, 50 << 20);
        assert_eq!(c.max_bytes, 50 << 20, "never above the per-entry limit");
        let d = DirectSettings::default().limits();
        assert_eq!(d.max_open_per_project, 10_000);
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
