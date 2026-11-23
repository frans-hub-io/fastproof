//! Writers killed in the middle of their batches: a backlog of results and log lines waits in
//! Kafka, two writer processes (the real binary) drain it, and one of them is killed with
//! SIGKILL whenever the stored data has moved on, several times, while the other one and the
//! restarted ones take over its partitions. In the end every result and every log line is
//! stored exactly once, the attempts of each launch (results sent directly, not from uploads)
//! keep the order they were produced in (their sequence numbers follow the Kafka order of the
//! partition), results reported as running and then completed show the completed status, the
//! series statistics count every launch once, the outbox holds one event per stored attempt and
//! log chunk, unsigned lines of trusted producers are charged to the launch budget, and the
//! offsets reach the end.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_KAFKA_BROKERS`; Unix only.

mod common;

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use common::{Server, TestDb};
use fastproof_stream::kafka::{Outgoing, Signer};
use serde_json::json;

const LAUNCHES: usize = 16;
const RESULTS_PER_LAUNCH: usize = 500;
const LINES_PER_RESULT: usize = 10;
/// Every this many results is first reported as running, then completed.
const RUNNING_EVERY: usize = 10;
const KILLS: usize = 4;
const PARTITIONS: i32 = 4;
/// Unsigned lines per launch from a trusted producer (no gateway signature).
const UNSIGNED_LINES: usize = 50;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// A writer process, killed when dropped (also when the test fails).
struct Writer(Child);

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn writer(db: &TestDb, dir: &std::path::Path, n: usize, kafka_vars: &[(&str, String)]) -> Writer {
    let tokens = dir.join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{}\n", common::ADMIN_TOKEN)).unwrap();
    let log = std::fs::File::create(dir.join(format!("writer{n}.log"))).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fastproof"));
    cmd.arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_DATABASE_MAX_CONNECTIONS", "6")
        .env("FASTPROOF_MIGRATE", "off")
        .env("FASTPROOF_ROLES", "writer")
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("FASTPROOF_BLOB_DIR", dir.join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .env("FASTPROOF_LOG_FORMAT", "text")
        .env("FASTPROOF_KAFKA_BATCH_MAX_RECORDS", "300")
        .env("FASTPROOF_KAFKA_TRUSTED_PRODUCERS", "true")
        .env("RUST_LOG", "warn")
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    for (k, v) in kafka_vars {
        cmd.env(k, v);
    }
    Writer(cmd.spawn().unwrap())
}

/// Stored attempts plus stored log lines: moves on with every committed batch.
async fn progress(db: &TestDb) -> i64 {
    count(
        db,
        "select (select count(*) from attempts) + (select coalesce(sum(line_count), 0) from attempt_log_chunks)::bigint",
    )
    .await
}

#[tokio::test]
async fn killed_writers_store_everything_exactly_once_and_in_order() {
    crash_run(false).await;
}

/// The same with records keyed as the gateway keys them now (launch and test, launch and
/// result id), so every launch spreads over all partitions and is written by both writers at
/// once: still exactly once, in partition order, and the counters are right.
#[tokio::test]
async fn killed_writers_of_spread_launches_store_everything_exactly_once() {
    crash_run(true).await;
}

async fn crash_run(spread: bool) {
    if !cfg!(unix) {
        return;
    }
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(brokers) = env("FASTPROOF_TEST_KAFKA_BROKERS") else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let prefix = format!("fpcrash{:08x}.", rand::random::<u32>());
    let kafka_vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_INGEST_MODE", "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS", brokers.clone()),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.clone()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS", PARTITIONS.to_string()),
    ];
    let vars: Vec<(&str, &str)> = kafka_vars
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .chain([("FASTPROOF_ROLES", "api,scheduler")])
        .collect();
    let api = Server::start_with(&db, &vars).await;
    api.project("crash").await;
    let mut launches = Vec::new();
    for _ in 0..LAUNCHES {
        launches.push(
            api.launch("crash", None).await["id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap(),
        );
    }

    let store = fastproof_store::Store::new(db.pool.clone());
    let key = store.stream_key("kafka-gateway").await.unwrap();
    let cfg = fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&|k| {
        kafka_vars
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
    }))
    .unwrap()
    .unwrap();
    let kafka = fastproof_stream::Kafka::connect(&cfg, Some(Signer::new(key)))
        .await
        .unwrap();
    let results_topic = format!("{prefix}ingest.results");
    let logs_topic = format!("{prefix}ingest.logs");
    let result_key = |launch: &uuid::Uuid, r: usize| {
        if spread {
            fastproof_stream::messages::result_key(
                *launch,
                &format!("crash.Suite{}#case{r}", r % 25),
            )
        } else {
            launch.to_string().into_bytes()
        }
    };
    let log_key = |launch: &uuid::Uuid, r: usize| {
        if spread {
            fastproof_stream::messages::log_key(*launch, &format!("t{r}"))
        } else {
            launch.to_string().into_bytes()
        }
    };
    let result = |launch: &uuid::Uuid, r: usize, status: &str| {
        let mut v = json!({"id": format!("t{r}"), "name": format!("case {r}"),
            "fullName": format!("crash.Suite{}#case{r}", r % 25), "status": status,
            "start": 1_795_000_000_000i64 + r as i64});
        if status == "failed" {
            v["error"] = json!({"message": format!("expected {r} but was {}", r + 1)});
        }
        Outgoing {
            key: result_key(launch, r),
            value: serde_json::to_vec(
                &json!({"v": 1, "project": "crash", "launchId": launch, "result": v}),
            )
            .unwrap(),
        }
    };
    let final_status = |r: usize| if r % 7 == 3 { "failed" } else { "passed" };
    for launch in &launches {
        let mut results = Vec::new();
        let mut lines = Vec::new();
        for r in 0..RESULTS_PER_LAUNCH {
            if r % RUNNING_EVERY == 0 {
                results.push(result(launch, r, "running"));
            } else {
                results.push(result(launch, r, final_status(r)));
            }
            // The running ones complete a little later in the same partition.
            if r >= 5 && (r - 5) % RUNNING_EVERY == 0 {
                results.push(result(launch, r - 5, final_status(r - 5)));
            }
            for seq in 0..LINES_PER_RESULT {
                lines.push(Outgoing {
                    key: log_key(launch, r),
                    value: serde_json::to_vec(&json!({"v": 1, "project": "crash",
                        "launchId": launch, "resultId": format!("t{r}"), "attempt": 0, "seq": seq,
                        "ts": 1_795_000_000_000i64 + seq as i64, "level": "info", "stream": "stdout",
                        "message": format!("line {seq} of case {r}: GET /api/items answered 200")}))
                    .unwrap(),
                });
            }
        }
        // Producer retries: a slice of the records again, later in the partition.
        let again: Vec<Outgoing> = results[100..140].to_vec();
        kafka.produce(&results_topic, results).await.unwrap();
        kafka.produce(&results_topic, again).await.unwrap();
        let again: Vec<Outgoing> = lines[1000..1200].to_vec();
        for part in lines.chunks(2000) {
            kafka.produce(&logs_topic, part.to_vec()).await.unwrap();
        }
        kafka.produce(&logs_topic, again).await.unwrap();
    }
    drop(kafka);
    // A trusted producer outside FastProof: unsigned lines, validated and charged by the writer.
    let unsigned = fastproof_stream::Kafka::connect(&cfg, None).await.unwrap();
    let mut unsigned_bytes = 0i64;
    for launch in &launches {
        let lines: Vec<Outgoing> = (0..UNSIGNED_LINES)
            .map(|seq| {
                let message = format!("external line {seq}");
                unsigned_bytes += (message.len() + "ext".len()) as i64;
                Outgoing {
                    key: launch.to_string().into_bytes(),
                    value: serde_json::to_vec(&json!({"v": 1, "project": "crash",
                        "launchId": launch, "resultId": "ext", "attempt": 0, "seq": seq,
                        "ts": 1_795_000_000_000i64, "level": "info", "stream": "stdout",
                        "message": message}))
                    .unwrap(),
                }
            })
            .collect();
        unsigned.produce(&logs_topic, lines).await.unwrap();
    }
    drop(unsigned);
    let n_results = (LAUNCHES * RESULTS_PER_LAUNCH) as i64;
    let n_lines = n_results * LINES_PER_RESULT as i64 + (LAUNCHES * UNSIGNED_LINES) as i64;

    let dir = tempfile::tempdir().unwrap();
    let mut started = 0;
    let mut spawn = |db: &TestDb| {
        started += 1;
        writer(db, dir.path(), started, &kafka_vars)
    };
    let mut writers = [spawn(&db), spawn(&db)];
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut kills = 0;
    let mut last = 0;
    let mut victim = 0;
    let mut settled: Option<(Instant, i64)> = None;
    loop {
        assert!(Instant::now() < deadline, "the backlog was not drained");
        let p = progress(&db).await;
        let done = count(
            &db,
            "select count(*) from attempts where status <> 'running'",
        )
        .await
            >= n_results
            && count(
                &db,
                "select coalesce(sum(line_count), 0)::bigint from attempt_log_chunks",
            )
            .await
                >= n_lines;
        if done {
            break;
        }
        // Offsets at the end and nothing stored for longer than a lease takes to expire, with
        // data missing: the checks below say what is lost.
        if kills == KILLS && at_end(&db).await {
            if settled.is_some_and(|(at, n)| n == p && at.elapsed() > Duration::from_secs(20)) {
                break;
            }
            if settled.is_none_or(|(_, n)| n != p) {
                settled = Some((Instant::now(), p));
            }
        } else {
            settled = None;
        }
        // Kill while batches are being written: right after stored data moved on.
        if kills < KILLS && p > last && p > 0 {
            tokio::time::sleep(Duration::from_millis(30 + 40 * kills as u64)).await;
            let w = &mut writers[victim].0;
            assert!(w.try_wait().unwrap().is_none(), "the writer exited early");
            w.kill().unwrap();
            w.wait().unwrap();
            kills += 1;
            last = progress(&db).await;
            eprintln!("killed writer {victim} at {last} stored rows");
            writers[victim] = spawn(&db);
            victim = 1 - victim;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        kills, KILLS,
        "every kill happened while the backlog drained"
    );
    wait_offsets(&db).await;
    for w in &mut writers {
        let _ = w.0.kill();
        let _ = w.0.wait();
    }

    assert_eq!(
        count(&db, "select count(*) from attempts").await,
        n_results,
        "every result exactly once"
    );
    assert_eq!(count(&db, "select count(*) from results").await, n_results);
    assert_eq!(
        count(
            &db,
            "select count(*) from attempts where status = 'running'"
        )
        .await,
        0,
        "completed results replace their running copies"
    );
    assert_eq!(
        count(&db, "select count(*) from (select launch_id, client_key from attempts group by 1, 2 having count(*) > 1) d").await,
        0
    );
    assert_eq!(
        count(
            &db,
            "select coalesce(sum(line_count), 0)::bigint from attempt_log_chunks"
        )
        .await,
        n_lines,
        "every log line exactly once"
    );
    assert_eq!(
        count(&db, &format!("select count(*) from (select launch_id, result_key, attempt from attempt_log_chunks \
            group by 1, 2, 3 having sum(line_count) <> case when result_key = 'ext' then {UNSIGNED_LINES} \
            else {LINES_PER_RESULT} end) d")).await,
        0,
        "every result has all its lines"
    );
    assert_eq!(
        count(&db, "select count(*) from (select launch_id, result_key, attempt from attempt_log_chunks c \
            where exists (select 1 from attempt_log_chunks o where o.launch_id = c.launch_id and o.result_key = c.result_key \
              and o.attempt = c.attempt and o.id <> c.id and o.first_seq <= c.last_seq and o.last_seq >= c.first_seq)) d").await,
        0,
        "no line is stored in two chunks"
    );
    // Results reported as running first may count their launch twice in the ring when another
    // launch's result came in between; every other series counts each launch exactly once.
    assert_eq!(
        count(
            &db,
            &format!(
                "select count(*) from series_stats st join series s on s.id = st.series_id \
            join test_cases t on t.id = s.test_case_id \
            where split_part(t.full_name, '#case', 2)::int % {RUNNING_EVERY} <> 0 \
              and (st.runs_tracked <> {LAUNCHES} or length(st.last_statuses) <> {LAUNCHES})"
            )
        )
        .await,
        0,
        "series statistics count every launch once"
    );
    assert_eq!(
        count(&db, "select count(*) from series_stats").await,
        RESULTS_PER_LAUNCH as i64
    );
    assert_eq!(
        count(
            &db,
            "select count(distinct payload->>'id') from events where type = 'attempt.stored'"
        )
        .await,
        n_results,
        "an outbox event for every stored attempt"
    );
    assert_eq!(
        count(&db, "select coalesce(sum((payload->>'lines')::bigint), 0)::bigint from events where type = 'log.chunk'").await,
        n_lines,
        "an outbox event for every stored chunk"
    );
    assert_eq!(
        count(&db, "select count(*) from events where type = 'log.chunk'").await,
        count(&db, "select count(*) from attempt_log_chunks").await
    );
    assert_eq!(
        count(
            &db,
            &format!(
                "select count(*) from launch_log_bytes where bytes >= {}",
                unsigned_bytes / LAUNCHES as i64
            )
        )
        .await,
        LAUNCHES as i64,
        "unsigned lines are charged to their launch"
    );
    for launch in &launches {
        let keys: Vec<String> =
            sqlx::query_scalar("select client_key from attempts where launch_id = $1 order by seq")
                .bind(launch)
                .fetch_all(&db.pool)
                .await
                .unwrap();
        // The attempts of each partition keep the order they were produced in.
        let partition =
            |r: usize| fastproof_stream::kafka::partition_for(&result_key(launch, r), PARTITIONS);
        for p in 0..PARTITIONS {
            let stored: Vec<&String> = keys
                .iter()
                .filter(|k| partition(k[3..].parse().unwrap()) == p)
                .collect();
            let expected: Vec<String> = (0..RESULTS_PER_LAUNCH)
                .filter(|r| partition(*r) == p)
                .map(|r| format!("r:t{r}"))
                .collect();
            assert_eq!(
                stored,
                expected.iter().collect::<Vec<_>>(),
                "attempts follow the partition order"
            );
        }
        if spread {
            let used: std::collections::BTreeSet<i32> =
                (0..RESULTS_PER_LAUNCH).map(partition).collect();
            assert_eq!(
                used.len(),
                PARTITIONS as usize,
                "a launch spreads over every partition"
            );
        }
        let v = api.launch_get("crash", &launch.to_string()).await;
        assert_eq!(v["counts"]["total"], RESULTS_PER_LAUNCH as i64);
        let failed = (0..RESULTS_PER_LAUNCH)
            .filter(|r| final_status(*r) == "failed")
            .count();
        assert_eq!(v["counts"]["failed"], failed as i64);
    }
    assert_eq!(
        count(
            &db,
            "select count(*) from launches l where l.count_total <> (select count(*) from results r where r.launch_id = l.id) \
               or l.count_passed <> (select count(*) from results r where r.launch_id = l.id and r.status = 'passed') \
               or l.first_result_at is distinct from (select min(started_at) from results r where r.launch_id = l.id) \
               or l.last_result_at is distinct from (select max(coalesce(stopped_at, started_at)) from results r where r.launch_id = l.id)"
        )
        .await,
        0,
        "launch counters and times equal a recount"
    );
    drop(api);
}

