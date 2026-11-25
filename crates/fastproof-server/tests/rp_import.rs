//! `fastproof rp import`: a fake ReportPortal read API (pagination of 2, a 503 and a 429 on
//! the way) is replayed into a real server through `/rp`: tree, retries, defects, logs by
//! level, attachments by policy and size, idempotence, the state file, a dry run, and the
//! refusals (plain http, wrong key). Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;
#[path = "common/rp_fake.rs"]
mod rp_fake;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::{ADMIN_TOKEN, Server, TestDb};
use rp_fake::{Fake, RP_KEY, start_fake};
use serde_json::{Value, json};

const DAY: i64 = 86_400_000;

fn launch(id: i64, uuid: &str, name: &str, start: i64, status: &str, mode: &str) -> Value {
    let mut l = json!({"id": id, "uuid": uuid, "name": name, "number": id, "startTime": start,
        "status": status, "mode": mode, "description": "nightly run",
        "attributes": [{"key": "build", "value": "7"}, {"key": "agent", "value": "agent-js", "system": true}]});
    if status != "IN_PROGRESS" {
        l["endTime"] = json!(start + 60_000);
    }
    l
}

fn item(
    id: i64,
    uuid: &str,
    name: &str,
    kind: &str,
    parent: Option<i64>,
    start: i64,
    status: &str,
) -> Value {
    let mut i = json!({"id": id, "uuid": uuid, "name": name, "type": kind, "startTime": start,
        "endTime": start + 50, "status": status, "hasStats": true, "launchId": 1});
    if let Some(p) = parent {
        i["parent"] = json!(p);
    }
    i
}

fn log(item: i64, t: i64, level: &str, msg: &str, file: Option<&str>) -> Value {
    let mut l = json!({"itemId": item, "logTime": t, "message": msg, "level": level});
    if let Some(f) = file {
        l["binaryContent"] = json!({"id": f, "contentType": "image/png"});
    }
    l
}

#[allow(clippy::field_reassign_with_default)]
fn dataset(now: i64) -> Fake {
    let t0 = now - 10 * DAY;
    let mut f = Fake::default();
    f.launches = vec![
        launch(1, "la-1", "nightly", t0, "FAILED", "DEFAULT"),
        launch(2, "la-2", "nightly", t0 + 1000, "IN_PROGRESS", "DEFAULT"),
        launch(3, "la-3", "old", now - 200 * DAY, "PASSED", "DEFAULT"),
    ];
    f.debug_launches = vec![launch(4, "la-4", "debugging", t0 + 2000, "PASSED", "DEBUG")];
    let mut refund = item(
        11,
        "it-11",
        "test_refund",
        "STEP",
        Some(10),
        t0 + 200,
        "FAILED",
    );
    refund["codeRef"] = json!("tests/test_shop.py:test_refund");
    refund["issue"] = json!({"issueType": "pb_ab12", "comment": "known",
        "externalSystemIssues": [{"ticketId": "SHOP-1", "url": "https://tracker.example.com/SHOP-1"}]});
    let mut cart = item(
        14,
        "it-14",
        "open cart",
        "STEP",
        Some(11),
        t0 + 210,
        "PASSED",
    );
    cart["hasStats"] = json!(false);
    let mut flaky = item(
        13,
        "it-13",
        "test_flaky",
        "STEP",
        Some(10),
        t0 + 400,
        "PASSED",
    );
    flaky["codeRef"] = json!("tests/test_shop.py:test_flaky");
    let mut first = item(15, "it-15", "test_flaky", "STEP", None, t0 + 300, "FAILED");
    first["codeRef"] = json!("tests/test_shop.py:test_flaky");
    flaky["retries"] = json!([first]);
    f.items.insert(
        1,
        vec![
            item(
                10,
                "it-10",
                "tests/test_shop.py",
                "SUITE",
                None,
                t0 + 100,
                "FAILED",
            ),
            refund,
            item(
                12,
                "it-12",
                "test_pays",
                "STEP",
                Some(10),
                t0 + 150,
                "PASSED",
            ),
            flaky,
            cart,
        ],
    );
    f.logs = vec![
        log(11, t0 + 201, "INFO", "refunding", None),
        log(11, t0 + 202, "WARN", "slow", None),
        log(
            11,
            t0 + 203,
            "ERROR",
            "AssertionError: 1 != 2\n  at tests/test_shop.py:20",
            Some("f1"),
        ),
        log(11, t0 + 202, "ERROR", "second", Some("f-big")),
        log(12, t0 + 160, "ERROR", "noise of a passed test", Some("f2")),
        log(14, t0 + 211, "WARN", "step warn", Some("f3")),
        log(15, t0 + 301, "ERROR", "first attempt failed", None),
    ];
    let png = |n: &str| {
        b"\x89PNG\r\n\x1a\n"
            .iter()
            .copied()
            .chain(n.bytes())
            .collect::<Vec<u8>>()
    };
    f.files.insert(
        "f1".into(),
        ("image/png".into(), png("one"), Some("shot.png".into())),
    );
    f.files
        .insert("f2".into(), ("image/png".into(), png("two"), None));
    f.files
        .insert("f3".into(), ("image/png".into(), png("three"), None));
    f.files
        .insert("f-big".into(), ("image/png".into(), vec![0u8; 3000], None));
    f
}

