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
            .field("outbox_retention_days", &self.outbox_retention_days)
            .field("integrations", &self.integrations)
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
