//! A Kafka broker that loses its topics while FastProof runs (a restart without persistent
//! data, or a deleted topic): with topic creation on, the API creates them again on the next
//! request and the writer carries on from the new logs, without restarting anything.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_KAFKA_BROKERS`.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn post_results(s: &Server, base: &str, from: usize, n: usize) {
    let body: String = (from..from + n)
        .map(|i| {
            format!(
                "{}\n",
                json!({"id": format!("t{i}"), "name": format!("case {i}"), "fullName": format!("r.Suite#case{i}"), "status": "passed"})
            )
        })
        .collect();
    let r = s
        .req(Method::POST, &format!("{base}/results"), Some(ADMIN_TOKEN))
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let b: Value = r.json().await.unwrap_or(Value::Null);
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["accepted"], n);
}

async fn wait_for_attempts(db: &TestDb, n: i64) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let c: i64 = sqlx::query_scalar("select count(*) from attempts")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if c >= n {
            assert_eq!(c, n, "no duplicates");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{c} of {n} attempts stored in time"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The sum of the end offsets of `topic` (forgetting what the client cached about it).
async fn end_offsets(admin: &fastproof_stream::Kafka, topic: &str) -> anyhow::Result<i64> {
    admin.recover_topic(topic).await?;
    let mut sum = 0;
    for p in 0..admin.partition_count(topic).await? {
        sum += admin
            .offset(topic, p, fastproof_stream::kafka::Start::Latest)
            .await?;
    }
    Ok(sum)
}

#[tokio::test]
async fn topics_lost_by_the_broker_are_created_again_without_a_restart() {
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
    let prefix = format!("fprecover{:08x}.", rand::random::<u32>());
    let vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_INGEST_MODE", "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS", brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.clone()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS", "2".into()),
    ];
    let extra: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let s = Server::start_with(&db, &extra).await;
    s.project("rc").await;
    let lid = s.launch("rc", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("/api/v1/projects/rc/launches/{lid}");

    post_results(&s, &base, 0, 40).await;
    wait_for_attempts(&db, 40).await;

    let admin = {
        let mut v = vars.clone();
        v.retain(|(k, _)| *k != "FASTPROOF_KAFKA_CREATE_TOPICS");
        let get = move |k: &str| v.iter().find(|(n, _)| *n == k).map(|(_, x)| x.clone());
        let cfg = fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&get))
            .unwrap()
            .unwrap();
        fastproof_stream::Kafka::connect(&cfg, None).await.unwrap()
    };
    let topics: Vec<String> = ["ingest.results", "ingest.logs", "ingest.uploads", "events"]
        .iter()
        .map(|t| admin.topic(t))
        .collect();
    let results = admin.topic("ingest.results");
    assert_eq!(end_offsets(&admin, &results).await.unwrap(), 40);
    for t in &topics {
        admin.delete_topic(t).await.unwrap();
    }
    // The server's live log readers may create the topics again at once; the results topic is
    // gone or empty either way.
    let deadline = Instant::now() + Duration::from_secs(30);
    while end_offsets(&admin, &results).await.is_ok_and(|n| n == 40) {
        assert!(Instant::now() < deadline, "{results} was not deleted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // The next request creates the topics again; the writer, whose committed offsets point
    // past the end of the new logs, reads them from the start.
    post_results(&s, &base, 40, 25).await;
    wait_for_attempts(&db, 65).await;
    for t in &topics {
        assert_eq!(admin.partition_count(t).await.unwrap(), 2, "{t}");
    }
    let metrics = s
        .http
        .get(format!("{}/metrics", s.ops))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("fastproof_kafka_topic_recoveries_total"),
        "the recovery is counted"
    );

    for t in &topics {
        let _ = admin.delete_topic(t).await;
    }
}
