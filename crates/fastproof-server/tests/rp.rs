//! The ReportPortal endpoint (`/rp`): response shapes, the item tree as native results,
//! attempts, steps, logs and files, settling and finishing, isolation and limits. Recorded
//! traffic of real agents (`crates/fastproof-rp/tests/recordings`, or the directory in
//! `FASTPROOF_TEST_RP_RECORDINGS`) is replayed when present.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const FAST: &[(&str, &str)] = &[
    ("FASTPROOF_RP_SETTLE_MS", "200"),
    ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "3"),
];

async fn rp(
    s: &Server,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    s.call(method, &format!("/rp/api/{path}"), Some(token), body)
        .await
}

/// A multipart body: `json_request_part` with `entries`, plus `files` (part name, file name,
/// content type, bytes).
fn multipart(entries: &Value, files: &[(&str, &str, &str, Vec<u8>)]) -> (String, Vec<u8>) {
    let boundary = "fp-boundary-7d1c";
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"json_request_part\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    b.extend_from_slice(entries.to_string().as_bytes());
    b.extend_from_slice(b"\r\n");
    for (part, name, ct, data) in files {
        b.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{part}\"; filename=\"{name}\"\r\nContent-Type: {ct}\r\n\r\n")
                .as_bytes(),
        );
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    }
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), b)
}

