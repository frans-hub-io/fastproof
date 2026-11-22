//! Server configuration from `FASTPROOF_*` environment variables. Every error names the
//! variable; secrets can come from files and never appear in `Debug` output.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use fastproof_blob::{BlobConfig, S3Config, Secret};
use fastproof_ingest::Limits;

use crate::db::DbSettings;
use franshub_http::TlsFiles;
use franshub_observe::LogFormat;
use franshub_oidc::OidcConfig;

/// When migrations run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateMode {
    /// At start-up, under an advisory lock.
    Auto,
    /// Expected to be done by `fastproof migrate` (a Helm Job); start-up checks them.
    Off,
}

/// How results reach PostgreSQL (`FASTPROOF_INGEST_MODE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestMode {
    /// HTTP, spool, PostgreSQL job queue, workers.
    Direct,
    /// The HTTP gateway produces to Kafka; writer consumers persist.
    Kafka,
}

/// Components that run in this process (`FASTPROOF_ROLES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles {
    pub api: bool,
    pub ingest: bool,
    pub writer: bool,
    pub sink: bool,
    pub scheduler: bool,
}

impl Roles {
    pub const ALL: Roles = Roles {
        api: true,
        ingest: true,
        writer: true,
        sink: true,
        scheduler: true,
    };

    /// `all` or a comma list of `api`, `ingest`, `writer`, `sink`, `scheduler`.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s == "all" {
            return Ok(Self::ALL);
        }
        let mut r = Roles {
            api: false,
            ingest: false,
            writer: false,
            sink: false,
            scheduler: false,
        };
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part {
                "api" => r.api = true,
                "ingest" => r.ingest = true,
                "writer" => r.writer = true,
                "sink" => r.sink = true,
                "scheduler" => r.scheduler = true,
                other => bail!(
                    "unknown role {other:?}: expected all or api, ingest, writer, sink, scheduler"
                ),
            }
        }
        if r == (Roles {
            api: false,
            ingest: false,
            writer: false,
            sink: false,
            scheduler: false,
        }) {
            bail!("no role given");
        }
        Ok(r)
    }
}

/// Where log search and similar failures go (`FASTPROOF_SEARCH_BACKEND`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchBackendKind {
    Postgres,
    ClickHouse,
    OpenSearch,
}

/// An analytics sink (`FASTPROOF_SINKS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkKind {
    ClickHouse,
    OpenSearch,
}

/// Limits of the live logs API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogLimits {
    /// Bytes of one message (`FASTPROOF_LOG_MAX_LINE_BYTES`).
    pub max_line_bytes: usize,
    /// Lines per request (`FASTPROOF_LOG_MAX_LINES_PER_REQUEST`).
    pub max_lines_per_request: usize,
    /// Body of one request (`FASTPROOF_LOG_MAX_REQUEST_BYTES`).
    pub max_request_bytes: u64,
    /// Log bytes accepted per launch (`FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH`).
    pub max_bytes_per_launch: i64,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 64 * 1024,
            max_lines_per_request: 5000,
            max_request_bytes: 8 * 1024 * 1024,
            max_bytes_per_launch: 256 * 1024 * 1024,
        }
    }
}

