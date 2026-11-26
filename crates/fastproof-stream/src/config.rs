//! Settings of the streaming mode from `FASTPROOF_*` variables (`31-streaming.md`). Secrets
//! come from files and never appear in `Debug` output.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};

/// A secret string; `Debug` prints `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Lookup of environment variables (tests pass a map).
pub struct Vars<'a>(pub &'a dyn Fn(&str) -> Option<String>);

impl Vars<'_> {
    pub fn var(&self, k: &str) -> Option<String> {
        (self.0)(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    pub fn parse<T: std::str::FromStr>(&self, k: &str, default: T) -> anyhow::Result<T>
    where
        T::Err: fmt::Display,
    {
        match self.var(k) {
            None => Ok(default),
            Some(v) => v
                .parse()
                .map_err(|e| anyhow!("{k}: invalid value {v:?}: {e}")),
        }
    }

    pub fn bool(&self, k: &str, default: bool) -> anyhow::Result<bool> {
        match self.var(k).map(|v| v.to_ascii_lowercase()) {
            None => Ok(default),
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Ok(true),
            Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Ok(false),
            Some(v) => bail!("{k}: expected true or false, got {v:?}"),
        }
    }

    pub fn path(&self, k: &str) -> Option<PathBuf> {
        self.var(k).map(PathBuf::from)
    }

    /// A secret read from the file named by `k`.
    pub fn secret_file(&self, k: &str) -> anyhow::Result<Option<Secret>> {
        match self.path(k) {
            None => Ok(None),
            Some(p) => {
                let v = std::fs::read_to_string(&p)
                    .with_context(|| format!("{k}: reading {}", p.display()))?;
                let v = v.trim();
                if v.is_empty() {
                    bail!("{k}: {} is empty", p.display());
                }
                Ok(Some(Secret::new(v)))
            }
        }
    }

    fn http_url(&self, k: &str) -> anyhow::Result<Option<url::Url>> {
        match self.var(k) {
            None => Ok(None),
            Some(v) => {
                let u = url::Url::parse(&v).map_err(|e| anyhow!("{k}: not a URL: {e}"))?;
                if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
                    bail!("{k}: expected an http(s) URL with a host");
                }
                if !u.username().is_empty() || u.password().is_some() {
                    bail!(
                        "{k}: put credentials in the _USER and _PASSWORD_FILE variables, not in the URL"
                    );
                }
                Ok(Some(u))
            }
        }
    }
}

/// SASL mechanisms rskafka supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaslMechanism {
    Plain,
    ScramSha256,
    ScramSha512,
}

impl std::str::FromStr for SaslMechanism {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "plain" => Self::Plain,
            "scram-sha-256" => Self::ScramSha256,
            "scram-sha-512" => Self::ScramSha512,
            _ => bail!("expected plain, scram-sha-256 or scram-sha-512"),
        })
    }
}

/// SASL credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sasl {
    pub mechanism: SaslMechanism,
    pub username: String,
    pub password: Secret,
}

/// Kafka connection and topic settings.
#[derive(Clone, Debug)]
pub struct KafkaConfig {
    pub brokers: Vec<String>,
    pub tls: bool,
    pub ca_file: Option<PathBuf>,
    pub sasl: Option<Sasl>,
    pub topic_prefix: String,
    pub create_topics: bool,
    pub partitions: i32,
    pub replication: i16,
    /// Accept records that the gateway did not sign (in-cluster producers behind Kafka ACLs).
    pub trusted_producers: bool,
    pub client_id: String,
    /// Largest record value produced (uncompressed).
    pub max_record_bytes: usize,
    /// Signed records older than this are rejected (`FASTPROOF_KAFKA_SIGNATURE_MAX_AGE_HOURS`).
    pub signature_max_age: std::time::Duration,
    /// Decoded bytes one partition fetch aims at (`FASTPROOF_KAFKA_FETCH_MAX_BYTES`).
    pub fetch_max_bytes: usize,
    /// Decoded record bytes a process holds across its partitions
    /// (`FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES`).
    pub max_inflight_bytes: usize,
    /// Records handled and committed together, at most (`FASTPROOF_KAFKA_BATCH_MAX_RECORDS`).
    pub batch_max_records: usize,
    /// Record bytes handled and committed together, at most (`FASTPROOF_KAFKA_BATCH_MAX_BYTES`).
    pub batch_max_bytes: usize,
    /// How long start-up waits for Kafka (`FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS`).
    pub connect_timeout: Duration,
}

