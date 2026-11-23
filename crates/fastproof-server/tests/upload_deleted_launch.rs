//! A launch deleted while one of its upload archives is being parsed: the processing stops
//! without retrying (no `upload.done`, no rows of the launch left, outcome `launch_deleted`),
//! every attachment blob it stored is known to an attachment row (so it is collected like any
//! unlinked attachment instead of staying in the bucket forever), and the next upload behind
//! it is processed (in Kafka mode: on the same partition, which does not stop).
//!
//! Needs `FASTPROOF_TEST_DATABASE_URL`; the Kafka variant also `FASTPROOF_TEST_KAFKA_BROKERS`.

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb, fixture, tar_gz};
use reqwest::Method;

/// Results (each with an attachment of its own) in the archive: enough that parsing and
/// storing the attachments takes a while.
const RESULTS: usize = 4000;

fn big_archive() -> (tempfile::TempDir, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..RESULTS {
        let uuid = format!("00000000-0000-4000-8000-{i:012}");
        let att = format!("{uuid}-attachment.txt");
        std::fs::write(
            dir.path().join(&att),
            format!("output of case {i}\n").repeat(40),
        )
        .unwrap();
        let r = serde_json::json!({"uuid": uuid, "historyId": format!("h{i}"), "name": format!("case {i}"),
            "fullName": format!("big.Suite{}#case{i}", i % 40), "status": if i % 9 == 0 { "failed" } else { "passed" },
            "stage": "finished", "start": 1_795_000_000_000i64 + i as i64, "stop": 1_795_000_000_100i64 + i as i64,
            "attachments": [{"name": "output", "source": att, "type": "text/plain"}]});
        std::fs::write(
            dir.path().join(format!("{uuid}-result.json")),
            serde_json::to_vec(&r).unwrap(),
        )
        .unwrap();
    }
    let bytes = tar_gz(dir.path(), &|_| true);
    (dir, bytes)
}

async fn count(db: &TestDb, sql: &str, launch: uuid::Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn blob_shas(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let base = root.join("projects/gone/attachments");
    let Ok(dirs) = std::fs::read_dir(&base) else {
        return out;
    };
    for d in dirs.flatten() {
        for f in std::fs::read_dir(d.path()).unwrap().flatten() {
            out.push(f.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

/// Uploads whose processing stopped because their launch was gone.
fn stopped() -> f64 {
    common::prometheus()
        .render()
        .lines()
        .filter(|l| {
            l.starts_with("fastproof_ingest_jobs_total")
                && (l.contains("outcome=\"launch_deleted\"") || l.contains("outcome=\"lost\""))
        })
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

async fn run(kafka: Option<String>) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let prefix = format!("fpupgone{:08x}.", rand::random::<u32>());
    let mut vars: Vec<(&str, String)> = Vec::new();
    if let Some(b) = &kafka {
        vars.extend([
            ("FASTPROOF_INGEST_MODE", "kafka".to_string()),
            ("FASTPROOF_KAFKA_BROKERS", b.clone()),
            ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.clone()),
            ("FASTPROOF_KAFKA_CREATE_TOPICS", "true".into()),
            ("FASTPROOF_KAFKA_PARTITIONS", "1".into()),
        ]);
    }
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let s = Server::start_with(&db, &vars).await;
    s.project("gone").await;
    let a = s.launch("gone", None).await["id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();
    let (_dir, bytes) = big_archive();
    let stopped_before = stopped();
    let (st, u) = s.upload("gone", &a.to_string(), bytes, "").await;
    assert_eq!(st, reqwest::StatusCode::ACCEPTED, "{u}");
    let upload = u["id"].as_str().unwrap().to_string();
    // Delete the launch as soon as its upload is being processed.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status: Option<String> =
            sqlx::query_scalar("select status from uploads where id = $1::uuid")
                .bind(&upload)
                .fetch_optional(&db.pool)
                .await
                .unwrap();
        match status.as_deref() {
            Some("processing") => break,
            Some("queued") => {}
            other => panic!("the upload was not caught while processing: {other:?}"),
        }
        assert!(Instant::now() < deadline, "the upload was not processed");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (st, b) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/gone/launches/{a}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{st} {b}");

    // The next upload, in another launch, is processed: nothing is stuck behind the first.
    let b_launch = s.launch("gone", None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let done = s
        .ingest(
            "gone",
            &b_launch,
            tar_gz(&fixture("pytest/allure-results"), &|_| true),
            "",
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");

    // The deleted launch's processing ends without leaving anything of it (direct mode may
    // also notice through its job lease, which went with the upload).
    let deadline = Instant::now() + Duration::from_secs(120);
    while stopped() <= stopped_before {
        assert!(
            Instant::now() < deadline,
            "the processing of the deleted launch's upload did not stop"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    for table in [
        "results",
        "attempts",
        "launch_containers",
        "attachment_links",
        "uploads",
    ] {
        assert_eq!(
            count(
                &db,
                &format!("select count(*) from {table} where launch_id = $1"),
                a
            )
            .await,
            0,
            "{table} rows of the deleted launch"
        );
    }
    assert_eq!(
        count(
            &db,
            "select count(*) from events where type = 'upload.done' and payload->>'launchId' = $1::text",
            a
        )
        .await,
        0,
        "upload.done for the deleted launch"
    );
    let known: HashSet<String> = sqlx::query_scalar("select sha256 from attachments")
        .fetch_all(&db.pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
    let blobs = blob_shas(&s.dir.path().join("blobs"));
    assert!(
        !blobs.is_empty(),
        "no attachment was stored before the deletion"
    );
    let orphans: Vec<&String> = blobs.iter().filter(|b| !known.contains(*b)).collect();
    assert!(
        orphans.is_empty(),
        "{} attachment blobs without a row",
        orphans.len()
    );
    if kafka.is_some() {
        let lag: i64 = sqlx::query_scalar(
            "select count(*) from kafka_offsets where group_name = 'fastproof-ingest' and next_offset <> high_watermark",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(lag, 0, "the uploads partition did not move on");
    }
}

/// Both modes in one test: they share the process-wide metrics the test waits on.
#[tokio::test]
async fn launch_deleted_during_upload_processing() {
    run(None).await;
    match std::env::var("FASTPROOF_TEST_KAFKA_BROKERS")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(b) => run(Some(b)).await,
        None => eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: Kafka mode skipped"),
    }
}
