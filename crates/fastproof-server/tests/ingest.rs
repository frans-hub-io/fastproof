//! Uploads through HTTP into PostgreSQL and blob storage: every fixture tool, shards, retries
//! across uploads, two replicas, S3, and the CLI. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use common::{ADMIN_TOKEN, Server, TestDb, fixture, read, tar_gz};
use fastproof_ingest::{IngestOptions, NullSink, ingest_path};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// Results (attempts) and distinct series the parser finds in the same bytes.
fn expected(bytes: &[u8], file_name: Option<&str>) -> (usize, usize) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("upload");
    std::fs::write(&p, bytes).unwrap();
    let b = ingest_path(
        &p,
        &IngestOptions {
            file_name: file_name.map(str::to_string),
            ..IngestOptions::default()
        },
        &mut NullSink::default(),
    )
    .unwrap();
    let series: HashSet<&str> = b
        .results
        .iter()
        .map(|r| r.identity.series_key.as_str())
        .collect();
    (b.results.len(), series.len())
}

#[tokio::test]
async fn every_fixture_tool() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fixtures").await;
    let cases: Vec<(&str, Vec<u8>, Option<&str>)> = vec![
        (
            "pytest allure",
            tar_gz(&fixture("pytest/allure-results"), &|_| true),
            None,
        ),
        (
            "pytest junit",
            read("pytest/junit-xunit2.xml"),
            Some("junit-xunit2.xml"),
        ),
        (
            "junit5 allure",
            tar_gz(&fixture("junit5/allure-results"), &|_| true),
            None,
        ),
        (
            "surefire",
            tar_gz(&fixture("junit5/surefire-reports"), &|_| true),
            None,
        ),
        (
            "playwright allure",
            tar_gz(&fixture("playwright/allure-results"), &|_| true),
            None,
        ),
        (
            "playwright json",
            read("playwright/results.json"),
            Some("results.json"),
        ),
        (
            "playwright junit",
            read("playwright/junit.xml"),
            Some("junit.xml"),
        ),
        (
            "playwright ctrf",
            read("playwright/ctrf/ctrf-report.json"),
            Some("ctrf-report.json"),
        ),
        (
            "vitest allure",
            tar_gz(&fixture("vitest/allure-results"), &|_| true),
            None,
        ),
        ("vitest junit", read("vitest/junit.xml"), Some("junit.xml")),
        ("gotestsum", read("gotestsum/junit.xml"), Some("junit.xml")),
    ];
    for (name, bytes, file_name) in cases {
        let (attempts, series) = expected(&bytes, file_name);
        assert!(attempts > 0, "{name}");
        let launch = s.launch("fixtures", Some(name)).await;
        let lid = launch["id"].as_str().unwrap();
        let q = file_name
            .map(|f| format!("?fileName={f}"))
            .unwrap_or_default();
        let u = s.ingest("fixtures", lid, bytes, &q).await;
        assert_eq!(u["status"], "done", "{name}: {u}");
        let fmt = u["format"].as_str().unwrap();
        assert!(
            name.contains(fmt)
                || (fmt == "junit" && name.contains("surefire"))
                || (fmt == "junit" && name == "gotestsum")
                || (fmt == "playwright" && name.contains("json")),
            "{name}: {fmt}"
        );
        assert_eq!(u["counts"]["results"], attempts as u64, "{name}: {u}");
        let l = s.launch_get("fixtures", lid).await;
        assert_eq!(l["counts"]["total"], series as u64, "{name}: {l}");
        let results = s.results("fixtures", lid, "").await;
        assert_eq!(results.len(), series, "{name}");
        let total_attempts: u64 = results
            .iter()
            .map(|r| r["attemptCount"].as_u64().unwrap())
            .sum();
        assert_eq!(
            total_attempts, attempts as u64,
            "{name}: every attempt is kept"
        );
        let one = &results[0];
        let (st, d) = s
            .get(
                &format!(
                    "/api/v1/projects/fixtures/results/{}",
                    one["id"].as_str().unwrap()
                ),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{name}");
        assert_eq!(
            d["attempts"].as_array().unwrap().len() as u64,
            one["attemptCount"].as_u64().unwrap()
        );
    }

    let launch = s.launch("fixtures", Some("pytest allure")).await;
    let lid = launch["id"].as_str().unwrap();
    let l = s.launch_get("fixtures", lid).await;
    assert!(l["counts"]["flaky"].as_i64().unwrap() >= 1, "{l}");
    let flaky = s.results("fixtures", lid, "&flaky=true").await;
    let (_, d) = s
        .get(
            &format!(
                "/api/v1/projects/fixtures/results/{}",
                flaky[0]["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let statuses: Vec<&str> = d["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["status"].as_str().unwrap())
        .collect();
    assert!(
        statuses.len() >= 2 && statuses.last() == Some(&"passed"),
        "{statuses:?}"
    );
    let all = s.results("fixtures", lid, "").await;
    let mut fixture_steps = 0;
    let mut attachment = None;
    for r in &all {
        let (_, d) = s
            .get(
                &format!(
                    "/api/v1/projects/fixtures/results/{}",
                    r["id"].as_str().unwrap()
                ),
                ADMIN_TOKEN,
            )
            .await;
        let text = d.to_string();
        if text.contains("\"kind\":\"fixture\"") {
            fixture_steps += 1;
        }
        if attachment.is_none() {
            attachment = find_attachment(&d);
        }
    }
    assert!(fixture_steps > 0, "container fixtures are attached");
    let aid = attachment.expect("pytest results have attachments");
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/fixtures/attachments/{aid}/content"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(!r.bytes().await.unwrap().is_empty());
}

fn find_attachment(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(Value::Array(a)) = m.get("attachments")
                && let Some(id) = a.iter().find_map(|x| x["id"].as_str())
            {
                return Some(id.to_string());
            }
            m.values().find_map(find_attachment)
        }
        Value::Array(a) => a.iter().find_map(find_attachment),
        _ => None,
    }
}

