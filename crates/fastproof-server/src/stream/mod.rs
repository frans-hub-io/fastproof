//! Streaming mode in the server: the HTTP gateway producing to Kafka, the writer that persists
//! `ingest.results` and `ingest.logs` with the offset committed in the same transaction, the
//! upload consumer of `ingest.uploads`, live log fan-out, and the start-up of the PostgreSQL
//! managed consumer groups, the outbox relay and the sinks per `FASTPROOF_ROLES`.

pub mod live;
pub mod uploads;
pub mod writer;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fastproof_model::ReportResult;
use fastproof_store::ingest::LaunchRef;
use fastproof_store::logs::LogLine;
use fastproof_stream::Kafka;
use fastproof_stream::consumer::{KafkaConsumer, OUTBOX_TOPIC};
use fastproof_stream::groups::{Group, PartitionRunner};
use fastproof_stream::kafka::{
    Outgoing, Start, TOPIC_EVENTS, TOPIC_LOGS, TOPIC_RESULTS, TOPIC_UPLOADS,
};
use fastproof_stream::messages::{LogRecord, ResultRecord, VERSION};
use fastproof_stream::sink::{KafkaSink, OutboxRelay, OutboxSink, SearchBackend, Sink};
use franshub_http::{ApiError, ApiResult};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{IngestMode, SearchBackendKind, SinkKind};
use crate::state::AppState;

/// Consumer group names (`31-streaming.md`).
pub const GROUP_WRITER: &str = "fastproof-writer";
pub const GROUP_INGEST: &str = "fastproof-ingest";
pub const GROUP_RELAY: &str = "fastproof-outbox-relay";

/// Name of the signing key of gateway records in `stream_keys`.
pub const SIGNING_KEY: &str = "kafka-gateway";

impl AppState {
    /// True when HTTP ingest goes to Kafka.
    pub fn kafka_gateway(&self) -> bool {
        self.cfg.ingest_mode == IngestMode::Kafka
    }

    /// The Kafka client (present in Kafka mode).
    pub fn kafka(&self) -> ApiResult<&Arc<Kafka>> {
        self.kafka
            .as_ref()
            .ok_or_else(|| ApiError::from(anyhow::anyhow!("Kafka is not connected")))
    }
}

/// What the gateway produced.
#[derive(Default)]
pub struct Produced {
    pub accepted: u64,
    pub rejected: Vec<(u64, String)>,
}

fn unavailable(e: anyhow::Error) -> ApiError {
    tracing::warn!(error = %format!("{e:#}"), "producing to Kafka failed");
    ApiError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "the ingest queue is not available; retry later",
    )
}

/// Produces validated results to `ingest.results` (key = launch id).
pub async fn produce_results(
    s: &AppState,
    launch: &LaunchRef,
    results: Vec<(u64, ReportResult)>,
) -> ApiResult<Produced> {
    let kafka = s.kafka()?;
    let max = kafka.config().max_record_bytes;
    let mut out = Produced::default();
    let mut records = Vec::with_capacity(results.len());
    for (index, r) in results {
        let rec = ResultRecord {
            v: VERSION,
            project: launch.project_key.clone(),
            launch_id: launch.id,
            result: Some(r),
            parsed: None,
            client_key: None,
            upload_id: None,
        };
        let value = serde_json::to_vec(&rec).map_err(anyhow::Error::from)?;
        if value.len() > max {
            out.rejected.push((
                index,
                format!("the result exceeds the Kafka record limit ({max} bytes)"),
            ));
            continue;
        }
        records.push(Outgoing {
            key: launch.id.to_string().into_bytes(),
            value,
        });
    }
    let n = records.len() as u64;
    kafka
        .produce(&kafka.topic(TOPIC_RESULTS), records)
        .await
        .map_err(unavailable)?;
    out.accepted = n;
    metrics::counter!("fastproof_results_ingested_total", "source" => "kafka-gateway").increment(n);
    Ok(out)
}

