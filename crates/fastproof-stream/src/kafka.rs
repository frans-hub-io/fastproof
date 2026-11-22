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

use crate::budget::{ByteBudget, RECORD_OVERHEAD, Reservation};
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
/// Compression of produced batches. rskafka parses a Snappy batch from memory after one
/// decompression, while LZ4 and zstd batches are parsed field by field through a streaming
/// decoder, which cost the writer about an eighth of its CPU; Snappy batches are 6 to 18 %
/// larger than LZ4. Consumers read every codec, whatever produced the records.
const PRODUCE_COMPRESSION: Compression = Compression::Snappy;

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

/// HMAC-SHA256 signatures of the records FastProof produces (key shared through PostgreSQL).
///
/// The [`SIGNATURE_HEADER`] value is `v2.<key id>.<unix ms>.<hex mac>`; the MAC covers the
/// version, topic, key id, time, record key and value. Records older than the maximum age (or
/// more than [`SIGNATURE_SKEW`] in the future) and records signed with another key fail.
#[derive(Clone)]
pub struct Signer {
    /// The MAC state after the key: cloned per record instead of hashing the key every time.
    keyed: Hmac<Sha256>,
    kid: String,
    max_age: Duration,
}

/// Clock difference tolerated for signature times in the future.
pub const SIGNATURE_SKEW: Duration = Duration::from_secs(300);
/// Default maximum age of a signed record (`FASTPROOF_KAFKA_SIGNATURE_MAX_AGE_HOURS`).
pub const SIGNATURE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

impl Signer {
    pub fn new(key: Vec<u8>) -> Self {
        let mut h = <Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut h, b"fastproof-kafka-kid\0");
        sha2::Digest::update(&mut h, &key);
        let kid = hex::encode(&sha2::Digest::finalize(h)[..8]);
        Self {
            keyed: <Hmac<Sha256> as KeyInit>::new_from_slice(&key).expect("any key size"),
            kid,
            max_age: SIGNATURE_MAX_AGE,
        }
    }

    /// Records signed longer ago than `max_age` fail verification.
    pub fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// The key id carried in signatures.
    pub fn key_id(&self) -> &str {
        &self.kid
    }

    fn mac(&self, topic: &str, at_ms: i64, key: &[u8], value: &[u8]) -> Hmac<Sha256> {
        let mut m = self.keyed.clone();
        m.update(b"v2\0");
        m.update(topic.as_bytes());
        m.update(&[0]);
        m.update(self.kid.as_bytes());
        m.update(&[0]);
        m.update(&at_ms.to_be_bytes());
        m.update(&(key.len() as u64).to_be_bytes());
        m.update(key);
        m.update(value);
        m
    }

    /// The header value for a record produced now.
    pub fn sign(&self, topic: &str, key: &[u8], value: &[u8]) -> Vec<u8> {
        self.sign_at(topic, chrono::Utc::now().timestamp_millis(), key, value)
    }

    /// The header value for a record signed at `at_ms` (milliseconds since the epoch).
    pub fn sign_at(&self, topic: &str, at_ms: i64, key: &[u8], value: &[u8]) -> Vec<u8> {
        let mac = hex::encode(self.mac(topic, at_ms, key, value).finalize().into_bytes());
        format!("v2.{}.{at_ms}.{mac}", self.kid).into_bytes()
    }

    pub fn verify(&self, topic: &str, r: &Record) -> bool {
        let Some(sig) = r.headers.get(SIGNATURE_HEADER) else {
            return false;
        };
        let Ok(sig) = std::str::from_utf8(sig) else {
            return false;
        };
        let mut parts = sig.splitn(4, '.');
        let (Some("v2"), Some(kid), Some(at), Some(mac)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        if kid != self.kid {
            return false;
        }
        let Ok(at_ms) = at.parse::<i64>() else {
            return false;
        };
        let now = chrono::Utc::now().timestamp_millis();
        let age = now.saturating_sub(at_ms);
        if age > self.max_age.as_millis() as i64 || -age > SIGNATURE_SKEW.as_millis() as i64 {
            return false;
        }
        let Some(mac) = decode_mac(mac) else {
            return false;
        };
        self.mac(
            topic,
            at_ms,
            r.key.as_deref().unwrap_or_default(),
            r.value.as_deref().unwrap_or_default(),
        )
        .verify_slice(&mac)
        .is_ok()
    }
}

