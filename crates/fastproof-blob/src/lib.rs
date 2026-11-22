//! Object storage of FastProof: attachments and raw uploads in a filesystem directory or an
//! S3-compatible bucket, behind one [`BlobStore`].
//!
//! Keys are built only through [`BlobKey`] constructors that validate every component, so a key
//! can never escape its prefix: `projects/<key>/attachments/<sha256[0..2]>/<sha256>` and
//! `uploads/<uuid>`.

use std::fmt;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{StreamExt, TryStreamExt};
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::buffered::BufWriter;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjPath;
use object_store::signer::{Method, Signer};
use object_store::{
    ClientOptions, GetOptions, GetRange, ObjectStore, ObjectStoreExt, PutPayload, RetryConfig,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncWriteExt, BufReader};

/// Installs ring as the process-wide rustls provider (the build has no other provider); safe
/// to call more than once.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Objects up to this size are written with one request; larger ones with a multipart upload.
pub const SINGLE_PUT_MAX: usize = 8 * 1024 * 1024;

/// A secret value: never printed by `Debug` or `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(v: impl Into<String>) -> Self {
        Self(v.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"<redacted>\"")
    }
}

/// S3-compatible storage settings.
#[derive(Clone, Debug)]
pub struct S3Config {
    /// Endpoint URL such as `https://s3.example.com`; `None` for AWS.
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    /// Path-style requests (`https://host/bucket/key`), needed by most S3-compatible servers.
    pub path_style: bool,
    /// PEM bundle of extra trusted CAs for the endpoint.
    pub ca_file: Option<PathBuf>,
    pub access_key: Option<Secret>,
    pub secret_key: Option<Secret>,
}

/// Where blobs live.
#[derive(Clone, Debug)]
pub enum BlobConfig {
    Fs { dir: PathBuf },
    S3(S3Config),
}

/// A validated object key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlobKey(String);

