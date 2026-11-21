//! Regression tests of the streaming security fixes: analytics sinks drop deleted launches and
//! projects and search hides them at once (A1, D2), ClickHouse query parameters keep
//! backslashes (B13), the `events` topic needs signatures (B14), and records of trusted
//! producers are validated and charged like gateway input (B14).
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` plus `FASTPROOF_TEST_KAFKA_BROKERS`,
//! `FASTPROOF_TEST_CLICKHOUSE_URL` and `FASTPROOF_TEST_OPENSEARCH_URL`; without their services
//! the tests pass without doing anything.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn kafka_vars(prefix: &str) -> Option<Vec<(String, String)>> {
    let brokers = env("FASTPROOF_TEST_KAFKA_BROKERS")?;
    Some(vec![
        ("FASTPROOF_INGEST_MODE".into(), "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS".into(), brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX".into(), prefix.into()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS".into(), "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS".into(), "2".into()),
    ])
}

fn sink_vars(ch_db: &str, os_prefix: &str) -> Option<Vec<(String, String)>> {
    let ch = env("FASTPROOF_TEST_CLICKHOUSE_URL")?;
    let os = env("FASTPROOF_TEST_OPENSEARCH_URL")?;
    let mut v = vec![
        (
            "FASTPROOF_SINKS".to_string(),
            "clickhouse,opensearch".to_string(),
        ),
        ("FASTPROOF_CLICKHOUSE_URL".into(), ch),
        ("FASTPROOF_CLICKHOUSE_DATABASE".into(), ch_db.into()),
        ("FASTPROOF_OPENSEARCH_URL".into(), os),
        ("FASTPROOF_OPENSEARCH_INDEX_PREFIX".into(), os_prefix.into()),
        ("FASTPROOF_OPENSEARCH_REPLICAS".into(), "0".into()),
        ("FASTPROOF_SINK_FLUSH_INTERVAL_MS".into(), "200".into()),
    ];
    for (k, t) in [
        (
            "FASTPROOF_TEST_CLICKHOUSE_USER",
            "FASTPROOF_CLICKHOUSE_USER",
        ),
        (
            "FASTPROOF_TEST_CLICKHOUSE_PASSWORD_FILE",
            "FASTPROOF_CLICKHOUSE_PASSWORD_FILE",
        ),
        (
            "FASTPROOF_TEST_OPENSEARCH_USER",
            "FASTPROOF_OPENSEARCH_USER",
        ),
        (
            "FASTPROOF_TEST_OPENSEARCH_PASSWORD_FILE",
            "FASTPROOF_OPENSEARCH_PASSWORD_FILE",
        ),
        (
            "FASTPROOF_TEST_OPENSEARCH_CA_FILE",
            "FASTPROOF_OPENSEARCH_CA_FILE",
        ),
    ] {
        if let Some(v2) = env(k) {
            v.push((t.into(), v2));
        }
    }
    Some(v)
}

async fn start(db: &TestDb, vars: &[(String, String)], extra: &[(&str, &str)]) -> Server {
    let mut all: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    all.extend_from_slice(extra);
    Server::start_with(db, &all).await
}

async fn wait_for<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn line(result: &str, seq: u64, message: &str) -> Value {
    json!({"resultId": result, "attempt": 0, "seq": seq, "ts": chrono::Utc::now().timestamp_millis(),
        "level": "info", "stream": "stdout", "message": message})
}

async fn post_lines(s: &Server, project: &str, lid: &str, lines: Vec<Value>) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/logs"),
            ADMIN_TOKEN,
            Value::Array(lines),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
}

async fn hits(s: &Server, project: &str, q: &str) -> Vec<Value> {
    let q: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/{project}/search/logs?q={q}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    h["items"].as_array().cloned().unwrap_or_default()
}

async fn os_count(os: &fastproof_stream::http::HttpBackend, prefix: &str, project: &str) -> u64 {
    let Ok(rb) = os.request(
        Method::POST,
        &format!(
            "/{prefix}logs-*,{prefix}results-*,{prefix}attempts-*/_count?ignore_unavailable=true"
        ),
    ) else {
        return u64::MAX;
    };
    let Ok(r) = rb
        .json(&json!({"query": {"term": {"project": project}}}))
        .send()
        .await
    else {
        return u64::MAX;
    };
    let v: Value = r.json().await.unwrap_or(Value::Null);
    v["count"].as_u64().unwrap_or(u64::MAX)
}

async fn sinks_drop_deleted_data(kafka: bool) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let tag = format!("{:08x}", rand::random::<u32>());
    let ch_db = format!("fpsec_{tag}");
    let os_prefix = format!("fpsec-{tag}-");
    let Some(mut vars) = sink_vars(&ch_db, &os_prefix) else {
        eprintln!(
            "FASTPROOF_TEST_CLICKHOUSE_URL or FASTPROOF_TEST_OPENSEARCH_URL is not set: skipped"
        );
        return;
    };
    let kprefix = format!("fpsec{tag}.");
    if kafka {
        let Some(k) = kafka_vars(&kprefix) else {
            eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
            return;
        };
        vars.extend(k);
    }
    let lookup = |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let ch = fastproof_stream::clickhouse::ClickHouse::new(
        fastproof_stream::ClickHouseConfig::from_vars(&fastproof_stream::Vars(&lookup))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let os_cfg = fastproof_stream::OpenSearchConfig::from_vars(&fastproof_stream::Vars(&lookup))
        .unwrap()
        .unwrap();
    let os =
        fastproof_stream::http::HttpBackend::new(os_cfg.endpoint, "FASTPROOF_OPENSEARCH").unwrap();
    let s = start(&db, &vars, &[("FASTPROOF_SEARCH_BACKEND", "clickhouse")]).await;
    s.project("del").await;
    let l1 = s.launch("del", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let l2 = s.launch("del", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    post_lines(
        &s,
        "del",
        &l1,
        (0..5)
            .map(|i| line("a", i, &format!("needle one {i}")))
            .collect(),
    )
    .await;
    post_lines(
        &s,
        "del",
        &l2,
        vec![
            line("b", 0, "needle two"),
            line("b", 1, r"copied to C:\temp\new needle"),
        ],
    )
    .await;
    let ch_rows = |sql: String| {
        let ch = ch.clone();
        async move {
            ch.exec(&sql, &[], None)
                .await
                .ok()
                .and_then(|t| t.trim().parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        }
    };
    let logs_of = |l: &str| format!("SELECT count() FROM {ch_db}.logs WHERE launch_id = '{l}'");
    wait_for("ClickHouse logs", 120, || async {
        ch_rows(logs_of(&l1)).await == 5 && ch_rows(logs_of(&l2)).await == 2
    })
    .await;
    wait_for("OpenSearch logs", 120, || async {
        os_count(&os, &os_prefix, "del").await == 7
    })
    .await;
    assert_eq!(hits(&s, "del", "needle").await.len(), 7);
    let bs = hits(&s, "del", r"C:\temp").await;
    assert_eq!(
        bs.len(),
        1,
        "a backslash in the query reaches ClickHouse as written"
    );

    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/del/launches/{l1}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success());
    let after = hits(&s, "del", "needle").await;
    assert_eq!(
        after.len(),
        2,
        "hits of a deleted launch are hidden at once"
    );
    assert!(after.iter().all(|h| h["launchId"] == l2.as_str()));
    wait_for("ClickHouse drops the deleted launch", 120, || async {
        ch_rows(logs_of(&l1)).await == 0
            && ch_rows(format!(
                "SELECT count() FROM {ch_db}.results WHERE launch_id = '{l1}'"
            ))
            .await
                == 0
    })
    .await;
    wait_for("OpenSearch drops the deleted launch", 120, || async {
        os_count(&os, &os_prefix, "del").await == 2
    })
    .await;
    assert_eq!(ch_rows(logs_of(&l2)).await, 2, "other launches stay");

    if kafka {
        let k = s.running.as_ref().unwrap().state.kafka.clone().unwrap();
        let unsigned = fastproof_stream::Kafka::connect(k.config(), None)
            .await
            .unwrap();
        let ev = json!({"v": 1, "id": 1, "type": "launch.deleted", "project": "del",
            "payload": {"id": l2, "launchId": l2}, "createdAt": chrono::Utc::now()});
        unsigned
            .produce(
                &k.topic(fastproof_stream::kafka::TOPIC_EVENTS),
                vec![fastproof_stream::kafka::Outgoing {
                    key: l2.as_bytes().to_vec(),
                    value: serde_json::to_vec(&ev).unwrap(),
                }],
            )
            .await
            .unwrap();
        let events_topic = k.topic(fastproof_stream::kafka::TOPIC_EVENTS);
        wait_for("the unsigned event is rejected", 60, || async {
            common::prometheus().render().lines().any(|l| {
                l.starts_with("fastproof_kafka_rejected_total")
                    && l.contains("reason=\"unsigned\"")
                    && l.contains(&events_topic)
            })
        })
        .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            ch_rows(logs_of(&l2)).await,
            2,
            "an unsigned event deletes nothing"
        );
    }

    let (st, _) = s
        .call(
            Method::DELETE,
            "/api/v1/projects/del",
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    wait_for("ClickHouse drops the deleted project", 120, || async {
        ch_rows(format!(
            "SELECT count() FROM {ch_db}.logs WHERE project = 'del'"
        ))
        .await
            == 0
            && ch_rows(format!(
                "SELECT count() FROM {ch_db}.results WHERE project = 'del'"
            ))
            .await
                == 0
    })
    .await;
    wait_for("OpenSearch drops the deleted project", 120, || async {
        os_count(&os, &os_prefix, "del").await == 0
    })
    .await;
    let _ = ch
        .exec(&format!("DROP DATABASE IF EXISTS {ch_db}"), &[], None)
        .await;
    let _ = os
        .request(Method::DELETE, &format!("/{os_prefix}*"))
        .unwrap()
        .send()
        .await;
    let _ = os
        .request(Method::DELETE, &format!("/_data_stream/{os_prefix}*"))
        .unwrap()
        .send()
        .await;
}

#[tokio::test]
async fn sinks_drop_deleted_data_in_direct_mode() {
    sinks_drop_deleted_data(false).await;
}

#[tokio::test]
async fn sinks_drop_deleted_data_in_kafka_mode() {
    sinks_drop_deleted_data(true).await;
}

/// B14: with trusted producers, unsigned log records are validated and charged to the
/// launch's log budget, and unsigned pre-parsed results are refused.
#[tokio::test]
async fn trusted_producer_records_are_checked() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let tag = format!("{:08x}", rand::random::<u32>());
    let Some(vars) = kafka_vars(&format!("fptrust{tag}.")) else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let s = start(
        &db,
        &vars,
        &[
            ("FASTPROOF_KAFKA_TRUSTED_PRODUCERS", "true"),
            ("FASTPROOF_LOG_MAX_LINE_BYTES", "1024"),
            ("FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH", "2048"),
        ],
    )
    .await;
    s.project("tp").await;
    let lid: uuid::Uuid = s.launch("tp", None).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let k = s.running.as_ref().unwrap().state.kafka.clone().unwrap();
    let producer = fastproof_stream::Kafka::connect(k.config(), None)
        .await
        .unwrap();
    let key = lid.to_string().into_bytes();
    let rec = |seq: u64, n: usize| {
        fastproof_stream::kafka::Outgoing {
        key: key.clone(),
        value: serde_json::to_vec(&json!({"v": 1, "project": "tp", "launchId": lid, "resultId": "r",
            "attempt": 0, "seq": seq, "ts": 1, "level": "info", "stream": "stdout", "message": "m".repeat(n)}))
        .unwrap(),
    }
    };
    producer
        .produce(
            &k.topic(fastproof_stream::kafka::TOPIC_LOGS),
            vec![rec(0, 100), rec(1, 4000)],
        )
        .await
        .unwrap();
    let parsed = fastproof_stream::messages::ResultRecord {
        v: 1,
        project: "tp".into(),
        launch_id: lid,
        result: None,
        parsed: Some(Box::default()),
        client_key: Some("forged".into()),
        upload_id: None,
    };
    let good = fastproof_stream::messages::ResultRecord {
        v: 1,
        project: "tp".into(),
        launch_id: lid,
        result: Some(
            serde_json::from_value(json!({"id": "ok", "name": "ok", "status": "passed"})).unwrap(),
        ),
        parsed: None,
        client_key: None,
        upload_id: None,
    };
    producer
        .produce(
            &k.topic(fastproof_stream::kafka::TOPIC_RESULTS),
            vec![
                fastproof_stream::kafka::Outgoing {
                    key: key.clone(),
                    value: serde_json::to_vec(&parsed).unwrap(),
                },
                fastproof_stream::kafka::Outgoing {
                    key: key.clone(),
                    value: serde_json::to_vec(&good).unwrap(),
                },
            ],
        )
        .await
        .unwrap();
    wait_for("the trusted result", 60, || async {
        s.results("tp", &lid.to_string(), "").await.len() == 1
    })
    .await;
    let names: Vec<String> = s
        .results("tp", &lid.to_string(), "")
        .await
        .iter()
        .map(|r| r["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, vec!["ok"], "the unsigned parsed record was refused");
    let logs = format!("/api/v1/projects/tp/launches/{lid}/logs");
    wait_for("the valid trusted line", 60, || async {
        let (_, b) = s.get(&logs, ADMIN_TOKEN).await;
        b["items"].as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    let (_, b) = s.get(&logs, ADMIN_TOKEN).await;
    let seqs: Vec<u64> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![0], "the oversized line was refused");
    producer
        .produce(
            &k.topic(fastproof_stream::kafka::TOPIC_LOGS),
            (2..8).map(|i| rec(i, 900)).collect(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let used: i64 = sqlx::query_scalar("select log_bytes from launches where id = $1")
        .bind(lid)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(used <= 2048, "{used} bytes charged");
    let (_, b) = s.get(&logs, ADMIN_TOKEN).await;
    assert!(
        b["items"].as_array().unwrap().len() < 7,
        "lines beyond the launch budget were refused: {b}"
    );
}