/// The 32 bytes of a hex-encoded HMAC-SHA256 (either case); `None` for anything else.
fn decode_mac(s: &str) -> Option<[u8; 32]> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nibble(b[2 * i])? << 4) | nibble(b[2 * i + 1])?;
    }
    Some(out)
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
    budget: Arc<ByteBudget>,
}

/// Smallest fetch request: the broker still returns at least one whole record batch.
pub const MIN_FETCH_BYTES: usize = 16 * 1024;

/// The fetch size of one partition, adapted to how much its records expand once decoded.
///
/// The broker counts `max_bytes` on the wire (compressed), while memory holds decoded records;
/// the first request is small, and later ones aim at [`KafkaConfig::fetch_max_bytes`] decoded
/// bytes from the expansion seen in full fetches.
#[derive(Clone, Copy, Debug)]
pub struct FetchWindow {
    request: usize,
}

impl FetchWindow {
    pub fn new(target: usize) -> Self {
        Self {
            request: (target / 16).clamp(MIN_FETCH_BYTES, target.max(MIN_FETCH_BYTES)),
        }
    }

    /// Bytes requested from the broker next.
    pub fn request(&self) -> usize {
        self.request
    }

    /// Adapts to a fetch of `self.request()` bytes that decoded to `decoded` bytes; `full`
    /// tells that the partition had more records, so the broker filled the request.
    pub fn observe(&mut self, decoded: usize, full: bool, target: usize) {
        if !full || decoded == 0 {
            return;
        }
        let ideal = (self.request as u128 * target as u128 / decoded as u128) as usize;
        let max = target.max(MIN_FETCH_BYTES);
        self.request = ideal
            .min(self.request.saturating_mul(2))
            .clamp(MIN_FETCH_BYTES, max);
    }
}

/// Records of one partition fetch, held in the process budget until released.
#[derive(Debug)]
pub struct Fetched {
    pub records: Vec<RecordAndOffset>,
    pub high_watermark: i64,
    /// The offset the fetch started at (after a reset of an out-of-range offset).
    pub from: i64,
    pub reservation: Reservation,
}

