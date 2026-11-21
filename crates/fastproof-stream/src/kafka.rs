//! Kafka through rskafka: TLS with a private CA, SASL PLAIN and SCRAM (ported from FastMock's
//! `fastmock-kafka`), topic creation, a murmur2 partitioner compatible with the Java client, and
//! gateway signatures that tell gateway records from records of other producers.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use hmac::{Hmac, KeyInit, Mac};
use rskafka::BackoffConfig;
use rskafka::client::partition::{Compression, OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder, Credentials, SaslConfig};
pub use rskafka::record::{Record, RecordAndOffset};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use sha2::Sha256;
use tokio::sync::Mutex;

use crate::config::{KafkaConfig, SaslMechanism};

/// Topic names after the prefix (`31-streaming.md`).
pub const TOPIC_RESULTS: &str = "ingest.results";
pub const TOPIC_LOGS: &str = "ingest.logs";
pub const TOPIC_UPLOADS: &str = "ingest.uploads";
pub const TOPIC_EVENTS: &str = "events";
/// Header holding the gateway signature of a record.
pub const SIGNATURE_HEADER: &str = "fastproof-sig";
/// Uncompressed bytes per produce request, at most.
const PRODUCE_BATCH_BYTES: usize = 512 * 1024;

/// Kafka's murmur2 (as `org.apache.kafka.common.utils.Utils.murmur2`).
pub fn murmur2(data: &[u8]) -> i32 {
    let seed: u32 = 0x9747_b28c;
    let m: u32 = 0x5bd1_e995;
    let r = 24;
    let len = data.len();
    let mut h: u32 = seed ^ (len as u32);
    let chunks = len / 4;
    for i in 0..chunks {
        let o = i * 4;
        let mut k = u32::from(data[o])
            | (u32::from(data[o + 1]) << 8)
            | (u32::from(data[o + 2]) << 16)
            | (u32::from(data[o + 3]) << 24);
        k = k.wrapping_mul(m);
        k ^= k >> r;
        k = k.wrapping_mul(m);
        h = h.wrapping_mul(m);
        h ^= k;
    }
    let tail = chunks * 4;
    let rem = len % 4;
    if rem == 3 {
        h ^= u32::from(data[tail + 2]) << 16;
    }
    if rem >= 2 {
        h ^= u32::from(data[tail + 1]) << 8;
    }
    if rem >= 1 {
        h ^= u32::from(data[tail]);
        h = h.wrapping_mul(m);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(m);
    h ^= h >> 15;
    h as i32
}

/// The partition of a key as the Java client's default partitioner picks it.
pub fn partition_for(key: &[u8], partitions: i32) -> i32 {
    if partitions <= 1 {
        return 0;
    }
    (murmur2(key) & 0x7fff_ffff) % partitions
}

fn read_ca_bundle(path: &Path) -> anyhow::Result<Vec<CertificateDer<'static>>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("reading {}", path.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parsing {}", path.display()))?;
    if certs.is_empty() {
        bail!("{} contains no PEM certificates", path.display());
    }
    Ok(certs)
}