/// `^[a-z0-9][a-z0-9-]{1,62}$`
pub fn valid_project_key(key: &str) -> bool {
    let b = key.as_bytes();
    (2..=63).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Lowercase hex of 64 characters.
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn valid_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

impl BlobKey {
    /// `projects/<key>/attachments/<aa>/<sha256>`.
    pub fn attachment(project_key: &str, sha256: &str) -> anyhow::Result<Self> {
        if !valid_project_key(project_key) {
            bail!("invalid project key in a blob key");
        }
        if !valid_sha256(sha256) {
            bail!("invalid sha256 in a blob key");
        }
        Ok(Self(format!(
            "projects/{project_key}/attachments/{}/{sha256}",
            &sha256[..2]
        )))
    }

    /// `uploads/<id>` (an id of lowercase letters, digits and dashes, such as a UUID).
    pub fn upload(id: &str) -> anyhow::Result<Self> {
        if !valid_id(id) {
            bail!("invalid upload id in a blob key");
        }
        Ok(Self(format!("uploads/{id}")))
    }

    /// `projects/<key>/`: everything of one project.
    pub fn project_prefix(project_key: &str) -> anyhow::Result<Self> {
        if !valid_project_key(project_key) {
            bail!("invalid project key in a blob key");
        }
        Ok(Self(format!("projects/{project_key}")))
    }

    /// `uploads/`.
    pub fn uploads_prefix() -> Self {
        Self("uploads".into())
    }

    /// `tm-jobs/<id>/<name>`: a file of a test-management import or export job (`name` of
    /// lowercase letters, digits, dots and dashes).
    pub fn tm_job(id: &str, name: &str) -> anyhow::Result<Self> {
        if !valid_id(id) {
            bail!("invalid job id in a blob key");
        }
        let ok = (1..=64).contains(&name.len())
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.');
        if !ok {
            bail!("invalid job file name in a blob key");
        }
        Ok(Self(format!("tm-jobs/{id}/{name}")))
    }

    /// Parses a stored key back, accepting only the shapes the constructors produce.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = s.split('/').collect();
        match parts.as_slice() {
            ["uploads", id] => Self::upload(id),
            ["tm-jobs", id, name] => Self::tm_job(id, name),
            ["projects", p, "attachments", aa, sha] if sha.len() == 64 && &sha[..2] == *aa => {
                Self::attachment(p, sha)
            }
            _ => bail!("unexpected blob key shape"),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn path(&self) -> ObjPath {
        ObjPath::from(self.0.as_str())
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A blob body being streamed.
pub struct BlobBody {
    pub size: u64,
    pub stream: BoxStream<'static, anyhow::Result<Bytes>>,
}

/// A byte range of a blob being streamed.
pub struct BlobRange {
    /// The size of the whole blob.
    pub total: u64,
    /// The bytes the stream yields: `range.end - range.start` of them.
    pub range: Range<u64>,
    pub stream: BoxStream<'static, anyhow::Result<Bytes>>,
}

/// The configured store.
#[derive(Clone)]
pub struct BlobStore {
    inner: Arc<dyn ObjectStore>,
    s3: Option<Arc<AmazonS3>>,
    streamed: Arc<AtomicU64>,
    backend: &'static str,
    description: String,
}

impl fmt::Debug for BlobStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlobStore")
            .field("backend", &self.backend)
            .field("location", &self.description)
            .finish()
    }
}

fn is_not_found(e: &object_store::Error) -> bool {
    matches!(e, object_store::Error::NotFound { .. })
}

impl BlobStore {
    /// Opens the store (creating the directory of the `fs` backend).
    pub fn open(cfg: &BlobConfig) -> anyhow::Result<Self> {
        match cfg {
            BlobConfig::Fs { dir } => {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating the blob directory {}", dir.display()))?;
                let fs = LocalFileSystem::new_with_prefix(dir)
                    .with_context(|| format!("opening the blob directory {}", dir.display()))?
                    .with_automatic_cleanup(true);
                Ok(Self {
                    inner: Arc::new(fs),
                    s3: None,
                    streamed: Arc::default(),
                    backend: "fs",
                    description: dir.display().to_string(),
                })
            }
            BlobConfig::S3(c) => {
                install_crypto_provider();
                let mut opts = ClientOptions::new()
                    .with_connect_timeout(Duration::from_secs(10))
                    .with_timeout(Duration::from_secs(300));
                if let Some(ep) = &c.endpoint {
                    if ep.starts_with("http://") {
                        opts = opts.with_allow_http(true);
                    } else if !ep.starts_with("https://") {
                        bail!("the S3 endpoint must start with https:// or http://");
                    }
                }
                if let Some(ca) = &c.ca_file {
                    let pem = std::fs::read(ca)
                        .with_context(|| format!("reading the S3 CA file {}", ca.display()))?;
                    let certs = object_store::Certificate::from_pem_bundle(&pem)
                        .with_context(|| format!("parsing the S3 CA file {}", ca.display()))?;
                    if certs.is_empty() {
                        bail!("the S3 CA file {} has no certificate", ca.display());
                    }
                    for cert in certs {
                        opts = opts.with_root_certificate(cert);
                    }
                }
                let mut b = AmazonS3Builder::new()
                    .with_region(&c.region)
                    .with_bucket_name(&c.bucket)
                    .with_virtual_hosted_style_request(!c.path_style)
                    .with_client_options(opts)
                    .with_retry(RetryConfig {
                        max_retries: 5,
                        retry_timeout: Duration::from_secs(60),
                        ..RetryConfig::default()
                    });
                if let Some(ep) = &c.endpoint {
                    b = b.with_endpoint(ep);
                }
                if let Some(k) = &c.access_key {
                    b = b.with_access_key_id(k.expose());
                }
                if let Some(k) = &c.secret_key {
                    b = b.with_secret_access_key(k.expose());
                }
                let s3 = Arc::new(b.build().context("configuring the S3 client")?);
                Ok(Self {
                    inner: s3.clone(),
                    s3: Some(s3),
                    streamed: Arc::default(),
                    backend: "s3",
                    description: format!("{}/{}", c.endpoint.as_deref().unwrap_or("aws"), c.bucket),
                })
            }
        }
    }

    /// `fs` or `s3`.
    pub fn backend(&self) -> &'static str {
        self.backend
    }

    /// Uploads a local file, streaming it (multipart above [`SINGLE_PUT_MAX`]).
    pub async fn put_file(&self, key: &BlobKey, file: &Path) -> anyhow::Result<u64> {
        let f = tokio::fs::File::open(file)
            .await
            .with_context(|| format!("opening {}", file.display()))?;
        let size = f.metadata().await?.len();
        if size as usize <= SINGLE_PUT_MAX {
            let bytes = tokio::fs::read(file).await?;
            self.inner
                .put(&key.path(), PutPayload::from(bytes))
                .await
                .with_context(|| format!("storing {key}"))?;
            return Ok(size);
        }
        let mut w = BufWriter::with_capacity(self.inner.clone(), key.path(), SINGLE_PUT_MAX);
        let mut r = BufReader::with_capacity(256 * 1024, f);
        match tokio::io::copy_buf(&mut r, &mut w).await {
            Ok(_) => {
                w.shutdown()
                    .await
                    .with_context(|| format!("storing {key}"))?;
                Ok(size)
            }
            Err(e) => {
                let _ = w.abort().await;
                Err(anyhow::Error::from(e).context(format!("storing {key}")))
            }
        }
    }

    /// Stores bytes.
    pub async fn put_bytes(&self, key: &BlobKey, bytes: Bytes) -> anyhow::Result<()> {
        self.inner
            .put(&key.path(), PutPayload::from(bytes))
            .await
            .with_context(|| format!("storing {key}"))?;
        Ok(())
    }

    /// Streams a blob, `None` when it does not exist.
    pub async fn get(&self, key: &BlobKey) -> anyhow::Result<Option<BlobBody>> {
        match self.inner.get(&key.path()).await {
            Ok(r) => {
                let size = r.meta.size;
                let stream = self.counted(r.into_stream());
                Ok(Some(BlobBody { size, stream }))
            }
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e).context(format!("reading {key}"))),
        }
    }

    /// Streams the bytes `range` of a blob (a ranged read of the store, never a read of what
    /// precedes it); `None` when the blob does not exist. The range must lie inside the blob.
    pub async fn get_range(
        &self,
        key: &BlobKey,
        range: Range<u64>,
    ) -> anyhow::Result<Option<BlobRange>> {
        if range.start >= range.end {
            bail!("empty range of {key}");
        }
        let opts = GetOptions {
            range: Some(GetRange::Bounded(range.clone())),
            ..GetOptions::default()
        };
        match self.inner.get_opts(&key.path(), opts).await {
            Ok(r) => {
                if r.range != range {
                    bail!(
                        "the store answered bytes {}..{} of {key} for {}..{}",
                        r.range.start,
                        r.range.end,
                        range.start,
                        range.end
                    );
                }
                Ok(Some(BlobRange {
                    total: r.meta.size,
                    range,
                    stream: self.counted(r.into_stream()),
                }))
            }
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e).context(format!("reading {key}"))),
        }
    }

    fn counted(
        &self,
        s: impl futures::Stream<Item = object_store::Result<Bytes>> + Send + 'static,
    ) -> BoxStream<'static, anyhow::Result<Bytes>> {
        let n = self.streamed.clone();
        s.map_err(anyhow::Error::from)
            .inspect_ok(move |b| {
                n.fetch_add(b.len() as u64, Ordering::Relaxed);
            })
            .boxed()
    }

    /// Bytes this store handle yielded to readers since it was opened (all clones share it):
    /// tests use it to show that a ranged read does not read the prefix.
    pub fn bytes_streamed(&self) -> u64 {
        self.streamed.load(Ordering::Relaxed)
    }

    /// Downloads a blob into a local file; `None` when it does not exist.
    pub async fn get_to_file(&self, key: &BlobKey, file: &Path) -> anyhow::Result<Option<u64>> {
        let Some(mut body) = self.get(key).await? else {
            return Ok(None);
        };
        let mut f = tokio::fs::File::create(file)
            .await
            .with_context(|| format!("creating {}", file.display()))?;
        let mut n = 0u64;
        while let Some(chunk) = body.stream.next().await {
            let chunk = chunk?;
            n += chunk.len() as u64;
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok(Some(n))
    }

    /// The size of a blob, `None` when it does not exist.
    pub async fn head(&self, key: &BlobKey) -> anyhow::Result<Option<u64>> {
        match self.inner.head(&key.path()).await {
            Ok(m) => Ok(Some(m.size)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::Error::from(e).context(format!("inspecting {key}"))),
        }
    }

    /// Deletes a blob; a missing one is not an error.
    pub async fn delete(&self, key: &BlobKey) -> anyhow::Result<()> {
        match self.inner.delete(&key.path()).await {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(anyhow::Error::from(e).context(format!("deleting {key}"))),
        }
    }

    /// Deletes every blob under a prefix key; returns how many.
    pub async fn delete_prefix(&self, prefix: &BlobKey) -> anyhow::Result<u64> {
        let p = prefix.path();
        let locations = self.inner.list(Some(&p)).map_ok(|m| m.location).boxed();
        let mut n = 0u64;
        let mut deleted = self.inner.delete_stream(locations);
        while let Some(r) = deleted.next().await {
            match r {
                Ok(_) => n += 1,
                Err(e) if is_not_found(&e) => {}
                Err(e) => {
                    return Err(anyhow::Error::from(e).context(format!("deleting {prefix}/*")));
                }
            }
        }
        Ok(n)
    }

    /// Keys under a prefix with their last-modified time (for cleaning up stale uploads).
    pub async fn list(
        &self,
        prefix: &BlobKey,
    ) -> anyhow::Result<Vec<(String, chrono::DateTime<chrono::Utc>)>> {
        let mut out = Vec::new();
        let mut s = self.inner.list(Some(&prefix.path()));
        while let Some(m) = s.next().await {
            let m = m?;
            out.push((m.location.to_string(), m.last_modified));
        }
        Ok(out)
    }

    /// A pre-signed GET URL (S3 only; `None` for the filesystem, which the server proxies).
    pub async fn presign_get(
        &self,
        key: &BlobKey,
        ttl: Duration,
    ) -> anyhow::Result<Option<String>> {
        self.presign(Method::GET, key, ttl).await
    }

    /// A pre-signed PUT URL (S3 only).
    pub async fn presign_put(
        &self,
        key: &BlobKey,
        ttl: Duration,
    ) -> anyhow::Result<Option<String>> {
        self.presign(Method::PUT, key, ttl).await
    }

    async fn presign(
        &self,
        method: Method,
        key: &BlobKey,
        ttl: Duration,
    ) -> anyhow::Result<Option<String>> {
        let Some(s3) = &self.s3 else { return Ok(None) };
        let url = s3
            .signed_url(method, &key.path(), ttl)
            .await
            .context("signing a URL")?;
        Ok(Some(url.to_string()))
    }

    /// Checks that the store answers (a HEAD of a key that need not exist).
    pub async fn check(&self) -> anyhow::Result<()> {
        let probe = ObjPath::from("uploads/readiness-probe");
        match self.inner.head(&probe).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(anyhow::Error::from(e).context("the blob store is not reachable")),
        }
    }
}