/// Default of `FASTPROOF_KAFKA_FETCH_MAX_BYTES` (raised to the maximum record size).
pub const DEFAULT_FETCH_MAX_BYTES: usize = 1024 * 1024;
/// Default of `FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES` (raised to the fetch size).
pub const DEFAULT_MAX_INFLIGHT_BYTES: usize = 64 * 1024 * 1024;
/// Largest fetch: the response size the client accepts.
pub const MAX_FETCH_BYTES: usize = 64 * 1024 * 1024;

impl KafkaConfig {
    /// From `FASTPROOF_KAFKA_*`; `None` when no brokers are set.
    pub fn from_vars(v: &Vars<'_>) -> anyhow::Result<Option<Self>> {
        let Some(brokers) = v.var("FASTPROOF_KAFKA_BROKERS") else {
            return Ok(None);
        };
        let brokers: Vec<String> = brokers
            .split(',')
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .collect();
        if brokers.is_empty() {
            bail!("FASTPROOF_KAFKA_BROKERS: expected host:port[,host:port]");
        }
        for b in &brokers {
            if b.contains("://") || !b.contains(':') {
                bail!("FASTPROOF_KAFKA_BROKERS: {b:?} is not host:port");
            }
        }
        let tls = v.bool("FASTPROOF_KAFKA_TLS", false)?;
        let ca_file = v.path("FASTPROOF_KAFKA_CA_FILE");
        if ca_file.is_some() && !tls {
            bail!("FASTPROOF_KAFKA_CA_FILE needs FASTPROOF_KAFKA_TLS=true");
        }
        let sasl = match v.var("FASTPROOF_KAFKA_SASL_MECHANISM") {
            None => {
                if v.var("FASTPROOF_KAFKA_USERNAME").is_some() {
                    bail!("FASTPROOF_KAFKA_USERNAME needs FASTPROOF_KAFKA_SASL_MECHANISM");
                }
                None
            }
            Some(m) => {
                let mechanism = m
                    .parse::<SaslMechanism>()
                    .map_err(|e| anyhow!("FASTPROOF_KAFKA_SASL_MECHANISM: {e}"))?;
                let username = v.var("FASTPROOF_KAFKA_USERNAME").ok_or_else(|| {
                    anyhow!(
                        "FASTPROOF_KAFKA_USERNAME is required with FASTPROOF_KAFKA_SASL_MECHANISM"
                    )
                })?;
                let password = v.secret_file("FASTPROOF_KAFKA_PASSWORD_FILE")?.ok_or_else(|| {
                    anyhow!("FASTPROOF_KAFKA_PASSWORD_FILE is required with FASTPROOF_KAFKA_SASL_MECHANISM")
                })?;
                Some(Sasl {
                    mechanism,
                    username,
                    password,
                })
            }
        };
        let topic_prefix = v
            .var("FASTPROOF_KAFKA_TOPIC_PREFIX")
            .unwrap_or_else(|| "fastproof.".into());
        if !topic_prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            || topic_prefix.len() > 200
        {
            bail!("FASTPROOF_KAFKA_TOPIC_PREFIX: letters, digits, '.', '_' and '-' only");
        }
        let partitions: i32 = v.parse("FASTPROOF_KAFKA_PARTITIONS", 12)?;
        if !(1..=1000).contains(&partitions) {
            bail!("FASTPROOF_KAFKA_PARTITIONS: 1..=1000");
        }
        let replication: i16 = v.parse("FASTPROOF_KAFKA_REPLICATION", -1)?;
        let max_record_bytes: usize = v.parse("FASTPROOF_KAFKA_MAX_RECORD_BYTES", 900_000)?;
        if !(16 * 1024..=64 * 1024 * 1024).contains(&max_record_bytes) {
            bail!("FASTPROOF_KAFKA_MAX_RECORD_BYTES: 16384..=67108864");
        }
        let max_age_hours: u64 = v.parse("FASTPROOF_KAFKA_SIGNATURE_MAX_AGE_HOURS", 168)?;
        if !(1..=24 * 365).contains(&max_age_hours) {
            bail!("FASTPROOF_KAFKA_SIGNATURE_MAX_AGE_HOURS: 1..=8760");
        }
        let fetch_max_bytes: usize = v.parse(
            "FASTPROOF_KAFKA_FETCH_MAX_BYTES",
            DEFAULT_FETCH_MAX_BYTES.max(max_record_bytes),
        )?;
        if fetch_max_bytes < max_record_bytes || fetch_max_bytes > MAX_FETCH_BYTES {
            bail!(
                "FASTPROOF_KAFKA_FETCH_MAX_BYTES: from FASTPROOF_KAFKA_MAX_RECORD_BYTES ({max_record_bytes}) to {MAX_FETCH_BYTES}"
            );
        }
        let max_inflight_bytes: usize = v.parse(
            "FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES",
            DEFAULT_MAX_INFLIGHT_BYTES.max(fetch_max_bytes),
        )?;
        if max_inflight_bytes < fetch_max_bytes || max_inflight_bytes > 64 << 30 {
            bail!(
                "FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES: from FASTPROOF_KAFKA_FETCH_MAX_BYTES ({fetch_max_bytes}) to 68719476736"
            );
        }
        let batch_max_records: usize = v.parse("FASTPROOF_KAFKA_BATCH_MAX_RECORDS", 1000)?;
        if !(1..=100_000).contains(&batch_max_records) {
            bail!("FASTPROOF_KAFKA_BATCH_MAX_RECORDS: 1..=100000");
        }
        let batch_max_bytes: usize = v.parse("FASTPROOF_KAFKA_BATCH_MAX_BYTES", 1024 * 1024)?;
        if !(16 * 1024..=MAX_FETCH_BYTES).contains(&batch_max_bytes) {
            bail!("FASTPROOF_KAFKA_BATCH_MAX_BYTES: 16384..=67108864");
        }
        let connect_timeout: u64 = v.parse("FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS", 300)?;
        if connect_timeout > 86_400 {
            bail!("FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS: 0..=86400");
        }
        Ok(Some(Self {
            fetch_max_bytes,
            max_inflight_bytes,
            batch_max_records,
            batch_max_bytes,
            connect_timeout: Duration::from_secs(connect_timeout),
            signature_max_age: std::time::Duration::from_secs(max_age_hours * 3600),
            brokers,
            tls,
            ca_file,
            sasl,
            topic_prefix,
            create_topics: v.bool("FASTPROOF_KAFKA_CREATE_TOPICS", false)?,
            partitions,
            replication,
            trusted_producers: v.bool("FASTPROOF_KAFKA_TRUSTED_PRODUCERS", false)?,
            client_id: v
                .var("FASTPROOF_KAFKA_CLIENT_ID")
                .unwrap_or_else(|| "fastproof".into()),
            max_record_bytes,
        }))
    }
}

