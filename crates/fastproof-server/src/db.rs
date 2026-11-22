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
    /// Longest statement on pooled connections (`FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS`,
    /// `None` when 0). Migrations and index builds run on connections without it.
    pub statement_timeout: Option<Duration>,
    /// Connections heavy reads leave free for ingest and workers
    /// (`FASTPROOF_DATABASE_INGEST_RESERVE`).
    pub ingest_reserve: u32,
    /// How long start-up waits for PostgreSQL (`FASTPROOF_DATABASE_WAIT`, formerly
    /// `FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS`).
    pub connect_timeout: Duration,
}

impl std::fmt::Debug for DbSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbSettings")
            .field("ca_file", &self.ca_file)
            .field("max_connections", &self.max_connections)
            .field("statement_timeout", &self.statement_timeout)
            .field("ingest_reserve", &self.ingest_reserve)
            .field("connect_timeout", &self.connect_timeout)
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
        let statement_timeout = match env.var("FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS") {
            None => Some(DEFAULT_STATEMENT_TIMEOUT),
            Some(v) => match v.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) if n <= 86_400 => Some(Duration::from_secs(n)),
                _ => bail!("FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS: 0 (off) or 1..=86400"),
            },
        };
        let ingest_reserve = match env.var("FASTPROOF_DATABASE_INGEST_RESERVE") {
            None => (max_connections / 4).max(1),
            Some(v) => v
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|n| *n < max_connections)
                .ok_or_else(|| {
                    anyhow!(
                        "FASTPROOF_DATABASE_INGEST_RESERVE: a number below FASTPROOF_DATABASE_MAX_CONNECTIONS"
                    )
                })?,
        };
        let connect_timeout = match (
            env.var("FASTPROOF_DATABASE_WAIT"),
            env.var("FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS"),
        ) {
            (Some(v), _) => parse_wait(&v).filter(|d| *d <= MAX_WAIT).ok_or_else(|| {
                anyhow!("FASTPROOF_DATABASE_WAIT: a duration up to 24h such as 300, 90s, 5m or 1h")
            })?,
            (None, Some(v)) => match v.trim().parse::<u64>() {
                Ok(n) if n <= 86_400 => Duration::from_secs(n),
                _ => bail!("FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS: 0..=86400"),
            },
            (None, None) => DEFAULT_CONNECT_TIMEOUT,
        };
        Ok(Self {
            connect_timeout,
            url,
            ca_file: env
                .var("FASTPROOF_DATABASE_CA_FILE")
                .map(|v| PathBuf::from(v.trim())),
            max_connections,
            statement_timeout,
            ingest_reserve,
        })
    }

    /// Heavy reads that may run at once: the pool minus the ingest reserve.
    pub fn heavy_slots(&self) -> usize {
        self.max_connections
            .saturating_sub(self.ingest_reserve)
            .max(1) as usize
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
        if let Some(t) = self.statement_timeout {
            opts = opts.options([("statement_timeout", format!("{}ms", t.as_millis()))]);
        }
        Ok(opts)
    }

    /// Connects a pool, retrying for up to [`Self::connect_timeout`] while PostgreSQL is not
    /// reachable yet (it may still be starting next to FastProof). Wrong credentials, a missing
    /// database or TLS errors are retried too: they are often fixed by whatever is still
    /// starting (a secret, an operator creating the database).
    pub async fn connect_waiting(
        &self,
        get: &(dyn Fn(&str) -> Option<String> + Sync),
    ) -> anyhow::Result<PgPool> {
        let opts = self.connect_options(get)?;
        wait_for(
            "PostgreSQL",
            "FASTPROOF_DATABASE_WAIT",
            self.connect_timeout,
            || async {
                // One plain connection first: it fails at once while the server is down,
                // where the pool would keep retrying until its acquire timeout.
                use sqlx::Connection;
                sqlx::PgConnection::connect_with(&opts)
                    .await?
                    .close()
                    .await?;
                self.connect(get).await
            },
        )
        .await
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

/// Default of `FASTPROOF_DATABASE_WAIT`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);

/// Largest `FASTPROOF_DATABASE_WAIT`.
pub const MAX_WAIT: Duration = Duration::from_secs(86_400);

/// Longest pause between two attempts of [`wait_for`].
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// The pause after `previous`: doubled, at most [`MAX_BACKOFF`].
pub fn next_backoff(previous: Duration) -> Duration {
    (previous * 2).min(MAX_BACKOFF)
}

/// A duration in seconds (`300`), or with a unit: `90s`, `5m`, `1h`.
pub fn parse_wait(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (n, unit) = match v.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => v.split_at(i),
        None => (v, "s"),
    };
    let n: u64 = n.parse().ok()?;
    let secs = match unit.trim() {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// A short, stable name of why a connection attempt failed, for logs and alerts:
/// `connection_refused`, `dns`, `timeout`, `tls`, `authentication`, `database_missing`,
/// `too_many_connections`, `starting_up`, `database` (another server error), `io` or `other`.
pub fn error_class(e: &anyhow::Error) -> &'static str {
    use std::io::ErrorKind;
    let io_class = |io: &std::io::Error| match io.kind() {
        ErrorKind::ConnectionRefused => "connection_refused",
        ErrorKind::TimedOut => "timeout",
        _ if io.to_string().contains("lookup") => "dns",
        _ => "io",
    };
    for cause in e.chain() {
        if let Some(se) = cause.downcast_ref::<sqlx::Error>() {
            return match se {
                sqlx::Error::Io(io) => io_class(io),
                sqlx::Error::Tls(_) => "tls",
                sqlx::Error::PoolTimedOut => "timeout",
                sqlx::Error::Database(d) => match d.code().as_deref() {
                    Some("28P01" | "28000") => "authentication",
                    Some("3D000") => "database_missing",
                    Some("53300") => "too_many_connections",
                    Some("57P03") => "starting_up",
                    _ => "database",
                },
                _ => "other",
            };
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_class(io);
        }
    }
    if e.to_string() == "timed out" {
        return "timeout";
    }
    "other"
}

/// Runs `attempt` until it succeeds, for up to `timeout` (at least once), with a backoff from
/// one to thirty seconds; every failure is logged as one line with its [`error_class`].
/// Start-up uses it for the services FastProof needs (`what`), so a pod that starts before them
/// waits instead of crashing (`/healthz` answers, `/readyz` says `starting`); `var` names the
/// setting of the timeout in the final error.
pub async fn wait_for<T, F, Fut>(
    what: &str,
    var: &str,
    timeout: Duration,
    mut attempt: F,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let started = std::time::Instant::now();
    let deadline = started + timeout;
    let mut backoff = Duration::from_secs(1);
    let mut n = 0u32;
    loop {
        n += 1;
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let cap = left.clamp(Duration::from_secs(1), Duration::from_secs(30));
        let r = match tokio::time::timeout(cap, attempt()).await {
            Ok(r) => r,
            Err(_) => Err(anyhow!("timed out")),
        };
        let e = match r {
            Ok(v) => {
                if n > 1 {
                    tracing::info!(
                        service = what,
                        attempts = n,
                        waited_seconds = started.elapsed().as_secs(),
                        "{what} is reachable"
                    );
                }
                return Ok(v);
            }
            Err(e) => e,
        };
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            bail!(
                "{what} was not reachable within {} s ({n} attempt(s); set {var} to wait longer): {e:#}",
                started.elapsed().as_secs()
            );
        }
        tracing::warn!(
            service = what,
            attempt = n,
            error_class = error_class(&e),
            retry_in_seconds = backoff.min(left).as_secs_f64(),
            gives_up_in_seconds = left.as_secs(),
            error = %format!("{e:#}"),
            "{what} is not reachable yet; waiting"
        );
        tokio::time::sleep(backoff.min(left)).await;
        backoff = next_backoff(backoff);
    }
}