/// Produces log lines to `ingest.logs` (key = launch id).
pub async fn produce_logs(
    s: &AppState,
    project: &str,
    launch_id: uuid::Uuid,
    lines: Vec<LogLine>,
) -> ApiResult<()> {
    let kafka = s.kafka()?;
    let mut records = Vec::with_capacity(lines.len());
    let key = launch_id.to_string().into_bytes();
    for l in lines {
        records.push(Outgoing {
            key: key.clone(),
            value: serde_json::to_vec(&LogRecord::new(project, launch_id, l))
                .map_err(anyhow::Error::from)?,
        });
    }
    kafka
        .produce(&kafka.topic(TOPIC_LOGS), records)
        .await
        .map_err(unavailable)
}

/// The search backend of `FASTPROOF_SEARCH_BACKEND` (`None` for PostgreSQL).
pub fn search_backend(cfg: &crate::Config) -> anyhow::Result<Option<Arc<dyn SearchBackend>>> {
    Ok(match cfg.search_backend {
        SearchBackendKind::Postgres => None,
        SearchBackendKind::ClickHouse => {
            Some(Arc::new(fastproof_stream::clickhouse::ClickHouse::new(
                cfg.clickhouse
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("FASTPROOF_CLICKHOUSE_URL is not set"))?,
            )?))
        }
        SearchBackendKind::OpenSearch => {
            Some(Arc::new(fastproof_stream::opensearch::OpenSearch::new(
                cfg.opensearch
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("FASTPROOF_OPENSEARCH_URL is not set"))?,
            )?))
        }
    })
}

fn sinks(cfg: &crate::Config) -> anyhow::Result<Vec<Arc<dyn Sink>>> {
    let mut out: Vec<Arc<dyn Sink>> = Vec::new();
    for k in &cfg.sinks {
        match k {
            SinkKind::ClickHouse => {
                out.push(Arc::new(fastproof_stream::clickhouse::ClickHouse::new(
                    cfg.clickhouse
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("FASTPROOF_CLICKHOUSE_URL is not set"))?,
                )?))
            }
            SinkKind::OpenSearch => {
                out.push(Arc::new(fastproof_stream::opensearch::OpenSearch::new(
                    cfg.opensearch
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("FASTPROOF_OPENSEARCH_URL is not set"))?,
                )?))
            }
        }
    }
    Ok(out)
}

/// Connects to Kafka with the shared signing key and checks (or creates) the topics.
pub async fn connect_kafka(
    cfg: &crate::Config,
    store: &fastproof_store::Store,
) -> anyhow::Result<Option<Arc<Kafka>>> {
    let Some(k) = &cfg.kafka else {
        return Ok(None);
    };
    if cfg.ingest_mode != IngestMode::Kafka {
        return Ok(None);
    }
    let key = store.stream_key(SIGNING_KEY).await?;
    let signer = fastproof_stream::kafka::Signer::new(key).with_max_age(k.signature_max_age);
    let kafka = Kafka::connect(k, Some(signer)).await?;
    kafka.ensure_topics().await?;
    Ok(Some(Arc::new(kafka)))
}

async fn group_with<R: PartitionRunner>(
    s: &AppState,
    name: &str,
    topics: Vec<(String, i32)>,
    runner: Arc<R>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    let g = Group {
        pool: s.store.pool().clone(),
        group: name.to_string(),
        member: s.instance.clone(),
        topics,
    };
    tokio::spawn(g.run(runner, stop))
}

async fn kafka_topics(kafka: &Kafka, names: &[&str]) -> anyhow::Result<Vec<(String, i32)>> {
    let mut out = Vec::new();
    for n in names {
        let t = kafka.topic(n);
        let p = kafka.partition_count(&t).await?;
        out.push((t, p));
    }
    Ok(out)
}

