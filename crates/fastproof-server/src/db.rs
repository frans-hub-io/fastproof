//! Database settings and the connection pool. Only `FASTPROOF_DATABASE_*` and the libpq
//! `PGSSLMODE`/`PGSSLROOTCERT` variables are read, so `fastproof migrate` needs nothing else.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use fastproof_blob::Secret;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

use crate::config::Env;

/// How to reach PostgreSQL.
#[derive(Clone)]
pub struct DbSettings {
    pub url: Secret,
    pub ca_file: Option<PathBuf>,
    pub max_connections: u32,
}

impl std::fmt::Debug for DbSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbSettings")
            .field("ca_file", &self.ca_file)
            .field("max_connections", &self.max_connections)
            .finish_non_exhaustive()
    }
}

impl DbSettings {
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let env = Env::new(get);
        let url = env.secret("FASTPROOF_DATABASE_URL")?.ok_or_else(|| {
            anyhow!("FASTPROOF_DATABASE_URL (or FASTPROOF_DATABASE_URL_FILE) is required")
        })?;
        if !url.expose().starts_with("postgres://") && !url.expose().starts_with("postgresql://") {
            bail!("FASTPROOF_DATABASE_URL: expected a postgres:// URL");
        }
        let max_connections = match env.var("FASTPROOF_DATABASE_MAX_CONNECTIONS") {
            None => 20,
            Some(v) => v
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n >= 2)
                .ok_or_else(|| {
                    anyhow!("FASTPROOF_DATABASE_MAX_CONNECTIONS: a number of at least 2")
                })?,
        };
        Ok(Self {
            url,
            ca_file: env
                .var("FASTPROOF_DATABASE_CA_FILE")
                .map(|v| PathBuf::from(v.trim())),
            max_connections,
        })
    }

    /// Connect options: the URL, then `PGSSLMODE`/`PGSSLROOTCERT` when the URL sets no
    /// `sslmode`, then `FASTPROOF_DATABASE_CA_FILE`, which also makes the default `verify-full`.
    pub fn connect_options(
        &self,
        get: &dyn Fn(&str) -> Option<String>,
    ) -> anyhow::Result<PgConnectOptions> {
        let url = self.url.expose();
        let mut opts = PgConnectOptions::from_str(url)
            .map_err(|_| anyhow!("FASTPROOF_DATABASE_URL: not a valid PostgreSQL URL"))?;
        let parsed =
            url::Url::parse(url).map_err(|_| anyhow!("FASTPROOF_DATABASE_URL: not a valid URL"))?;
        let url_mode = parsed
            .query_pairs()
            .any(|(k, _)| k == "sslmode" || k == "ssl-mode");
        if !url_mode {
            if let Some(m) = get("PGSSLMODE").filter(|v| !v.trim().is_empty()) {
                let mode = PgSslMode::from_str(m.trim())
                    .map_err(|_| anyhow!("PGSSLMODE: unknown mode {:?}", m.trim()))?;
                opts = opts.ssl_mode(mode);
            } else if self.ca_file.is_some() {
                opts = opts.ssl_mode(PgSslMode::VerifyFull);
            }
            if let Some(c) = get("PGSSLROOTCERT").filter(|v| !v.trim().is_empty()) {
                opts = opts.ssl_root_cert(c.trim());
            }
        }
        if let Some(ca) = &self.ca_file {
            if !ca.is_file() {
                bail!("FASTPROOF_DATABASE_CA_FILE: {} is not a file", ca.display());
            }
            opts = opts.ssl_root_cert(ca);
        }
        Ok(opts)
    }

    /// Connects a pool and logs the effective TLS mode.
    pub async fn connect(
        &self,
        get: &(dyn Fn(&str) -> Option<String> + Sync),
    ) -> anyhow::Result<PgPool> {
        let opts = self.connect_options(get)?;
        let mode = ssl_mode_name(opts.get_ssl_mode());
        tracing::info!(
            sslmode = mode,
            host = opts.get_host(),
            database = opts.get_database().unwrap_or_default(),
            "connecting to PostgreSQL"
        );
        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(opts)
            .await
            .with_context(|| format!("connecting to PostgreSQL (sslmode={mode})"))
    }
}

/// The libpq name of a mode.
pub fn ssl_mode_name(m: PgSslMode) -> &'static str {
    match m {
        PgSslMode::Disable => "disable",
        PgSslMode::Allow => "allow",
        PgSslMode::Prefer => "prefer",
        PgSslMode::Require => "require",
        PgSslMode::VerifyCa => "verify-ca",
        PgSslMode::VerifyFull => "verify-full",
    }
}

/// Runs the platform and FastProof migrations (each under its advisory lock).
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    franshub_store_pg::migrate(pool)
        .await
        .context("platform migrations")?;
    fastproof_store::migrate(pool)
        .await
        .context("FastProof migrations")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: &str, ca: Option<&str>) -> DbSettings {
        DbSettings {
            url: Secret::new(url),
            ca_file: ca.map(PathBuf::from),
            max_connections: 5,
        }
    }

    #[test]
    fn ssl_modes() {
        let none = |_: &str| None;
        let o = settings("postgres://u@db.example.com/fp", None)
            .connect_options(&none)
            .unwrap();
        assert_eq!(ssl_mode_name(o.get_ssl_mode()), "prefer");
        let env = |k: &str| (k == "PGSSLMODE").then(|| "require".to_string());
        let o = settings("postgres://u@db.example.com/fp", None)
            .connect_options(&env)
            .unwrap();
        assert_eq!(ssl_mode_name(o.get_ssl_mode()), "require");
        let o = settings("postgres://u@db.example.com/fp?sslmode=disable", None)
            .connect_options(&env)
            .unwrap();
        assert_eq!(ssl_mode_name(o.get_ssl_mode()), "disable");
        let ca = tempfile::NamedTempFile::new().unwrap();
        let o = settings("postgres://u@db.example.com/fp", ca.path().to_str())
            .connect_options(&none)
            .unwrap();
        assert_eq!(ssl_mode_name(o.get_ssl_mode()), "verify-full");
        assert!(
            settings(
                "postgres://u@db.example.com/fp",
                Some("/nonexistent/ca.pem")
            )
            .connect_options(&none)
            .is_err()
        );
        let e = DbSettings::from_lookup(&|_| None).unwrap_err().to_string();
        assert!(e.contains("FASTPROOF_DATABASE_URL"));
    }
}
