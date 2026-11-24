//! Import of Allure history: Allure 2 history directories and Allure 3 history files (generated
//! by the real tools, `tests/fixtures/allure-history`), joined to the series live results use,
//! idempotent, bounded, with malformed files. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/allure-history")
}

fn read(rel: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(rel)).unwrap()
}

fn allure2_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("history", read("allure2/history/history.json")),
        ("historyTrend", read("allure2/history/history-trend.json")),
        ("durationTrend", read("allure2/history/duration-trend.json")),
        (
            "categoriesTrend",
            read("allure2/history/categories-trend.json"),
        ),
        ("retryTrend", read("allure2/history/retry-trend.json")),
    ]
}

fn results_zip(edit: impl Fn(&mut Value)) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        let mut names: Vec<_> = std::fs::read_dir(fixtures().join("allure-results"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        for p in names {
            let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            edit(&mut v);
            z.start_file(p.file_name().unwrap().to_str().unwrap(), o)
                .unwrap();
            z.write_all(v.to_string().as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn multipart(boundary: &str, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (field, bytes) in files {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{field}.json\"\r\n\r\n"
            )
            .as_bytes(),
        );
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

async fn post_import(
    s: &Server,
    token: &str,
    project: &str,
    query: &str,
    files: &[(&str, Vec<u8>)],
) -> (StatusCode, Value) {
    let boundary = "fastproof-test-boundary";
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{project}/history-imports?{query}"),
            Some(token),
        )
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart(boundary, files))
        .send()
        .await
        .unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

async fn wait_import(s: &Server, project: &str, id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let (st, b) = s
            .get(
                &format!("/api/v1/projects/{project}/history-imports/{id}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        if b["status"] == "done" || b["status"] == "failed" {
            return b;
        }
        assert!(std::time::Instant::now() < deadline, "import not done: {b}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn import(s: &Server, project: &str, query: &str, files: &[(&str, Vec<u8>)]) -> Value {
    let (st, b) = post_import(s, ADMIN_TOKEN, project, query, files).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    wait_import(s, project, b["id"].as_str().unwrap()).await
}

async fn keep_forever(s: &Server, project: &str) {
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/{project}/settings"),
            Some(ADMIN_TOKEN),
            Some(json!({"retention": {"rawDays": 36500}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

async fn live_launch(s: &Server, project: &str, branch: &str, zip: Vec<u8>) -> String {
    let (st, l) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "live", "vcs": {"branch": branch}}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let id = l["id"].as_str().unwrap().to_string();
    let u = s.ingest(project, &id, zip, "?format=allure").await;
    assert_eq!(u["status"], "done", "{u}");
    id
}

async fn stats(db: &TestDb, project: &str, name: &str) -> (String, String) {
    sqlx::query_as(
        "select st.last_statuses, st.last_status from series_stats st \
         join series s on s.id = st.series_id join test_cases t on t.id = s.test_case_id \
         join projects p on p.id = s.project_id where p.key = $1 and t.full_name = $2",
    )
    .bind(project)
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn allure2_history_joins_live_series_in_time_order_and_is_idempotent() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ah").await;
    keep_forever(&s, "ah").await;
    let live = live_launch(&s, "ah", "main", results_zip(|_| {})).await;
    assert_eq!(count(&db, "select count(*) from series").await, 5);

    let j = import(&s, "ah", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["pointsTotal"], 3);
    assert_eq!(j["pointsDone"], 3);
    assert_eq!(j["launchesCreated"], 3);
    assert_eq!(j["resultsImported"], 15);
    assert_eq!(j["resultsUnmatched"], 0);
    assert_eq!(j["resultsExisting"], 0);
    assert_eq!(count(&db, "select count(*) from series").await, 5);
    assert_eq!(count(&db, "select count(*) from test_cases").await, 5);
    assert_eq!(count(&db, "select count(*) from launches").await, 4);

    let (_, list) = s
        .get("/api/v1/projects/ah/launches?limit=50", ADMIN_TOKEN)
        .await;
    let imported: Vec<&Value> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| {
            l["attributes"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x["key"] == "imported"))
        })
        .collect();
    assert_eq!(imported.len(), 3, "{list}");
    for l in &imported {
        assert_eq!(l["status"], "finished");
        assert!(l["startedAt"].as_str().unwrap().starts_with("2023-11-"));
        assert_eq!(l["counts"]["total"], 5);
        assert_eq!(l["vcs"]["branch"], "main");
    }
    let newest = imported
        .iter()
        .max_by_key(|l| l["startedAt"].as_str().unwrap().to_string())
        .unwrap();
    let attrs: Vec<(String, String)> = newest["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["key"].as_str().unwrap().into(),
                a["value"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(attrs.contains(&("history.report.total".into(), "5".into())));
    assert!(attrs.contains(&("history.report.durationMs".into(), "4530".into())));
    assert!(attrs.contains(&("history.report.runs".into(), "5".into())));

    let (a, b) = stats(&db, "ah", "com.example.CartTest.addsItem").await;
    assert_eq!((a.as_str(), b.as_str()), ("FPPP", "passed"));
    let (a, _) = stats(&db, "ah", "com.example.CartTest.removesItem").await;
    assert_eq!(a, "PBPP");
    let (first, last): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select min(first_seen_at), max(last_seen_at) from test_cases")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(first.to_rfc3339().starts_with("2023-11-"), "{first}");
    assert!(last > first + chrono::Duration::days(365), "{last}");
    let last_launch: Option<String> = sqlx::query_scalar(
        "select last_launch_id::text from series_stats st join series s on s.id = st.series_id \
         join test_cases t on t.id = s.test_case_id where t.full_name = 'com.example.CartTest.addsItem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(last_launch.as_deref(), Some(live.as_str()));
    let runs: i32 = sqlx::query_scalar(
        "select runs_tracked from series_stats st join series s on s.id = st.series_id \
         join test_cases t on t.id = s.test_case_id where t.full_name = 'com.example.CartTest.addsItem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(runs, 4);

    let again = import(&s, "ah", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(again["status"], "done", "{again}");
    assert_eq!(again["launchesCreated"], 0);
    assert_eq!(again["resultsImported"], 0);
    assert_eq!(again["resultsExisting"], 15);
    assert_eq!(count(&db, "select count(*) from launches").await, 4);
    assert_eq!(count(&db, "select count(*) from results").await, 20);
    let (a, _) = stats(&db, "ah", "com.example.CartTest.addsItem").await;
    assert_eq!(a, "FPPP");

    let (_, one) = s
        .get(
            &format!(
                "/api/v1/projects/ah/history-imports/{}",
                again["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(one["status"], "done");
    let (_, all) = s
        .get("/api/v1/projects/ah/history-imports", ADMIN_TOKEN)
        .await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2);
    let files_left = count(
        &db,
        "select count(*) from history_imports where files <> '[]'::jsonb and status = 'done'",
    )
    .await;
    assert!(files_left <= 2);
}

#[tokio::test]
async fn imported_history_feeds_transitions_and_new_failures_of_live_runs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("nf").await;
    keep_forever(&s, "nf").await;
    let live = live_launch(&s, "nf", "main", results_zip(|_| {})).await;
    import(&s, "nf", "source=allure2&branch=main", &allure2_files()).await;
    let failing = live_launch(
        &s,
        "nf",
        "main",
        results_zip(|v| {
            if v["name"] == "logsIn" {
                v["status"] = json!("failed");
            }
        }),
    )
    .await;
    assert_ne!(live, failing);
    let r = s.results("nf", &failing, "").await;
    let logs_in = r.iter().find(|x| x["name"] == "logsIn").unwrap();
    assert_eq!(logs_in["transition"], "regressed", "{logs_in}");
    let stable = r
        .iter()
        .find(|x| x["name"] == "rejectsBadPassword")
        .unwrap();
    assert!(stable["transition"].is_null(), "{stable}");
    let (st, nf) = s
        .get(
            &format!("/api/v1/projects/nf/launches/{failing}/results?newFailures=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{nf}");
    assert!(
        nf["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "logsIn"),
        "{nf}"
    );
    let (a, _) = stats(&db, "nf", "com.example.LoginTest.logsIn").await;
    assert_eq!(a, "PPPPF");
}

#[tokio::test]
async fn allure3_history_creates_named_tests_that_live_results_join() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("a3").await;
    keep_forever(&s, "a3").await;
    let file = read("allure3/history.jsonl");
    let j = import(
        &s,
        "a3",
        "source=allure3&branch=main",
        &[("history", file.clone())],
    )
    .await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["pointsDone"], 3);
    assert_eq!(j["launchesCreated"], 3);
    assert_eq!(j["resultsImported"], 15);
    assert_eq!(count(&db, "select count(*) from series").await, 5);
    let names: Vec<String> = sqlx::query_scalar("select full_name from test_cases order by 1")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(
        names.contains(&"com.example.CartTest.addsItem".to_string()),
        "{names:?}"
    );

    let live = live_launch(&s, "a3", "main", results_zip(|_| {})).await;
    assert_eq!(count(&db, "select count(*) from series").await, 5);
    assert_eq!(count(&db, "select count(*) from test_cases").await, 5);
    let (a, _) = stats(&db, "a3", "com.example.CartTest.addsItem").await;
    assert_eq!(a, "FPPP");
    let r = s.results("a3", &live, "").await;
    assert_eq!(r.len(), 5);
    assert!(r.iter().all(|x| x["transition"].is_null()), "{r:?}");

    let again = import(
        &s,
        "a3",
        "source=allure3&branch=main",
        &[("history", file.clone())],
    )
    .await;
    assert_eq!(again["launchesCreated"], 0);
    assert_eq!(again["resultsExisting"], 15);
    let other = import(
        &s,
        "a3",
        "source=allure3&branch=release",
        &[("history", file)],
    )
    .await;
    assert_eq!(
        other["launchesCreated"], 3,
        "another branch is another history: {other}"
    );
}

#[tokio::test]
async fn tests_the_project_does_not_know_are_counted_and_old_reports_left_out() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("un").await;
    keep_forever(&s, "un").await;
    let j = import(&s, "un", "source=allure2", &allure2_files()).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["launchesCreated"], 0);
    assert_eq!(j["resultsUnmatched"], 15);
    assert!(
        j["warnings"][0].as_str().unwrap().contains("does not know"),
        "{j}"
    );
    assert_eq!(count(&db, "select count(*) from launches").await, 0);

    s.project("old").await;
    live_launch(&s, "old", "main", results_zip(|_| {})).await;
    let j = import(&s, "old", "source=allure2", &allure2_files()).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["launchesCreated"], 0);
    assert_eq!(j["resultsSkipped"], 15);
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("retention window")),
        "{j}"
    );

    s.project("env").await;
    keep_forever(&s, "env").await;
    live_launch(&s, "env", "main", results_zip(|_| {})).await;
    let j = import(
        &s,
        "env",
        "source=allure2&environment=stage",
        &allure2_files(),
    )
    .await;
    assert_eq!(j["resultsUnmatched"], 0, "{j}");
    assert_eq!(
        j["resultsImported"], 15,
        "a known test gets its series in the new environment: {j}"
    );
    assert_eq!(
        count(&db, "select count(*) from series s join projects p on p.id = s.project_id where p.key = 'env'").await,
        10
    );
    assert_eq!(
        count(&db, "select count(*) from test_cases t join projects p on p.id = t.project_id where p.key = 'env'").await,
        5
    );
}

#[tokio::test]
async fn bad_requests_malformed_files_and_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("bad").await;
    keep_forever(&s, "bad").await;
    let hist = read("allure2/history/history.json");

    for (q, files, code) in [
        ("source=nope", vec![("history", hist.clone())], 400),
        ("source=allure2", vec![("other", hist.clone())], 400),
        ("source=allure2", vec![], 400),
        ("source=allure2", vec![("history", Vec::new())], 400),
        (
            "source=allure2",
            vec![("history", hist.clone()), ("history", hist.clone())],
            400,
        ),
        (
            "source=allure3",
            vec![("history", hist.clone()), ("retryTrend", b"[]".to_vec())],
            400,
        ),
        (
            "source=allure2&branch=a%00b",
            vec![("history", hist.clone())],
            400,
        ),
        (
            "source=allure2",
            vec![
                ("history", hist.clone()),
                ("retryTrend", vec![b' '; 4 * 1024 * 1024 + 1]),
            ],
            413,
        ),
    ] {
        let (st, b) = post_import(&s, ADMIN_TOKEN, "bad", q, &files).await;
        assert_eq!(st.as_u16(), code, "{q}: {b}");
    }
    let r = s
        .req(
            Method::POST,
            "/api/v1/projects/bad/history-imports?source=allure2",
            Some(ADMIN_TOKEN),
        )
        .body(hist.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(count(&db, "select count(*) from history_imports").await, 0);

    for (source, bytes, needle) in [
        ("allure2", b"{\"a\":".to_vec(), "not an Allure 2 history"),
        ("allure2", b"[1,2,3]".to_vec(), "not an Allure 2 history"),
        ("allure2", b"{}".to_vec(), "no past reports"),
        (
            "allure3",
            b"not json\n[1]\n{}\n".to_vec(),
            "no past reports",
        ),
        (
            "allure2",
            format!("{}{}", "[".repeat(100), "]".repeat(100)).into_bytes(),
            "max_json_depth",
        ),
    ] {
        let j = import(
            &s,
            "bad",
            &format!("source={source}"),
            &[("history", bytes)],
        )
        .await;
        assert_eq!(j["status"], "failed", "{source}: {j}");
        assert!(j["error"].as_str().unwrap().contains(needle), "{j}");
    }

    let mixed = format!(
        "garbage\n{}",
        String::from_utf8(read("allure3/history.jsonl")).unwrap()
    );
    let j = import(
        &s,
        "bad",
        "source=allure3",
        &[("history", mixed.into_bytes())],
    )
    .await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["launchesCreated"], 3);
    assert!(
        j["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("skipped")),
        "{j}"
    );

    let viewer = s.token("viewer", "viewer", &["bad"]).await;
    let (st, _) = post_import(&s, &viewer, "bad", "source=allure2", &allure2_files()).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.get("/api/v1/projects/bad/history-imports", &viewer).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s.get("/api/v1/projects/bad/history-imports", "wrong").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = s
        .get(
            "/api/v1/projects/bad/history-imports/00000000-0000-0000-0000-000000000000",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    sqlx::query(
        "insert into history_imports (id, project_id, source, status, created_by) \
         select gen_random_uuid(), id, 'allure2', 'queued', 'x' from projects where key = 'bad'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (st, b) = post_import(&s, ADMIN_TOKEN, "bad", "source=allure2", &allure2_files()).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "history_import_busy");
    let blobs = count(
        &db,
        "select count(*) from history_imports where status = 'queued'",
    )
    .await;
    assert_eq!(blobs, 1);
}

async fn run_cli(s: &Server, token_file: &std::path::Path, extra: &[&str]) -> i32 {
    use clap::Parser;
    let mut args: Vec<String> = ["fastproof", "import", "allure-history"]
        .iter()
        .map(|x| x.to_string())
        .collect();
    args.extend(extra.iter().map(|x| x.to_string()));
    args.extend(
        [
            "--url",
            &s.base,
            "--token-file",
            token_file.to_str().unwrap(),
            "-p",
            "cli",
        ]
        .iter()
        .map(|x| x.to_string()),
    );
    let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
    fastproof_server::cli::run(cli).await.unwrap()
}

#[tokio::test]
async fn the_command_line_sends_a_history_directory_and_a_history_file() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cli").await;
    keep_forever(&s, "cli").await;
    live_launch(&s, "cli", "main", results_zip(|_| {})).await;
    let tmp = tempfile::tempdir().unwrap();
    let token_file = tmp.path().join("token");
    std::fs::write(&token_file, ADMIN_TOKEN).unwrap();

    let dir = fixtures().join("allure2");
    let code = run_cli(
        &s,
        &token_file,
        &[dir.to_str().unwrap(), "--branch", "main"],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(count(&db, "select count(*) from launches").await, 4);
    let (a, _) = stats(&db, "cli", "com.example.CartTest.addsItem").await;
    assert_eq!(a, "FPPP");

    let file = fixtures().join("allure3/history.jsonl");
    let code = run_cli(
        &s,
        &token_file,
        &[file.to_str().unwrap(), "--branch", "release", "--no-wait"],
    )
    .await;
    assert_eq!(code, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (_, l) = s
            .get("/api/v1/projects/cli/history-imports", ADMIN_TOKEN)
            .await;
        if l["items"][0]["status"] == "done" {
            assert_eq!(l["items"][0]["source"], "allure3");
            assert_eq!(l["items"][0]["launchesCreated"], 3);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{l}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let broken = tmp.path().join("history.json");
    std::fs::write(&broken, b"{\"a\":").unwrap();
    let code = run_cli(
        &s,
        &token_file,
        &[broken.to_str().unwrap(), "--source", "allure2"],
    )
    .await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn results_dated_in_the_future_are_left_out() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("win").await;
    keep_forever(&s, "win").await;
    let text = String::from_utf8(read("allure3/history.jsonl")).unwrap();
    let first = text.lines().next().unwrap();
    let mut point: Value = serde_json::from_str(first).unwrap();
    let results = point["testResults"].as_object_mut().unwrap();
    let mut keys: Vec<String> = results.keys().cloned().collect();
    keys.sort();
    results.get_mut(&keys[0]).unwrap()["start"] = json!(4_000_000_000_000i64);
    results.get_mut(&keys[0]).unwrap()["stop"] = json!(4_000_000_000_500i64);
    let j = import(
        &s,
        "win",
        "source=allure3",
        &[("history", format!("{point}\n").into_bytes())],
    )
    .await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["resultsImported"], 4, "{j}");
    assert_eq!(j["resultsSkipped"], 1, "{j}");
    let year: i32 =
        sqlx::query_scalar("select max(extract(year from created_at))::int from launches")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(year <= 2026, "{year}");
}

fn app(s: &Server) -> &fastproof_server::state::AppState {
    &s.running.as_ref().unwrap().state
}

fn point_with(edit: impl Fn(&mut Value, &[String])) -> Vec<u8> {
    let text = String::from_utf8(read("allure3/history.jsonl")).unwrap();
    let mut point: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let mut keys: Vec<String> = point["testResults"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    keys.sort();
    edit(&mut point, &keys);
    format!("{point}\n").into_bytes()
}

#[tokio::test]
async fn a1_a3_the_statistics_are_locked_and_a_failed_chunk_leaves_nothing_to_repair() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lk").await;
    keep_forever(&s, "lk").await;
    live_launch(&s, "lk", "main", results_zip(|_| {})).await;
    let before = stats(&db, "lk", "com.example.CartTest.addsItem").await;

    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("select 1 from series_stats for update")
        .fetch_all(&mut *blocker)
        .await
        .unwrap();
    let (st, b) = post_import(
        &s,
        ADMIN_TOKEN,
        "lk",
        "source=allure2&branch=main",
        &allure2_files(),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let id = b["id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (_, j) = s
        .get(
            &format!("/api/v1/projects/lk/history-imports/{id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        j["status"], "running",
        "the import waits for the statistics rows: {j}"
    );
    assert_eq!(
        count(
            &db,
            "select count(*) from series_stats where runs_tracked > 1"
        )
        .await,
        0
    );
    blocker.commit().await.unwrap();
    let j = wait_import(&s, "lk", &id).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(
        stats(&db, "lk", "com.example.CartTest.addsItem").await.0,
        "FPPP"
    );
    assert_ne!(before.0, "FPPP");

    s.project("fl").await;
    keep_forever(&s, "fl").await;
    live_launch(&s, "fl", "main", results_zip(|_| {})).await;
    sqlx::query(
        "create function history_test_boom() returns trigger language plpgsql as \
         $$ begin raise exception 'boom'; end $$",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger history_test_boom before update on series_stats \
         for each row execute function history_test_boom()",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let j = import(&s, "fl", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(j["status"], "failed", "{j}");
    assert_eq!(count(&db, "select count(*) from results r join launches l on l.id = r.launch_id join projects p on p.id = l.project_id where p.key = 'fl'").await, 5);
    assert_eq!(
        stats(&db, "fl", "com.example.CartTest.addsItem").await.0,
        "P"
    );
    sqlx::query("drop trigger history_test_boom on series_stats")
        .execute(&db.pool)
        .await
        .unwrap();
    let j = import(&s, "fl", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["resultsImported"], 15);
    assert_eq!(
        stats(&db, "fl", "com.example.CartTest.addsItem").await.0,
        "FPPP"
    );
}

#[tokio::test]
async fn a2_times_are_bounded_and_nothing_lasts_in_the_future() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tm").await;
    keep_forever(&s, "tm").await;
    let now = chrono::Utc::now().timestamp_millis();
    let bytes = point_with(|p, keys| {
        p["timestamp"] = json!(now - 60_000);
        for (i, k) in keys.iter().enumerate() {
            let r = &mut p["testResults"][k];
            r["start"] = json!(now - 120_000);
            r["stop"] = json!(now - 119_000);
            r["duration"] = json!(1000);
            match i {
                0 => r["stop"] = json!(4_000_000_000_000i64),
                1 => r["duration"] = json!(315_000_000_000i64),
                2 => r["stop"] = json!(now + 3_600_000),
                _ => {}
            }
        }
    });
    let j = import(&s, "tm", "source=allure3", &[("history", bytes)]).await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["resultsImported"], 2, "{j}");
    assert_eq!(j["resultsSkipped"], 3, "{j}");
    for q in [
        "select count(*) from launches where finished_at > now() or created_at > now()",
        "select count(*) from series where last_seen_at > now() or first_seen_at > now()",
        "select count(*) from test_cases where last_seen_at > now() or first_seen_at > now()",
        "select count(*) from projects where last_launch_at > now()",
    ] {
        assert_eq!(count(&db, q).await, 0, "{q}");
    }
    let ahead = point_with(|p, _| p["timestamp"] = json!(now + 600_000));
    let j = import(&s, "tm", "source=allure3&branch=x", &[("history", ahead)]).await;
    assert_eq!(j["launchesCreated"], 0, "{j}");
    let old = point_with(|p, keys| {
        p["timestamp"] = json!(1_000_000_000_000i64);
        for k in keys {
            p["testResults"][k]["start"] = json!(1_000_000_000_000i64);
            p["testResults"][k]["stop"] = json!(1_000_000_001_000i64);
        }
    });
    let j = import(&s, "tm", "source=allure3&branch=y", &[("history", old)]).await;
    assert_eq!(
        j["launchesCreated"], 0,
        "older than ten years is never imported: {j}"
    );
}

#[tokio::test]
async fn a5_a6_the_caps_are_checked_before_the_body_and_per_caller_and_server() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("p0").await;
    keep_forever(&s, "p0").await;
    live_launch(&s, "p0", "main", results_zip(|_| {})).await;
    import(&s, "p0", "source=allure2&branch=main", &allure2_files()).await;
    let me: String = sqlx::query_scalar("select owner_key from history_imports limit 1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    s.project("p1").await;
    s.project("p2").await;
    let big = vec![
        ("history", read("allure2/history/history.json")),
        ("retryTrend", vec![b' '; 4 * 1024 * 1024 + 1]),
    ];
    sqlx::query(
        "insert into history_imports (id, project_id, source, status, created_by, owner_key) \
         select gen_random_uuid(), id, 'allure2', 'running', 'x', 'other' from projects where key = 'p1'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (st, b) = post_import(&s, ADMIN_TOKEN, "p1", "source=allure2", &big).await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "busy answers before the oversize body is read: {b}"
    );

    sqlx::query(
        "insert into history_imports (id, project_id, source, status, created_by, owner_key) \
         select gen_random_uuid(), id, 'allure2', 'running', 'x', $1 from projects where key = 'p2'",
    )
    .bind(&me)
    .execute(&db.pool)
    .await
    .unwrap();
    s.project("p3").await;
    let (st, b) = post_import(&s, ADMIN_TOKEN, "p3", "source=allure2", &allure2_files()).await;
    assert_eq!(st, StatusCode::CONFLICT, "one import per caller: {b}");
    assert_eq!(b["code"], "history_import_busy");

    sqlx::query("update history_imports set owner_key = 'o2' where project_id = (select id from projects where key = 'p2')")
        .execute(&db.pool)
        .await
        .unwrap();
    for i in 0..2 {
        sqlx::query(
            "insert into history_imports (id, project_id, source, status, created_by, owner_key) \
             select gen_random_uuid(), id, 'allure2', 'queued', 'x', $1 from projects where key = 'p3'",
        )
        .bind(format!("o{}", i + 3))
        .execute(&db.pool)
        .await
        .unwrap();
    }
    s.project("p4").await;
    let (st, b) = post_import(&s, ADMIN_TOKEN, "p4", "source=allure2", &allure2_files()).await;
    assert_eq!(st, StatusCode::CONFLICT, "four on the server: {b}");

    sqlx::query("update history_imports set updated_at = now() - interval '13 hours' where status = 'queued'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(app(&s).store.fail_stale_history_imports().await.unwrap(), 2);
}

#[tokio::test]
async fn a8_a9_a10_imports_never_write_into_foreign_launches_run_no_analysis_and_keep_names() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("src").await;
    keep_forever(&s, "src").await;
    live_launch(&s, "src", "main", results_zip(|_| {})).await;
    import(&s, "src", "source=allure2&branch=main", &allure2_files()).await;
    let ext: Vec<String> = sqlx::query_scalar(
        "select external_id from launches where external_id like 'allure-history:%' order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(ext.len(), 3);

    s.project("vic").await;
    keep_forever(&s, "vic").await;
    live_launch(&s, "vic", "main", results_zip(|_| {})).await;
    let (st, l) = s
        .post(
            "/api/v1/projects/vic/launches",
            ADMIN_TOKEN,
            json!({"name": "mine", "externalId": ext[0], "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let hijacked = l["id"].as_str().unwrap().to_string();
    let events_before = count(&db, "select count(*) from events where type in ('result.updated', 'attempt.stored', 'launch.created')").await;
    let j = import(&s, "vic", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(j["launchesCreated"], 3, "{j}");
    let mine = s.launch_get("vic", &hijacked).await;
    assert_eq!(mine["counts"]["total"], 0, "{mine}");
    assert_eq!(
        count(&db, "select count(*) from launches where external_id like 'allure-history:%' and project_id = (select id from projects where key = 'vic')").await,
        4
    );
    assert_eq!(
        count(&db, "select count(*) from events where type in ('result.updated', 'attempt.stored', 'launch.created')").await,
        events_before,
        "the import emits no events"
    );
    for q in [
        "select count(*) from results where defect is not null and launch_id in (select id from launches where attributes @> '[{\"key\":\"imported\"}]')",
        "select count(*) from series_mutes",
        "select count(*) from failure_fingerprints",
    ] {
        assert_eq!(count(&db, q).await, 0, "{q}");
    }

    s.project("nm").await;
    keep_forever(&s, "nm").await;
    live_launch(&s, "nm", "main", results_zip(|_| {})).await;
    let live_name: String = sqlx::query_scalar(
        "select display_name from test_cases t join projects p on p.id = t.project_id \
         where p.key = 'nm' and t.full_name = 'com.example.CartTest.addsItem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let tc: String = sqlx::query_scalar(
        "select test_case_hash from test_cases t join projects p on p.id = t.project_id \
         where p.key = 'nm' and t.full_name = 'com.example.CartTest.addsItem'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let other = format!("{tc}.{}", "ab".repeat(16));
    let bytes = point_with(|p, keys| {
        let r = p["testResults"][&keys[0]].clone();
        let mut r = r;
        r["name"] = json!("OLD NAME");
        r["fullName"] = json!("old.Name");
        p["testResults"] = json!({});
        p["testResults"][&other] = r;
        let mut mismatch = p["testResults"][&other].clone();
        mismatch["environment"] = json!("stage");
        p["testResults"][format!(
            "{}.{}.{}",
            "cd".repeat(16),
            "ef".repeat(16),
            "00".repeat(16)
        )] = mismatch;
        let mut missing = p["testResults"][&other].clone();
        missing["environment"] = json!("default");
        p["testResults"][format!(
            "{}.{}.{}",
            "ab".repeat(16),
            "ef".repeat(16),
            "11".repeat(16)
        )] = missing;
    });
    let j = import(
        &s,
        "nm",
        "source=allure3&branch=main",
        &[("history", bytes)],
    )
    .await;
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["resultsImported"], 1, "{j}");
    assert_eq!(j["resultsSkipped"], 2, "{j}");
    let names: Vec<String> = sqlx::query_scalar("select display_name from test_cases t join projects p on p.id = t.project_id where p.key = 'nm' and t.test_case_hash = $1")
        .bind(&tc)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(names, vec![live_name]);
}

#[tokio::test]
async fn a12_permission_upload_limit_audit_and_project_deletion() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_MAX_UPLOAD_BYTES", "3000")]).await;
    s.project("ad").await;
    keep_forever(&s, "ad").await;
    live_launch(&s, "ad", "main", results_zip(|_| {})).await;
    let uploader = s.token("ci", "uploader", &["ad"]).await;
    let (st, _) = post_import(&s, &uploader, "ad", "source=allure2", &allure2_files()).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a CI token with launches:write cannot import"
    );

    let (st, b) = post_import(
        &s,
        ADMIN_TOKEN,
        "ad",
        "source=allure2",
        &[("history", vec![b' '; 3001])],
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    let j = import(&s, "ad", "source=allure2&branch=main", &allure2_files()).await;
    assert_eq!(j["status"], "done", "{j}");
    let bad = import(
        &s,
        "ad",
        "source=allure2",
        &[("history", b"{\"a\":".to_vec())],
    )
    .await;
    assert_eq!(bad["status"], "failed");
    let (_, a) = s
        .get("/api/v1/audit?action=history.import*&limit=50", ADMIN_TOKEN)
        .await;
    let actions: Vec<String> = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["action"].as_str().unwrap().to_string())
        .collect();
    for want in [
        "history.import",
        "history.import.done",
        "history.import.failed",
    ] {
        assert!(actions.iter().any(|x| x == want), "{want} in {actions:?}");
    }

    sqlx::query(
        "insert into history_imports (id, project_id, source, status, created_by, files) \
         select gen_random_uuid(), id, 'allure2', 'failed', 'x', \
           '[{\"name\":\"history\",\"blobKey\":\"tm-jobs/abc/history\",\"size\":1}]'::jsonb from projects where key = 'ad'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (st, b) = s
        .call(
            Method::DELETE,
            "/api/v1/projects/ad",
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    assert_eq!(
        count(
            &db,
            "select count(*) from blob_deletions where prefix = 'tm-jobs/abc/history'"
        )
        .await,
        1
    );
}
