//! Streaming mode: live logs (direct mode, PostgreSQL only), Kafka ingest end to end, the
//! writer's exactly-once commit, partitions shared by two writers, SASL/SCRAM over TLS, the
//! ClickHouse and OpenSearch sinks with search, and the bench.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` plus `FASTPROOF_TEST_KAFKA_BROKERS`,
//! `FASTPROOF_TEST_KAFKA_SASL_BROKERS` (with `_CA_FILE`, `_USERNAME`, `_PASSWORD_FILE`),
//! `FASTPROOF_TEST_CLICKHOUSE_URL` and `FASTPROOF_TEST_OPENSEARCH_URL`; tests without their
//! services pass without doing anything.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb, fixture, tar_gz};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn prefix() -> String {
    format!("fptest{:08x}.", rand::random::<u32>())
}

/// Server variables of Kafka mode on the plaintext test broker.
fn kafka_vars(prefix: &str) -> Option<Vec<(String, String)>> {
    let brokers = env("FASTPROOF_TEST_KAFKA_BROKERS")?;
    Some(vec![
        ("FASTPROOF_INGEST_MODE".into(), "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS".into(), brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX".into(), prefix.into()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS".into(), "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS".into(), "4".into()),
    ])
}

async fn start(db: &TestDb, vars: &[(String, String)], extra: &[(&str, &str)]) -> Server {
    let mut all: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    all.extend_from_slice(extra);
    Server::start_with(db, &all).await
}