/// An HTTP(S) endpoint with optional basic credentials and a private CA.
#[derive(Clone, Debug)]
pub struct HttpEndpoint {
    pub url: url::Url,
    pub user: Option<String>,
    pub password: Option<Secret>,
    pub ca_file: Option<PathBuf>,
}

/// ClickHouse sink and search settings.
#[derive(Clone, Debug)]
pub struct ClickHouseConfig {
    pub endpoint: HttpEndpoint,
    pub database: String,
    pub raw_ttl_days: u32,
    /// Days the Analysis aggregates and the triage history are kept
    /// (`FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS`, 30..=3650, default 730).
    pub agg_ttl_days: u32,
}

/// Default of `FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS`.
pub const DEFAULT_AGG_TTL_DAYS: u32 = 730;

/// OpenSearch sink and search settings.
#[derive(Clone, Debug)]
pub struct OpenSearchConfig {
    pub endpoint: HttpEndpoint,
    pub index_prefix: String,
    pub raw_ttl_days: u32,
    /// Replicas of the indexes FastProof creates.
    pub replicas: u32,
}

fn endpoint(v: &Vars<'_>, prefix: &str) -> anyhow::Result<Option<HttpEndpoint>> {
    let Some(url) = v.http_url(&format!("{prefix}_URL"))? else {
        return Ok(None);
    };
    let ca_file = v.path(&format!("{prefix}_CA_FILE"));
    if ca_file.is_some() && url.scheme() != "https" {
        bail!("{prefix}_CA_FILE needs an https {prefix}_URL");
    }
    let user = v.var(&format!("{prefix}_USER"));
    let password = v.secret_file(&format!("{prefix}_PASSWORD_FILE"))?;
    if password.is_some() && user.is_none() {
        bail!("{prefix}_PASSWORD_FILE needs {prefix}_USER");
    }
    Ok(Some(HttpEndpoint {
        url,
        user,
        password,
        ca_file,
    }))
}

fn ttl(v: &Vars<'_>, k: &str) -> anyhow::Result<u32> {
    let d: u32 = v.parse(k, 90)?;
    if !(1..=36500).contains(&d) {
        bail!("{k}: 1..=36500");
    }
    Ok(d)
}

fn agg_ttl(v: &Vars<'_>) -> anyhow::Result<u32> {
    let k = "FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS";
    let d: u32 = v.parse(k, DEFAULT_AGG_TTL_DAYS)?;
    if !(30..=3650).contains(&d) {
        bail!("{k}: 30..=3650");
    }
    Ok(d)
}