async fn run_cli(args: Vec<String>) -> anyhow::Result<i32> {
    let mut a = vec!["fastproof".to_string()];
    a.extend(args);
    let cli = <fastproof_server::cli::Cli as clap::Parser>::try_parse_from(a).unwrap();
    fastproof_server::cli::run(cli).await
}

fn import_args(fp: &str, rp: &str, dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = [
        "rp",
        "import",
        "--url",
        fp,
        "--token-file",
        dir.join("fp-token").to_str().unwrap(),
        "--project",
        "shop",
        "--rp-url",
        rp,
        "--rp-project",
        "team",
        "--rp-key-file",
        dir.join("rp-key").to_str().unwrap(),
        "--state",
        dir.join("state.json").to_str().unwrap(),
        "--retry-delay-ms",
        "1",
        "--max-attachment",
        "2KiB",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

async fn native_launches(s: &Server) -> Vec<Value> {
    let mut all = Vec::new();
    for debug in ["false", "true"] {
        let (_, b) = s
            .get(
                &format!("/api/v1/projects/shop/launches?limit=100&debug={debug}"),
                ADMIN_TOKEN,
            )
            .await;
        all.extend(b["items"].as_array().unwrap().iter().cloned());
    }
    all
}

async fn wait_finished(s: &Server, external: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(l) = native_launches(s)
            .await
            .into_iter()
            .find(|l| l["externalId"] == external)
            && l["status"] != "running"
        {
            return l;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{external} not finished"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn replays_a_launch_with_its_history_and_repeats_safely() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_RP_SETTLE_MS", "200"),
            ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "3"),
        ],
    )
    .await;
    s.project("shop").await;
    let token = s.token("importer", "uploader", &["shop"]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fp-token"), &token).unwrap();
    std::fs::write(dir.path().join("rp-key"), format!("{RP_KEY}\n")).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let (rp, fake) = start_fake(dataset(now)).await;
    fake.faults
        .lock()
        .unwrap()
        .extend(["launch".to_string(), "item".to_string()]);

    let code = run_cli(import_args(&s.base, &rp, dir.path(), &[]))
        .await
        .unwrap();
    assert_eq!(code, 0);

    let launches = native_launches(&s).await;
    assert_eq!(
        launches.len(),
        1,
        "the running, old and debug launches are left: {launches:?}"
    );
    let l = wait_finished(&s, "rp:la-1").await;
    assert_eq!(l["name"], "nightly");
    assert_eq!(l["counts"]["total"], 3, "{l}");
    assert_eq!(l["counts"]["passed"], 2);
    assert_eq!(l["counts"]["failed"], 1);
    assert_eq!(l["counts"]["flaky"], 1);
    assert!(
        l["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["key"] == "build" && a["value"] == "7"),
        "{l}"
    );
    assert!(
        l["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["key"] == "imported" && a["value"] == "reportportal"),
        "{l}"
    );
    assert!(
        l["startedAt"].as_str().unwrap().starts_with(
            &chrono::DateTime::from_timestamp_millis(now - 10 * DAY)
                .unwrap()
                .format("%Y-%m-%dT%H:%M")
                .to_string()
        ),
        "the original start time is kept: {l}"
    );
    let lid = l["id"].as_str().unwrap().to_string();
    let results = s.results("shop", &lid, "").await;
    let by_name: HashMap<&str, &Value> = results
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r))
        .collect();
    let refund = by_name["test_refund"];
    assert_eq!(refund["status"], "failed");
    assert_eq!(refund["message"], "AssertionError: 1 != 2");
    assert_eq!(by_name["test_pays"]["status"], "passed");
    assert_eq!(by_name["test_flaky"]["status"], "passed");
    let (_, detail) = s
        .get(
            &format!(
                "/api/v1/projects/shop/results/{}",
                refund["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let text = detail.to_string();
    assert!(text.contains("shot.png"), "{text}");
    assert!(
        text.contains("attachment-f3"),
        "a file of a step of a failed test is copied: {text}"
    );
    assert!(
        !text.contains("f-big") && !text.contains("attachment-f2"),
        "{text}"
    );
    assert!(
        detail["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["name"] == "rp.issue" && x["value"] == "productBug"),
        "a custom defect type becomes its group: {detail}"
    );
    assert!(text.contains("SHOP-1"), "{text}");
    let hits = fake.hits.lock().unwrap().clone();
    assert!(hits.iter().any(|h| h.ends_with("/data/team/f1")));
    assert!(
        hits.iter().any(|h| h.ends_with("/data/team/f-big")),
        "too large: asked, not copied"
    );
    assert!(
        !hits.iter().any(|h| h.ends_with("/data/team/f2")),
        "passed tests keep no files"
    );
    assert!(
        fake.faults.lock().unwrap().is_empty(),
        "the 429 and the 503 were retried"
    );
    let state: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(state["launches"]["la-1"]["items"], 6);
    assert!(
        !String::from_utf8_lossy(&std::fs::read(dir.path().join("state.json")).unwrap())
            .contains(RP_KEY)
    );

    let before = fake.hits.lock().unwrap().len();
    let code = run_cli(import_args(&s.base, &rp, dir.path(), &[]))
        .await
        .unwrap();
    assert_eq!(code, 0);
    let item_calls = fake.hits.lock().unwrap()[before..]
        .iter()
        .filter(|h| h.ends_with("/item"))
        .count();
    assert_eq!(item_calls, 0, "a done launch is skipped by the state file");

    std::fs::remove_file(dir.path().join("state.json")).unwrap();
    let code = run_cli(import_args(
        &s.base,
        &rp,
        dir.path(),
        &["--logs", "none", "--attachments", "none"],
    ))
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        native_launches(&s).await.len(),
        1,
        "same uuids: no second launch"
    );
    let again = s.results("shop", &lid, "").await;
    assert_eq!(again.len(), 3, "the same results");

    let code = run_cli(import_args(
        &s.base,
        &rp,
        dir.path(),
        &["--include-debug", "--since", "all"],
    ))
    .await
    .unwrap();
    assert_eq!(code, 0);
    let all = native_launches(&s).await;
    let ext: Vec<&str> = all
        .iter()
        .filter_map(|l| l["externalId"].as_str())
        .collect();
    assert!(
        ext.contains(&"rp:la-4") && ext.contains(&"rp:la-3"),
        "{ext:?}"
    );
    assert!(
        !ext.contains(&"rp:la-2"),
        "a launch still running in ReportPortal is skipped"
    );
    assert!(all.iter().find(|l| l["externalId"] == "rp:la-4").unwrap()["debug"] == true);
}

#[tokio::test]
async fn a_dry_run_counts_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rp-key"), RP_KEY).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let (rp, _) = start_fake(dataset(now)).await;
    let code = run_cli(
        [
            "rp",
            "import",
            "--dry-run",
            "--rp-url",
            rp.as_str(),
            "--rp-project",
            "team",
            "--rp-key-file",
            dir.path().join("rp-key").to_str().unwrap(),
            "--state",
            dir.path().join("state.json").to_str().unwrap(),
            "--retry-delay-ms",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    )
    .await
    .unwrap();
    assert_eq!(code, 0);
    assert!(!dir.path().join("state.json").exists());

    let reader = fastproof_server::cli::rp::reader::RpReader::new(
        &fastproof_server::cli::rp::reader::RpConn {
            url: rp,
            project: "team".into(),
            key: RP_KEY.into(),
            ca_file: None,
            allow_http: false,
            rate: 1000.0,
            retry_delay: Duration::from_millis(1),
        },
    )
    .unwrap();
    let launch = reader
        .launches_page(1, false, None, None)
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|l| l.uuid == "la-1")
        .unwrap();
    let opts = fastproof_server::cli::rp::replay::Options {
        min_level: Some("warn"),
        attachments: fastproof_server::cli::rp::replay::Attachments::Failed,
        max_attachment: 1 << 20,
    };
    let c = fastproof_server::cli::rp::replay::survey_launch(&reader, &launch, &opts)
        .await
        .unwrap();
    assert_eq!((c.items, c.logs, c.files), (6, 6, 3), "{c:?}");
    let c = fastproof_server::cli::rp::replay::survey_launch(
        &reader,
        &launch,
        &fastproof_server::cli::rp::replay::Options {
            min_level: None,
            ..opts
        },
    )
    .await
    .unwrap();
    assert_eq!((c.logs, c.files), (0, 0));
}

#[tokio::test]
async fn refuses_plain_http_to_a_remote_host_a_wrong_key_and_a_foreign_state_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rp-key"), "wrong").unwrap();
    std::fs::write(dir.path().join("fp-token"), "fp_x").unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let (rp, _) = start_fake(dataset(now)).await;

    let e = run_cli(import_args(
        "http://127.0.0.1:1",
        "http://rp.example.com",
        dir.path(),
        &["--dry-run"],
    ))
    .await
    .unwrap_err();
    assert!(e.to_string().contains("unencrypted"), "{e:#}");

    let e = run_cli(import_args(
        "http://127.0.0.1:1",
        &rp,
        dir.path(),
        &["--dry-run"],
    ))
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("401"), "{e:#}");

    std::fs::write(dir.path().join("rp-key"), RP_KEY).unwrap();
    std::fs::write(
        dir.path().join("state.json"),
        r#"{"version":1,"rpUrl":"https://other.example.com","rpProject":"x","project":"y","launches":{}}"#,
    )
    .unwrap();
    let e = run_cli(import_args("http://127.0.0.1:1", &rp, dir.path(), &[]))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("belongs to another import"), "{e:#}");
}