/// The rustls client configuration: the Mozilla roots plus the private CA.
pub fn tls_config(ca_file: Option<&Path>) -> anyhow::Result<Arc<rustls::ClientConfig>> {
    let mut roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    if let Some(ca) = ca_file {
        for c in read_ca_bundle(ca)? {
            roots
                .add(c)
                .with_context(|| format!("{} contains an invalid CA certificate", ca.display()))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

fn builder(cfg: &KafkaConfig) -> anyhow::Result<ClientBuilder> {
    let mut b = ClientBuilder::new(cfg.brokers.clone())
        .client_id(cfg.client_id.clone())
        .max_message_size(64 * 1024 * 1024)
        .backoff_config(BackoffConfig {
            init_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            base: 3.0,
            deadline: Some(Duration::from_secs(30)),
        });
    if cfg.tls {
        b = b.tls_config(tls_config(cfg.ca_file.as_deref()).context("FASTPROOF_KAFKA_CA_FILE")?);
    }
    if let Some(s) = &cfg.sasl {
        let c = Credentials::new(s.username.clone(), s.password.expose().to_string());
        b = b.sasl_config(match s.mechanism {
            SaslMechanism::Plain => SaslConfig::Plain(c),
            SaslMechanism::ScramSha256 => SaslConfig::ScramSha256(c),
            SaslMechanism::ScramSha512 => SaslConfig::ScramSha512(c),
        });
    }
    Ok(b)
}

/// HMAC-SHA256 signatures of gateway records (key shared through PostgreSQL).
#[derive(Clone)]
pub struct Signer {
    key: Arc<Vec<u8>>,
}

impl Signer {
    pub fn new(key: Vec<u8>) -> Self {
        Self { key: Arc::new(key) }
    }

    fn mac(&self, topic: &str, key: &[u8], value: &[u8]) -> Hmac<Sha256> {
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key).expect("any key size");
        m.update(topic.as_bytes());
        m.update(&[0]);
        m.update(&(key.len() as u64).to_be_bytes());
        m.update(key);
        m.update(value);
        m
    }

    pub fn sign(&self, topic: &str, key: &[u8], value: &[u8]) -> Vec<u8> {
        hex::encode(self.mac(topic, key, value).finalize().into_bytes()).into_bytes()
    }

    pub fn verify(&self, topic: &str, r: &Record) -> bool {
        let Some(sig) = r.headers.get(SIGNATURE_HEADER) else {
            return false;
        };
        let Ok(sig) = hex::decode(sig) else {
            return false;
        };
        self.mac(
            topic,
            r.key.as_deref().unwrap_or_default(),
            r.value.as_deref().unwrap_or_default(),
        )
        .verify_slice(&sig)
        .is_ok()
    }
}

/// A record to produce: the key picks the partition.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// A connected client.
pub struct Kafka {
    client: Client,
    cfg: KafkaConfig,
    signer: Option<Signer>,
    partitions: Mutex<HashMap<String, i32>>,
    clients: Mutex<HashMap<(String, i32), Arc<PartitionClient>>>,
}

impl std::fmt::Debug for Kafka {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kafka")
            .field("brokers", &self.cfg.brokers)
            .finish_non_exhaustive()
    }
}

/// Where a consumer without a stored offset starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Earliest,
    Latest,
}