/// A file written in the spool directory with its SHA-256 and size.
#[derive(Debug)]
pub struct Spooled {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}

impl Spooled {
    /// Removes the spool file.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Why spooling stopped.
#[derive(Debug)]
pub enum SpoolError {
    /// More than the allowed bytes arrived.
    TooLarge { max: u64 },
    /// The body stream failed.
    Body(anyhow::Error),
    /// Writing the spool file failed.
    Io(std::io::Error),
}

impl fmt::Display for SpoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpoolError::TooLarge { max } => write!(f, "the body exceeds {max} bytes"),
            SpoolError::Body(e) => write!(f, "reading the body failed: {e:#}"),
            SpoolError::Io(e) => write!(f, "writing the spool file failed: {e}"),
        }
    }
}

impl std::error::Error for SpoolError {}

/// A writer that hashes and counts what passes through it.
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    size: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            size: 0,
        }
    }

    /// The hex SHA-256, the byte count and the inner writer.
    pub fn finish(self) -> (String, u64, W) {
        (hex::encode(self.hasher.finalize()), self.size, self.inner)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.size += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Writes a byte stream to `path`, hashing it, and stops with [`SpoolError::TooLarge`] after
/// `max` bytes (the partial file is removed).
pub async fn spool_stream<S, E>(path: &Path, mut body: S, max: u64) -> Result<Spooled, SpoolError>
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<anyhow::Error>,
{
    let result = async {
        let mut f = tokio::fs::File::create(path)
            .await
            .map_err(SpoolError::Io)?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| SpoolError::Body(e.into()))?;
            size += chunk.len() as u64;
            if size > max {
                return Err(SpoolError::TooLarge { max });
            }
            hasher.update(&chunk);
            f.write_all(&chunk).await.map_err(SpoolError::Io)?;
        }
        f.flush().await.map_err(SpoolError::Io)?;
        f.sync_data().await.map_err(SpoolError::Io)?;
        Ok(Spooled {
            path: path.to_path_buf(),
            sha256: hex::encode(hasher.finalize()),
            size,
        })
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

/// SHA-256 of a file, read in chunks.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let mut w = HashingWriter::new(std::io::sink());
    std::io::copy(&mut f, &mut w)?;
    let (h, n, _) = w.finish();
    Ok((h, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_validated() {
        let sha = "ab".repeat(32);
        assert_eq!(
            BlobKey::attachment("team-a", &sha).unwrap().as_str(),
            format!("projects/team-a/attachments/ab/{sha}")
        );
        assert!(BlobKey::attachment("../x", &sha).is_err());
        assert!(BlobKey::attachment("team-a", "../../etc/passwd").is_err());
        assert!(BlobKey::attachment("Team", &sha).is_err());
        assert!(BlobKey::attachment("team-a", &sha.to_uppercase()).is_err());
        assert!(BlobKey::upload("0193a1b2-0000-7000-8000-000000000000").is_ok());
        assert!(BlobKey::upload("../x").is_err());
        assert!(BlobKey::upload("a/b").is_err());
        assert!(BlobKey::upload("").is_err());
        let k = BlobKey::attachment("team-a", &sha).unwrap();
        assert_eq!(BlobKey::parse(k.as_str()).unwrap(), k);
        assert!(BlobKey::parse("projects/team-a/attachments/cd/".to_string().as_str()).is_err());
        assert!(BlobKey::parse(&format!("projects/team-a/attachments/cd/{sha}")).is_err());
        assert!(BlobKey::parse("etc/passwd").is_err());
    }

    #[test]
    fn secrets_are_redacted() {
        let c = S3Config {
            endpoint: Some("https://s3.example.com".into()),
            region: "us-east-1".into(),
            bucket: "b".into(),
            path_style: true,
            ca_file: None,
            access_key: Some(Secret::new("AKIDEXAMPLE")),
            secret_key: Some(Secret::new("wJalrXUtnFEMI")),
        };
        let s = format!("{:?}", BlobConfig::S3(c));
        assert!(
            !s.contains("AKIDEXAMPLE") && !s.contains("wJalrXUtnFEMI"),
            "{s}"
        );
        assert!(s.contains("redacted"));
    }

    #[test]
    fn hashing_writer() {
        let mut w = HashingWriter::new(Vec::new());
        w.write_all(b"abc").unwrap();
        let (h, n, v) = w.finish();
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(n, 3);
        assert_eq!(v, b"abc");
    }

    #[tokio::test]
    async fn fs_round_trip_and_spool_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(&BlobConfig::Fs {
            dir: dir.path().join("blobs"),
        })
        .unwrap();
        store.check().await.unwrap();
        let spool = dir.path().join("spool");
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"hello ")),
            Ok(Bytes::from_static(b"world")),
        ];
        let s = spool_stream(&spool, futures::stream::iter(chunks), 100)
            .await
            .unwrap();
        assert_eq!(s.size, 11);
        let key = BlobKey::attachment("team-a", &s.sha256).unwrap();
        store.put_file(&key, &s.path).await.unwrap();
        assert_eq!(store.head(&key).await.unwrap(), Some(11));
        let out = dir.path().join("out");
        assert_eq!(store.get_to_file(&key, &out).await.unwrap(), Some(11));
        assert_eq!(std::fs::read(&out).unwrap(), b"hello world");
        assert!(
            store
                .presign_get(&key, Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .delete_prefix(&BlobKey::project_prefix("team-a").unwrap())
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.head(&key).await.unwrap(), None);
        store.delete(&key).await.unwrap();

        let big: Vec<Result<Bytes, std::io::Error>> =
            (0..4).map(|_| Ok(Bytes::from(vec![0u8; 64]))).collect();
        let err = spool_stream(&spool, futures::stream::iter(big), 100)
            .await
            .unwrap_err();
        assert!(matches!(err, SpoolError::TooLarge { max: 100 }));
        assert!(!spool.exists());
    }

    #[tokio::test]
    async fn fs_range_reads_only_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(&BlobConfig::Fs {
            dir: dir.path().join("blobs"),
        })
        .unwrap();
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let sha = "ab".repeat(32);
        let key = BlobKey::attachment("team-a", &sha).unwrap();
        store
            .put_bytes(&key, Bytes::from(data.clone()))
            .await
            .unwrap();
        let r = store
            .get_range(&key, 99_000..99_500)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((r.total, r.range.clone()), (100_000, 99_000..99_500));
        let got: Vec<Bytes> = r.stream.try_collect().await.unwrap();
        assert_eq!(got.concat(), &data[99_000..99_500]);
        assert_eq!(store.bytes_streamed(), 500);
        assert!(store.get_range(&key, 0..0).await.is_err());
        assert!(
            store
                .get_range(
                    &BlobKey::attachment("team-a", &"cd".repeat(32)).unwrap(),
                    0..1
                )
                .await
                .unwrap()
                .is_none()
        );
        let all = store.get(&key).await.unwrap().unwrap();
        let _: Vec<Bytes> = all.stream.try_collect().await.unwrap();
        assert_eq!(store.bytes_streamed(), 100_500);
    }

    #[tokio::test]
    async fn fs_large_file_uses_multipart() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::open(&BlobConfig::Fs {
            dir: dir.path().join("blobs"),
        })
        .unwrap();
        let file = dir.path().join("big");
        let data: Vec<u8> = (0..(SINGLE_PUT_MAX + 12345))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&file, &data).unwrap();
        let (sha, n) = sha256_file(&file).unwrap();
        assert_eq!(n as usize, data.len());
        let key = BlobKey::attachment("team-a", &sha).unwrap();
        store.put_file(&key, &file).await.unwrap();
        let out = dir.path().join("out");
        store.get_to_file(&key, &out).await.unwrap();
        assert_eq!(sha256_file(&out).unwrap().0, sha);
    }
}