fn write(dir: &Path, name: &str, v: Value) {
    std::fs::write(dir.join(name), v.to_string()).unwrap();
}

#[tokio::test]
async fn shards_containers_and_retries_across_uploads() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("shards").await;
    let launch = s.launch("shards", Some("ci-42")).await;
    let lid = launch["id"].as_str().unwrap();

    let d1 = tempfile::tempdir().unwrap();
    write(
        d1.path(),
        "c1-container.json",
        json!({"uuid": "c1", "children": ["u1", "u2"],
        "befores": [{"name": "setUp db", "status": "passed", "start": 10, "stop": 20}],
        "afters": [{"name": "tearDown db", "status": "passed", "start": 3000, "stop": 3010}]}),
    );
    let d2 = tempfile::tempdir().unwrap();
    write(
        d2.path(),
        "u1-result.json",
        json!({"uuid": "u1", "name": "adds", "fullName": "shop.CartTest#adds",
        "status": "failed", "stage": "finished", "start": 1000, "stop": 1100,
        "statusDetails": {"message": "boom"}, "labels": [{"name": "suite", "value": "cart"}],
        "attachments": [{"name": "log", "source": "u1-attachment.txt", "type": "text/plain"}]}),
    );
    std::fs::write(d2.path().join("u1-attachment.txt"), "shared log").unwrap();
    write(
        d2.path(),
        "v1-result.json",
        json!({"uuid": "v1", "name": "pays", "fullName": "shop.CartTest#pays",
        "status": "passed", "stage": "finished", "start": 1000, "stop": 1200}),
    );
    let d3 = tempfile::tempdir().unwrap();
    write(
        d3.path(),
        "u2-result.json",
        json!({"uuid": "u2", "name": "adds", "fullName": "shop.CartTest#adds",
        "status": "passed", "stage": "finished", "start": 2000, "stop": 2100,
        "attachments": [{"name": "log", "source": "u2-attachment.txt", "type": "text/plain"}]}),
    );
    std::fs::write(d3.path().join("u2-attachment.txt"), "shared log").unwrap();

    for d in [&d1, &d2, &d3] {
        let u = s
            .ingest("shards", lid, tar_gz(d.path(), &|_| true), "")
            .await;
        assert_eq!(u["status"], "done", "{u}");
    }
    let again = s
        .ingest("shards", lid, tar_gz(d3.path(), &|_| true), "")
        .await;
    assert_eq!(again["status"], "done");

    let l = s.launch_get("shards", lid).await;
    assert_eq!(l["counts"]["total"], 2, "{l}");
    assert_eq!(l["counts"]["passed"], 2);
    assert_eq!(l["counts"]["flaky"], 1);
    let results = s.results("shards", lid, "").await;
    let adds = results.iter().find(|r| r["name"] == "adds").unwrap();
    assert_eq!(
        adds["attemptCount"], 2,
        "the same allure uuid sent twice is one attempt"
    );
    assert!(adds["flakyInRun"].as_bool().unwrap());
    let (_, d) = s
        .get(
            &format!(
                "/api/v1/projects/shards/results/{}",
                adds["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let attempts = d["attempts"].as_array().unwrap();
    assert_eq!(attempts[0]["status"], "failed");
    assert_eq!(adds["message"], json!(null), "the final attempt passed");
    let pays_failed = results.iter().all(|r| r["message"].is_null());
    assert!(pays_failed);
    assert_eq!(attempts[1]["status"], "passed");
    for a in attempts {
        let steps: Vec<&str> = a["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            steps,
            vec!["setUp db", "tearDown db"],
            "container uploaded first, fixtures attach"
        );
    }
    let a0 = attempts[0]["attachments"][0]["id"].as_str().unwrap();
    let a1 = attempts[1]["attachments"][0]["id"].as_str().unwrap();
    let body = |id: &str| {
        let id = id.to_string();
        let s = &s;
        async move {
            s.req(
                Method::GET,
                &format!("/api/v1/projects/shards/attachments/{id}/content"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
        }
    };
    assert_eq!(
        body(a0).await,
        body(a1).await,
        "identical bodies are stored once"
    );
    let n: i64 = sqlx::query_scalar("select count(*) from attachments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    let d4 = tempfile::tempdir().unwrap();
    write(
        d4.path(),
        "w1-result.json",
        json!({"uuid": "w1", "name": "late", "fullName": "shop.CartTest#late",
        "status": "passed", "stage": "finished", "start": 5000, "stop": 5100}),
    );
    s.ingest("shards", lid, tar_gz(d4.path(), &|_| true), "")
        .await;
    let d5 = tempfile::tempdir().unwrap();
    write(
        d5.path(),
        "c2-container.json",
        json!({"uuid": "c2", "children": ["w1"],
        "befores": [{"name": "late setUp", "status": "passed", "start": 4000, "stop": 4001}]}),
    );
    s.ingest("shards", lid, tar_gz(d5.path(), &|_| true), "")
        .await;
    let late = s.results("shards", lid, "&q=late").await;
    let (_, d) = s
        .get(
            &format!(
                "/api/v1/projects/shards/results/{}",
                late[0]["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        d["attempts"][0]["steps"][0]["name"], "late setUp",
        "container after result attaches too"
    );
    assert_eq!(d["attempts"][0]["steps"][0]["kind"], "fixture");
}

#[tokio::test]
async fn two_replicas_process_each_upload_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let shared = tempfile::tempdir().unwrap();
    let blobs = shared.path().to_string_lossy().to_string();
    let extra = [("FASTPROOF_BLOB_DIR", blobs.as_str())];
    let (a, b) = tokio::join!(
        Server::start_with(&db, &extra),
        Server::start_with(&db, &extra)
    );
    a.project("multi").await;
    let mut pending = Vec::new();
    let payloads = [
        ("gotestsum/junit.xml", "junit.xml"),
        ("vitest/junit.xml", "junit.xml"),
        ("pytest/junit-xunit2.xml", "junit.xml"),
        ("playwright/junit.xml", "junit.xml"),
        ("gotestsum/junit-norerun.xml", "junit.xml"),
        ("playwright/ctrf/ctrf-report.json", "ctrf.json"),
    ];
    for (i, (rel, name)) in payloads.iter().enumerate() {
        let srv = if i % 2 == 0 { &a } else { &b };
        let launch = srv.launch("multi", Some(&format!("l{i}"))).await;
        let lid = launch["id"].as_str().unwrap().to_string();
        let (st, u) = srv
            .upload("multi", &lid, read(rel), &format!("?fileName={name}"))
            .await;
        assert_eq!(st, StatusCode::ACCEPTED);
        pending.push((lid, u["id"].as_str().unwrap().to_string()));
    }
    for (lid, uid) in &pending {
        let u = b.wait_upload("multi", lid, uid).await;
        assert_eq!(u["status"], "done", "{u}");
    }
    let jobs: Vec<(String, i32)> = sqlx::query_as("select status, attempts from ingest_jobs")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(jobs.len(), payloads.len());
    assert!(jobs.iter().all(|(s, n)| s == "done" && *n == 1), "{jobs:?}");
    let done: i64 = sqlx::query_scalar("select count(*) from events where type = 'upload.done'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(done, payloads.len() as i64);
    let workers: Vec<String> = sqlx::query_scalar(
        "select distinct locked_by from ingest_jobs where locked_by is not null",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    eprintln!("workers that processed uploads: {workers:?}");

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let jobs: Vec<(String, i64, Option<String>)> =
            sqlx::query_as("select name, run_count, last_status from scheduled_jobs order by name")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        if jobs.len() >= 4 && jobs.iter().all(|j| j.2.is_some()) {
            for (name, runs, status) in &jobs {
                assert_eq!(*runs, 1, "{name} ran once across both replicas");
                assert_eq!(status.as_deref(), Some("ok"), "{name}");
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scheduled jobs did not run: {jobs:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn s3_backend() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(env) = common::s3_env() else {
        eprintln!("FASTPROOF_TEST_S3_* not set: skipped");
        return;
    };
    let extra: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let s = Server::start_with(&db, &extra).await;
    let key = format!("s3-{:012x}", rand::random::<u64>() >> 16);
    s.project(&key).await;
    let launch = s.launch(&key, None).await;
    let lid = launch["id"].as_str().unwrap();
    let ready = reqwest::get(format!("{}/readyz", s.ops)).await.unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let bytes = tar_gz(&fixture("pytest/allure-results"), &|_| true);
    let u = s.ingest(&key, lid, bytes.clone(), "").await;
    assert_eq!(u["status"], "done", "{u}");
    let stored: i64 = sqlx::query_scalar("select count(*) from attachments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(stored > 0);
    let u2 = s.ingest(&key, lid, bytes, "").await;
    assert_eq!(u2["status"], "done");
    let again: i64 = sqlx::query_scalar("select count(*) from attachments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, again, "dedup on S3 too");
    let results = s.results(&key, lid, "").await;
    let mut aid = None;
    for r in &results {
        let (_, d) = s
            .get(
                &format!(
                    "/api/v1/projects/{key}/results/{}",
                    r["id"].as_str().unwrap()
                ),
                ADMIN_TOKEN,
            )
            .await;
        if let Some(a) = find_attachment(&d) {
            aid = Some(a);
            break;
        }
    }
    let aid = aid.unwrap();
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/{key}/attachments/{aid}/content"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    let raw: i64 = sqlx::query_scalar("select count(*) from uploads where status = 'done'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(raw, 2);

    let blobs = s.running.as_ref().unwrap().state.blobs.clone();
    let uploads: Vec<String> = sqlx::query_scalar("select blob_key from uploads")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    for k in uploads {
        let k = fastproof_blob::BlobKey::parse(&k).unwrap();
        assert_eq!(
            blobs.head(&k).await.unwrap(),
            None,
            "processed uploads are deleted"
        );
    }
    let n = blobs
        .delete_prefix(&fastproof_blob::BlobKey::project_prefix(&key).unwrap())
        .await
        .unwrap();
    assert_eq!(n as i64, stored);
}

fn token_file(dir: &Path) -> PathBuf {
    let p = dir.join("token");
    std::fs::write(&p, ADMIN_TOKEN).unwrap();
    p
}

#[tokio::test]
async fn cli_upload_and_launch_commands() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cli").await;
    let tf = token_file(s.dir.path());
    let run = |args: Vec<String>| async move {
        let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
        fastproof_server::cli::run(cli).await.unwrap()
    };
    let common = |cmd: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = vec!["fastproof".into()];
        v.extend(cmd.iter().map(|s| s.to_string()));
        v.extend([
            "--url".into(),
            s.base.clone(),
            "--token-file".into(),
            tf.to_string_lossy().into(),
            "-p".into(),
            "cli".into(),
        ]);
        v
    };
    let dir = fixture("pytest/allure-results");
    let mut a = common(&[
        "upload",
        dir.to_str().unwrap(),
        "--launch",
        "build-1",
        "--wait",
    ]);
    a.extend([
        "--attr".into(),
        "team=qa".into(),
        "--environment".into(),
        "linux".into(),
    ]);
    assert_eq!(run(a).await, 0);
    let file = fixture("gotestsum/junit.xml");
    assert_eq!(
        run(common(&[
            "upload",
            file.to_str().unwrap(),
            "--launch",
            "build-1",
            "--finish"
        ]))
        .await,
        0
    );
    let (_, list) = s.get("/api/v1/projects/cli/launches", ADMIN_TOKEN).await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "both uploads joined launch build-1");
    let l = &items[0];
    assert_eq!(l["status"], "finished");
    assert_eq!(l["externalId"], "build-1");
    assert_eq!(l["environment"]["key"], "linux");
    assert_eq!(l["attributes"][0]["value"], "qa");
    let (_, pytest_series) = expected(&tar_gz(&dir, &|_| true), None);
    let (_, go_series) = expected(&read("gotestsum/junit.xml"), Some("junit.xml"));
    assert_eq!(l["counts"]["total"], (pytest_series + go_series) as u64);
    assert_eq!(
        run(common(&[
            "launch", "start", "--launch", "build-2", "--name", "second"
        ]))
        .await,
        0
    );
    assert_eq!(
        run(common(&[
            "launch",
            "finish",
            "--launch",
            "build-2",
            "--status",
            "interrupted"
        ]))
        .await,
        0
    );
    let (_, list) = s.get("/api/v1/projects/cli/launches", ADMIN_TOKEN).await;
    assert_eq!(list["items"][0]["status"], "interrupted");
    let cli = fastproof_server::cli::Cli::try_parse_from(common(&[
        "upload",
        "/nonexistent/x",
        "--launch",
        "z",
    ]))
    .unwrap();
    assert!(fastproof_server::cli::run(cli).await.is_err());
}

#[tokio::test]
async fn cli_watch_uploads_while_tests_run() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("watch").await;
    let tf = token_file(s.dir.path());
    let results_dir = s.dir.path().join("allure-results");
    std::fs::create_dir_all(&results_dir).unwrap();
    let args = vec![
        "fastproof".to_string(),
        "watch".into(),
        results_dir.to_string_lossy().into(),
        "--url".into(),
        s.base.clone(),
        "--token-file".into(),
        tf.to_string_lossy().into(),
        "-p".into(),
        "watch".into(),
        "--launch".into(),
        "live-1".into(),
        "--interval-ms".into(),
        "150".into(),
        "--idle-exit".into(),
        "2".into(),
        "--finish".into(),
    ];
    let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
    let task = tokio::spawn(fastproof_server::cli::run(cli));
    let src = fixture("pytest/allure-results");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let (results, others): (Vec<PathBuf>, Vec<PathBuf>) = files
        .into_iter()
        .partition(|p| p.to_string_lossy().ends_with("-result.json"));
    let half = results.len() / 2;
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(results_dir.join(".allure-write-0001.tmp"), "partial").unwrap();
    for group in [others, results[..half].to_vec(), results[half..].to_vec()] {
        for f in group {
            std::fs::copy(&f, results_dir.join(f.file_name().unwrap())).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    let code = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(code, 0);
    let (_, list) = s.get("/api/v1/projects/watch/launches", ADMIN_TOKEN).await;
    let l = &list["items"][0];
    assert_eq!(l["status"], "finished", "{l}");
    let (_, series) = expected(&tar_gz(&src, &|_| true), None);
    assert_eq!(l["counts"]["total"], series as u64);
    let lid = l["id"].as_str().unwrap();
    let (_, uploads) = s
        .get(
            &format!("/api/v1/projects/watch/launches/{lid}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    let uploads = uploads["items"].as_array().unwrap();
    assert!(
        uploads.len() >= 2,
        "files went in several uploads: {}",
        uploads.len()
    );
    for u in uploads {
        assert_eq!(u["status"], "done", "{u}");
        let w = u["warnings"].to_string();
        assert!(
            !w.contains("not in the upload"),
            "attachments travel with their results: {w}"
        );
    }
}