impl ClickHouseConfig {
    pub fn from_vars(v: &Vars<'_>) -> anyhow::Result<Option<Self>> {
        let Some(endpoint) = endpoint(v, "FASTPROOF_CLICKHOUSE")? else {
            return Ok(None);
        };
        let database = v
            .var("FASTPROOF_CLICKHOUSE_DATABASE")
            .unwrap_or_else(|| "fastproof".into());
        if database.is_empty()
            || database.len() > 64
            || !database
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            bail!("FASTPROOF_CLICKHOUSE_DATABASE: letters, digits and '_' only");
        }
        Ok(Some(Self {
            endpoint,
            database,
            raw_ttl_days: ttl(v, "FASTPROOF_CLICKHOUSE_RAW_TTL_DAYS")?,
            agg_ttl_days: agg_ttl(v)?,
        }))
    }
}

impl OpenSearchConfig {
    pub fn from_vars(v: &Vars<'_>) -> anyhow::Result<Option<Self>> {
        let Some(endpoint) = endpoint(v, "FASTPROOF_OPENSEARCH")? else {
            return Ok(None);
        };
        let index_prefix = v
            .var("FASTPROOF_OPENSEARCH_INDEX_PREFIX")
            .unwrap_or_else(|| "fastproof-".into());
        if index_prefix.is_empty()
            || index_prefix.len() > 100
            || index_prefix.starts_with(['-', '_', '+', '.'])
            || !index_prefix.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            })
        {
            bail!(
                "FASTPROOF_OPENSEARCH_INDEX_PREFIX: lower-case letters, digits, '-', '_' and '.' only"
            );
        }
        Ok(Some(Self {
            endpoint,
            index_prefix,
            raw_ttl_days: ttl(v, "FASTPROOF_OPENSEARCH_RAW_TTL_DAYS")?,
            replicas: v.parse("FASTPROOF_OPENSEARCH_REPLICAS", 1)?,
        }))
    }
}

/// Batching of the sinks.
#[derive(Clone, Copy, Debug)]
pub struct SinkTuning {
    /// Outbox events per batch, at most.
    pub batch_size: usize,
    /// A partial batch is written after this long.
    pub flush_interval: Duration,
}

impl SinkTuning {
    pub fn from_vars(v: &Vars<'_>) -> anyhow::Result<Self> {
        let batch_size: usize = v.parse("FASTPROOF_SINK_BATCH_SIZE", 2000)?;
        if !(1..=100_000).contains(&batch_size) {
            bail!("FASTPROOF_SINK_BATCH_SIZE: 1..=100000");
        }
        let ms: u64 = v.parse("FASTPROOF_SINK_FLUSH_INTERVAL_MS", 1000)?;
        if !(10..=600_000).contains(&ms) {
            bail!("FASTPROOF_SINK_FLUSH_INTERVAL_MS: 10..=600000");
        }
        Ok(Self {
            batch_size,
            flush_interval: Duration::from_millis(ms),
        })
    }
}

