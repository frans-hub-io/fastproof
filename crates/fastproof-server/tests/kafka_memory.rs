//! Memory of the Kafka writer under a backlog: about 250 MB of results and log lines (some of
//! them large) wait in Kafka, then the real binary starts with `FASTPROOF_ROLES=writer` and a
//! small in-flight budget. Its peak RSS (`VmHWM`) must stay under a bound while it drains the
//! whole backlog with exact counts.
//!
//! Gated on `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_KAFKA_BROKERS`; Linux only (reads
//! `/proc`). `FASTPROOF_TEST_WRITER_BIN` runs another build of the binary as the writer (for
//! example the static release build of the container image).

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{Server, TestDb};
use fastproof_stream::kafka::{Outgoing, Signer};
use serde_json::{Value, json};

/// Peak RSS the writer may reach with a 32 MiB in-flight budget.
const BOUND_MIB: u64 = 200;
/// The writer is killed above this to protect the machine running the test.
const KILL_MIB: u64 = 1024;
const LAUNCHES: usize = 20;
const RESULTS_PER_LAUNCH: usize = 500;
const LINES_PER_RESULT: usize = 20;

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

fn status_kib(pid: u32, field: &str) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    s.lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Log-like text of about `n` bytes: words from a small vocabulary with numbers.
    fn text(&mut self, n: usize) -> String {
        const WORDS: &[&str] = &[
            "GET",
            "POST",
            "/api/v1/orders",
            "status=200",
            "status=500",
            "user",
            "checkout",
            "timeout",
            "retrying",
            "connection",
            "pool",
            "acquired",
            "released",
            "cart",
            "payment",
            "accepted",
            "declined",
            "INFO",
            "WARN",
            "DEBUG",
            "elapsed_ms",
            "trace_id",
            "span",
            "worker",
            "queue",
            "commit",
            "rollback",
            "assert",
            "expected",
            "actual",
        ];
        let mut s = String::with_capacity(n + 32);
        while s.len() < n {
            let r = self.next();
            s.push_str(WORDS[(r % WORDS.len() as u64) as usize]);
            s.push(if r & 0x100 == 0 { ' ' } else { '=' });
            if r & 0x3000 == 0 {
                s.push_str(&format!("{:x} ", r >> 20));
            }
        }
        s
    }
}