async fn post_multipart(
    s: &Server,
    path: &str,
    token: &str,
    ct: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let r = s
        .req(Method::POST, &format!("/rp/api/{path}"), Some(token))
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

async fn wait_launch(s: &Server, project: &str, launch: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let l = s.launch_get(project, launch).await;
        if l["status"] != "running" {
            return l;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "launch not finished: {l}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn launch_id_of(s: &Server, project: &str, number: i64) -> String {
    let (_, b) = s
        .get(
            &format!("/api/v1/projects/{project}/launches?limit=100"),
            ADMIN_TOKEN,
        )
        .await;
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["number"] == number)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn item(s: &Server, path: &str, token: &str, body: Value) -> String {
    let (st, b) = rp(s, Method::POST, path, token, Some(body)).await;
    assert_eq!(st, StatusCode::CREATED, "{path}: {b}");
    b["id"].as_str().unwrap().to_string()
}

async fn finish(s: &Server, project: &str, uuid: &str, token: &str, body: Value) {
    let (st, b) = rp(
        s,
        Method::PUT,
        &format!("v2/{project}/item/{uuid}"),
        token,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["message"],
        format!("TestItem with ID = '{uuid}' successfully finished.")
    );
}

#[tokio::test]
async fn a_pytest_like_run_becomes_native_results() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, FAST).await;
    s.project("shop").await;
    let token = s.token("ci", "uploader", &["shop"]).await;

    let (st, info) = rp(&s, Method::GET, "info", &token, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(info["build"]["version"], "5.14.0");
    assert!(info.get("extensions").is_none());
    let (st, settings) = rp(&s, Method::GET, "v1/shop/settings", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{settings}");
    assert_eq!(settings["subTypes"]["PRODUCT_BUG"][0]["locator"], "pb001");
    assert_eq!(
        settings["subTypes"]["TO_INVESTIGATE"][0]["typeRef"],
        "TO_INVESTIGATE"
    );

    let (st, l) = rp(
        &s,
        Method::POST,
        "v2/shop/launch",
        &token,
        Some(json!({"name": "nightly", "startTime": "2026-11-24T10:00:00.123456+0000", "description": "recorded run",
            "attributes": [{"key": "branch", "value": "main"}, {"key": "build", "value": "42"}, {"value": "smoke"},
                {"key": "os", "value": "Linux", "system": true}]})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let launch = l["id"].as_str().unwrap().to_string();
    assert_eq!(l["number"], 1);
    let fp_launch = launch.clone();
    let native = s.launch_get("shop", &fp_launch).await;
    assert_eq!(native["vcs"]["branch"], "main");
    assert_eq!(native["ci"]["buildOrder"], 42);
    assert_eq!(native["environment"]["values"]["os"], "Linux");
    assert!(
        native["startedAt"]
            .as_str()
            .unwrap()
            .starts_with("2026-11-24T10:00:00.123")
    );

    let suite = item(&s, "v2/shop/item", &token, json!({"name": "tests/test_shop.py", "type": "SUITE", "launchUuid": launch, "startTime": 1_795_000_000_000i64})).await;
    let pass = item(&s, &format!("v2/shop/item/{suite}"), &token, json!({"name": "test_pays", "type": "STEP",
        "launchUuid": launch, "startTime": 1_795_000_000_100i64, "codeRef": "tests/test_shop.py:test_pays",
        "attributes": [{"key": "owner", "value": "team-a"}], "parameters": [{"key": "card", "value": "visa"}]})).await;
    let step = item(
        &s,
        &format!("v2/shop/item/{pass}"),
        &token,
        json!({"name": "open cart", "type": "STEP", "hasStats": false,
        "launchUuid": launch, "startTime": 1_795_000_000_110i64}),
    )
    .await;
    finish(
        &s,
        "shop",
        &step,
        &token,
        json!({"endTime": 1_795_000_000_150i64, "status": "passed", "launchUuid": launch}),
    )
    .await;
    finish(
        &s,
        "shop",
        &pass,
        &token,
        json!({"endTime": 1_795_000_000_200i64, "status": "PASSED", "launchUuid": launch}),
    )
    .await;
    let fail = item(&s, &format!("v2/shop/item/{suite}"), &token, json!({"name": "test_refund", "type": "STEP",
        "launchUuid": launch, "startTime": 1_795_000_000_300i64, "codeRef": "tests/test_shop.py:test_refund"})).await;
    let (ct, body) = multipart(
        &json!([
            {"itemUuid": fail, "launchUuid": launch, "time": "2026-11-24T10:00:01.000+0000", "level": "INFO", "message": "refunding"},
            {"itemUuid": fail, "launchUuid": launch, "time": 1_795_000_000_350i64, "level": 40000,
                "message": "AssertionError: 1 != 2\n  at tests/test_shop.py:20", "file": {"name": "shot.png"}},
            {"itemUuid": fail, "launchUuid": launch, "time": 1_795_000_000_360i64, "level": "ERROR", "message": "lost", "file": {"name": "missing.png"}},
            {"itemUuid": "no-such-item", "launchUuid": launch, "time": 1_795_000_000_360i64, "level": "INFO", "message": "x"},
            {"launchUuid": launch, "time": 1_795_000_000_360i64, "level": "WARN", "message": "launch line"}
        ]),
        &[(
            "file",
            "shot.png",
            "image/png",
            b"\x89PNG\r\n\x1a\nfake".to_vec(),
        )],
    );
    let (st, b) = post_multipart(&s, "v2/shop/log", &token, &ct, body).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let responses = b["responses"].as_array().unwrap();
    assert_eq!(responses.len(), 5);
    assert!(
        responses[0]["id"].is_string()
            && responses[1]["id"].is_string()
            && responses[4]["id"].is_string()
    );
    assert!(
        responses[2]["message"]
            .as_str()
            .unwrap()
            .contains("missing.png"),
        "{b}"
    );
    assert!(
        responses[3]["message"]
            .as_str()
            .unwrap()
            .contains("not found"),
        "{b}"
    );
    finish(&s, "shop", &fail, &token, json!({"endTime": 1_795_000_000_400i64, "status": "FAILED", "launchUuid": launch,
        "issue": {"issueType": "pb001", "comment": "known", "externalSystemIssues": [{"ticketId": "SHOP-1", "url": "https://tracker.example.com/SHOP-1"}]}})).await;
    let skip = item(
        &s,
        &format!("v2/shop/item/{suite}"),
        &token,
        json!({"name": "test_skip", "type": "STEP",
        "launchUuid": launch, "startTime": 1_795_000_000_500i64}),
    )
    .await;
    finish(
        &s,
        "shop",
        &skip,
        &token,
        json!({"status": "SKIPPED", "launchUuid": launch, "issue": {"issueType": "NOT_ISSUE"}}),
    )
    .await;
    finish(
        &s,
        "shop",
        &skip,
        &token,
        json!({"status": "PASSED", "launchUuid": launch}),
    )
    .await;
    for attempt in 0..2 {
        let r = item(&s, &format!("v2/shop/item/{suite}"), &token, json!({"name": "test_flaky", "type": "STEP",
            "launchUuid": launch, "startTime": 1_795_000_001_000i64 + attempt * 100, "codeRef": "tests/test_shop.py:test_flaky",
            "retry": attempt > 0})).await;
        finish(
            &s,
            "shop",
            &r,
            &token,
            json!({"endTime": 1_795_000_001_050i64 + attempt * 100,
            "status": if attempt == 0 { "FAILED" } else { "PASSED" }, "launchUuid": launch}),
        )
        .await;
    }
    finish(&s, "shop", &suite, &token, json!({"launchUuid": launch})).await;
    let (st, f) = rp(
        &s,
        Method::PUT,
        &format!("v1/shop/launch/{launch}/finish"),
        &token,
        Some(json!({"endTime": 1_795_000_002_000i64})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["id"], launch);
    assert_eq!(f["number"], 1);
    assert!(
        f["link"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/p/shop/launches/{fp_launch}"))
    );

    let done = wait_launch(&s, "shop", &fp_launch).await;
    assert_eq!(done["status"], "finished");
    assert_eq!(done["counts"]["total"], 4, "{done}");
    assert_eq!(done["counts"]["passed"], 2);
    assert_eq!(done["counts"]["failed"], 1);
    assert_eq!(done["counts"]["skipped"], 1);
    assert_eq!(done["counts"]["flaky"], 1);
    let results = s.results("shop", &fp_launch, "").await;
    let by_name: HashMap<&str, &Value> = results
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r))
        .collect();
    let refund = by_name["test_refund"];
    assert_eq!(refund["status"], "failed");
    assert_eq!(refund["message"], "AssertionError: 1 != 2");
    let (_, detail) = s
        .get(
            &format!(
                "/api/v1/projects/shop/results/{}",
                refund["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let a = &detail["attempts"][0];
    assert_eq!(a["attachments"].as_array().unwrap().len(), 1, "{detail}");
    assert!(
        detail["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "rp.issue" && l["value"] == "productBug"),
        "{detail}"
    );
    assert!(
        detail["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["url"] == "https://tracker.example.com/SHOP-1"),
        "{detail}"
    );
    let pays = by_name["test_pays"];
    let (_, detail) = s
        .get(
            &format!(
                "/api/v1/projects/shop/results/{}",
                pays["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(detail["fullName"], "tests/test_shop.py:test_pays");
    assert_eq!(
        detail["attempts"][0]["steps"][0]["name"], "open cart",
        "{detail}"
    );
    assert!(
        detail["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "owner" && l["value"] == "team-a")
    );
    assert!(
        detail["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "suite" && l["value"] == "tests/test_shop.py")
    );
    let flaky = by_name["test_flaky"];
    assert_eq!(flaky["status"], "passed");
    assert_eq!(flaky["flakyInRun"], true, "{flaky}");

    let (_, logs) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{fp_launch}/logs?resultId=rp:{fail}"),
            ADMIN_TOKEN,
        )
        .await;
    let lines = logs["items"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{logs}");
    assert!(lines.iter().any(|l| {
        l["message"]
            .as_str()
            .unwrap()
            .ends_with("[attachment: shot.png]")
    }));
    let (_, logs) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{fp_launch}/logs?resultId=rp-launch"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(logs["items"][0]["message"], "launch line");

    let (st, b) = rp(
        &s,
        Method::GET,
        &format!("v1/shop/launch/uuid/{launch}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["id"], 1);
    assert_eq!(b["status"], "FAILED");
    let (st, b) = rp(
        &s,
        Method::GET,
        &format!("v1/shop/launch?filter.in.uuid={launch}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["content"][0]["uuid"], launch);
    let (st, b) = rp(
        &s,
        Method::GET,
        &format!("v1/shop/item/uuid/{pass}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b["id"].is_number());
    assert_eq!(b["status"], "PASSED");
}

#[tokio::test]
async fn v1_shapes_leaf_rule_fixtures_and_unsupported_routes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, FAST).await;
    s.project("web").await;
    let token = s.token("ci", "uploader", &["web"]).await;
    let (st, l) = rp(
        &s,
        Method::POST,
        "v1/web/launch",
        &token,
        Some(json!({"name": "e2e", "uuid": "pw-launch-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    assert_eq!(l["id"], "pw-launch-1");
    let (st, again) = rp(
        &s,
        Method::POST,
        "v1/web/launch",
        &token,
        Some(json!({"name": "e2e", "uuid": "pw-launch-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(again["number"], l["number"]);
    let lu = "pw-launch-1";
    let (st, file) = rp(
        &s,
        Method::POST,
        "v1/web/item/",
        &token,
        Some(json!({"name": "cart.spec.ts", "type": "SUITE", "launchUuid": lu})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert!(file["uniqueId"].as_str().unwrap().starts_with("auto:"));
    let file = file["id"].as_str().unwrap().to_string();
    let describe = item(
        &s,
        &format!("v1/web/item/{file}"),
        &token,
        json!({"name": "Cart", "type": "TEST", "launchUuid": lu}),
    )
    .await;
    let before = item(
        &s,
        &format!("v1/web/item/{describe}"),
        &token,
        json!({"name": "seed", "type": "BEFORE_CLASS", "launchUuid": lu}),
    )
    .await;
    finish(
        &s,
        "web",
        &before,
        &token,
        json!({"status": "FAILED", "launchUuid": lu}),
    )
    .await;
    let ok_fixture = item(
        &s,
        &format!("v1/web/item/{describe}"),
        &token,
        json!({"name": "login", "type": "BEFORE_METHOD", "launchUuid": lu}),
    )
    .await;
    finish(
        &s,
        "web",
        &ok_fixture,
        &token,
        json!({"status": "PASSED", "launchUuid": lu}),
    )
    .await;
    let test = item(
        &s,
        &format!("v1/web/item/{describe}"),
        &token,
        json!({"name": "adds", "type": "STEP", "launchUuid": lu,
        "codeRef": "e2e/cart.spec.ts/Cart/adds", "uuid": "client-item-1"}),
    )
    .await;
    assert_eq!(test, "client-item-1");
    let dup = item(
        &s,
        &format!("v1/web/item/{describe}"),
        &token,
        json!({"name": "adds", "type": "STEP", "launchUuid": lu, "uuid": "client-item-1"}),
    )
    .await;
    assert_eq!(dup, test);
    let open = item(
        &s,
        &format!("v1/web/item/{describe}"),
        &token,
        json!({"name": "hangs", "type": "STEP", "launchUuid": lu}),
    )
    .await;
    finish(
        &s,
        "web",
        &test,
        &token,
        json!({"status": "passed", "launchUuid": lu}),
    )
    .await;
    finish(&s, "web", &describe, &token, json!({"launchUuid": lu})).await;
    let (st, b) = rp(
        &s,
        Method::PUT,
        "v1/web/item/no-such-item",
        &token,
        Some(json!({"status": "PASSED"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(b["errorCode"], 4040);
    let (st, b) = rp(
        &s,
        Method::POST,
        "v1/web/launch/merge",
        &token,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        b,
        json!({"errorCode": 5000, "message": "not supported by FastProof"})
    );
    let (st, _) = rp(&s, Method::GET, "v1/web/dashboard", &token, None).await;
    assert_eq!(st, StatusCode::NOT_IMPLEMENTED);
    let (st, _) = rp(
        &s,
        Method::PUT,
        &format!("v1/web/launch/{lu}/finish"),
        &token,
        Some(json!({"status": "PASSED"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let fp_launch = launch_id_of(&s, "web", l["number"].as_i64().unwrap()).await;
    let done = wait_launch(&s, "web", &fp_launch).await;
    assert_eq!(done["status"], "finished");
    let results = s.results("web", &fp_launch, "").await;
    let names: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!(results.len(), 3, "{names:?}");
    assert!(names.contains(&("adds", "passed")));
    assert!(names.contains(&("Cart: seed", "broken")));
    assert!(names.contains(&("hangs", "broken")), "{names:?}");
    let hangs = results.iter().find(|r| r["name"] == "hangs").unwrap();
    assert_eq!(hangs["message"], "interrupted");
    let _ = open;
}

#[tokio::test]
async fn auth_isolation_aliases_and_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_RP_MAX_ITEMS_PER_LAUNCH", "3"),
            ("FASTPROOF_RP_MAX_REQUEST_BYTES", "4096"),
        ],
    )
    .await;
    s.project("alpha").await;
    s.project("beta").await;
    let a = s.token("a", "uploader", &["alpha"]).await;
    let viewer = s.token("v", "viewer", &["alpha"]).await;

    let (st, b) = s.call(Method::GET, "/rp/api/info", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(b["errorCode"], 4003);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v2/alpha/launch",
        &viewer,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v2/beta/launch",
        &a,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v2/Nobody_Here/launch",
        &a,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v2/Nobody_Here/launch",
        ADMIN_TOKEN,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v3/alpha/launch",
        &a,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/alpha/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"rp": {"aliases": ["Team_A", "default_personal"]}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/beta/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"rp": {"aliases": ["team_a"]}})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/beta/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"rp": {"aliases": ["../x"]}})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, l) = rp(
        &s,
        Method::POST,
        "v2/team_a/launch",
        &a,
        Some(json!({"name": "aliased", "uuid": "L1"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let (st, b) = rp(
        &s,
        Method::POST,
        "v2/alpha/launch",
        &a,
        Some(json!({"name": "x", "uuid": "../etc"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");

    let suite = item(
        &s,
        "v2/alpha/item",
        &a,
        json!({"name": "s", "type": "SUITE", "launchUuid": "L1"}),
    )
    .await;
    let (_, beta_launch) = rp(
        &s,
        Method::POST,
        "v2/beta/launch",
        ADMIN_TOKEN,
        Some(json!({"name": "b"})),
    )
    .await;
    let admin_beta = item(
        &s,
        "v2/beta/item",
        ADMIN_TOKEN,
        json!({"name": "s", "type": "SUITE", "launchUuid": beta_launch["id"]}),
    )
    .await;
    let (st, _) = rp(
        &s,
        Method::POST,
        &format!("v2/beta/item/{suite}"),
        ADMIN_TOKEN,
        Some(json!({"name": "t", "launchUuid": "L1"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a launch of another project is not found"
    );
    let (st, _) = rp(
        &s,
        Method::PUT,
        &format!("v2/alpha/item/{admin_beta}"),
        &a,
        Some(json!({"status": "PASSED"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "an item of another project is not found"
    );
    let (st, _) = rp(
        &s,
        Method::GET,
        &format!("v2/alpha/item/uuid/{admin_beta}"),
        &a,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let mut last = StatusCode::CREATED;
    for i in 0..120 {
        let (st, _) = rp(
            &s,
            Method::POST,
            &format!("v2/alpha/item/{suite}"),
            &a,
            Some(json!({"name": format!("t{i}"), "launchUuid": "L1"})),
        )
        .await;
        last = st;
        if st != StatusCode::CREATED {
            break;
        }
    }
    assert_eq!(last, StatusCode::PAYLOAD_TOO_LARGE);

    let (ct, body) = multipart(
        &json!([{"launchUuid": "L1", "message": "x", "level": "INFO"}]),
        &[(
            "file",
            "big.bin",
            "application/octet-stream",
            vec![7u8; 8000],
        )],
    );
    let (st, b) = post_multipart(&s, "v2/alpha/log", &a, &ct, body).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    let (st, _) = post_multipart(&s, "v2/alpha/log", &a, "multipart/form-data; boundary=x", b"--x\r\nContent-Disposition: form-data; name=\"f\"; filename=\"../../a\"\r\n\r\nzz\r\n--x--\r\n".to_vec()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no json_request_part");
    let r = s
        .req(Method::POST, "/rp/api/v2/alpha/log/entry", Some(&a))
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (st, _) = rp(
        &s,
        Method::POST,
        "v2/alpha/launch",
        &a,
        Some(json!({"name": "x".repeat(2 * 1024 * 1024)})),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn joins_reruns_late_logs_and_idle_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, FAST).await;
    s.project("ci").await;
    let token = s.token("ci", "uploader", &["ci"]).await;
    let other = s.token("other", "uploader", &["ci"]).await;
    let native = s.launch("ci", None).await;
    let fp = native["id"].as_str().unwrap().to_string();
    for shard in ["a", "b"] {
        let suite = item(
            &s,
            "v2/ci/item",
            &token,
            json!({"name": format!("shard {shard}"), "type": "SUITE", "launchUuid": fp}),
        )
        .await;
        let t = item(
            &s,
            &format!("v2/ci/item/{suite}"),
            &token,
            json!({"name": format!("t-{shard}"), "type": "STEP", "launchUuid": fp}),
        )
        .await;
        finish(
            &s,
            "ci",
            &t,
            &token,
            json!({"status": "PASSED", "launchUuid": fp}),
        )
        .await;
        finish(&s, "ci", &suite, &token, json!({"launchUuid": fp})).await;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while s.results("ci", &fp, "").await.len() < 2 {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (st, _) = s
        .call(
            Method::POST,
            &format!("/api/v1/projects/ci/launches/{fp}/finish"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (_, l) = rp(
        &s,
        Method::POST,
        "v2/ci/launch",
        &token,
        Some(json!({"name": "rerunnable"})),
    )
    .await;
    let lu = l["id"].as_str().unwrap().to_string();
    let t = item(
        &s,
        "v2/ci/item",
        &token,
        json!({"name": "late", "type": "STEP", "launchUuid": lu, "codeRef": "t.py:late"}),
    )
    .await;
    finish(
        &s,
        "ci",
        &t,
        &token,
        json!({"status": "FAILED", "launchUuid": lu}),
    )
    .await;
    let fp2 = launch_id_of(&s, "ci", l["number"].as_i64().unwrap()).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while s
        .results("ci", &fp2, "")
        .await
        .iter()
        .all(|r| r["status"] == "running")
    {
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (ct, body) = multipart(
        &json!([{"itemUuid": t, "launchUuid": lu, "level": "ERROR", "message": "late error"}]),
        &[],
    );
    let (st, _) = post_multipart(&s, "v2/ci/log", &token, &ct, body).await;
    assert_eq!(st, StatusCode::CREATED);
    let t2 = item(
        &s,
        "v2/ci/item",
        &token,
        json!({"name": "held", "type": "STEP", "launchUuid": lu, "codeRef": "t.py:held"}),
    )
    .await;
    finish(
        &s,
        "ci",
        &t2,
        &token,
        json!({"status": "FAILED", "launchUuid": lu}),
    )
    .await;
    let (ct, body) = multipart(
        &json!([{"itemUuid": t2, "launchUuid": lu, "level": "ERROR", "message": "held error"}]),
        &[],
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (st, _) = post_multipart(&s, "v2/ci/log", &token, &ct, body).await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = rp(
        &s,
        Method::PUT,
        &format!("v2/ci/launch/{lu}/finish"),
        &token,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    wait_launch(&s, "ci", &fp2).await;
    let results = s.results("ci", &fp2, "").await;
    let held = results.iter().find(|r| r["name"] == "held").unwrap();
    assert_eq!(
        held["message"], "held error",
        "a launch with late lines holds its results until it finishes"
    );

    let (st, b) = rp(
        &s,
        Method::POST,
        "v2/ci/launch",
        &other,
        Some(json!({"name": "rerunnable", "rerun": true})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    let (st, b) = rp(
        &s,
        Method::POST,
        "v2/ci/launch",
        &token,
        Some(json!({"name": "rerunnable", "rerun": true})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["id"], lu);
    assert_eq!(s.launch_get("ci", &fp2).await["status"], "running");
    let t3 = item(
        &s,
        "v2/ci/item",
        &token,
        json!({"name": "late", "type": "STEP", "launchUuid": lu, "codeRef": "t.py:late"}),
    )
    .await;
    finish(
        &s,
        "ci",
        &t3,
        &token,
        json!({"status": "PASSED", "launchUuid": lu}),
    )
    .await;
    let (st, _) = rp(
        &s,
        Method::PUT,
        &format!("v2/ci/launch/{lu}/finish"),
        &token,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let done = wait_launch(&s, "ci", &fp2).await;
    assert_eq!(done["counts"]["flaky"], 1, "a rerun adds attempts: {done}");

    let (_, l) = rp(
        &s,
        Method::POST,
        "v2/ci/launch",
        &token,
        Some(json!({"name": "abandoned"})),
    )
    .await;
    let lu = l["id"].as_str().unwrap().to_string();
    item(
        &s,
        "v2/ci/item",
        &token,
        json!({"name": "never ends", "type": "STEP", "launchUuid": lu}),
    )
    .await;
    sqlx::query("update rp_launches set last_call_at = now() - interval '2 hours' where uuid = $1")
        .bind(&lu)
        .execute(&db.pool)
        .await
        .unwrap();
    let n = s
        .running
        .as_ref()
        .unwrap()
        .state
        .store
        .rp_expire_idle(3600)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let fp3 = launch_id_of(&s, "ci", l["number"].as_i64().unwrap()).await;
    let done = wait_launch(&s, "ci", &fp3).await;
    assert_eq!(done["status"], "interrupted");
    assert_eq!(done["counts"]["broken"], 1);
}

fn recordings_dir() -> Option<std::path::PathBuf> {
    let d = match std::env::var("FASTPROOF_TEST_RP_RECORDINGS") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../fastproof-rp/tests/recordings"),
    };
    d.is_dir().then_some(d)
}

fn replace_ids(v: &Value, ids: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => Value::String(ids.get(s).cloned().unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|x| replace_ids(x, ids)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), replace_ids(x, ids)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn map_path(path: &str, ids: &HashMap<String, String>) -> String {
    path.split('/')
        .map(|seg| ids.get(seg).cloned().unwrap_or_else(|| seg.to_string()))
        .collect::<Vec<_>>()
        .join("/")
}

/// Every key of the recorded answer is in ours (the agents read them).
fn same_keys(recorded: &Value, ours: &Value, at: &str) {
    if let (Some(r), Some(o)) = (recorded.as_object(), ours.as_object()) {
        for k in r.keys() {
            if matches!(k.as_str(), "status" | "name") && at.contains("launch") {
                continue;
            }
            assert!(o.contains_key(k), "{at}: answer lacks {k}: {ours}");
        }
    }
}

/// Replays a recording; returns the FastProof launches it reported into.
async fn replay(s: &Server, token: &str, file: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(file).unwrap();
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut launches = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let e: Value = serde_json::from_str(line).unwrap();
        if e["injected"] == true {
            continue;
        }
        let method = Method::from_bytes(e["method"].as_str().unwrap().as_bytes()).unwrap();
        let path = map_path(e["path"].as_str().unwrap(), &ids);
        let mut query = String::new();
        if let Some(q) = e["query"].as_object() {
            for (k, v) in q {
                for x in v.as_array().into_iter().flatten() {
                    query.push(if query.is_empty() { '?' } else { '&' });
                    query.push_str(&format!(
                        "{k}={}",
                        ids.get(x.as_str().unwrap())
                            .cloned()
                            .unwrap_or_else(|| x.as_str().unwrap().to_string())
                    ));
                }
            }
        }
        for key in ["launchUuid"] {
            let lu = e["body"][key].as_str().map(str::to_string).or_else(|| {
                e["multipart"].as_array().and_then(|p| {
                    p.iter()
                        .find_map(|x| x["json"][0][key].as_str().map(str::to_string))
                })
            });
            if let Some(lu) = lu
                && !ids.contains_key(&lu)
            {
                let l = s.launch("example-project", None).await;
                let id = l["id"].as_str().unwrap().to_string();
                ids.insert(lu, id.clone());
                launches.push(id);
            }
        }
        let rb = s.req(method.clone(), &format!("{path}{query}"), Some(token));
        let resp = if let Some(parts) = e["multipart"].as_array() {
            let boundary = "replay-boundary-0042";
            let mut b = Vec::new();
            for p in parts {
                let name = p["name"].as_str().unwrap_or("file");
                let disp = match p["filename"].as_str() {
                    Some(f) => format!("form-data; name=\"{name}\"; filename=\"{f}\""),
                    None => format!("form-data; name=\"{name}\""),
                };
                b.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Disposition: {disp}\r\nContent-Type: {}\r\n\r\n",
                        p["contentType"]
                            .as_str()
                            .unwrap_or("application/octet-stream")
                    )
                    .as_bytes(),
                );
                if p.get("json").is_some() {
                    b.extend_from_slice(replace_ids(&p["json"], &ids).to_string().as_bytes());
                } else {
                    let n = p["size"].as_u64().unwrap_or(0) as usize;
                    b.extend((0..n).map(|i| (i % 251) as u8));
                }
                b.extend_from_slice(b"\r\n");
            }
            b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            rb.header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(b)
            .send()
            .await
            .unwrap()
        } else if e.get("body").is_some() {
            rb.json(&replace_ids(&e["body"], &ids))
                .send()
                .await
                .unwrap()
        } else {
            rb.send().await.unwrap()
        };
        let st = resp.status();
        let ours: Value = resp.json().await.unwrap_or(Value::Null);
        let recorded = &e["response"];
        let at = format!("{} {} #{}", e["method"], e["path"], e["seq"]);
        assert_eq!(
            st.as_u16() as u64,
            recorded["status"].as_u64().unwrap(),
            "{at}: {ours}"
        );
        same_keys(&recorded["body"], &ours, &at);
        if let (Some(r), Some(o)) = (recorded["body"]["id"].as_str(), ours["id"].as_str()) {
            ids.insert(r.to_string(), o.to_string());
            if e["path"].as_str().unwrap().ends_with("/launch") && e["method"] == "POST" {
                let number = ours["number"].as_i64().unwrap();
                launches.push(launch_id_of(s, "example-project", number).await);
            }
        }
        if let (Some(r), Some(o)) = (
            recorded["body"]["responses"].as_array(),
            ours["responses"].as_array(),
        ) {
            assert_eq!(r.len(), o.len(), "{at}");
            for (a, b) in r.iter().zip(o) {
                same_keys(a, b, &at);
            }
        }
    }
    launches.sort();
    launches.dedup();
    launches
}

/// Results expected of each recording: total, failed (each with an error message).
const EXPECTED: &[(&str, i64, i64)] = &[
    ("playwright-v1-steps", 11, 2),
    ("playwright-v2-nosteps", 11, 2),
    ("playwright-v2-shard1-join", 9, 1),
    ("playwright-v2-shard2-join", 2, 1),
    ("playwright-v2-steps", 11, 2),
    ("pytest-plain", 19, 8),
    ("pytest-xdist-join", 19, 8),
    ("robot-deep", 5, 1),
    ("testng-join-uuid", 16, 3),
    ("testng-sync", 16, 3),
    ("testng-v2-async", 16, 3),
];

/// Replays recordings (fault injections aside), finishes what the agents left running, and
/// checks the results of each against [`EXPECTED`].
async fn replay_all(s: &Server, token: &str, only: &[&str]) {
    let Some(dir) = recordings_dir() else {
        eprintln!("no ReportPortal recordings: skipped");
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "ndjson") && !p.to_string_lossy().contains("fault")
        })
        .filter(|p| only.is_empty() || only.iter().any(|o| p.to_string_lossy().contains(o)))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut summary = Vec::new();
    for f in &files {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let launches = replay(s, token, f).await;
        assert_eq!(launches.len(), 1, "{stem}");
        let l = &launches[0];
        let launch = s.launch_get("example-project", l).await;
        if launch["status"] == "running" {
            let (st, _) = s
                .call(
                    Method::POST,
                    &format!("/api/v1/projects/example-project/launches/{l}/finish"),
                    Some(ADMIN_TOKEN),
                    None,
                )
                .await;
            assert_eq!(st, StatusCode::OK);
        }
        let want = EXPECTED.iter().find(|e| e.0 == stem).map(|e| (e.1, e.2));
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let results = loop {
            let r = s.results("example-project", l, "").await;
            let settled = !r.is_empty() && r.iter().all(|x| x["status"] != "running");
            if settled && want.is_none_or(|(n, _)| r.len() as i64 == n) {
                break r;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{stem}: results not settled: {r:?}"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        let failed: Vec<&Value> = results.iter().filter(|r| r["status"] == "failed").collect();
        let with_message = failed.iter().filter(|r| r["message"].is_string()).count();
        let (mut steps, mut files) = (0, 0);
        for r in &results {
            let (_, d) = s
                .get(
                    &format!(
                        "/api/v1/projects/example-project/results/{}",
                        r["id"].as_str().unwrap()
                    ),
                    ADMIN_TOKEN,
                )
                .await;
            for a in d["attempts"].as_array().unwrap() {
                steps += a["steps"].as_array().map_or(0, |x| x.len());
                files += a["attachments"].as_array().map_or(0, |x| x.len());
            }
        }
        if let Some((_, f)) = want {
            assert_eq!(failed.len() as i64, f, "{stem}");
            assert_eq!(
                with_message,
                failed.len(),
                "{stem}: failures without an error message"
            );
        }
        if stem.ends_with("-steps") || stem.contains("robot") {
            assert!(steps > 0, "{stem}: no steps");
        }
        if stem.starts_with("playwright") || stem.starts_with("pytest") {
            assert!(files > 0, "{stem}: no attachments");
        }
        summary.push(format!(
            "{stem}: {} results, failed with message {with_message}/{}, steps {steps}, attachments {files}",
            results.len(),
            failed.len()
        ));
    }
    eprintln!("{}", summary.join("\n"));
}

#[tokio::test]
async fn recorded_agents_replay_into_native_results() {
    if recordings_dir().is_none() {
        eprintln!("no ReportPortal recordings: skipped");
        return;
    }
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, FAST).await;
    s.project("example-project").await;
    let token = s.token("ci", "uploader", &["example-project"]).await;
    replay_all(&s, &token, &[]).await;
}

#[tokio::test]
async fn kafka_mode_replays_the_same() {
    let Ok(brokers) = std::env::var("FASTPROOF_TEST_KAFKA_BROKERS") else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    if recordings_dir().is_none() {
        eprintln!("no ReportPortal recordings: skipped");
        return;
    }
    let Some(db) = TestDb::new().await else {
        return;
    };
    let prefix = format!("fptest{:08x}.", rand::random::<u32>());
    let mut vars: Vec<(&str, &str)> = vec![
        ("FASTPROOF_INGEST_MODE", "kafka"),
        ("FASTPROOF_KAFKA_BROKERS", &brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", &prefix),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", "true"),
        ("FASTPROOF_KAFKA_PARTITIONS", "4"),
    ];
    vars.extend_from_slice(FAST);
    let s = Server::start_with(&db, &vars).await;
    s.project("example-project").await;
    let token = s.token("ci", "uploader", &["example-project"]).await;
    replay_all(
        &s,
        &token,
        &[
            "playwright-v2-steps",
            "pytest-plain",
            "testng-v2-async",
            "robot-deep",
        ],
    )
    .await;
}