/// Everything `fastproof serve` needs.
#[derive(Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub ops_listen: SocketAddr,
    pub public_url: Option<url::Url>,
    pub db: DbSettings,
    pub migrate: MigrateMode,
    pub blob: BlobConfig,
    pub spool_dir: PathBuf,
    pub max_upload_bytes: u64,
    pub ingest_workers: usize,
    pub access_file: Option<PathBuf>,
    pub admin_tokens_file: Option<PathBuf>,
    pub oidc: Option<OidcConfig>,
    pub tls: Option<TlsFiles>,
    pub log_format: LogFormat,
    pub ui: bool,
    pub valkey_url: Option<Secret>,
    /// Keep raw uploads in blob storage after processing (`FASTPROOF_RETAIN_UPLOADS`).
    pub retain_uploads: bool,
    /// Ingest limits; `max_upload_bytes` follows `FASTPROOF_MAX_UPLOAD_BYTES`.
    pub limits: Limits,
    /// Days of outbox events kept (`FASTPROOF_OUTBOX_RETENTION_DAYS`).
    pub outbox_retention_days: i64,
    /// Gates, notifications, VCS, secrets, egress (`crate::integrations::config`).
    pub integrations: crate::integrations::config::IntegrationsConfig,
    pub ingest_mode: IngestMode,
    pub roles: Roles,
    pub search_backend: SearchBackendKind,
    pub sinks: Vec<SinkKind>,
    pub kafka: Option<fastproof_stream::KafkaConfig>,
    pub clickhouse: Option<fastproof_stream::ClickHouseConfig>,
    pub opensearch: Option<fastproof_stream::OpenSearchConfig>,
    pub sink_tuning: fastproof_stream::SinkTuning,
    pub log_limits: LogLimits,
    /// Per-caller and per-replica guards (`crate::limits::GuardSettings`).
    pub guards: crate::limits::GuardSettings,
    /// Limits of test management (`FASTPROOF_TM_*`).
    pub tm: fastproof_store::tm::TmLimits,
    /// Allow `http` base URLs for imports from Allure TestOps (`FASTPROOF_TESTOPS_ALLOW_HTTP`).
    pub testops_allow_http: bool,
    /// How long after the upgrade the case backfill waits before it decides per project
    /// (`FASTPROOF_TM_BACKFILL_DELAY`, default 10 minutes): projects with `caseSync` off by
    /// then are skipped.
    pub tm_backfill_delay: std::time::Duration,
}

/// Default of `FASTPROOF_TM_BACKFILL_DELAY`.
pub const DEFAULT_TM_BACKFILL_DELAY: std::time::Duration = std::time::Duration::from_secs(600);

/// A delay of `0`, `<n>`, `<n>s`, `<n>m`, `<n>h` or `<n>d` (seconds without a unit), at most 30 days.
fn delay(
    env: &Env<'_>,
    k: &str,
    default: std::time::Duration,
) -> anyhow::Result<std::time::Duration> {
    let Some(v) = env.var(k) else {
        return Ok(default);
    };
    let t = v.trim().to_ascii_lowercase();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow!("{k}: invalid value {t:?}: use 600, 30s, 10m, 1h or 1d"))?;
    let secs = match unit {
        "" | "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        _ => bail!("{k}: invalid value {t:?}: use 600, 30s, 10m, 1h or 1d"),
    };
    match secs {
        Some(s) if s <= 30 * 86_400 => Ok(std::time::Duration::from_secs(s)),
        _ => bail!("{k}: at most 30 days"),
    }
}

/// `FASTPROOF_TM_<NAME>` limits, each within `1..=max`.
fn tm_limits(env: &Env<'_>) -> anyhow::Result<fastproof_store::tm::TmLimits> {
    let d = fastproof_store::tm::TmLimits::default();
    let n = |name: &str, default: usize, max: usize| -> anyhow::Result<usize> {
        let k = format!("FASTPROOF_TM_{name}");
        let v: usize = env.parse(&k, default)?;
        if !(1..=max).contains(&v) {
            bail!("{k}: 1..={max}");
        }
        Ok(v)
    };
    let b = |name: &str, default: u64, max: u64| -> anyhow::Result<u64> {
        let k = format!("FASTPROOF_TM_{name}");
        let v: u64 = env.parse(&k, default)?;
        if !(1..=max).contains(&v) {
            bail!("{k}: 1..={max}");
        }
        Ok(v)
    };
    const GIB: u64 = 1024 * 1024 * 1024;
    Ok(fastproof_store::tm::TmLimits {
        max_steps: n("MAX_STEPS", d.max_steps, 10_000)?,
        max_step_depth: n("MAX_STEP_DEPTH", d.max_step_depth, 10)?,
        max_markdown_bytes: n("MAX_MARKDOWN_BYTES", d.max_markdown_bytes, 1024 * 1024)?,
        max_body_bytes: n("MAX_BODY_BYTES", d.max_body_bytes, 16 * 1024 * 1024)?,
        max_attachment_bytes: b("MAX_ATTACHMENT_BYTES", d.max_attachment_bytes, 4 * GIB)?,
        max_case_attachments: n("MAX_CASE_ATTACHMENTS", d.max_case_attachments, 10_000)?,
        max_step_attachments: n("MAX_STEP_ATTACHMENTS", d.max_step_attachments, 1000)?,
        max_fields: n("MAX_FIELDS", d.max_fields, 1000)?,
        max_field_options: n("MAX_FIELD_OPTIONS", d.max_field_options, 10_000)?,
        max_folders: n("MAX_FOLDERS", d.max_folders, 1_000_000)?,
        max_folder_depth: n("MAX_FOLDER_DEPTH", d.max_folder_depth, 100)?,
        max_cases: n("MAX_CASES", d.max_cases, 10_000_000)?,
        max_plan_cases: n("MAX_PLAN_CASES", d.max_plan_cases, 1_000_000)?,
        max_run_items: n("MAX_RUN_ITEMS", d.max_run_items, 1_000_000)?,
        max_bulk: n("MAX_BULK", d.max_bulk, 100_000)?,
        max_import_bytes: b("MAX_IMPORT_BYTES", d.max_import_bytes, 16 * GIB)?,
        max_import_zip_bytes: b("MAX_IMPORT_ZIP_BYTES", d.max_import_zip_bytes, 64 * GIB)?,
        max_import_rows: n("MAX_IMPORT_ROWS", d.max_import_rows, 10_000_000)?,
        max_import_cell_bytes: n(
            "MAX_IMPORT_CELL_BYTES",
            d.max_import_cell_bytes,
            16 * 1024 * 1024,
        )?,
        max_tags: n("MAX_TAGS", d.max_tags, 1000)?,
        max_name_bytes: n("MAX_NAME_BYTES", d.max_name_bytes, 4096)?,
    })
}