/// Starts the streaming background work of this process's roles.
pub async fn start(s: &AppState, stop: &CancellationToken) -> anyhow::Result<Vec<JoinHandle<()>>> {
    let mut tasks = Vec::new();
    let roles = s.cfg.roles;
    if roles.api {
        match &s.kafka {
            Some(k) => tasks.push(tokio::spawn(live::listen_kafka(
                k.clone(),
                s.logs.clone(),
                stop.clone(),
            ))),
            None => {
                let pool = s.store.pool().clone();
                let first =
                    crate::live::first_subscription(&pool, &[fastproof_store::logs::LOGS_CHANNEL])
                        .await;
                tasks.push(tokio::spawn(live::listen_pg(
                    pool,
                    s.store.clone(),
                    s.logs.clone(),
                    stop.clone(),
                    first,
                )))
            }
        }
    }
    if let Some(kafka) = &s.kafka {
        if roles.writer {
            let topics = kafka_topics(kafka, &[TOPIC_RESULTS, TOPIC_LOGS]).await?;
            let consumer = Arc::new(KafkaConsumer {
                kafka: kafka.clone(),
                handler: Arc::new(writer::Writer::new(s.clone(), kafka.clone())),
                start: Start::Earliest,
                max_wait_ms: 200,
            });
            tasks.push(group_with(s, GROUP_WRITER, topics, consumer, stop.clone()).await);
            let relay = Arc::new(OutboxRelay {
                store: s.store.clone(),
                kafka: kafka.clone(),
            });
            tasks.push(
                group_with(
                    s,
                    GROUP_RELAY,
                    vec![(OUTBOX_TOPIC.to_string(), 1)],
                    relay,
                    stop.clone(),
                )
                .await,
            );
        }
        if roles.ingest {
            let topics = kafka_topics(kafka, &[TOPIC_UPLOADS]).await?;
            let consumer = Arc::new(KafkaConsumer {
                kafka: kafka.clone(),
                handler: Arc::new(uploads::UploadHandler::new(s.clone(), stop.clone())),
                start: Start::Earliest,
                max_wait_ms: 500,
            });
            tasks.push(group_with(s, GROUP_INGEST, topics, consumer, stop.clone()).await);
        }
    }
    if roles.sink {
        for sink in sinks(&s.cfg)? {
            let s = s.clone();
            let stop = stop.clone();
            tasks.push(tokio::spawn(async move {
                let mut backoff = Duration::from_secs(1);
                loop {
                    match sink.prepare().await {
                        Ok(()) => break,
                        Err(e) => {
                            tracing::warn!(sink = sink.name(), error = %format!("{e:#}"), "preparing the sink failed; retrying");
                            metrics::counter!("fastproof_sink_errors_total", "sink" => sink.name()).increment(1);
                        }
                    }
                    tokio::select! {
                        _ = stop.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
                let group = format!("fastproof-sink-{}", sink.name());
                let task = match s.kafka.clone() {
                    Some(kafka) => {
                        let topics = match kafka_topics(&kafka, &[TOPIC_EVENTS]).await {
                            Ok(t) => t,
                            Err(e) => {
                                tracing::error!(error = %format!("{e:#}"), "the events topic is not available");
                                return;
                            }
                        };
                        let Some(signer) = kafka.signer().cloned() else {
                            tracing::error!("the events topic needs the signing key");
                            return;
                        };
                        let consumer = Arc::new(KafkaConsumer {
                            kafka,
                            handler: Arc::new(KafkaSink::new(s.store.clone(), sink, signer)),
                            start: Start::Earliest,
                            max_wait_ms: s.cfg.sink_tuning.flush_interval.as_millis().min(5000) as i32,
                        });
                        group_with(&s, &group, topics, consumer, stop.clone()).await
                    }
                    None => {
                        let runner = Arc::new(OutboxSink {
                            store: s.store.clone(),
                            sink,
                            tuning: s.cfg.sink_tuning,
                        });
                        group_with(&s, &group, vec![(OUTBOX_TOPIC.to_string(), 1)], runner, stop.clone()).await
                    }
                };
                let _ = task.await;
            }));
        }
    }
    Ok(tasks)
}

/// Groups items by a key, keeping their order within a key.
pub fn group_by<K: Ord, T>(items: impl IntoIterator<Item = (K, T)>) -> BTreeMap<K, Vec<T>> {
    let mut m: BTreeMap<K, Vec<T>> = BTreeMap::new();
    for (k, t) in items {
        m.entry(k).or_default().push(t);
    }
    m
}
