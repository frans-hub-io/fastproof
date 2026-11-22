//! Regressions from the black-box test: live results whose final line carries another identity
//! than the `running` line, ANSI escapes in error messages, settings merged per area, audit of
//! launch creation and uploads, log line limits, the platform routes in the OpenAPI document,
//! and the CLI finishing launches.

mod common;

use std::path::{Path, PathBuf};

use common::{ADMIN_TOKEN, Server, TestDb, fixture};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const T0: i64 = 1_795_249_000_000;

async fn ndjson(s: &Server, base: &str, lines: &[Value]) -> Value {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let r = s
        .req(Method::POST, &format!("{base}/results"), Some(ADMIN_TOKEN))
        .header("content-type", "application/x-ndjson")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    r.json().await.unwrap()
}

fn running(id: &str, name: &str, full: &str) -> Value {
    json!({"id": id, "name": name, "fullName": full, "status": "running", "start": T0, "retry": 0})
}

fn done(id: &str, name: &str, full: &str, status: &str) -> Value {
    json!({"id": id, "name": name, "fullName": full, "status": status, "start": T0, "stop": T0 + 50,
        "retry": 0})
}

async fn orphans(db: &TestDb) -> (i64, i64) {
    let series: i64 = sqlx::query_scalar(
        "select count(*) from series s where not exists (select 1 from results r where r.series_id = s.id)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let cases: i64 = sqlx::query_scalar(
        "select count(*) from test_cases t where not exists (select 1 from series s where s.test_case_id = t.id)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (series, cases)
}

#[tokio::test]
async fn live_final_line_with_another_identity_completes_the_running_result() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("bb").await;
    let l = s.launch("bb", None).await;
    let lid = l["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/bb/launches/{lid}");

    // Separate requests: the final adds a test case id, parameters, or another full name.
    for (id, extra) in [
        ("probe-1", json!({"testCaseId": "CHECKOUT-1"})),
        (
            "probe-5",
            json!({"parameters": [{"name": "browser", "value": "chrome"}]}),
        ),
        ("probe-6", json!({"fullName": "probe#probe-renamed"})),
    ] {
        let out = ndjson(
            &s,
            &base,
            &[running(id, &format!("{id} test"), &format!("probe#{id}"))],
        )
        .await;
        assert_eq!(out["accepted"], 1, "{out}");
        let mut fin = done(id, &format!("{id} test"), &format!("probe#{id}"), "passed");
        for (k, v) in extra.as_object().unwrap() {
            fin[k] = v.clone();
        }
        let out = ndjson(&s, &base, &[fin.clone()]).await;
        assert_eq!(
            (out["accepted"].as_u64(), out["duplicates"].as_u64()),
            (Some(1), Some(0)),
            "{out}"
        );
        let out = ndjson(&s, &base, &[fin]).await;
        assert_eq!(
            (out["accepted"].as_u64(), out["duplicates"].as_u64()),
            (Some(0), Some(1)),
            "{out}"
        );
    }

    // Running and final of the same id in one batch: both applied, nothing is a duplicate.
    let mut fin = done("probe-7", "probe 7", "probe#probe-7", "failed");
    fin["testCaseId"] = json!("CHECKOUT-7");
    fin["error"] = json!({"message": "boom"});
    let out = ndjson(
        &s,
        &base,
        &[running("probe-7", "probe 7", "probe#probe-7"), fin],
    )
    .await;
    assert_eq!(
        (out["accepted"].as_u64(), out["duplicates"].as_u64()),
        (Some(2), Some(0)),
        "{out}"
    );

    // pytest parametrize: both running lines carry no parameters (one series); the finals do.
    let full = "tests/test_param.py::test_p";
    let out = ndjson(
        &s,
        &base,
        &[
            running("py-c", "test_p[chrome]", full),
            running("py-f", "test_p[firefox]", full),
        ],
    )
    .await;
    assert_eq!(out["accepted"], 2);
    let rows = s.results("bb", lid, "&q=test_p").await;
    assert_eq!(
        rows.len(),
        1,
        "one running series before the finals: {rows:?}"
    );
    let mut chrome = done("py-c", "test_p[chrome]", full, "passed");
    chrome["parameters"] = json!([{"name": "browser", "value": "chrome"}]);
    let mut firefox = done("py-f", "test_p[firefox]", full, "failed");
    firefox["parameters"] = json!([{"name": "browser", "value": "firefox"}]);
    firefox["error"] = json!({"message": "assert 1 == 2"});
    ndjson(&s, &base, &[chrome]).await;
    let rows = s.results("bb", lid, "&q=test_p").await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().any(|r| r["status"] == "running"),
        "firefox still runs"
    );
    ndjson(&s, &base, &[firefox]).await;

    let rows = s.results("bb", lid, "").await;
    assert!(
        rows.iter().all(|r| r["status"] != "running"),
        "no result stays running: {rows:?}"
    );
    let status = |name: &str| -> Vec<(String, i64)> {
        rows.iter()
            .filter(|r| r["name"] == name)
            .map(|r| {
                (
                    r["status"].as_str().unwrap().to_string(),
                    r["attemptCount"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(status("probe-1 test"), vec![("passed".into(), 1)]);
    assert_eq!(status("probe-5 test"), vec![("passed".into(), 1)]);
    assert_eq!(status("probe-6 test"), vec![("passed".into(), 1)]);
    assert_eq!(status("probe 7"), vec![("failed".into(), 1)]);
    assert_eq!(status("test_p[chrome]"), vec![("passed".into(), 1)]);
    assert_eq!(status("test_p[firefox]"), vec![("failed".into(), 1)]);
    assert_eq!(rows.len(), 6, "{rows:?}");
    let six = rows.iter().find(|r| r["name"] == "probe-6 test").unwrap();
    assert_eq!(six["fullName"], "probe#probe-renamed");
    assert_eq!(six["clientIds"], json!(["probe-6"]));

    let l = s.launch_get("bb", lid).await;
    assert_eq!(l["counts"]["total"], 6, "{l}");
    assert_eq!(l["counts"]["passed"], 4);
    assert_eq!(l["counts"]["failed"], 2);
    assert_eq!(
        orphans(&db).await,
        (0, 0),
        "series and test cases of the running identities are gone"
    );

    // Logs keyed by the client id still belong to the moved attempt.
    let (st, d) = s
        .get(
            &format!(
                "/api/v1/projects/bb/results/{}",
                six["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["attempts"].as_array().unwrap().len(), 1, "{d}");
    let removed: i64 =
        sqlx::query_scalar("select count(*) from events where type = 'result.removed'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(removed >= 4, "{removed}");
}

#[tokio::test]
async fn ansi_escapes_are_removed_from_error_texts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ansi").await;
    let l = s.launch("ansi", None).await;
    let lid = l["id"].as_str().unwrap();
    let base = format!("/api/v1/projects/ansi/launches/{lid}");
    let raw = "Error: \u{1b}[2mexpect(\u{1b}[22m\u{1b}[31mlocator\u{1b}[39m\u{1b}[2m).toBeVisible()\u{1b}[22m \u{1b}]8;;https://example.com\u{7}link\u{1b}]8;;\u{7}";
    let mut a = done("a1", "colour", "ansi#colour", "failed");
    a["error"] = json!({"message": raw, "trace": format!("{raw}\n    at spec.ts:3")});
    a["stdout"] = json!("\u{1b}[32mgreen output\u{1b}[0m");
    let mut b = done("a2", "plain", "ansi#plain", "failed");
    b["error"] = json!({"message": "Error: expect(locator).toBeVisible() link", "trace": "Error: expect(locator).toBeVisible() link\n    at spec.ts:3"});
    ndjson(&s, &base, &[a, b]).await;
    let rows = s.results("ansi", lid, "").await;
    let colour = rows.iter().find(|r| r["name"] == "colour").unwrap();
    let plain = rows.iter().find(|r| r["name"] == "plain").unwrap();
    assert_eq!(
        colour["message"], "Error: expect(locator).toBeVisible() link",
        "{colour}"
    );
    assert_eq!(
        colour["fingerprint"], plain["fingerprint"],
        "same failure, same fingerprint"
    );
    let (_, d) = s
        .get(
            &format!(
                "/api/v1/projects/ansi/results/{}",
                colour["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let text = d.to_string();
    assert!(
        text.contains("\\u001b[32mgreen output"),
        "stdout keeps its colours: {text}"
    );
    assert!(
        !d["attempts"][0]["error"].to_string().contains("\\u001b"),
        "{}",
        d["attempts"][0]
    );
    let hits = s.results("ansi", lid, "&q=locator").await;
    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn put_settings_keeps_areas_left_out() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("set").await;
    let path = "/api/v1/projects/set/settings";
    let put = |body: Value| s.call(Method::PUT, path, Some(ADMIN_TOKEN), Some(body));
    let (st, b) = put(json!({
        "gates": [{"name": "main", "enabled": true, "rules": [{"metric": "passRate", "op": "gte", "value": 90}]}],
        "issueTrackers": [{"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}", "keyRegex": "[A-Z]+-[0-9]+"}],
        "flaky": {"historyDepth": 20, "stabilizationPeriod": 5, "includePassedTests": false}
    }))
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = put(json!({"retention": {"rawDays": 30, "keepImportant": true}})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["retention"]["rawDays"], 30);
    assert_eq!(b["gates"].as_array().unwrap().len(), 1, "gates kept: {b}");
    assert_eq!(b["issueTrackers"].as_array().unwrap().len(), 1);
    assert_eq!(b["flaky"]["historyDepth"], 20);
    let (_, b) = put(json!({"issueTrackers": null, "flaky": null})).await;
    assert_eq!(b["issueTrackers"], json!([]), "null resets an area: {b}");
    assert_eq!(b["flaky"]["historyDepth"], 10);
    assert_eq!(b["gates"].as_array().unwrap().len(), 1);
    let (st, _) = put(json!([1])).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, b) = put(json!({"retention": {"rawDays": 0}})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (_, b) = s.get(path, ADMIN_TOKEN).await;
    assert_eq!(
        b["retention"]["rawDays"], 30,
        "a rejected PUT changes nothing"
    );
}

#[tokio::test]
async fn audit_records_launches_uploads_rules_and_triage() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("aud").await;
    let editor = s.token("ed", "editor", &["aud"]).await;
    let (st, l) = s
        .post(
            "/api/v1/projects/aud/launches",
            &editor,
            json!({"name": "n", "externalId": "x-1"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let lid = l["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            "/api/v1/projects/aud/launches",
            &editor,
            json!({"name": "n", "externalId": "x-1"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "joining is not a change");
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/aud/launches/{lid}/uploads?format=junit"),
            Some(&editor),
        )
        .header("content-type", "application/octet-stream")
        .body(common::read("gotestsum/junit.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let up: Value = r.json().await.unwrap();
    s.wait_upload("aud", lid, up["id"].as_str().unwrap()).await;
    let results = s.results("aud", lid, "&status=failed,broken").await;
    let (st, b) = s
        .post(
            "/api/v1/projects/aud/rules",
            &editor,
            json!({"name": "r1", "match": {"messageRegex": "x"}, "action": {"comment": "c"}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    if let Some(r) = results.first() {
        let (st, b) = s
            .post(
                "/api/v1/projects/aud/triage",
                &editor,
                json!({"resultIds": [r["id"]], "comment": "looked"}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
    }
    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    let actions: Vec<&str> = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    for want in ["launch.create", "upload.create", "rule.create"] {
        assert_eq!(
            actions.iter().filter(|x| **x == want).count(),
            1,
            "{want}: {actions:?}"
        );
    }
    if !results.is_empty() {
        assert!(actions.contains(&"results.triage"), "{actions:?}");
    }
    let item = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "upload.create")
        .unwrap();
    assert_eq!(item["principal"], "ed");
    assert_eq!(item["scope"], "aud");
    assert_eq!(item["detail"]["launchId"], lid);
}

#[tokio::test]
async fn log_lines_are_cut_and_limits_name_their_unit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("logs").await;
    let l = s.launch("logs", None).await;
    let lid = l["id"].as_str().unwrap();
    let path = format!("/api/v1/projects/logs/launches/{lid}/logs");
    let long = "é".repeat(40_000);
    let lines = json!([
        {"resultId": "r1", "attempt": 0, "seq": 1, "ts": T0, "level": "info", "stream": "log", "message": long},
        {"resultId": "r1", "attempt": 0, "seq": 2, "ts": "2026-11-21T10:00:00Z", "level": "fatal", "stream": "log", "message": "down"}
    ]);
    let (st, out) = s.post(&path, ADMIN_TOKEN, lines).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["accepted"], 2, "{out}");
    assert_eq!(out["rejected"], json!([]));
    let (_, got) = s.get(&format!("{path}?resultId=r1"), ADMIN_TOKEN).await;
    let items = got["items"].as_array().unwrap();
    let cut = items.iter().find(|i| i["seq"] == 1).unwrap();
    let m = cut["message"].as_str().unwrap();
    assert!(m.len() <= 64 * 1024, "{}", m.len());
    assert!(m.ends_with("…[truncated]"));
    let fatal = items.iter().find(|i| i["seq"] == 2).unwrap();
    assert_eq!(fatal["level"], "error");

    let many: Vec<Value> = (0..5001)
        .map(|i| json!({"resultId": "r2", "attempt": 0, "seq": i, "ts": T0, "level": "info", "stream": "log", "message": "x"}))
        .collect();
    let (st, out) = s.post(&path, ADMIN_TOKEN, json!(many)).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        out["message"],
        "the body exceeds max_lines_per_request (max 5000 lines)"
    );
}

#[tokio::test]
async fn openapi_documents_the_platform_routes() {
    let doc: Value = serde_json::from_str(&fastproof_server::app::openapi_text()).unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/api/v1/me",
        "/api/v1/tokens",
        "/api/v1/tokens/options",
        "/api/v1/tokens/self",
        "/api/v1/tokens/{id}",
    ] {
        assert!(paths.contains_key(p), "{p}");
    }
    assert!(paths["/api/v1/tokens"]["post"].is_object());
    assert!(paths["/api/v1/tokens/{id}"]["delete"].is_object());
    let schemas = &doc["components"]["schemas"];
    let body = &schemas["NewToken"]["properties"];
    for f in [
        "name",
        "role",
        "scopes",
        "global",
        "expiresInDays",
        "description",
    ] {
        assert!(body.get(f).is_some(), "{f}");
    }
    assert!(schemas["TokenList"]["properties"]["tokens"].is_object());
}

#[tokio::test]
async fn platform_token_shapes_match_the_document() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tok").await;
    let (st, created) = s
        .post(
            "/api/v1/tokens",
            ADMIN_TOKEN,
            json!({"name": "ci", "role": "uploader", "scopes": ["tok"]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    let doc: Value = serde_json::from_str(&fastproof_server::app::openapi_text()).unwrap();
    let props = |name: &str| -> Vec<String> {
        doc["components"]["schemas"][name]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    for k in created.as_object().unwrap().keys() {
        assert!(props("Token").contains(k) || k == "token", "{k} documented");
    }
    let days = (chrono::DateTime::parse_from_rfc3339(created["expiresAt"].as_str().unwrap())
        .unwrap()
        - chrono::DateTime::parse_from_rfc3339(created["createdAt"].as_str().unwrap()).unwrap())
    .num_days();
    assert_eq!(days, 90, "default lifetime");
    let (_, list) = s.get("/api/v1/tokens", ADMIN_TOKEN).await;
    assert!(list["tokens"].is_array(), "{list}");
    let (_, me) = s.get("/api/v1/me", ADMIN_TOKEN).await;
    for k in me.as_object().unwrap().keys() {
        assert!(props("Me").contains(k), "{k} documented");
    }
}

fn token_file(dir: &Path) -> PathBuf {
    let p = dir.join("token");
    std::fs::write(&p, ADMIN_TOKEN).unwrap();
    p
}

#[tokio::test]
async fn cli_upload_finishes_its_own_launch_and_finish_waits_for_uploads() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cli2").await;
    let tf = token_file(s.dir.path());
    let run = |args: Vec<String>| async move {
        let cli = <fastproof_server::cli::Cli as clap::Parser>::try_parse_from(args).unwrap();
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
            "cli2".into(),
            // The test expects a launch without an external id, also when the suite runs in CI.
            "--no-ci".into(),
        ]);
        v
    };
    let dir = fixture("pytest/allure-results");
    let dir = dir.to_str().unwrap();
    assert_eq!(run(common(&["upload", dir])).await, 0);
    let (_, list) = s.get("/api/v1/projects/cli2/launches", ADMIN_TOKEN).await;
    let own = &list["items"][0];
    assert_eq!(own["status"], "finished", "{own}");
    assert!(own["counts"]["total"].as_u64().unwrap() > 0);

    assert_eq!(run(common(&["upload", dir, "--no-finish"])).await, 0);
    let (_, list) = s.get("/api/v1/projects/cli2/launches", ADMIN_TOKEN).await;
    assert_eq!(list["items"][0]["status"], "running");

    assert_eq!(
        run(common(&["upload", dir, "--launch", "shared-1"])).await,
        0
    );
    let (_, list) = s
        .get(
            "/api/v1/projects/cli2/launches?externalId=shared-1",
            ADMIN_TOKEN,
        )
        .await;
    let shared = &list["items"][0];
    assert_eq!(shared["status"], "running", "shared launches stay open");
    let sid = shared["id"].as_str().unwrap().to_string();
    assert_eq!(
        run(common(&["launch", "finish", "--launch", "shared-1"])).await,
        0
    );
    let (_, uploads) = s
        .get(
            &format!("/api/v1/projects/cli2/launches/{sid}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    for u in uploads["items"].as_array().unwrap() {
        assert_eq!(u["status"], "done", "finish waited for the upload: {u}");
    }
    let l = s.launch_get("cli2", &sid).await;
    assert_eq!(l["status"], "finished");
    assert_eq!(l["counts"]["total"], own["counts"]["total"]);
}