impl Default for SinkTuning {
    fn default() -> Self {
        Self {
            batch_size: 2000,
            flush_interval: Duration::from_secs(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    type M = HashMap<&'static str, String>;

    fn kafka(m: &M) -> anyhow::Result<Option<KafkaConfig>> {
        let m = m.clone();
        KafkaConfig::from_vars(&Vars(&move |k| m.get(k).cloned()))
    }

    fn ch(m: &M) -> anyhow::Result<Option<ClickHouseConfig>> {
        let m = m.clone();
        ClickHouseConfig::from_vars(&Vars(&move |k| m.get(k).cloned()))
    }

    fn os(m: &M) -> anyhow::Result<Option<OpenSearchConfig>> {
        let m = m.clone();
        OpenSearchConfig::from_vars(&Vars(&move |k| m.get(k).cloned()))
    }

    #[test]
    fn kafka_settings() {
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "kafka-secret-123\n").unwrap();
        let mut m = M::new();
        assert!(kafka(&m).unwrap().is_none());
        m.insert(
            "FASTPROOF_KAFKA_BROKERS",
            "k1.example.com:9093, k2.example.com:9093".into(),
        );
        m.insert("FASTPROOF_KAFKA_TLS", "true".into());
        m.insert("FASTPROOF_KAFKA_SASL_MECHANISM", "SCRAM-SHA-512".into());
        m.insert("FASTPROOF_KAFKA_USERNAME", "fastproof".into());
        m.insert("FASTPROOF_KAFKA_PASSWORD_FILE", pw.to_string_lossy().into());
        let c = kafka(&m).unwrap().unwrap();
        assert_eq!(c.brokers.len(), 2);
        assert_eq!(c.topic_prefix, "fastproof.");
        assert_eq!(c.partitions, 12);
        assert!(!c.create_topics && !c.trusted_producers);
        assert_eq!(
            (c.fetch_max_bytes, c.max_inflight_bytes),
            (1 << 20, 64 << 20)
        );
        assert_eq!((c.batch_max_records, c.batch_max_bytes), (1000, 1 << 20));
        assert_eq!(c.connect_timeout, Duration::from_secs(300));
        let sasl = c.sasl.clone().unwrap();
        assert_eq!(sasl.mechanism, SaslMechanism::ScramSha512);
        assert_eq!(sasl.password.expose(), "kafka-secret-123");
        assert!(!format!("{c:?}").contains("kafka-secret"));
        for (k, val) in [
            ("FASTPROOF_KAFKA_SASL_MECHANISM", "gssapi"),
            ("FASTPROOF_KAFKA_PARTITIONS", "0"),
            ("FASTPROOF_KAFKA_TOPIC_PREFIX", "a b"),
            ("FASTPROOF_KAFKA_BROKERS", "kafka://x:1"),
            ("FASTPROOF_KAFKA_TLS", "maybe"),
            ("FASTPROOF_KAFKA_FETCH_MAX_BYTES", "65536"),
            ("FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES", "524288"),
            ("FASTPROOF_KAFKA_BATCH_MAX_RECORDS", "0"),
            ("FASTPROOF_KAFKA_BATCH_MAX_BYTES", "100"),
            ("FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS", "x"),
        ] {
            let mut m2 = m.clone();
            m2.insert(k, val.into());
            let e = format!("{:#}", kafka(&m2).unwrap_err());
            assert!(e.contains(k), "{k}: {e}");
        }
        let mut m2 = m.clone();
        m2.remove("FASTPROOF_KAFKA_PASSWORD_FILE");
        assert!(kafka(&m2).is_err());
        let mut m2 = m.clone();
        m2.insert("FASTPROOF_KAFKA_MAX_RECORD_BYTES", (8 << 20).to_string());
        let c = kafka(&m2).unwrap().unwrap();
        assert_eq!(
            (c.fetch_max_bytes, c.max_inflight_bytes),
            (8 << 20, 64 << 20),
            "the fetch size follows the record size"
        );
        m2.insert("FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES", (32 << 20).to_string());
        m2.insert("FASTPROOF_KAFKA_FETCH_MAX_BYTES", (16 << 20).to_string());
        let c = kafka(&m2).unwrap().unwrap();
        assert_eq!(
            (c.fetch_max_bytes, c.max_inflight_bytes),
            (16 << 20, 32 << 20)
        );
    }

    #[test]
    fn http_backends() {
        let mut m = M::new();
        m.insert(
            "FASTPROOF_CLICKHOUSE_URL",
            "https://u:p@ch.example.com:8443".to_string(),
        );
        assert!(ch(&m).is_err());
        m.insert(
            "FASTPROOF_CLICKHOUSE_URL",
            "https://ch.example.com:8443".into(),
        );
        m.insert("FASTPROOF_CLICKHOUSE_DATABASE", "bad-name".into());
        assert!(ch(&m).is_err());
        m.remove("FASTPROOF_CLICKHOUSE_DATABASE");
        let c = ch(&m).unwrap().unwrap();
        assert_eq!(
            (c.database.as_str(), c.raw_ttl_days, c.agg_ttl_days),
            ("fastproof", 90, 730)
        );
        m.insert("FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS", "29".into());
        assert!(
            ch(&m)
                .unwrap_err()
                .to_string()
                .contains("FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS")
        );
        m.insert("FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS", "3650".into());
        assert_eq!(ch(&m).unwrap().unwrap().agg_ttl_days, 3650);
        m.remove("FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS");
        m.insert(
            "FASTPROOF_OPENSEARCH_URL",
            "http://os.example.com:9200".into(),
        );
        m.insert("FASTPROOF_OPENSEARCH_CA_FILE", "/ca.pem".into());
        assert!(os(&m).is_err());
        m.remove("FASTPROOF_OPENSEARCH_CA_FILE");
        m.insert("FASTPROOF_OPENSEARCH_INDEX_PREFIX", "Upper-".into());
        assert!(os(&m).is_err());
        m.remove("FASTPROOF_OPENSEARCH_INDEX_PREFIX");
        assert_eq!(os(&m).unwrap().unwrap().replicas, 1);
    }
}