fn redact_url(u: &str) -> String {
    match url::Url::parse(u) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("redacted"));
            }
            let pairs: Vec<(String, String)> = parsed
                .query_pairs()
                .map(|(k, v)| {
                    let k = k.to_string();
                    if k.contains("password") || k.contains("secret") {
                        (k, "redacted".into())
                    } else {
                        (k, v.to_string())
                    }
                })
                .collect();
            if !pairs.is_empty() {
                parsed.query_pairs_mut().clear().extend_pairs(pairs);
            }
            parsed.to_string()
        }
        Err(_) => "<unparsable, redacted>".into(),
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("ops_listen", &self.ops_listen)
            .field("public_url", &self.public_url.as_ref().map(|u| u.as_str()))
            .field("database_url", &redact_url(self.db.url.expose()))
            .field("database_ca_file", &self.db.ca_file)
            .field("database_max_connections", &self.db.max_connections)
            .field("migrate", &self.migrate)
            .field("blob", &self.blob)
            .field("spool_dir", &self.spool_dir)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("ingest_workers", &self.ingest_workers)
            .field("access_file", &self.access_file)
            .field("admin_tokens_file", &self.admin_tokens_file)
            .field("oidc", &self.oidc)
            .field("tls", &self.tls.as_ref().map(|t| &t.cert_file))
            .field("log_format", &self.log_format)
            .field("ui", &self.ui)
            .field(
                "valkey_url",
                &self.valkey_url.as_ref().map(|v| redact_url(v.expose())),
            )
            .field("retain_uploads", &self.retain_uploads)
            .field("testops_allow_http", &self.testops_allow_http)
            .field("outbox_retention_days", &self.outbox_retention_days)
            .field("integrations", &self.integrations)
            .field("ingest_mode", &self.ingest_mode)
            .field("roles", &self.roles)
            .field("search_backend", &self.search_backend)
            .field("sinks", &self.sinks)
            .field("kafka", &self.kafka)
            .field("clickhouse", &self.clickhouse)
            .field("opensearch", &self.opensearch)
            .field("log_limits", &self.log_limits)
            .field("guards", &self.guards)
            .field("tm", &self.tm)
            .field("tm_backfill_delay", &self.tm_backfill_delay)
            .finish_non_exhaustive()
    }
}