/// Default of `FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS`.
pub const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(120);

/// A one-connection pool like `pool` but without a statement timeout, for migrations, index
/// builds and other maintenance that may legitimately run long. Close it when done.
pub async fn unbounded(pool: &PgPool) -> anyhow::Result<PgPool> {
    let opts = (*pool.connect_options())
        .clone()
        .options([("statement_timeout", "0")]);
    Ok(PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(opts)
        .await?)
}

/// Runs the platform and FastProof migrations (each under its advisory lock), on a connection
/// without a statement timeout.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let m = unbounded(pool).await.context("connecting for migrations")?;
    let r = async {
        franshub_store_pg::migrate(&m)
            .await
            .context("platform migrations")?;
        fastproof_store::migrate(&m)
            .await
            .context("FastProof migrations")?;
        Ok(())
    }
    .await;
    m.close().await;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: &str, ca: Option<&str>) -> DbSettings {
        DbSettings {
            url: Secret::new(url),
            ca_file: ca.map(PathBuf::from),
            max_connections: 5,
            statement_timeout: None,
            ingest_reserve: 1,
            connect_timeout: Duration::ZERO,
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

    #[test]
    fn timeouts_and_reserve() {
        let url = |k: &str| {
            (k == "FASTPROOF_DATABASE_URL").then(|| "postgres://u@db.example.com/fp".to_string())
        };
        let d = DbSettings::from_lookup(&url).unwrap();
        assert_eq!(d.statement_timeout, Some(DEFAULT_STATEMENT_TIMEOUT));
        assert_eq!(
            (d.max_connections, d.ingest_reserve, d.heavy_slots()),
            (20, 5, 15)
        );
        let o = d.connect_options(&|_| None).unwrap();
        assert_eq!(o.get_options(), Some("-c statement_timeout=120000ms"));
        let vars = |k: &str| match k {
            "FASTPROOF_DATABASE_URL" => Some("postgres://u@db.example.com/fp".to_string()),
            "FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS" => Some("0".into()),
            "FASTPROOF_DATABASE_MAX_CONNECTIONS" => Some("4".into()),
            "FASTPROOF_DATABASE_INGEST_RESERVE" => Some("4".into()),
            _ => None,
        };
        let e = DbSettings::from_lookup(&vars).unwrap_err().to_string();
        assert!(e.contains("FASTPROOF_DATABASE_INGEST_RESERVE"), "{e}");
        assert_eq!(d.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        let vars = |k: &str| match k {
            "FASTPROOF_DATABASE_URL" => Some("postgres://u@db.example.com/fp".to_string()),
            "FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS" => Some("90000".into()),
            _ => None,
        };
        let e = DbSettings::from_lookup(&vars).unwrap_err().to_string();
        assert!(
            e.contains("FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn waiting_retries_until_success_or_the_deadline() {
        let mut calls = 0;
        let v = wait_for("svc", "VAR", Duration::from_secs(10), || {
            calls += 1;
            let ok = calls >= 2;
            async move { if ok { Ok(7) } else { Err(anyhow!("refused")) } }
        })
        .await
        .unwrap();
        assert_eq!((v, calls), (7, 2));
        let started = std::time::Instant::now();
        let e = wait_for::<(), _, _>("svc", "VAR", Duration::from_millis(1500), || async {
            Err(anyhow!("refused"))
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("svc was not reachable")
                && e.contains("3 attempt(s)")
                && e.contains("VAR")
                && e.contains("refused"),
            "{e}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let e = wait_for::<(), _, _>("svc", "VAR", Duration::ZERO, || async {
            Err(anyhow!("refused"))
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("(1 attempt(s);"), "{e}");
    }

    #[test]
    fn backoff_from_one_to_thirty_seconds() {
        let mut b = Duration::from_secs(1);
        let mut seq = vec![b.as_secs()];
        for _ in 0..7 {
            b = next_backoff(b);
            seq.push(b.as_secs());
        }
        assert_eq!(seq, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn wait_settings() {
        assert_eq!(parse_wait("300"), Some(Duration::from_secs(300)));
        assert_eq!(parse_wait(" 90s "), Some(Duration::from_secs(90)));
        assert_eq!(parse_wait("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_wait("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_wait("0"), Some(Duration::ZERO));
        for bad in ["", "m", "5d", "-1", "1.5m", "99999999999999999999h"] {
            assert_eq!(parse_wait(bad), None, "{bad}");
        }
        let vars = |wait: Option<&str>, old: Option<&str>| {
            let (wait, old) = (wait.map(str::to_string), old.map(str::to_string));
            move |k: &str| match k {
                "FASTPROOF_DATABASE_URL" => Some("postgres://u@db.example.com/fp".to_string()),
                "FASTPROOF_DATABASE_WAIT" => wait.clone(),
                "FASTPROOF_DATABASE_CONNECT_TIMEOUT_SECONDS" => old.clone(),
                _ => None,
            }
        };
        let wait = |w, o| DbSettings::from_lookup(&vars(w, o)).map(|d| d.connect_timeout);
        assert_eq!(wait(None, None).unwrap(), Duration::from_secs(300));
        assert_eq!(wait(Some("2m"), None).unwrap(), Duration::from_secs(120));
        assert_eq!(wait(None, Some("45")).unwrap(), Duration::from_secs(45));
        assert_eq!(
            wait(Some("10s"), Some("45")).unwrap(),
            Duration::from_secs(10),
            "FASTPROOF_DATABASE_WAIT wins"
        );
        let e = wait(Some("25h"), None).unwrap_err().to_string();
        assert!(e.contains("FASTPROOF_DATABASE_WAIT"), "{e}");
    }

    #[tokio::test]
    async fn error_classes() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let opts: PgConnectOptions = format!("postgres://u:pw@127.0.0.1:{port}/fp")
            .parse()
            .unwrap();
        use sqlx::Connection;
        let e = anyhow::Error::from(sqlx::PgConnection::connect_with(&opts).await.unwrap_err())
            .context("connecting");
        assert_eq!(error_class(&e), "connection_refused", "{e:#}");
        let opts: PgConnectOptions = "postgres://u:pw@db.invalid/fp".parse().unwrap();
        let e = anyhow::Error::from(sqlx::PgConnection::connect_with(&opts).await.unwrap_err());
        assert_eq!(error_class(&e), "dns", "{e:#}");
        assert_eq!(error_class(&anyhow!("timed out")), "timeout");
        assert_eq!(error_class(&anyhow!("something else")), "other");
        if let Ok(url) = std::env::var("FASTPROOF_TEST_DATABASE_URL") {
            let opts: PgConnectOptions = url.parse().unwrap();
            let missing = opts.database("fastproof_no_such_database");
            let e = anyhow::Error::from(
                sqlx::PgConnection::connect_with(&missing)
                    .await
                    .unwrap_err(),
            );
            assert_eq!(error_class(&e), "database_missing", "{e:#}");
        }
    }
}
