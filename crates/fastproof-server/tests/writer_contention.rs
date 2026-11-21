//! Kafka writers persisting launches that share test identities: launches with the same tests
//! land on different partitions, two writers persist them while the gateway receives them,
//! and every launch reports its own share of the tests in its own order. No deadlock may happen (PostgreSQL's deadlock counter stays
//! put), and every result arrives exactly once.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_KAFKA_BROKERS`.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const LAUNCHES: usize = 48;
const TESTS: usize = 1000;
const CHUNK: usize = 50;
const PER_LAUNCH: usize = 500;

fn shuffled(seed: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..TESTS).collect();
    let mut s = 0x9e37_79b9_7f4a_7c15u64 ^ (seed as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn deadlocks(db: &TestDb) -> i64 {
    let _ = sqlx::query("select pg_stat_force_next_flush()")
        .execute(&db.pool)
        .await;
    count(
        db,
        "select deadlocks from pg_stat_database where datname = current_database()",
    )
    .await
}

#[tokio::test]
async fn writers_persist_launches_sharing_tests_without_deadlocks() {
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
    let prefix = format!("fpdl{:08x}.", rand::random::<u32>());
    let vars = [
        ("FASTPROOF_INGEST_MODE", "kafka"),
        ("FASTPROOF_KAFKA_BROKERS", brokers.as_str()),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.as_str()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true"),
        ("FASTPROOF_KAFKA_PARTITIONS", "6"),
        ("FASTPROOF_DATABASE_MAX_CONNECTIONS", "12"),
    ];
    let with = |roles: &'static str| {
        let mut v = vars.to_vec();
        v.push(("FASTPROOF_ROLES", roles));
        v
    };
    let api = Server::start_with(&db, &with("api,scheduler")).await;
    api.project("dl").await;
    let mut launches = Vec::new();
    for _ in 0..LAUNCHES {
        launches.push(
            api.launch("dl", None).await["id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let before = deadlocks(&db).await;
    let w1 = Server::start_with(&db, &with("writer")).await;
    let w2 = Server::start_with(&db, &with("writer")).await;
    let started = Instant::now();
    // Every launch reports its own share of the tests, in its own order, while the writers
    // run: their batches overlap partly, as in a CI farm running suites in parallel.
    let senders: Vec<_> = launches
        .iter()
        .enumerate()
        .map(|(li, l)| {
            let base = api.base.clone();
            let l = l.clone();
            tokio::spawn(async move {
                let client = Server::client_only(&base);
                let mine: Vec<usize> = shuffled(li).into_iter().take(PER_LAUNCH).collect();
                for part in mine.chunks(CHUNK) {
                    let body: String = part
                        .iter()
                        .map(|i| {
                            let status = if (i + li).is_multiple_of(9) { "failed" } else { "passed" };
                            let v: Value = json!({"id": format!("t{i}"), "name": format!("case {i}"),
                                "fullName": format!("shop.Suite{}#case{i}", i % 30), "status": status,
                                "start": 1_795_000_000_000i64 + *i as i64, "stop": 1_795_000_000_010i64 + *i as i64});
                            format!("{v}\n")
                        })
                        .collect();
                    let r = client
                        .req(
                            Method::POST,
                            &format!("/api/v1/projects/dl/launches/{l}/results"),
                            Some(ADMIN_TOKEN),
                        )
                        .header("content-type", "application/x-ndjson")
                        .body(body)
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(r.status(), StatusCode::ACCEPTED);
                }
            })
        })
        .collect();
    for s in senders {
        s.await.unwrap();
    }
    let total = (LAUNCHES * PER_LAUNCH) as i64;
    let deadline = Instant::now() + Duration::from_secs(300);
    while count(&db, "select count(*) from attempts").await < total {
        assert!(Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let elapsed = started.elapsed();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let found = deadlocks(&db).await - before;
    eprintln!(
        "{LAUNCHES} launches x {PER_LAUNCH} of {TESTS} shared tests through 2 writers: {:.2} s, {found} deadlocks",
        elapsed.as_secs_f64()
    );
    assert_eq!(found, 0, "PostgreSQL detected deadlocks");
    assert_eq!(count(&db, "select count(*) from attempts").await, total);
    assert_eq!(count(&db, "select count(*) from results").await, total);
    let used: std::collections::HashSet<usize> = (0..LAUNCHES)
        .flat_map(|li| shuffled(li).into_iter().take(PER_LAUNCH))
        .collect();
    assert_eq!(
        count(&db, "select count(*) from test_cases").await,
        used.len() as i64
    );
    assert_eq!(
        count(&db, "select count(*) from series").await,
        used.len() as i64
    );
    for l in &launches {
        let v = api.launch_get("dl", l).await;
        assert_eq!(v["counts"]["total"], PER_LAUNCH as i64);
    }
    drop((w1, w2, api));
}