/// Reads environment variables through a lookup function (tests pass a map).
pub struct Env<'a> {
    get: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> Env<'a> {
    pub fn new(get: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self { get }
    }

    /// A set, non-blank variable.
    pub fn var(&self, k: &str) -> Option<String> {
        (self.get)(k).filter(|v| !v.trim().is_empty())
    }

    fn parse<T: std::str::FromStr>(&self, k: &str, default: T) -> anyhow::Result<T>
    where
        T::Err: fmt::Display,
    {
        match self.var(k) {
            None => Ok(default),
            Some(v) => v
                .trim()
                .parse()
                .map_err(|e| anyhow!("{k}: invalid value {:?}: {e}", v.trim())),
        }
    }

    fn bool(&self, k: &str, default: bool) -> anyhow::Result<bool> {
        match self.var(k).map(|v| v.trim().to_ascii_lowercase()) {
            None => Ok(default),
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Ok(true),
            Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Ok(false),
            Some(v) => bail!("{k}: expected true or false, got {v:?}"),
        }
    }

    fn path(&self, k: &str) -> Option<PathBuf> {
        self.var(k).map(|v| PathBuf::from(v.trim()))
    }

    /// A secret from `K` or from the file named by `K_FILE` (not both).
    pub fn secret(&self, k: &str) -> anyhow::Result<Option<Secret>> {
        let file_key = format!("{k}_FILE");
        match (self.var(k), self.path(&file_key)) {
            (Some(_), Some(_)) => bail!("{k} and {file_key} are both set: use one"),
            (Some(v), None) => Ok(Some(Secret::new(v.trim().to_string()))),
            (None, Some(p)) => {
                let v = std::fs::read_to_string(&p)
                    .with_context(|| format!("{file_key}: reading {}", p.display()))?;
                let v = v.trim();
                if v.is_empty() {
                    bail!("{file_key}: {} is empty", p.display());
                }
                Ok(Some(Secret::new(v.to_string())))
            }
            (None, None) => Ok(None),
        }
    }
}