/// A hostile ReportPortal: an endless error body, a message with terminal controls, pages that
/// ignore the page number (with times that change on every answer), endless distinct pages,
/// and logs of items that were not asked for.
async fn hostile(State(mode): State<&'static str>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let q: HashMap<String, String> =
        url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let n: i64 = q.get("page.page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let ok = |v: Value| (StatusCode::OK, Json(v)).into_response();
    if path == "/api/info" {
        return match mode {
            "endless-error" => {
                let chunk = bytes::Bytes::from(vec![b'x'; 64 * 1024]);
                let body = axum::body::Body::from_stream(futures::stream::repeat_with(move || {
                    Ok::<_, std::io::Error>(chunk.clone())
                }));
                (StatusCode::BAD_REQUEST, body).into_response()
            }
            "evil-message" => (
                StatusCode::FORBIDDEN,
                Json(json!({"message": "\u{1b}]0;owned\u{7}\nerror: forged line"})),
            )
                .into_response(),
            _ => ok(json!({"build": {"version": "5.14.1"}})),
        };
    }
    let t0 = chrono::Utc::now().timestamp_millis() - DAY;
    if path.ends_with("/launch") {
        return ok(json!({"content": [launch(1, "la-1", "nightly", t0, "PASSED", "DEFAULT")]}));
    }
    if path.ends_with("/item") {
        let content: Vec<Value> = if mode == "endless" {
            (0..300)
                .map(|k| {
                    let id = n * 1000 + k;
                    item(id, &format!("it-{id}"), "t", "STEP", None, t0, "PASSED")
                })
                .collect()
        } else {
            vec![item(10, "it-10", "t", "STEP", None, t0, "PASSED")]
        };
        return ok(json!({"content": content}));
    }
    if path.ends_with("/log") {
        let msg = if mode == "endless" {
            format!("{n}{}", "y".repeat(10_000))
        } else {
            "same".to_string()
        };
        static ANSWERS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let t = t0 + ANSWERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = if mode == "endless" { n * 10 } else { 0 };
        let mut mine = log(10, t, "ERROR", &msg, None);
        mine["id"] = json!(base + 1);
        let mut other = log(999, t0, "ERROR", "of an item not asked for", None);
        other["id"] = json!(base + 2);
        return ok(json!({"content": [mine, other]}));
    }
    StatusCode::NOT_FOUND.into_response()
}

async fn start_hostile(mode: &'static str) -> String {
    let app = axum::Router::new().fallback(hostile).with_state(mode);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn hostile_reader(url: String) -> fastproof_server::cli::rp::reader::RpReader {
    fastproof_server::cli::rp::reader::RpReader::new(&fastproof_server::cli::rp::reader::RpConn {
        url,
        project: "team".into(),
        key: RP_KEY.into(),
        ca_file: None,
        allow_http: false,
        rate: 1000.0,
        retry_delay: Duration::from_millis(1),
    })
    .unwrap()
}

#[tokio::test]
async fn a_hostile_reportportal_cannot_hang_or_flood_the_import() {
    use fastproof_server::cli::rp::reader::Limits;
    use fastproof_server::cli::rp::replay::{Attachments, Options, survey_launch};
    let within = Duration::from_secs(30);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rp-key"), RP_KEY).unwrap();
    let dry_run = |rp: String| {
        let key = dir.path().join("rp-key");
        async move {
            let args: Vec<String> = [
                "rp",
                "import",
                "--dry-run",
                "--rp-url",
                rp.as_str(),
                "--rp-project",
                "team",
                "--rp-key-file",
                key.to_str().unwrap(),
                "--retry-delay-ms",
                "1",
                "--rate",
                "1000",
                "--logs",
                "all",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            tokio::time::timeout(within, run_cli(args))
                .await
                .expect("the import ends")
        }
    };

    let e = dry_run(start_hostile("endless-error").await)
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("400"), "{e:#}");

    let e = dry_run(start_hostile("evil-message").await)
        .await
        .unwrap_err();
    let text = format!("{e:#}");
    assert!(text.contains("403") && text.contains("owned"), "{text}");
    assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
    assert!(!text.contains(RP_KEY));

    let rp = start_hostile("same-page").await;
    assert_eq!(dry_run(rp.clone()).await.unwrap(), 0);
    let reader = hostile_reader(rp);
    let launch = reader
        .launches_page(1, false, None, None)
        .await
        .unwrap()
        .unwrap()
        .remove(0);
    let opts = Options {
        min_level: Some("trace"),
        attachments: Attachments::All,
        max_attachment: 1 << 20,
    };
    let c = tokio::time::timeout(within, survey_launch(&reader, &launch, &opts))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.items, c.logs),
        (1, 1),
        "repeated pages end the lists; logs of other items are dropped"
    );

    let reader = hostile_reader(start_hostile("endless").await).with_limits(Limits {
        max_items: 1000,
        max_item_bytes: 1 << 30,
        max_log_bytes: 64 << 10,
        max_pages: 1000,
    });
    let e = tokio::time::timeout(within, reader.items(1))
        .await
        .unwrap()
        .unwrap_err();
    assert!(e.to_string().contains("more than 1000 items"), "{e:#}");
    let e = tokio::time::timeout(within, reader.logs(&[10], Some("trace")))
        .await
        .unwrap()
        .unwrap_err();
    assert!(e.to_string().contains("larger than"), "{e:#}");
}