/// Every committed offset of the writers is at the high watermark it saw.
async fn at_end(db: &TestDb) -> bool {
    let lagging = count(
        db,
        "select count(*) from kafka_offsets where group_name = 'fastproof-writer' and next_offset <> high_watermark",
    )
    .await;
    let committed = count(
        db,
        "select count(*) from kafka_offsets where group_name = 'fastproof-writer'",
    )
    .await;
    lagging == 0 && committed > 0
}

async fn wait_offsets(db: &TestDb) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if at_end(db).await {
            return;
        }
        assert!(Instant::now() < deadline, "offsets did not reach the end");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Records of a launch deleted while the writer has it cached: they are dropped, and the
/// records behind them in the partition are stored (the partition used to stop on them).
#[tokio::test]
async fn records_of_a_deleted_launch_do_not_stop_the_partition() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(brokers) = env("FASTPROOF_TEST_KAFKA_BROKERS") else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let prefix = format!("fpgone{:08x}.", rand::random::<u32>());
    let kafka_vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_INGEST_MODE", "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS", brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.clone()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS", "1".into()),
    ];
    let vars: Vec<(&str, &str)> = kafka_vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let s = Server::start_with(&db, &vars).await;
    s.project("gone").await;
    let a = s.launch("gone", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = s.launch("gone", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let result = |n: usize| json!({"id": format!("t{n}"), "name": format!("case {n}"), "fullName": format!("gone.S#c{n}"), "status": "passed"});
    let body: String = (0..3).map(|n| format!("{}\n", result(n))).collect();
    let r = s
        .req(
            reqwest::Method::POST,
            &format!("/api/v1/projects/gone/launches/{a}/results"),
            Some(common::ADMIN_TOKEN),
        )
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::ACCEPTED);
    let deadline = Instant::now() + Duration::from_secs(60);
    while count(&db, "select count(*) from attempts").await < 3 {
        assert!(Instant::now() < deadline, "launch A was not stored");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (st, _) = s
        .call(
            reqwest::Method::DELETE,
            &format!("/api/v1/projects/gone/launches/{a}"),
            Some(common::ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, reqwest::StatusCode::NO_CONTENT);

    // Records of the deleted launch reach the partition before those of the live one.
    let store = fastproof_store::Store::new(db.pool.clone());
    let key = store.stream_key("kafka-gateway").await.unwrap();
    let cfg = fastproof_stream::KafkaConfig::from_vars(&fastproof_stream::Vars(&|k| {
        kafka_vars
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
    }))
    .unwrap()
    .unwrap();
    let kafka = fastproof_stream::Kafka::connect(&cfg, Some(Signer::new(key)))
        .await
        .unwrap();
    for (launch, from) in [(&a, 3), (&b, 0)] {
        let results: Vec<Outgoing> = (from..from + 5)
            .map(|n| Outgoing {
                key: launch.clone().into_bytes(),
                value: serde_json::to_vec(
                    &json!({"v": 1, "project": "gone", "launchId": launch, "result": result(n)}),
                )
                .unwrap(),
            })
            .collect();
        kafka
            .produce(&format!("{prefix}ingest.results"), results)
            .await
            .unwrap();
        let lines: Vec<Outgoing> = (0..20)
            .map(|seq| Outgoing {
                key: launch.clone().into_bytes(),
                value: serde_json::to_vec(&json!({"v": 1, "project": "gone", "launchId": launch,
                    "resultId": "t0", "attempt": 0, "seq": seq, "ts": 1_795_000_000_000i64,
                    "level": "info", "stream": "stdout", "message": format!("line {seq}")}))
                .unwrap(),
            })
            .collect();
        kafka
            .produce(&format!("{prefix}ingest.logs"), lines)
            .await
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let results = count(
            &db,
            &format!("select count(*) from attempts where launch_id = '{b}'"),
        )
        .await;
        let lines = count(
            &db,
            &format!("select coalesce(sum(line_count), 0)::bigint from attempt_log_chunks where launch_id = '{b}'"),
        )
        .await;
        if results == 5 && lines == 20 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the records behind the deleted launch were not stored ({results} results, {lines} lines)"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        count(
            &db,
            &format!("select count(*) from attempts where launch_id = '{a}'")
        )
        .await,
        0
    );
}
