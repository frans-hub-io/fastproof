//! Rerun from a launch against a local fake CI (GitHub Actions, GitLab pipeline triggers and a
//! generic webhook) on a real PostgreSQL. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method as AxMethod, StatusCode as AxStatus, Uri};
use axum::response::{IntoResponse, Response};
use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const KEY_HEX: &str = "8f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

#[derive(Clone, Debug)]
struct Rec {
    method: String,
    path: String,
    auth: String,
    private_token: String,
    content_type: String,
    body: String,
}

#[derive(Default)]
struct FakeState {
    log: Mutex<Vec<Rec>>,
    force_status: AtomicU16,
}

struct Ci {
    addr: SocketAddr,
    st: Arc<FakeState>,
}

impl Ci {
    async fn start() -> Self {
        let st = Arc::new(FakeState::default());
        let app = axum::Router::new().fallback(handle).with_state(st.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        Self { addr, st }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> Vec<Rec> {
        self.st.log.lock().unwrap().clone()
    }
}

async fn handle(
    State(st): State<Arc<FakeState>>,
    method: AxMethod,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let h = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    st.log.lock().unwrap().push(Rec {
        method: method.to_string(),
        path: uri.path().to_string(),
        auth: h("authorization"),
        private_token: h("private-token"),
        content_type: h("content-type"),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    let forced = st.force_status.load(Ordering::SeqCst);
    if forced != 0 {
        return AxStatus::from_u16(forced).unwrap().into_response();
    }
    let path = uri.path();
    if path.ends_with("/dispatches") {
        AxStatus::NO_CONTENT.into_response()
    } else if path.ends_with("/trigger/pipeline") {
        let host = h("host");
        (
            AxStatus::CREATED,
            axum::Json(
                json!({"id": 77, "web_url": format!("http://{host}/group/app/-/pipelines/77")}),
            ),
        )
            .into_response()
    } else {
        AxStatus::OK.into_response()
    }
}

async fn start(db: &TestDb, ci: &Ci, public: bool) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let mut vars = vec![
        (
            "FASTPROOF_EGRESS_ALLOW".to_string(),
            "127.0.0.1".to_string(),
        ),
        (
            "FASTPROOF_SECRETS_KEY_FILE".into(),
            key.to_string_lossy().to_string(),
        ),
    ];
    if public {
        vars.push((
            "FASTPROOF_PUBLIC_URL".into(),
            "https://fastproof.example.com".into(),
        ));
    }
    let _ = ci;
    let extra: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    (Server::start_with(db, &extra).await, dir)
}

async fn put_secret(s: &Server, name: &str, value: &str, purpose: &str, origin: &str) {
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/shop/secrets/{name}"),
            Some(ADMIN_TOKEN),
            Some(json!({"value": value, "purpose": purpose, "origin": origin})),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
}

async fn put_settings(s: &Server, doc: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        "/api/v1/projects/shop/settings",
        Some(ADMIN_TOKEN),
        Some(doc),
    )
    .await
}

async fn launch_with_results(s: &Server) -> (String, Vec<Value>) {
    let l = s.launch("shop", None).await;
    let id = l["id"].as_str().unwrap().to_string();
    let body: Vec<Value> = [
        ("a", "passed"),
        ("b", "failed"),
        ("c", "broken"),
        ("d", "skipped"),
    ]
    .iter()
    .map(|(n, st)| {
        let mut r = json!({"id": format!("{id}-{n}"), "name": n, "fullName": format!("suite.{n}"),
            "status": st, "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64});
        if *st == "failed" || *st == "broken" {
            r["error"] = json!({"message": format!("{n} failed")});
        }
        r
    })
    .collect();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/results"),
            ADMIN_TOKEN,
            Value::Array(body),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let mut tries = 0;
    loop {
        let r = s.results("shop", &id, "").await;
        if r.len() == 4 || tries > 100 {
            return (id, r);
        }
        tries += 1;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn names(plan: &Value) -> Vec<String> {
    let mut v: Vec<String> = plan["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["selector"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn github_dispatch_carries_the_plan_and_links_the_new_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db, &ci, true).await;
    s.project("shop").await;
    put_secret(&s, "gh", "ghp_secret_value_1", "ci", &ci.url()).await;
    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "github", "url": ci.url(), "repository": "acme/shop",
            "workflow": "tests.yml", "ref": "main", "tokenSecretRef": "gh"}}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
    let (id, _) = launch_with_results(&s).await;

    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/rerun"),
            ADMIN_TOKEN,
            json!({"mode": "failed"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["status"], "dispatched");
    assert_eq!(b["testCount"], 2);
    assert_eq!(b["provider"], "github");
    let external = b["externalId"].as_str().unwrap().to_string();
    let plan_id = b["planId"].as_str().unwrap().to_string();

    let reqs = ci.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(
        reqs[0].path,
        "/repos/acme/shop/actions/workflows/tests.yml/dispatches"
    );
    assert_eq!(reqs[0].auth, "Bearer ghp_secret_value_1");
    let sent: Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(sent["ref"], "main");
    assert_eq!(sent["inputs"]["fastproof_plan_id"], plan_id);
    assert_eq!(sent["inputs"]["fastproof_launch"], external);
    assert_eq!(sent["inputs"]["fastproof_source_launch"], id);
    assert_eq!(
        sent["inputs"]["fastproof_url"],
        "https://fastproof.example.com"
    );

    let (st, plan) = s
        .get(
            &format!("/api/v1/projects/shop/test-plans/{plan_id}/testplan.json"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{plan}");
    assert_eq!(names(&plan), vec!["suite.b", "suite.c"]);

    let (st, l) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{id}/reruns"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["configured"], true);
    assert_eq!(l["items"][0]["launchId"], Value::Null);

    let (st, nl) = s
        .post(
            "/api/v1/projects/shop/launches",
            ADMIN_TOKEN,
            json!({"name": "rerun", "externalId": external}),
        )
        .await;
    assert!(st.is_success(), "{nl}");
    let new_id = nl["id"].as_str().unwrap().to_string();
    let (_, l) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{id}/reruns"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(l["items"][0]["launchId"], new_id);
    let (_, n) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{new_id}/reruns"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(n["rerunOf"]["sourceLaunchId"], id);

    let (_, audit) = s.get("/api/v1/audit?limit=20", ADMIN_TOKEN).await;
    assert!(audit.to_string().contains("launch.rerun"), "{audit}");
    assert!(!audit.to_string().contains("ghp_secret_value_1"));
}

#[tokio::test]
async fn gitlab_trigger_and_webhook() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db, &ci, false).await;
    s.project("shop").await;
    put_secret(&s, "gl", "glptt-trigger-1", "ci", &ci.url()).await;
    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "gitlab", "url": ci.url(), "repository": "group/app",
            "ref": "release/1.0", "tokenSecretRef": "gl"}}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
    let (id, results) = launch_with_results(&s).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/rerun"),
            ADMIN_TOKEN,
            json!({"mode": "failed", "statuses": ["failed"]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["testCount"], 1);
    assert_eq!(
        b["ciRef"].as_str().unwrap(),
        format!("{}/group/app/-/pipelines/77", ci.url())
    );
    let r = &ci.requests()[0];
    assert_eq!(r.path, "/api/v4/projects/group%2Fapp/trigger/pipeline");
    assert_eq!(r.content_type, "application/x-www-form-urlencoded");
    assert_eq!(r.auth, "");
    assert_eq!(r.private_token, "");
    assert!(r.body.contains("token=glptt-trigger-1"), "{}", r.body);
    assert!(r.body.contains("ref=release%2F1.0"), "{}", r.body);
    assert!(
        r.body.contains("variables%5BFASTPROOF_PLAN_ID%5D="),
        "{}",
        r.body
    );
    assert!(
        r.body.contains(&format!(
            "variables%5BFASTPROOF_LAUNCH%5D={}",
            b["externalId"].as_str().unwrap()
        )),
        "{}",
        r.body
    );

    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
    put_secret(&s, "hook", "hook-bearer-1", "ci", &ci.url()).await;
    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "webhook", "url": format!("{}/hook", ci.url()),
            "tokenSecretRef": "hook"}}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
    let selected: Vec<&str> = results
        .iter()
        .filter(|r| r["name"] == "b" || r["name"] == "a")
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(selected.len(), 2);
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/rerun"),
            ADMIN_TOKEN,
            json!({"mode": "selected", "resultIds": selected}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["mode"], "selected");
    assert_eq!(b["testCount"], 2);
    let r = ci.requests().pop().unwrap();
    assert_eq!(r.path, "/hook");
    assert_eq!(r.auth, "Bearer hook-bearer-1");
    let sent: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(sent["event"], "rerun");
    assert_eq!(sent["newLaunchExternalId"], b["externalId"]);
    assert_eq!(names(&sent["testplan"]), vec!["suite.a", "suite.b"]);
    assert!(sent["planUrl"].is_null());

    tokio::time::sleep(std::time::Duration::from_secs(11)).await;
    let before = ci.requests().len();
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .arg("rerun")
        .args(["--url", &s.base, "--project", "shop", "--launch", &id])
        .env("FASTPROOF_TOKEN", ADMIN_TOKEN)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(printed.starts_with("rerun-"), "{printed}");
    let reqs = ci.requests();
    assert_eq!(reqs.len(), before + 1);
    let sent: Value = serde_json::from_str(&reqs.last().unwrap().body).unwrap();
    assert_eq!(sent["newLaunchExternalId"], printed);
    assert_eq!(names(&sent["testplan"]), vec!["suite.b", "suite.c"]);
}

#[tokio::test]
async fn refusals_limits_and_permissions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db, &ci, true).await;
    s.project("shop").await;
    let (id, results) = launch_with_results(&s).await;
    let path = format!("/api/v1/projects/shop/launches/{id}/rerun");

    let (st, b) = s.post(&path, ADMIN_TOKEN, json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");

    put_secret(&s, "gh", "ghp_secret_value_2", "ci", &ci.url()).await;
    put_secret(&s, "wrong", "x", "vcs", &ci.url()).await;
    put_secret(&s, "elsewhere", "x", "ci", "https://other.example.com").await;
    for bad in ["wrong", "elsewhere"] {
        let (st, b) = put_settings(
            &s,
            json!({"rerun": {"provider": "github", "url": ci.url(), "repository": "acme/shop",
                "workflow": "tests.yml", "tokenSecretRef": bad}}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }
    for bad in [
        json!({"provider": "jenkins", "url": ci.url()}),
        json!({"provider": "github", "url": ci.url(), "repository": "acme/shop", "tokenSecretRef": "gh"}),
        json!({"provider": "gitlab", "url": ci.url(), "repository": "acme/shop"}),
        json!({"provider": "webhook", "url": ci.url(), "repository": "acme/shop"}),
        json!({"provider": "github", "url": ci.url(), "repository": "acme/shop", "workflow": "t.yml", "ref": "../x", "tokenSecretRef": "gh"}),
    ] {
        let (st, b) = put_settings(&s, json!({"rerun": bad})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    }
    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "github", "url": ci.url(), "repository": "acme/shop",
            "workflow": "tests.yml", "tokenSecretRef": "gh"}}),
    )
    .await;
    assert!(st.is_success(), "{b}");

    let viewer = s.token("v", "viewer", &["shop"]).await;
    s.project("elsewhere").await;
    let other = s.token("o", "editor", &["elsewhere"]).await;
    let (st, _) = s.post(&path, &viewer, json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.post(&path, &other, json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.post(&path, "bogus", json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{id}/reruns"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(!b.to_string().contains("ghp_secret"));
    assert!(ci.requests().is_empty());

    for body in [
        json!({"mode": "selected"}),
        json!({"mode": "selected", "resultIds": []}),
        json!({"mode": "selected", "resultIds": [uuid::Uuid::now_v7()]}),
        json!({"mode": "selected", "resultIds": [results[0]["id"]], "statuses": ["failed"]}),
        json!({"mode": "failed", "resultIds": [results[0]["id"]]}),
        json!({"mode": "failed", "statuses": ["nope"]}),
    ] {
        let (st, b) = s.post(&path, ADMIN_TOKEN, body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}: {b}");
    }
    let (st, _) = s
        .post(&path, ADMIN_TOKEN, json!({"mode": "everything"}))
        .await;
    assert!(st.is_client_error());
    assert!(ci.requests().is_empty());

    ci.st.force_status.store(422, Ordering::SeqCst);
    let (st, b) = s.post(&path, ADMIN_TOKEN, json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{b}");
    assert!(b.to_string().contains("HTTP 422"), "{b}");
    assert!(!b.to_string().contains("ghp_secret"));
    assert_eq!(ci.requests().len(), 1);

    let (st, b) = s.post(&path, ADMIN_TOKEN, json!({"mode": "failed"})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(ci.requests().len(), 1);

    let (_, l) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{id}/reruns"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    assert_eq!(l["items"][0]["status"], "failed");
    assert!(l["items"][0]["error"].as_str().unwrap().contains("422"));

    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "github", "url": "http://10.255.255.1", "repository": "acme/shop",
            "workflow": "tests.yml", "tokenSecretRef": "gh"}}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "origin of the secret: {b}");
}

#[tokio::test]
async fn egress_policy_applies() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let k = key.to_string_lossy().to_string();
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", "hooks.example.com"),
            ("FASTPROOF_SECRETS_KEY_FILE", &k),
        ],
    )
    .await;
    s.project("shop").await;
    put_secret(&s, "gh", "ghp_secret_value_3", "ci", &ci.url()).await;
    let (st, b) = put_settings(
        &s,
        json!({"rerun": {"provider": "github", "url": ci.url(), "repository": "acme/shop",
            "workflow": "tests.yml", "tokenSecretRef": "gh"}}),
    )
    .await;
    assert!(st.is_success(), "{b}");
    let (id, _) = launch_with_results(&s).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/rerun"),
            ADMIN_TOKEN,
            json!({"mode": "failed"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{b}");
    assert!(ci.requests().is_empty());
    assert!(!b.to_string().contains(&ci.url()));
}