impl Config {
    /// From the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(&|k| std::env::var(k).ok())
    }

    /// From a lookup function.
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let env = Env::new(get);
        let listen = env.parse("FASTPROOF_LISTEN", SocketAddr::from(([0, 0, 0, 0], 8080)))?;
        let ops_listen = env.parse(
            "FASTPROOF_OPS_LISTEN",
            SocketAddr::from(([0, 0, 0, 0], 9090)),
        )?;
        let public_url = match env.var("FASTPROOF_PUBLIC_URL") {
            None => None,
            Some(v) => {
                let u = url::Url::parse(v.trim())
                    .map_err(|e| anyhow!("FASTPROOF_PUBLIC_URL: not a URL: {e}"))?;
                if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
                    bail!("FASTPROOF_PUBLIC_URL: expected an http(s) URL with a host");
                }
                Some(u)
            }
        };
        let db = DbSettings::from_lookup(get)?;
        let migrate = match env
            .var("FASTPROOF_MIGRATE")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("auto") => MigrateMode::Auto,
            Some("off") => MigrateMode::Off,
            Some(v) => bail!("FASTPROOF_MIGRATE: expected auto or off, got {v:?}"),
        };
        let blob = match env
            .var("FASTPROOF_BLOB_BACKEND")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("fs") => BlobConfig::Fs {
                dir: env
                    .path("FASTPROOF_BLOB_DIR")
                    .unwrap_or_else(|| PathBuf::from("/var/lib/fastproof/blobs")),
            },
            Some("s3") => {
                let bucket = env.var("FASTPROOF_S3_BUCKET").ok_or_else(|| {
                    anyhow!("FASTPROOF_S3_BUCKET is required with FASTPROOF_BLOB_BACKEND=s3")
                })?;
                let endpoint = env
                    .var("FASTPROOF_S3_ENDPOINT")
                    .map(|v| v.trim().to_string());
                if let Some(e) = &endpoint {
                    url::Url::parse(e)
                        .map_err(|err| anyhow!("FASTPROOF_S3_ENDPOINT: not a URL: {err}"))?;
                }
                let access_key = env.secret("FASTPROOF_S3_ACCESS_KEY")?;
                let secret_key = env.secret("FASTPROOF_S3_SECRET_KEY")?;
                if access_key.is_some() != secret_key.is_some() {
                    bail!(
                        "FASTPROOF_S3_ACCESS_KEY and FASTPROOF_S3_SECRET_KEY (or their _FILE variants) must be set together"
                    );
                }
                BlobConfig::S3(S3Config {
                    endpoint,
                    region: env
                        .var("FASTPROOF_S3_REGION")
                        .map(|v| v.trim().to_string())
                        .unwrap_or_else(|| "us-east-1".into()),
                    bucket: bucket.trim().to_string(),
                    path_style: env.bool("FASTPROOF_S3_PATH_STYLE", true)?,
                    ca_file: env.path("FASTPROOF_S3_CA_FILE"),
                    access_key,
                    secret_key,
                })
            }
            Some(v) => bail!("FASTPROOF_BLOB_BACKEND: expected fs or s3, got {v:?}"),
        };
        let max_upload_bytes: u64 = env.parse("FASTPROOF_MAX_UPLOAD_BYTES", 1_073_741_824)?;
        if max_upload_bytes < 1024 {
            bail!("FASTPROOF_MAX_UPLOAD_BYTES: at least 1024");
        }
        let ingest_workers: usize = env.parse("FASTPROOF_INGEST_WORKERS", 2)?;
        if ingest_workers > 64 {
            bail!("FASTPROOF_INGEST_WORKERS: at most 64");
        }
        let oidc = OidcConfig::from_env("FASTPROOF_", &|k| (env.get)(k))
            .context("FASTPROOF_OIDC_*")?
            .map(|mut c| {
                if c.redirect_url.is_empty() {
                    let base = public_url
                        .as_ref()
                        .ok_or_else(|| anyhow!("FASTPROOF_PUBLIC_URL is required with OIDC"))?;
                    c.redirect_url = base
                        .join("/auth/callback")
                        .map_err(|e| anyhow!("FASTPROOF_PUBLIC_URL: {e}"))?
                        .to_string();
                }
                Ok::<_, anyhow::Error>(c)
            })
            .transpose()?;
        if oidc.is_some() && public_url.is_none() {
            bail!("FASTPROOF_PUBLIC_URL is required with OIDC");
        }
        let tls = match (
            env.path("FASTPROOF_TLS_CERT_FILE"),
            env.path("FASTPROOF_TLS_KEY_FILE"),
        ) {
            (Some(cert_file), Some(key_file)) => {
                let client_ca_file = env.path("FASTPROOF_TLS_CLIENT_CA_FILE");
                Some(TlsFiles {
                    cert_file,
                    key_file,
                    client_auth_required: client_ca_file.is_some(),
                    client_ca_file,
                })
            }
            (None, None) => {
                if env.var("FASTPROOF_TLS_CLIENT_CA_FILE").is_some() {
                    bail!(
                        "FASTPROOF_TLS_CLIENT_CA_FILE needs FASTPROOF_TLS_CERT_FILE and FASTPROOF_TLS_KEY_FILE"
                    );
                }
                None
            }
            _ => bail!("FASTPROOF_TLS_CERT_FILE and FASTPROOF_TLS_KEY_FILE must be set together"),
        };
        let log_format = env
            .var("FASTPROOF_LOG_FORMAT")
            .map(|v| v.parse::<LogFormat>())
            .transpose()
            .map_err(|e| anyhow!("FASTPROOF_LOG_FORMAT: {e}"))?
            .unwrap_or(LogFormat::Json);
        let limits = Limits {
            max_upload_bytes,
            ..Limits::default()
        };
        let vars = fastproof_stream::Vars(get);
        let kafka = fastproof_stream::KafkaConfig::from_vars(&vars)?;
        let clickhouse = fastproof_stream::ClickHouseConfig::from_vars(&vars)?;
        let opensearch = fastproof_stream::OpenSearchConfig::from_vars(&vars)?;
        let ingest_mode = match env
            .var("FASTPROOF_INGEST_MODE")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("direct") => IngestMode::Direct,
            Some("kafka") => IngestMode::Kafka,
            Some(v) => bail!("FASTPROOF_INGEST_MODE: expected direct or kafka, got {v:?}"),
        };
        if ingest_mode == IngestMode::Kafka && kafka.is_none() {
            bail!("FASTPROOF_KAFKA_BROKERS is required with FASTPROOF_INGEST_MODE=kafka");
        }
        let roles = match env.var("FASTPROOF_ROLES") {
            None => Roles::ALL,
            Some(v) => Roles::parse(&v).map_err(|e| anyhow!("FASTPROOF_ROLES: {e}"))?,
        };
        let search_backend = match env
            .var("FASTPROOF_SEARCH_BACKEND")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("postgres") => SearchBackendKind::Postgres,
            Some("clickhouse") => SearchBackendKind::ClickHouse,
            Some("opensearch") => SearchBackendKind::OpenSearch,
            Some(v) => bail!(
                "FASTPROOF_SEARCH_BACKEND: expected postgres, clickhouse or opensearch, got {v:?}"
            ),
        };
        let mut sinks = Vec::new();
        for part in env
            .var("FASTPROOF_SINKS")
            .unwrap_or_default()
            .split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
        {
            let k = match part.as_str() {
                "clickhouse" => SinkKind::ClickHouse,
                "opensearch" => SinkKind::OpenSearch,
                other => bail!(
                    "FASTPROOF_SINKS: unknown sink {other:?}: expected clickhouse, opensearch"
                ),
            };
            if !sinks.contains(&k) {
                sinks.push(k);
            }
        }
        let needs_ch = search_backend == SearchBackendKind::ClickHouse
            || sinks.contains(&SinkKind::ClickHouse);
        let needs_os = search_backend == SearchBackendKind::OpenSearch
            || sinks.contains(&SinkKind::OpenSearch);
        if needs_ch && clickhouse.is_none() {
            bail!(
                "FASTPROOF_CLICKHOUSE_URL is required by FASTPROOF_SINKS or FASTPROOF_SEARCH_BACKEND"
            );
        }
        if needs_os && opensearch.is_none() {
            bail!(
                "FASTPROOF_OPENSEARCH_URL is required by FASTPROOF_SINKS or FASTPROOF_SEARCH_BACKEND"
            );
        }
        let defaults = LogLimits::default();
        let log_limits = LogLimits {
            max_line_bytes: env.parse("FASTPROOF_LOG_MAX_LINE_BYTES", defaults.max_line_bytes)?,
            max_lines_per_request: env.parse(
                "FASTPROOF_LOG_MAX_LINES_PER_REQUEST",
                defaults.max_lines_per_request,
            )?,
            max_request_bytes: env.parse(
                "FASTPROOF_LOG_MAX_REQUEST_BYTES",
                defaults.max_request_bytes,
            )?,
            max_bytes_per_launch: env.parse(
                "FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH",
                defaults.max_bytes_per_launch,
            )?,
        };
        if !(256..=1024 * 1024).contains(&log_limits.max_line_bytes) {
            bail!("FASTPROOF_LOG_MAX_LINE_BYTES: 256..=1048576");
        }
        if !(1..=100_000).contains(&log_limits.max_lines_per_request) {
            bail!("FASTPROOF_LOG_MAX_LINES_PER_REQUEST: 1..=100000");
        }
        if !(1024..=256 * 1024 * 1024).contains(&log_limits.max_request_bytes) {
            bail!("FASTPROOF_LOG_MAX_REQUEST_BYTES: 1024..=268435456");
        }
        if log_limits.max_bytes_per_launch < 1024 {
            bail!("FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH: at least 1024");
        }
        Ok(Self {
            listen,
            ops_listen,
            public_url,
            db,
            migrate,
            blob,
            spool_dir: env
                .path("FASTPROOF_SPOOL_DIR")
                .unwrap_or_else(|| PathBuf::from("/tmp/fastproof")),
            max_upload_bytes,
            ingest_workers,
            access_file: env.path("FASTPROOF_ACCESS_FILE"),
            admin_tokens_file: env.path("FASTPROOF_ADMIN_TOKENS_FILE"),
            oidc,
            tls,
            log_format,
            ui: env.bool("FASTPROOF_UI", true)?,
            valkey_url: env.secret("FASTPROOF_VALKEY_URL")?,
            retain_uploads: env.bool("FASTPROOF_RETAIN_UPLOADS", false)?,
            limits,
            outbox_retention_days: env.parse("FASTPROOF_OUTBOX_RETENTION_DAYS", 7)?,
            integrations: crate::integrations::config::IntegrationsConfig::from_env(&env)?,
            ingest_mode,
            roles,
            search_backend,
            sinks,
            kafka,
            clickhouse,
            opensearch,
            sink_tuning: fastproof_stream::SinkTuning::from_vars(&vars)?,
            log_limits,
            guards: crate::limits::GuardSettings::from_env(&env)?,
            tm: tm_limits(&env)?,
            testops_allow_http: env.bool("FASTPROOF_TESTOPS_ALLOW_HTTP", false)?,
            tm_backfill_delay: delay(
                &env,
                "FASTPROOF_TM_BACKFILL_DELAY",
                DEFAULT_TM_BACKFILL_DELAY,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn cfg(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(&|k| m.get(k).cloned())
    }

    #[test]
    fn defaults_and_required() {
        let e = cfg(&[]).unwrap_err().to_string();
        assert!(e.contains("FASTPROOF_DATABASE_URL"), "{e}");
        let c = cfg(&[(
            "FASTPROOF_DATABASE_URL",
            "postgres://u:pw@db.example.com/fp",
        )])
        .unwrap();
        assert_eq!(c.listen.port(), 8080);
        assert_eq!(c.ops_listen.port(), 9090);
        assert_eq!(c.ingest_workers, 2);
        assert_eq!(c.max_upload_bytes, 1_073_741_824);
        assert_eq!(c.migrate, MigrateMode::Auto);
        assert!(c.ui);
        assert!(matches!(c.blob, BlobConfig::Fs { .. }));
    }

    #[test]
    fn tm_limits_from_env() {
        let base = ("FASTPROOF_DATABASE_URL", "postgres://db.example.com/fp");
        let c = cfg(&[base]).unwrap();
        assert_eq!(c.tm, fastproof_store::tm::TmLimits::default());
        assert_eq!(c.tm.max_markdown_bytes, 16_384);
        assert_eq!(c.tm.max_import_zip_bytes, 2_147_483_648);
        let c = cfg(&[
            base,
            ("FASTPROOF_TM_MAX_FOLDERS", "500"),
            ("FASTPROOF_TM_MAX_IMPORT_BYTES", "1048576"),
        ])
        .unwrap();
        assert_eq!(c.tm.max_folders, 500);
        assert_eq!(c.tm.max_import_bytes, 1_048_576);
        for (k, v) in [
            ("FASTPROOF_TM_MAX_FOLDERS", "0"),
            ("FASTPROOF_TM_MAX_STEP_DEPTH", "11"),
            ("FASTPROOF_TM_MAX_TAGS", "many"),
            ("FASTPROOF_TM_MAX_IMPORT_ZIP_BYTES", "-1"),
        ] {
            let e = cfg(&[base, (k, v)]).unwrap_err().to_string();
            assert!(e.contains(k), "{e}");
        }
    }

    #[test]
    fn tm_backfill_delay_from_env() {
        let base = ("FASTPROOF_DATABASE_URL", "postgres://db.example.com/fp");
        let k = "FASTPROOF_TM_BACKFILL_DELAY";
        assert_eq!(
            cfg(&[base]).unwrap().tm_backfill_delay,
            std::time::Duration::from_secs(600)
        );
        for (v, secs) in [
            ("0", 0),
            ("90", 90),
            ("45s", 45),
            ("10m", 600),
            ("2h", 7200),
            ("1d", 86_400),
        ] {
            assert_eq!(
                cfg(&[base, (k, v)]).unwrap().tm_backfill_delay,
                std::time::Duration::from_secs(secs),
                "{v}"
            );
        }
        for v in ["-1", "10x", "m", "31d", "99999999999999999999"] {
            let e = cfg(&[base, (k, v)]).unwrap_err().to_string();
            assert!(e.contains(k), "{v}: {e}");
        }
    }

    #[test]
    fn streaming_settings() {
        let base = ("FASTPROOF_DATABASE_URL", "postgres://db.example.com/fp");
        let c = cfg(&[base]).unwrap();
        assert_eq!(c.ingest_mode, IngestMode::Direct);
        assert_eq!(c.roles, Roles::ALL);
        assert_eq!(c.search_backend, SearchBackendKind::Postgres);
        assert!(c.sinks.is_empty() && c.kafka.is_none());
        let c = cfg(&[
            base,
            ("FASTPROOF_INGEST_MODE", "kafka"),
            ("FASTPROOF_KAFKA_BROKERS", "kafka.example.com:9092"),
            ("FASTPROOF_ROLES", "api, writer"),
            ("FASTPROOF_SINKS", "clickhouse"),
            ("FASTPROOF_CLICKHOUSE_URL", "http://ch.example.com:8123"),
        ])
        .unwrap();
        assert_eq!(c.ingest_mode, IngestMode::Kafka);
        assert!(c.roles.api && c.roles.writer && !c.roles.sink && !c.roles.ingest);
        assert_eq!(c.sinks, vec![SinkKind::ClickHouse]);
        for (k, v) in [
            ("FASTPROOF_INGEST_MODE", "kafka"),
            ("FASTPROOF_ROLES", "api,cook"),
            ("FASTPROOF_SEARCH_BACKEND", "opensearch"),
            ("FASTPROOF_SINKS", "elastic"),
            ("FASTPROOF_LOG_MAX_LINE_BYTES", "1"),
        ] {
            let e = format!("{:#}", cfg(&[base, (k, v)]).unwrap_err());
            assert!(e.contains("FASTPROOF_"), "{k}: {e}");
        }
    }

    #[test]
    fn errors_name_the_variable() {
        let base = ("FASTPROOF_DATABASE_URL", "postgres://db.example.com/fp");
        for (k, v) in [
            ("FASTPROOF_LISTEN", "nope"),
            ("FASTPROOF_MIGRATE", "sometimes"),
            ("FASTPROOF_BLOB_BACKEND", "tape"),
            ("FASTPROOF_INGEST_WORKERS", "-1"),
            ("FASTPROOF_UI", "maybe"),
            ("FASTPROOF_LOG_FORMAT", "xml"),
            ("FASTPROOF_PUBLIC_URL", "ftp://x"),
        ] {
            let e = format!("{:#}", cfg(&[base, (k, v)]).unwrap_err());
            assert!(e.contains(k), "{k}: {e}");
        }
        let e = format!(
            "{:#}",
            cfg(&[base, ("FASTPROOF_BLOB_BACKEND", "s3")]).unwrap_err()
        );
        assert!(e.contains("FASTPROOF_S3_BUCKET"), "{e}");
        let e = format!(
            "{:#}",
            cfg(&[base, ("FASTPROOF_OIDC_ISSUER", "https://idp.example.com")]).unwrap_err()
        );
        assert!(e.contains("FASTPROOF_PUBLIC_URL"), "{e}");
        let e = format!(
            "{:#}",
            cfg(&[
                ("FASTPROOF_DATABASE_URL", "x"),
                ("FASTPROOF_DATABASE_URL_FILE", "/x")
            ])
            .unwrap_err()
        );
        assert!(e.contains("both set"), "{e}");
    }

    #[test]
    fn secrets_from_files_and_redacted_debug() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        std::fs::write(
            &db,
            "postgres://fastproof:db-pass-123@db.example.com/fp?sslmode=verify-full\n",
        )
        .unwrap();
        let ak = dir.path().join("ak");
        std::fs::write(&ak, "AKIDSECRETACCESS").unwrap();
        let sk = dir.path().join("sk");
        std::fs::write(&sk, "very-secret-key-456\n").unwrap();
        let c = cfg(&[
            ("FASTPROOF_DATABASE_URL_FILE", db.to_str().unwrap()),
            ("FASTPROOF_BLOB_BACKEND", "s3"),
            ("FASTPROOF_S3_BUCKET", "reports"),
            ("FASTPROOF_S3_ENDPOINT", "https://s3.example.com"),
            ("FASTPROOF_S3_ACCESS_KEY_FILE", ak.to_str().unwrap()),
            ("FASTPROOF_S3_SECRET_KEY_FILE", sk.to_str().unwrap()),
            (
                "FASTPROOF_VALKEY_URL",
                "redis://:valkey-pass-789@cache.example.com",
            ),
            ("FASTPROOF_PUBLIC_URL", "https://fastproof.example.com"),
            ("FASTPROOF_OIDC_ISSUER", "https://idp.example.com/realms/qa"),
            ("FASTPROOF_OIDC_CLIENT_ID", "fastproof"),
            ("FASTPROOF_OIDC_CLIENT_SECRET", "oidc-secret-000"),
        ])
        .unwrap();
        assert_eq!(
            c.db.url.expose(),
            "postgres://fastproof:db-pass-123@db.example.com/fp?sslmode=verify-full"
        );
        assert_eq!(
            c.oidc.as_ref().unwrap().redirect_url,
            "https://fastproof.example.com/auth/callback"
        );
        let s = format!("{c:?}");
        for secret in [
            "db-pass-123",
            "AKIDSECRETACCESS",
            "very-secret-key-456",
            "valkey-pass-789",
            "oidc-secret-000",
        ] {
            assert!(!s.contains(secret), "{secret} leaked: {s}");
        }
        assert!(s.contains("db.example.com"));
        let e = format!(
            "{:#}",
            cfg(&[
                ("FASTPROOF_DATABASE_URL", "postgres://db.example.com/fp"),
                ("FASTPROOF_S3_SECRET_KEY_FILE", "/nonexistent/fastproof-sk"),
                ("FASTPROOF_BLOB_BACKEND", "s3"),
                ("FASTPROOF_S3_BUCKET", "b"),
            ])
            .unwrap_err()
        );
        assert!(e.contains("FASTPROOF_S3_SECRET_KEY_FILE"), "{e}");
    }
}