#[tokio::test]
async fn writer_memory_stays_bounded_while_draining_a_backlog() {
    if !std::path::Path::new("/proc/self/status").exists() {
        eprintln!("no /proc: skipped");
        return;
    }
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(brokers) = env("FASTPROOF_TEST_KAFKA_BROKERS") else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let prefix = format!("fpmem{:08x}.", rand::random::<u32>());
    let kafka_vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_INGEST_MODE", "kafka".into()),
        ("FASTPROOF_KAFKA_BROKERS", brokers.clone()),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.clone()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
        ("FASTPROOF_KAFKA_PARTITIONS", "6".into()),
    ];
    // The gateway alone creates the schema, topics and signing key; nothing consumes.
    let vars: Vec<(&str, &str)> = kafka_vars
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .chain([("FASTPROOF_ROLES", "api,scheduler")])
        .collect();
    let api = Server::start_with(&db, &vars).await;
    api.project("mem").await;
    let mut launches = Vec::new();
    for _ in 0..LAUNCHES {
        launches.push(
            api.launch("mem", None).await["id"]
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
    let mut rng = Rng(42);
    let mut bytes = 0usize;
    let (mut n_results, mut n_lines) = (0i64, 0i64);
    let started = Instant::now();
    for (li, launch) in launches.iter().enumerate() {
        let k = launch.to_string().into_bytes();
        let mut results = Vec::new();
        let mut lines = Vec::new();
        for r in 0..RESULTS_PER_LAUNCH {
            let id = format!("t{r}");
            let mut result = json!({"id": id, "name": format!("case {r}"),
                "fullName": format!("mem.Suite{}#case{r}", r % 40),
                "status": if r % 10 == 0 { "failed" } else { "passed" },
                "start": 1_795_000_000_000i64 + r as i64, "stop": 1_795_000_001_000i64 + r as i64,
                "error": {"message": rng.text(120), "trace": rng.text(600)}});
            if (li * RESULTS_PER_LAUNCH + r) % 100 == 7 {
                result["description"] = Value::String(rng.text(300 * 1024));
            }
            let v = serde_json::to_vec(
                &json!({"v": 1, "project": "mem", "launchId": launch, "result": result}),
            )
            .unwrap();
            bytes += v.len();
            results.push(Outgoing {
                key: k.clone(),
                value: v,
            });
            n_results += 1;
            for seq in 0..LINES_PER_RESULT {
                let size = if (r * LINES_PER_RESULT + seq) % 500 == 3 {
                    60 * 1024
                } else {
                    900
                };
                let v = serde_json::to_vec(&json!({"v": 1, "project": "mem", "launchId": launch,
                    "resultId": id, "attempt": 0, "seq": seq, "ts": 1_795_000_000_000i64 + seq as i64,
                    "level": if seq % 7 == 0 { "warn" } else { "info" }, "stream": "stdout",
                    "message": rng.text(size)}))
                .unwrap();
                bytes += v.len();
                lines.push(Outgoing {
                    key: k.clone(),
                    value: v,
                });
                n_lines += 1;
            }
        }
        kafka.produce(&results_topic, results).await.unwrap();
        for part in lines.chunks(2000) {
            kafka.produce(&logs_topic, part.to_vec()).await.unwrap();
        }
    }
    eprintln!(
        "produced {n_results} results and {n_lines} log lines ({} MiB) in {:.1} s",
        bytes / (1024 * 1024),
        started.elapsed().as_secs_f64()
    );
    assert!(bytes > 200 * 1024 * 1024, "the backlog is large: {bytes}");
    drop(kafka);

    // The writer: the real binary in its own process.
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{}\n", common::ADMIN_TOKEN)).unwrap();
    let log = std::fs::File::create(dir.path().join("writer.log")).unwrap();
    let bin =
        env("FASTPROOF_TEST_WRITER_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_fastproof").into());
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_MIGRATE", "off")
        .env("FASTPROOF_ROLES", "writer")
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{}", free_port()))
        .env("FASTPROOF_BLOB_DIR", dir.path().join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.path().join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .env("FASTPROOF_LOG_FORMAT", "text")
        .env(
            "FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES",
            env("FASTPROOF_TEST_WRITER_BUDGET").unwrap_or_else(|| (32 << 20).to_string()),
        )
        .env("RUST_LOG", "warn")
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    for (k, v) in &kafka_vars {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(900);
    let mut peak = 0u64;
    let started = Instant::now();
    let outcome = loop {
        if let Some(h) = status_kib(pid, "VmHWM:") {
            peak = peak.max(h / 1024);
        }
        if let Ok(Some(st)) = child.try_wait() {
            break Err(format!("the writer exited: {st}"));
        }
        if peak > KILL_MIB {
            break Err(format!("peak RSS {peak} MiB"));
        }
        if Instant::now() > deadline {
            break Err("timed out".to_string());
        }
        let attempts: i64 = sqlx::query_scalar("select count(*) from attempts")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let stored: i64 = sqlx::query_scalar(
            "select coalesce(sum(line_count), 0)::bigint from attempt_log_chunks",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if attempts >= n_results && stored >= n_lines {
            break Ok((attempts, stored));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let elapsed = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    if let Some(h) = status_kib(pid, "VmHWM:") {
        peak = peak.max(h / 1024);
    }
    let tail = std::fs::read_to_string(dir.path().join("writer.log")).unwrap_or_default();
    let tail: String = tail
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!(
        "writer: peak RSS {peak} MiB, {:.1} s, outcome {outcome:?}",
        elapsed.as_secs_f64()
    );
    let (attempts, stored) = outcome.unwrap_or_else(|e| panic!("{e}\n{tail}"));
    assert_eq!(attempts, n_results, "every result exactly once");
    assert_eq!(stored, n_lines, "every log line exactly once");
    assert!(
        peak <= BOUND_MIB,
        "peak RSS of the writer {peak} MiB exceeds {BOUND_MIB} MiB\n{tail}"
    );
    let lagging: i64 = sqlx::query_scalar(
        "select count(*) from kafka_offsets where group_name = 'fastproof-writer' and next_offset <> high_watermark",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(lagging, 0, "offsets are committed to the end");
    drop(api);
}