/// Bytes a record is counted for in the budget.
pub fn record_bytes(r: &RecordAndOffset) -> usize {
    r.record.approximate_size() + RECORD_OVERHEAD
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
            budget: ByteBudget::new(cfg.max_inflight_bytes),
        })
    }

    pub fn config(&self) -> &KafkaConfig {
        &self.cfg
    }

    /// The in-flight byte budget of this process.
    pub fn budget(&self) -> &Arc<ByteBudget> {
        &self.budget
    }

    /// Fetches from `offset` within the process budget: reserves
    /// [`KafkaConfig::fetch_max_bytes`] first (waiting while the budget is spent), then holds
    /// the decoded size of the records returned (records before `from` are dropped).
    pub async fn fetch_bounded(
        &self,
        topic: &str,
        partition: i32,
        offset: i64,
        window: &mut FetchWindow,
        max_wait_ms: i32,
    ) -> anyhow::Result<Fetched> {
        let target = self.cfg.fetch_max_bytes;
        let mut reservation = self.budget.reserve(target).await;
        let request = window.request();
        let (records, high_watermark, from) = self
            .fetch(
                topic,
                partition,
                offset,
                request.min(i32::MAX as usize - 1) as i32 + 1,
                max_wait_ms,
            )
            .await?;
        let records: Vec<RecordAndOffset> =
            records.into_iter().filter(|r| r.offset >= from).collect();
        let decoded: usize = records.iter().map(record_bytes).sum();
        reservation.resize(decoded);
        let full = records
            .last()
            .is_some_and(|r| r.offset + 1 < high_watermark);
        window.observe(decoded, full, target);
        metrics::histogram!("fastproof_kafka_fetch_bytes").record(decoded as f64);
        Ok(Fetched {
            records,
            high_watermark,
            from,
            reservation,
        })
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
                    pc.produce(std::mem::take(&mut batch), PRODUCE_COMPRESSION)
                        .await
                        .with_context(|| format!("producing to {topic}/{p}"))?;
                    bytes = 0;
                }
                bytes += sz;
                batch.push(r);
            }
            count += batch.len();
            pc.produce(batch, PRODUCE_COMPRESSION)
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
        let bytes = 1..max_bytes.max(1024);
        match pc.fetch_records(offset, bytes.clone(), max_wait_ms).await {
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
                let (records, hwm) = pc.fetch_records(reset, bytes, max_wait_ms).await?;
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
    fn fetch_windows_aim_at_the_decoded_target() {
        let target = 4 << 20;
        let mut w = FetchWindow::new(target);
        assert_eq!(w.request(), 256 << 10);
        // Records expanding eight times: the request settles at an eighth of the target.
        for _ in 0..10 {
            let decoded = w.request() * 8;
            w.observe(decoded, true, target);
        }
        assert_eq!(w.request(), target / 8);
        // Incompressible records: the request grows to the target, doubling at most.
        let before = w.request();
        w.observe(before, true, target);
        assert_eq!(w.request(), before * 2);
        for _ in 0..10 {
            let decoded = w.request();
            w.observe(decoded, true, target);
        }
        assert_eq!(w.request(), target);
        // A partial fetch says nothing about the expansion.
        w.observe(1, false, target);
        assert_eq!(w.request(), target);
        // Extreme expansion stops at the minimum.
        w.observe(target * 1000, true, target);
        assert_eq!(w.request(), MIN_FETCH_BYTES);
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
        assert!(!Signer::new(vec![8; 32]).verify("t", &r));
        r.value = Some(b"{\"v\":2}".to_vec());
        assert!(!s.verify("t", &r));
    }

    #[test]
    fn macs_decode_like_hex() {
        let bytes: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect();
        let lower = hex::encode(&bytes);
        assert_eq!(decode_mac(&lower).unwrap().to_vec(), bytes);
        assert_eq!(decode_mac(&lower.to_uppercase()).unwrap().to_vec(), bytes);
        assert!(decode_mac(&lower[..62]).is_none());
        assert!(decode_mac(&format!("{lower}00")).is_none());
        assert!(decode_mac(&format!("g{}", &lower[1..])).is_none());
        assert!(decode_mac(&"é".repeat(32)).is_none());
    }

    #[test]
    fn signatures_carry_key_id_and_time() {
        let s = Signer::new(vec![7; 32]).with_max_age(Duration::from_secs(3600));
        let rec = |sig: Vec<u8>| Record {
            key: Some(b"k".to_vec()),
            value: Some(b"v".to_vec()),
            headers: BTreeMap::from([(SIGNATURE_HEADER.to_string(), sig)]),
            timestamp: chrono::Utc::now(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let sig = String::from_utf8(s.sign("t", b"k", b"v")).unwrap();
        assert!(sig.starts_with(&format!("v2.{}.", s.key_id())), "{sig}");
        assert!(s.verify("t", &rec(s.sign_at("t", now - 60_000, b"k", b"v"))));
        assert!(!s.verify("t", &rec(s.sign_at("t", now - 7_200_000, b"k", b"v"))));
        assert!(!s.verify("t", &rec(s.sign_at("t", now + 3_600_000, b"k", b"v"))));
        let replayed = sig.replacen(
            &format!(".{}.", sig.split('.').nth(2).unwrap()),
            &format!(".{}.", now - 1234),
            1,
        );
        assert!(!s.verify("t", &rec(replayed.into_bytes())));
        let mut other_kid = Signer::new(vec![7; 32]);
        other_kid.kid = "0000000000000000".into();
        assert!(!s.verify("t", &rec(other_kid.sign("t", b"k", b"v"))));
        let legacy = {
            let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(&[7; 32]).unwrap();
            m.update(b"t\0");
            m.update(&1u64.to_be_bytes());
            m.update(b"kv");
            hex::encode(m.finalize().into_bytes()).into_bytes()
        };
        assert!(!s.verify("t", &rec(legacy)));
    }
}