impl Kafka {
    /// Connects; `signer` signs every produced record.
    pub async fn connect(cfg: &KafkaConfig, signer: Option<Signer>) -> anyhow::Result<Self> {
        let b = builder(cfg)?;
        let client = tokio::time::timeout(Duration::from_secs(30), b.build())
            .await
            .context("connecting to Kafka timed out")?
            .context("connecting to Kafka")?;
        Ok(Self {
            client,
            cfg: cfg.clone(),
            signer,
            partitions: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &KafkaConfig {
        &self.cfg
    }

    pub fn signer(&self) -> Option<&Signer> {
        self.signer.as_ref()
    }

    /// The full name of a topic.
    pub fn topic(&self, name: &str) -> String {
        format!("{}{name}", self.cfg.topic_prefix)
    }

    /// Creates the missing FastProof topics when `FASTPROOF_KAFKA_CREATE_TOPICS` is on, and
    /// checks that they exist.
    pub async fn ensure_topics(&self) -> anyhow::Result<()> {
        let wanted: Vec<String> = [TOPIC_RESULTS, TOPIC_LOGS, TOPIC_UPLOADS, TOPIC_EVENTS]
            .iter()
            .map(|t| self.topic(t))
            .collect();
        let existing: Vec<String> = self
            .client
            .list_topics()
            .await
            .context("listing Kafka topics")?
            .into_iter()
            .map(|t| t.name)
            .collect();
        let missing: Vec<&String> = wanted.iter().filter(|t| !existing.contains(t)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        if !self.cfg.create_topics {
            bail!(
                "Kafka topics {} do not exist: create them or set FASTPROOF_KAFKA_CREATE_TOPICS=true",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let controller = self.client.controller_client()?;
        for t in missing {
            match controller
                .create_topic(t.clone(), self.cfg.partitions, self.cfg.replication, 30_000)
                .await
            {
                Ok(()) => {
                    tracing::info!(topic = %t, partitions = self.cfg.partitions, "created Kafka topic")
                }
                Err(e) if e.to_string().contains("TopicAlreadyExists") => {}
                Err(e) => return Err(e).with_context(|| format!("creating Kafka topic {t}")),
            }
        }
        for _ in 0..100 {
            let topics = self.client.list_topics().await?;
            if wanted.iter().all(|w| {
                topics
                    .iter()
                    .any(|t| &t.name == w && !t.partitions.is_empty())
            }) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("Kafka topics were created but their metadata did not appear")
    }

    /// Partitions of a topic (cached).
    pub async fn partition_count(&self, topic: &str) -> anyhow::Result<i32> {
        if let Some(n) = self.partitions.lock().await.get(topic) {
            return Ok(*n);
        }
        let n = self
            .client
            .list_topics()
            .await?
            .into_iter()
            .find(|t| t.name == topic)
            .map(|t| t.partitions.len() as i32)
            .filter(|n| *n > 0)
            .with_context(|| format!("Kafka topic {topic} does not exist"))?;
        self.partitions.lock().await.insert(topic.to_string(), n);
        Ok(n)
    }

    /// A client of one partition (cached).
    pub async fn partition(
        &self,
        topic: &str,
        partition: i32,
    ) -> anyhow::Result<Arc<PartitionClient>> {
        let key = (topic.to_string(), partition);
        if let Some(c) = self.clients.lock().await.get(&key) {
            return Ok(c.clone());
        }
        let c = Arc::new(
            self.client
                .partition_client(topic.to_string(), partition, UnknownTopicHandling::Retry)
                .await
                .with_context(|| format!("connecting to {topic}/{partition}"))?,
        );
        self.clients.lock().await.insert(key, c.clone());
        Ok(c)
    }

    /// Produces records keyed by their key (murmur2), signed when a signer is set. Records of
    /// one key keep their order. Fails when a value exceeds `max_record_bytes`.
    pub async fn produce(&self, topic: &str, records: Vec<Outgoing>) -> anyhow::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let n = self.partition_count(topic).await?;
        let now = chrono::Utc::now();
        let mut by_partition: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
        for o in records {
            if o.value.len() > self.cfg.max_record_bytes {
                bail!(
                    "a record of {} bytes exceeds FASTPROOF_KAFKA_MAX_RECORD_BYTES ({})",
                    o.value.len(),
                    self.cfg.max_record_bytes
                );
            }
            let p = partition_for(&o.key, n);
            let mut headers = BTreeMap::new();
            if let Some(s) = &self.signer {
                headers.insert(
                    SIGNATURE_HEADER.to_string(),
                    s.sign(topic, &o.key, &o.value),
                );
            }
            by_partition.entry(p).or_default().push(Record {
                key: Some(o.key),
                value: Some(o.value),
                headers,
                timestamp: now,
            });
        }
        let started = std::time::Instant::now();
        let mut total = 0usize;
        let sends = by_partition.into_iter().map(|(p, recs)| async move {
            let pc = self.partition(topic, p).await?;
            let mut batch = Vec::new();
            let mut bytes = 0;
            let mut count = 0usize;
            for r in recs {
                let sz = r.approximate_size();
                if !batch.is_empty() && bytes + sz > PRODUCE_BATCH_BYTES {
                    count += batch.len();
                    pc.produce(std::mem::take(&mut batch), Compression::Lz4)
                        .await
                        .with_context(|| format!("producing to {topic}/{p}"))?;
                    bytes = 0;
                }
                bytes += sz;
                batch.push(r);
            }
            count += batch.len();
            pc.produce(batch, Compression::Lz4)
                .await
                .with_context(|| format!("producing to {topic}/{p}"))?;
            Ok::<_, anyhow::Error>(count)
        });
        for r in futures::future::join_all(sends).await {
            total += r?;
        }
        metrics::counter!("fastproof_kafka_produced_total", "topic" => topic.to_string())
            .increment(total as u64);
        metrics::histogram!("fastproof_kafka_produce_seconds")
            .record(started.elapsed().as_secs_f64());
        Ok(())
    }

    /// The offset of a partition at `at`.
    pub async fn offset(&self, topic: &str, partition: i32, at: Start) -> anyhow::Result<i64> {
        let pc = self.partition(topic, partition).await?;
        Ok(pc
            .get_offset(match at {
                Start::Earliest => OffsetAt::Earliest,
                Start::Latest => OffsetAt::Latest,
            })
            .await?)
    }

    /// Fetches records from `offset`; returns them and the high watermark. An offset that is no
    /// longer (or not yet) in the log resets to the earliest offset.
    pub async fn fetch(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        max_bytes: i32,
        max_wait_ms: i32,
    ) -> anyhow::Result<(Vec<RecordAndOffset>, i64, i64)> {
        let pc = self.partition(topic, partition).await?;
        match pc
            .fetch_records(offset, 1..max_bytes.max(1024), max_wait_ms)
            .await
        {
            Ok((records, hwm)) => Ok((records, hwm, offset)),
            Err(rskafka::client::error::Error::ServerError {
                protocol_error: rskafka::client::error::ProtocolError::OffsetOutOfRange,
                ..
            }) => {
                let earliest = pc.get_offset(OffsetAt::Earliest).await?;
                let latest = pc.get_offset(OffsetAt::Latest).await?;
                let reset = if offset > latest { latest } else { earliest };
                tracing::warn!(
                    topic,
                    partition,
                    offset,
                    reset,
                    "Kafka offset out of range; resetting"
                );
                let (records, hwm) = pc
                    .fetch_records(reset, 1..max_bytes.max(1024), max_wait_ms)
                    .await?;
                Ok((records, hwm, reset))
            }
            Err(e) => Err(e).with_context(|| format!("fetching from {topic}/{partition}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmur2_matches_the_java_client() {
        assert_eq!(murmur2(b"21"), -973932308);
        assert_eq!(murmur2(b"foobar"), -790332482);
        assert_eq!(murmur2(b"a-little-bit-long-string"), -985981536);
        assert_eq!(murmur2(b"a-little-bit-longer-string"), -1486304829);
        assert_eq!(
            murmur2(b"lkjh234lh9fiuh90y23oiuhsafujhadof229phr9h19h89h8"),
            -58897971
        );
        assert_eq!(murmur2(b"abc"), 479470107);
        assert_eq!(partition_for(b"foobar", 1), 0);
        let p = partition_for(b"0195b2c4-7d1e-7000-8000-000000000001", 12);
        assert!((0..12).contains(&p));
    }

    #[test]
    fn signatures() {
        let s = Signer::new(vec![7; 32]);
        let mut r = Record {
            key: Some(b"k".to_vec()),
            value: Some(b"{\"v\":1}".to_vec()),
            headers: BTreeMap::new(),
            timestamp: chrono::Utc::now(),
        };
        assert!(!s.verify("t", &r));
        r.headers
            .insert(SIGNATURE_HEADER.into(), s.sign("t", b"k", b"{\"v\":1}"));
        assert!(s.verify("t", &r));
        assert!(!s.verify("other", &r));
        r.value = Some(b"{\"v\":2}".to_vec());
        assert!(!s.verify("t", &r));
        assert!(!Signer::new(vec![8; 32]).verify("t", &r));
    }
}