async fn post_ndjson(s: &Server, path: &str, lines: &[Value]) -> (StatusCode, Value) {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let r = s
        .req(Method::POST, path, Some(ADMIN_TOKEN))
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

fn line(result: &str, attempt: u32, seq: u64, level: &str, message: &str) -> Value {
    json!({"resultId": result, "attempt": attempt, "seq": seq, "ts": 1_795_000_000_000i64 + seq as i64,
        "level": level, "stream": "stdout", "message": message})
}

async fn wait_for<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Reads SSE events until `until` says stop or the time runs out.
async fn sse(
    s: &Server,
    path: &str,
    secs: u64,
    until: impl Fn(&[(String, Value)]) -> bool,
) -> Vec<(String, Value)> {
    let r = s
        .req(Method::GET, path, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let mut body = r.bytes_stream();
    let mut buf = String::new();
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let chunk = tokio::select! {
            c = body.next() => c,
            _ = tokio::time::sleep_until(deadline) => None,
        };
        let Some(Ok(c)) = chunk else { break };
        buf.push_str(&String::from_utf8_lossy(&c));
        while let Some(i) = buf.find("\n\n") {
            let block: String = buf.drain(..i + 2).collect();
            let mut name = String::new();
            let mut data = String::new();
            for l in block.lines() {
                if let Some(v) = l.strip_prefix("event:") {
                    name = v.trim().to_string();
                } else if let Some(v) = l.strip_prefix("data:") {
                    data.push_str(v.trim());
                }
            }
            if !name.is_empty() {
                events.push((name, serde_json::from_str(&data).unwrap_or(Value::Null)));
            }
        }
        if until(&events) {
            break;
        }
    }
    events
}

fn log_lines(events: &[(String, Value)]) -> Vec<Value> {
    events
        .iter()
        .filter(|(n, _)| n == "log")
        .flat_map(|(_, v)| v["lines"].as_array().cloned().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn direct_mode_live_logs_running_results_and_search() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("live").await;
    let launch = s.launch("live", None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let base = format!("/api/v1/projects/live/launches/{lid}");

    // A result opened as running and completed later by the same client id.
    let (st, b) = s
        .post(&format!("{base}/results"), ADMIN_TOKEN,
            json!({"id": "r1", "name": "pays", "fullName": "shop.Checkout#pays", "status": "running", "start": 1}))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let rs = s.results("live", &lid, "").await;
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["status"], "running");
    assert_eq!(s.results("live", &lid, "&status=running").await.len(), 1);
    let (st, b) = s
        .post(&format!("{base}/results"), ADMIN_TOKEN,
            json!({"id": "r1", "name": "pays", "fullName": "shop.Checkout#pays", "status": "failed", "start": 1, "stop": 9,
                "error": {"message": "Timeout 3000 ms waiting for #pay"}}))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["accepted"], 1, "{b}");
    let rs = s.results("live", &lid, "").await;
    assert_eq!(
        (
            rs.len(),
            rs[0]["status"].as_str(),
            rs[0]["attemptCount"].as_i64()
        ),
        (1, Some("failed"), Some(1))
    );
    let (_, b) = s
        .post(&format!("{base}/results"), ADMIN_TOKEN,
            json!({"id": "r1", "name": "pays", "fullName": "shop.Checkout#pays", "status": "running"}))
        .await;
    assert_eq!(b["duplicates"], 1, "a finished result is not reopened: {b}");

    // Live stream: subscribe first, then send lines.
    let s2 = &s;
    let stream_path = format!("{base}/logs/stream?resultId=r1");
    let reader = tokio::spawn({
        let base_url = s.base.clone();
        let path = stream_path.clone();
        async move {
            let srv = common::Server::client_only(&base_url);
            sse(&srv, &path, 20, |ev| log_lines(ev).len() >= 300).await
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let lines: Vec<Value> = (0..300)
        .map(|i| {
            line(
                "r1",
                0,
                i,
                if i % 50 == 49 { "error" } else { "info" },
                &format!("line {i} of checkout"),
            )
        })
        .collect();
    let other: Vec<Value> = (0..5)
        .map(|i| line("r2", 0, i, "info", "other result"))
        .collect();
    let (st, b) = post_ndjson(s2, &format!("{base}/logs"), &lines[..150]).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["accepted"], 150);
    post_ndjson(s2, &format!("{base}/logs"), &other).await;
    let (_, b) = post_ndjson(s2, &format!("{base}/logs"), &lines[100..]).await;
    assert_eq!(
        (b["accepted"].as_u64(), b["duplicates"].as_u64()),
        (Some(150), Some(50)),
        "{b}"
    );
    let events = reader.await.unwrap();
    assert_eq!(events[0].0, "ready");
    let got = log_lines(&events);
    let seqs: Vec<u64> = got.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert_eq!(
        seqs,
        (0..300).collect::<Vec<_>>(),
        "live lines arrive once and in order"
    );
    assert!(got.iter().all(|l| l["resultId"] == "r1"));

    // Stored logs: paging backwards, filters.
    let (st, page) = s
        .get(&format!("{base}/logs?resultId=r1&limit=100"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{page}");
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 100);
    assert_eq!(items[0]["seq"], 200);
    assert_eq!(items[99]["seq"], 299);
    let next = page["next"].as_str().unwrap();
    let (_, page2) = s
        .get(
            &format!("{base}/logs?resultId=r1&limit=100&before={next}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(page2["items"][99]["seq"], 199);
    let (_, errs) = s
        .get(&format!("{base}/logs?level=error"), ADMIN_TOKEN)
        .await;
    assert_eq!(errs["items"].as_array().unwrap().len(), 6);
    let (_, q) = s
        .get(&format!("{base}/logs?q=LINE%2042%20OF"), ADMIN_TOKEN)
        .await;
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
    let (st, _) = s.get(&format!("{base}/logs?level=loud"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Limits and validation; an empty body is the reporters' feature probe.
    let (st, _) = post_ndjson(&s, &format!("{base}/logs"), &[]).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let r = s
        .req(Method::POST, &format!("{base}/logs"), Some(ADMIN_TOKEN))
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let (_, b) = post_ndjson(
        &s,
        &format!("{base}/logs"),
        &[
            json!({"resultId": "", "attempt": 0, "seq": 1, "ts": 1,
        "level": "info", "stream": "stdout", "message": "x"}),
            json!({"bogus": 1}),
        ],
    )
    .await;
    assert_eq!(b["rejected"].as_array().unwrap().len(), 2, "{b}");
    let r = s
        .req(Method::POST, &format!("{base}/logs"), Some(ADMIN_TOKEN))
        .header("content-type", "text/plain")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let viewer = s.token("viewer", "viewer", &["live"]).await;
    let r = s
        .req(Method::POST, &format!("{base}/logs"), Some(&viewer))
        .header("content-type", "application/json")
        .body("[]")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = s
        .req(Method::GET, &format!("{base}/logs/stream"), None)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    s.project("other").await;
    let outsider = s.token("outsider", "admin", &["other"]).await;
    let r = s
        .req(Method::GET, &format!("{base}/logs/stream"), Some(&outsider))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // Search and similar failures in PostgreSQL.
    let (st, hits) = s
        .get(
            "/api/v1/projects/live/search/logs?q=of%20checkout&level=error",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    assert_eq!(hits["items"].as_array().unwrap().len(), 6, "{hits}");
    assert_eq!(hits["items"][0]["launchId"], lid.as_str());
    let launch2 = s.launch("live", None).await;
    let l2 = launch2["id"].as_str().unwrap();
    s.post(&format!("/api/v1/projects/live/launches/{l2}/results"), ADMIN_TOKEN, json!([
        {"id": "a", "name": "pays", "fullName": "shop.Checkout#pays", "status": "failed", "error": {"message": "Timeout 5000 ms waiting for #pay"}},
        {"id": "b", "name": "cart", "fullName": "shop.Cart#adds", "status": "broken", "error": {"message": "Timeout 5000 ms waiting for #cart button"}},
        {"id": "c", "name": "login", "fullName": "shop.Login#ok", "status": "failed", "error": {"message": "NullPointerException in LoginService"}}
    ])).await;
    let rid = rs[0]["id"].as_str().unwrap();
    let (st, sim) = s
        .get(
            &format!("/api/v1/projects/live/results/{rid}/similar"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{sim}");
    let items = sim["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{sim}");
    assert_eq!(items[0]["score"], 1.0);
    assert_eq!(items[0]["name"], "pays");
    assert!(items[1]["score"].as_f64().unwrap() < 1.0);
}

/// The listeners subscribe before `start` returns: a notification sent right after it reaches
/// the live hubs (it used to be lost while the listeners were still connecting, which is slow
/// with SCRAM authentication).
#[tokio::test]
async fn listeners_are_subscribed_when_start_returns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let state = &s.running.as_ref().unwrap().state;
    let launch = uuid::Uuid::now_v7();
    let logs = state.logs.subscribe("early", launch, |_| true);
    let mut live = state.live.subscribe();
    let notice = json!({"project": "early", "launchId": launch, "chunkId": 1,
        "lines": [line("r", 0, 7, "info", "first line")]});
    let event = json!({"project": "early", "type": "launch.updated", "data": {"id": launch}});
    for (channel, payload) in [
        (fastproof_store::logs::LOGS_CHANNEL, notice),
        (fastproof_store::LIVE_CHANNEL, event),
    ] {
        sqlx::query("select pg_notify($1, $2)")
            .bind(channel)
            .bind(payload.to_string())
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let ev = tokio::time::timeout(Duration::from_secs(10), live.recv())
        .await
        .expect("the live event arrives")
        .unwrap();
    assert_eq!(ev.project, "early");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (dropped, lines) = logs.sub.take(10, 1 << 20);
        assert_eq!(dropped, 0);
        if let Some(l) = lines.first() {
            assert_eq!((l.seq, l.message.as_str()), (7, "first line"));
            break;
        }
        assert!(Instant::now() < deadline, "the log line arrives");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn sse_drops_oldest_lines_with_a_gap_event() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_LOG_MAX_LINES_PER_REQUEST", "10000")]).await;
    s.project("gap").await;
    let lid = s.launch("gap", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("/api/v1/projects/gap/launches/{lid}");
    let r = s
        .req(
            Method::GET,
            &format!("{base}/logs/stream"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let mut body = r.bytes_stream();
    let _ready = body.next().await;
    // 9000 lines commit at once and reach the hub as one burst, faster than 20 events of 500
    // lines per second drain them: the subscriber keeps the newest 2000 and reports the rest
    // as a gap.
    let lines: Vec<Value> = (0..9000).map(|i| line("r", 0, i, "info", "x")).collect();
    let (st, b) = post_ndjson(&s, &format!("{base}/logs"), &lines).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut events: Vec<(String, Value)> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let c = tokio::select! { c = body.next() => c, _ = tokio::time::sleep(Duration::from_secs(2)) => None };
        let Some(Ok(c)) = c else { break };
        buf.push_str(&String::from_utf8_lossy(&c));
        while let Some(i) = buf.find("\n\n") {
            let block: String = buf.drain(..i + 2).collect();
            let name = block
                .lines()
                .find_map(|l| l.strip_prefix("event:"))
                .unwrap_or("")
                .trim()
                .to_string();
            let data: String = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            events.push((name, serde_json::from_str(&data).unwrap_or(Value::Null)));
        }
        if log_lines(&events).last().is_some_and(|l| l["seq"] == 8999) {
            break;
        }
    }
    let gaps: u64 = events
        .iter()
        .filter(|(n, _)| n == "gap")
        .map(|(_, v)| v["skipped"].as_u64().unwrap())
        .sum();
    let lines = log_lines(&events);
    assert!(gaps > 0, "a gap event reports dropped lines");
    assert_eq!(
        lines.last().unwrap()["seq"],
        8999,
        "the newest lines survive"
    );
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "order is kept");
    assert!(
        lines.len() as u64 + gaps >= 9000 - 2000,
        "{} lines, {gaps} dropped",
        lines.len()
    );
}

/// Rows of a table in the test database.
async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn kafka_mode_end_to_end() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let p = prefix();
    let Some(vars) = kafka_vars(&p) else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let s = start(&db, &vars, &[]).await;
    s.project("kf").await;
    let lid = s.launch("kf", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("/api/v1/projects/kf/launches/{lid}");

    // Results API and logs through Kafka.
    let results: Vec<Value> = (0..50)
        .map(|i| json!({"id": format!("t{i}"), "name": format!("case {i}"), "fullName": format!("k.Suite#case{i}"),
            "status": if i % 10 == 0 { "failed" } else { "passed" }}))
        .collect();
    let (st, b) = post_ndjson(&s, &format!("{base}/results"), &results).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["accepted"], 50);
    let reader = tokio::spawn({
        let base_url = s.base.clone();
        let path = format!("{base}/logs/stream");
        async move {
            sse(&common::Server::client_only(&base_url), &path, 30, |ev| {
                log_lines(ev).len() >= 100
            })
            .await
        }
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let lines: Vec<Value> = (0..100)
        .map(|i| line("t1", 0, i, "info", &format!("kafka line {i}")))
        .collect();
    let (st, b) = post_ndjson(&s, &format!("{base}/logs"), &lines).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let live = log_lines(&reader.await.unwrap());
    assert_eq!(
        live.len(),
        100,
        "live lines through the per-replica Kafka reader"
    );
    assert!(
        live.windows(2)
            .all(|w| w[0]["seq"].as_u64() < w[1]["seq"].as_u64())
    );

    // An archive: stored in blob storage, parsed by the ingest role, written by the writer.
    let up = s
        .ingest(
            "kf",
            &lid,
            tar_gz(&fixture("pytest/allure-results"), &|_| true),
            "",
        )
        .await;
    assert_eq!(up["status"], "done", "{up}");
    let expected = up["counts"]["results"].as_u64().unwrap();
    wait_for("results in PostgreSQL", 60, || async {
        count(&db, "select count(*) from attempts").await as u64 >= 50 + expected
    })
    .await;
    wait_for("stored logs", 30, || async {
        let (_, p) = s
            .get(&format!("{base}/logs?resultId=t1"), ADMIN_TOKEN)
            .await;
        p["items"].as_array().map_or(0, |a| a.len()) == 100
    })
    .await;

    // The same archive in direct mode gives the same test identities.
    let db2 = TestDb::new().await.unwrap();
    let d = Server::start(&db2).await;
    d.project("kf").await;
    let l2 = d.launch("kf", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    d.ingest(
        "kf",
        &l2,
        tar_gz(&fixture("pytest/allure-results"), &|_| true),
        "",
    )
    .await;
    let ids = |pool: sqlx::PgPool| async move {
        let mut v: Vec<String> = sqlx::query_scalar(
            "select test_case_hash from test_cases where full_name not like 'k.Suite#%'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        v.sort();
        v
    };
    assert_eq!(ids(db.pool.clone()).await, ids(db2.pool.clone()).await);

    // Records not signed by the gateway are dropped unless producers are trusted.
    let kafka = fastproof_stream::Kafka::connect(
        &fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&|k| {
            vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
        }))
        .unwrap()
        .unwrap(),
        None,
    )
    .await
    .unwrap();
    let rec = json!({"v": 1, "project": "kf", "launchId": lid, "result": {"id": "forged", "name": "forged", "status": "passed"}});
    kafka
        .produce(
            &format!("{p}ingest.results"),
            vec![fastproof_stream::kafka::Outgoing {
                key: lid.as_bytes().to_vec(),
                value: serde_json::to_vec(&rec).unwrap(),
            }],
        )
        .await
        .unwrap();
    let (_, b) = post_ndjson(
        &s,
        &format!("{base}/results"),
        &[json!({"id": "after", "name": "after", "status": "passed"})],
    )
    .await;
    assert_eq!(b["accepted"], 1);
    wait_for("the record after the forged one", 30, || async {
        count(
            &db,
            "select count(*) from attempts where client_key = 'r:after'",
        )
        .await
            == 1
    })
    .await;
    assert_eq!(
        count(
            &db,
            "select count(*) from attempts where client_key = 'r:forged'"
        )
        .await,
        0
    );

    // Offsets are committed and lag is zero.
    wait_for("offsets at the end", 30, || async {
        count(&db, "select count(*) from kafka_offsets where group_name = 'fastproof-writer' and next_offset <> high_watermark").await == 0
    })
    .await;
    let (_, m) = s.call(Method::GET, "/metrics", None, None).await;
    let _ = m;
    let metrics = reqwest::get(format!("{}/metrics", s.ops))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("fastproof_kafka_lag"),
        "lag metrics are exported"
    );
}

#[tokio::test]
async fn kafka_writer_commits_exactly_once_across_restarts_and_shares_partitions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let p = prefix();
    let Some(vars) = kafka_vars(&p) else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    // The gateway alone: records wait in Kafka.
    let api = start(&db, &vars, &[("FASTPROOF_ROLES", "api,scheduler")]).await;
    api.project("eo").await;
    let mut launches = Vec::new();
    for _ in 0..8 {
        launches.push(
            api.launch("eo", None).await["id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let per_launch = 400;
    for l in &launches {
        for chunk in 0..4 {
            let rs: Vec<Value> = (0..per_launch / 4)
                .map(|i| {
                    let n = chunk * (per_launch / 4) + i;
                    json!({"id": format!("t{n}"), "name": format!("case {n}"), "fullName": format!("eo.S#c{n}"), "status": "passed"})
                })
                .collect();
            let (st, b) = post_ndjson(
                &api,
                &format!("/api/v1/projects/eo/launches/{l}/results"),
                &rs,
            )
            .await;
            assert_eq!(st, StatusCode::ACCEPTED, "{b}");
        }
        // Producer retries: the same results again.
        let again: Vec<Value> = (0..20).map(|n| json!({"id": format!("t{n}"), "name": format!("case {n}"), "fullName": format!("eo.S#c{n}"), "status": "passed"})).collect();
        post_ndjson(
            &api,
            &format!("/api/v1/projects/eo/launches/{l}/results"),
            &again,
        )
        .await;
    }
    let total = (launches.len() * per_launch) as i64;
    assert_eq!(count(&db, "select count(*) from attempts").await, 0);

    // A writer stops part way through; a second one continues from the committed offsets.
    let mut w1 = start(&db, &vars, &[("FASTPROOF_ROLES", "writer")]).await;
    wait_for("some progress", 60, || async {
        count(&db, "select count(*) from attempts").await > 0
    })
    .await;
    w1.stop().await;
    let after_stop = count(&db, "select count(*) from attempts").await;
    assert!(after_stop <= total);
    // Two writers share the partitions.
    let w2 = start(&db, &vars, &[("FASTPROOF_ROLES", "writer")]).await;
    let w3 = start(&db, &vars, &[("FASTPROOF_ROLES", "writer")]).await;
    wait_for("both writers hold partitions", 60, || async {
        count(&db, "select count(distinct owner) from kafka_partition_leases where group_name = 'fastproof-writer' and owner is not null and lease_until > now()").await == 2
    })
    .await;
    wait_for("every result", 120, || async {
        count(&db, "select count(*) from attempts").await >= total
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        count(&db, "select count(*) from attempts").await,
        total,
        "no attempt is written twice"
    );
    assert_eq!(count(&db, "select count(*) from results").await, total);
    assert_eq!(
        count(&db, "select count(*) from (select launch_id, client_key from attempts group by 1, 2 having count(*) > 1) d").await,
        0
    );
    for l in &launches {
        let v = api.launch_get("eo", l).await;
        assert_eq!(v["counts"]["total"], per_launch as i64);
    }
    // The assignment converges to 4 of the 8 partitions (2 topics x 4) per writer.
    wait_for("an even assignment", 30, || async {
        let leases: Vec<(String, i32)> = sqlx::query_as(
            "select owner, count(*)::int from kafka_partition_leases where group_name = 'fastproof-writer' \
             and owner is not null and lease_until > now() group by owner",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        leases.len() == 2 && leases.iter().all(|l| l.1 == 4)
    })
    .await;
    wait_for("offsets at the end", 30, || async {
        count(&db, "select count(*) from kafka_offsets where group_name = 'fastproof-writer' and next_offset <> high_watermark").await == 0
    })
    .await;
    drop(w2);
    drop(w3);
}

#[tokio::test]
async fn a_stale_member_cannot_commit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _s = Server::start(&db).await;
    let ctx = |member: &str| fastproof_stream::groups::PartitionCtx {
        pool: db.pool.clone(),
        group: "g".into(),
        member: member.into(),
        topic: "t".into(),
        partition: 0,
    };
    sqlx::query("insert into kafka_partition_leases values ('g', 't', 0, 'new', now() + interval '10 seconds')")
        .execute(&db.pool)
        .await
        .unwrap();
    let e = ctx("old").commit(5, None).await.unwrap_err();
    assert!(
        e.downcast_ref::<fastproof_stream::groups::LeaseLost>()
            .is_some(),
        "{e:#}"
    );
    ctx("new").commit(7, Some(9)).await.unwrap();
    assert_eq!(ctx("old").stored_offset().await.unwrap(), Some(7));
}

#[tokio::test]
async fn kafka_sasl_scram_over_tls_with_a_private_ca() {
    let (Some(brokers), Some(ca)) = (
        env("FASTPROOF_TEST_KAFKA_SASL_BROKERS"),
        env("FASTPROOF_TEST_KAFKA_CA_FILE"),
    ) else {
        eprintln!("FASTPROOF_TEST_KAFKA_SASL_BROKERS is not set: skipped");
        return;
    };
    fastproof_blob::install_crypto_provider();
    let p = prefix();
    let cases = [
        (
            "scram-sha-512",
            env("FASTPROOF_TEST_KAFKA_USERNAME"),
            env("FASTPROOF_TEST_KAFKA_PASSWORD_FILE"),
        ),
        (
            "scram-sha-256",
            env("FASTPROOF_TEST_KAFKA_USERNAME_256"),
            env("FASTPROOF_TEST_KAFKA_PASSWORD_FILE_256"),
        ),
    ];
    for (mech, user, pw) in cases {
        let (Some(user), Some(pw)) = (user, pw) else {
            continue;
        };
        let vars: Vec<(&str, String)> = vec![
            ("FASTPROOF_KAFKA_BROKERS", brokers.clone()),
            ("FASTPROOF_KAFKA_TLS", "true".into()),
            ("FASTPROOF_KAFKA_CA_FILE", ca.clone()),
            ("FASTPROOF_KAFKA_SASL_MECHANISM", mech.into()),
            ("FASTPROOF_KAFKA_USERNAME", user),
            ("FASTPROOF_KAFKA_PASSWORD_FILE", pw),
            ("FASTPROOF_KAFKA_TOPIC_PREFIX", p.clone()),
            ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
            ("FASTPROOF_KAFKA_PARTITIONS", "3".into()),
        ];
        let get = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let cfg = fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&get))
            .unwrap()
            .unwrap();
        let k = fastproof_stream::Kafka::connect(
            &cfg,
            Some(fastproof_stream::kafka::Signer::new(vec![1; 32])),
        )
        .await
        .unwrap_or_else(|e| panic!("{mech}: {e:#}"));
        k.ensure_topics().await.unwrap();
        let topic = k.topic("events");
        let key = b"0195b2c4-7d1e-7000-8000-00000000abcd".to_vec();
        let part = fastproof_stream::kafka::partition_for(&key, 3);
        let before = k
            .offset(&topic, part, fastproof_stream::kafka::Start::Latest)
            .await
            .unwrap();
        k.produce(
            &topic,
            vec![fastproof_stream::kafka::Outgoing {
                key: key.clone(),
                value: format!("{{\"via\":\"{mech}\"}}").into_bytes(),
            }],
        )
        .await
        .unwrap();
        let (recs, hwm, _) = k.fetch(&topic, part, before, 1 << 20, 1000).await.unwrap();
        assert_eq!(hwm, before + 1);
        let r = &recs.iter().find(|r| r.offset == before).unwrap().record;
        assert_eq!(
            r.value.as_deref(),
            Some(format!("{{\"via\":\"{mech}\"}}").as_bytes())
        );
        assert!(k.signer().unwrap().verify(&topic, r));
    }
    // A wrong password is refused.
    let bad = fastproof_stream::KafkaConfig {
        sasl: Some(fastproof_stream::config::Sasl {
            mechanism: fastproof_stream::config::SaslMechanism::ScramSha512,
            username: "fastproof".into(),
            password: fastproof_stream::config::Secret::new("wrong"),
        }),
        ..fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&|k| match k {
            "FASTPROOF_KAFKA_BROKERS" => Some(brokers.clone()),
            "FASTPROOF_KAFKA_TLS" => Some("true".into()),
            "FASTPROOF_KAFKA_CA_FILE" => Some(ca.clone()),
            _ => None,
        }))
        .unwrap()
        .unwrap()
    };
    assert!(fastproof_stream::Kafka::connect(&bad, None).await.is_err());
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

async fn sinks_fill_and_search(kafka: bool) {
    sinks_fill_and_search_with(kafka, false).await;
}

/// `batched`: log lines announced with format 2 events (`log.chunks`).
async fn sinks_fill_and_search_with(kafka: bool, batched: bool) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let tag = format!("{:08x}", rand::random::<u32>());
    let ch_db = format!("fptest_{tag}");
    let os_prefix = format!("fptest-{tag}-");
    let Some(mut vars) = sink_vars(&ch_db, &os_prefix) else {
        eprintln!(
            "FASTPROOF_TEST_CLICKHOUSE_URL or FASTPROOF_TEST_OPENSEARCH_URL is not set: skipped"
        );
        return;
    };
    if kafka {
        let Some(k) = kafka_vars(&prefix()) else {
            eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
            return;
        };
        vars.extend(k);
    }
    if batched {
        vars.push(("FASTPROOF_OUTBOX_LOG_EVENTS".into(), "batch".into()));
    }
    let s = start(&db, &vars, &[("FASTPROOF_SEARCH_BACKEND", "clickhouse")]).await;
    s.project("sk").await;
    let lid = s.launch("sk", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let base = format!("/api/v1/projects/sk/launches/{lid}");
    let results: Vec<Value> = (0..30)
        .map(|i| {
            let mut r = json!({"id": format!("t{i}"), "name": format!("case {i}"), "fullName": format!("sk.S#c{i}"),
                "status": if i % 3 == 0 { "failed" } else { "passed" },
                "steps": [{"name": "open", "status": "passed", "steps": [{"name": "click", "status": "passed"}]}]});
            if i % 3 == 0 {
                r["error"] = json!({"message": if i % 2 == 0 { format!("Connection refused to db-{i}") } else { format!("Assertion failed: total {i}") }});
            }
            r
        })
        .collect();
    let (st, b) = post_ndjson(&s, &format!("{base}/results"), &results).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let lines: Vec<Value> = (0..200)
        .map(|i| {
            line(
                &format!("t{}", i % 30),
                0,
                i,
                if i % 40 == 0 { "error" } else { "info" },
                &if i % 40 == 0 {
                    format!("ERROR upstream gateway timeout after {i} ms")
                } else {
                    format!("ordinary line {i}")
                },
            )
        })
        .collect();
    post_ndjson(&s, &format!("{base}/logs"), &lines).await;

    let ch = fastproof_stream::clickhouse::ClickHouse::new(
        fastproof_stream::ClickHouseConfig::from_vars(&fastproof_stream::Vars(&|k| {
            vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
        }))
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let ch_count = |table: &'static str| {
        let ch = ch.clone();
        let db = ch_db.clone();
        async move {
            ch.exec(
                &format!("SELECT count() FROM (SELECT * FROM {db}.{table} FINAL)"),
                &[],
                None,
            )
            .await
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0)
        }
    };
    wait_for("ClickHouse rows", 90, || async {
        ch_count("results").await == 30
            && ch_count("attempts").await == 30
            && ch_count("logs").await == 200
            && ch_count("steps").await == 60
    })
    .await;
    let events = |kind: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from events where type = $1")
                .bind(kind)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    if batched {
        assert_eq!(events("log.chunk").await, 0);
        assert!(events("log.chunks").await > 0);
    } else {
        assert_eq!(events("log.chunks").await, 0);
    }
    let rollup = ch
        .exec(
            &format!("SELECT uniqMerge(runs), uniqIfMerge(failed) FROM {ch_db}.series_daily"),
            &[],
            None,
        )
        .await
        .unwrap();
    assert_eq!(rollup.trim(), "30\t10", "series_daily rollup");

    let (st, hits) = s
        .get(
            "/api/v1/projects/sk/search/logs?q=gateway%20timeout",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    assert_eq!(hits["items"].as_array().unwrap().len(), 5, "{hits}");
    let rs = s.results("sk", &lid, "&status=failed").await;
    let refused = rs
        .iter()
        .find(|r| r["message"].as_str().unwrap_or("").contains("refused"))
        .unwrap();
    let rid = refused["id"].as_str().unwrap();
    let (st, sim) = s
        .get(
            &format!("/api/v1/projects/sk/results/{rid}/similar"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{sim}");
    let items = sim["items"].as_array().unwrap();
    assert!(!items.is_empty() && items.len() >= 4, "{sim}");
    assert!(
        items
            .iter()
            .all(|i| i["message"].as_str().unwrap().contains("refused")),
        "{sim}"
    );

    // The same data through OpenSearch (TLS and basic auth when configured).
    let o = start(
        &db,
        &vars,
        &[
            ("FASTPROOF_SEARCH_BACKEND", "opensearch"),
            ("FASTPROOF_ROLES", "api"),
            ("FASTPROOF_SINKS", ""),
        ],
    )
    .await;
    wait_for("OpenSearch documents", 90, || async {
        let (_, h) = o
            .get(
                "/api/v1/projects/sk/search/logs?q=gateway%20timeout",
                ADMIN_TOKEN,
            )
            .await;
        h["items"].as_array().map_or(0, |a| a.len()) == 5
    })
    .await;
    wait_for("OpenSearch results", 60, || async {
        let (_, sim) = o
            .get(
                &format!("/api/v1/projects/sk/results/{rid}/similar"),
                ADMIN_TOKEN,
            )
            .await;
        sim["items"].as_array().is_some_and(|a| a.len() >= 4)
    })
    .await;
    let (_, sim) = o
        .get(
            &format!("/api/v1/projects/sk/results/{rid}/similar"),
            ADMIN_TOKEN,
        )
        .await;
    assert!(
        sim["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["message"].as_str().unwrap().contains("refused")),
        "{sim}"
    );
    let (_, other) = o
        .get("/api/v1/projects/other/search/logs?q=gateway", ADMIN_TOKEN)
        .await;
    assert!(other["items"].is_null() || other["items"].as_array().unwrap().is_empty());

    // Sink positions are committed.
    wait_for("sink offsets", 30, || async {
        count(&db, "select count(distinct group_name) from kafka_offsets where group_name like 'fastproof-sink-%'").await == 2
    })
    .await;
    let _ = ch
        .exec(&format!("DROP DATABASE IF EXISTS {ch_db}"), &[], None)
        .await;
}

#[tokio::test]
async fn sinks_fill_clickhouse_and_opensearch_in_direct_mode() {
    sinks_fill_and_search(false).await;
}

#[tokio::test]
async fn sinks_fill_clickhouse_and_opensearch_in_kafka_mode() {
    sinks_fill_and_search(true).await;
}

#[tokio::test]
async fn sinks_take_batched_log_events_in_direct_mode() {
    sinks_fill_and_search_with(false, true).await;
}

#[tokio::test]
async fn sinks_take_batched_log_events_in_kafka_mode() {
    sinks_fill_and_search_with(true, true).await;
}

async fn bench(
    s: &Server,
    kafka_env: Option<&[(String, String)]>,
) -> fastproof_server::cli::bench::BenchReport {
    let dir = tempfile::tempdir().unwrap();
    let tok = dir.path().join("token");
    std::fs::write(&tok, ADMIN_TOKEN).unwrap();
    if let Some(vars) = kafka_env {
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }
    }
    let args = fastproof_server::cli::bench::BenchArgs {
        conn: fastproof_server::cli::Conn {
            url: s.base.clone(),
            token_file: Some(tok),
            project: "bench".into(),
        },
        launches: env("FASTPROOF_TEST_BENCH_LAUNCHES")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3),
        results: env("FASTPROOF_TEST_BENCH_RESULTS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(200),
        log_lines: env("FASTPROOF_TEST_BENCH_LOG_LINES")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
        parallel: 3,
        attachments_percent: 2,
        create_project: true,
        kafka: kafka_env.is_some(),
        wait_timeout: 300,
    };
    let r = fastproof_server::cli::bench::run(args).await.unwrap();
    eprintln!(
        "bench ({}): {}",
        if kafka_env.is_some() {
            "Kafka producer"
        } else {
            "HTTP"
        },
        serde_json::to_string(&r).unwrap()
    );
    r
}

#[tokio::test]
async fn bench_smoke_direct_and_kafka() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let r = bench(&s, None).await;
    assert!(r.results_sent >= 600 && r.log_lines_sent >= 6000, "{r:?}");
    assert!(count(&db, "select count(*) from results").await >= 600);
    let Some(db2) = TestDb::new().await else {
        return;
    };
    let Some(mut vars) = kafka_vars(&prefix()) else {
        return;
    };
    vars.push(("FASTPROOF_KAFKA_TRUSTED_PRODUCERS".into(), "true".into()));
    let k = start(&db2, &vars, &[]).await;
    let via_http = bench(&k, None).await;
    assert!(via_http.lag_seconds >= 0.0);
    let direct = bench(&k, Some(&vars)).await;
    assert!(direct.results_sent >= 600, "{direct:?}");
}
