//! A consumer that starts on a partition checks the record its stored offset follows: when the
//! topic was created again in between (deleted, or lost with the broker's data), the new log
//! is read from its start, also when it already reaches past the stored offset (which used to
//! skip its first records); when the topic is the same, nothing is read again.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_KAFKA_BROKERS`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::TestDb;
use fastproof_stream::consumer::{BatchHandler, KafkaConsumer};
use fastproof_stream::groups::{PartitionCtx, PartitionRunner};
use fastproof_stream::kafka::{Outgoing, RecordAndOffset, Start, TOPIC_RESULTS};
use fastproof_stream::{Kafka, KafkaConfig, Vars};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Collect(Mutex<Vec<String>>);

#[async_trait]
impl BatchHandler for Collect {
    async fn handle(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next_offset: i64,
        hwm: i64,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().extend(
            records
                .iter()
                .map(|r| String::from_utf8(r.record.value.clone().unwrap_or_default()).unwrap()),
        );
        ctx.commit(next_offset, Some(hwm)).await
    }
}

async fn produce(k: &Kafka, topic: &str, tag: &str, n: usize) {
    let recs: Vec<Outgoing> = (0..n)
        .map(|i| Outgoing {
            key: b"k".to_vec(),
            value: format!("{tag}-{i}").into_bytes(),
        })
        .collect();
    k.produce(topic, recs).await.unwrap();
}

/// Runs a consumer on partition 0 until it has handed over `n` records (or 30 s pass), then
/// waits a little for anything more and stops it; returns what it read.
async fn consume(k: &Arc<Kafka>, db: &TestDb, topic: &str, n: usize) -> Vec<String> {
    let handler = Arc::new(Collect::default());
    let consumer = Arc::new(KafkaConsumer {
        kafka: k.clone(),
        handler: handler.clone(),
        start: Start::Earliest,
        max_wait_ms: 100,
    });
    let ctx = PartitionCtx {
        pool: db.pool.clone(),
        group: "g".into(),
        member: "m".into(),
        topic: topic.to_string(),
        partition: 0,
    };
    let stop = CancellationToken::new();
    let task = tokio::spawn(consumer.run(ctx, stop.clone()));
    let deadline = Instant::now() + Duration::from_secs(30);
    while handler.0.lock().unwrap().len() < n && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
    handler.0.lock().unwrap().clone()
}

#[tokio::test]
async fn a_topic_created_again_is_read_from_its_start() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(brokers) = std::env::var("FASTPROOF_TEST_KAFKA_BROKERS")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    let prefix = format!("fpreoff{:08x}.", rand::random::<u32>());
    let vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_KAFKA_BROKERS", brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS", "1".into()),
    ];
    let get = move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let cfg = KafkaConfig::from_vars(&Vars(&get)).unwrap().unwrap();
    let k = Arc::new(Kafka::connect(&cfg, None).await.unwrap());
    k.ensure_topics().await.unwrap();
    let topic = k.topic(TOPIC_RESULTS);
    sqlx::query(
        "insert into kafka_partition_leases values ('g', $1, 0, 'm', now() + interval '1 hour')",
    )
    .bind(&topic)
    .execute(&db.pool)
    .await
    .unwrap();

    produce(&k, &topic, "a", 10).await;
    assert_eq!(consume(&k, &db, &topic, 10).await.len(), 10);
    // The same topic: a new start reads only what is new.
    produce(&k, &topic, "b", 2).await;
    assert_eq!(
        consume(&k, &db, &topic, 2).await,
        vec!["b-0".to_string(), "b-1".to_string()]
    );

    // Created again and already longer than the stored offset (12).
    k.delete_topic(&topic).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    k.recover_topic(&topic).await.unwrap();
    produce(&k, &topic, "c", 15).await;
    let got = consume(&k, &db, &topic, 15).await;
    assert_eq!(got.len(), 15, "{got:?}");
    assert_eq!(got.first().map(String::as_str), Some("c-0"));

    // Created again and shorter than the stored offset (15).
    k.delete_topic(&topic).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    k.recover_topic(&topic).await.unwrap();
    produce(&k, &topic, "d", 4).await;
    let got = consume(&k, &db, &topic, 4).await;
    assert_eq!(got.len(), 4, "{got:?}");
    let _ = k.delete_topic(&topic).await;
}
