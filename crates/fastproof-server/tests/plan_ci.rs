//! CI from a plan (§7.4): launches per environment, dispatch to a local fake CI (GitHub
//! Actions, GitLab pipeline triggers, webhook), variables and refs from the allowlists, limits
//! shared with reruns, SSRF, permissions, IDOR, the dispatch list, the plan check and the
//! `fastproof plan run` CLI. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method as AxMethod, StatusCode as AxStatus, Uri};
use axum::response::{IntoResponse, Response};
use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::tm::{CaseBody, CaseStep};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

const KEY_HEX: &str = "8f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const TOKEN: &str = "ghp_secret_plan_token_1";

#[derive(Clone, Debug)]
struct Rec {
    path: String,
    auth: String,
    event: String,
    body: String,
}

#[derive(Default)]
struct FakeState {
    log: Mutex<Vec<Rec>>,
    force_status: AtomicU16,
    redirect: AtomicBool,
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

    fn clear(&self) {
        self.st.log.lock().unwrap().clear();
    }
}

async fn handle(
    State(st): State<Arc<FakeState>>,
    _method: AxMethod,
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
        path: uri.path().to_string(),
        auth: h("authorization"),
        event: h("x-fastproof-event"),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    if st.redirect.load(Ordering::SeqCst) {
        return (
            AxStatus::TEMPORARY_REDIRECT,
            [("location", "http://127.0.0.1:9/elsewhere")],
        )
            .into_response();
    }
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

async fn start(db: &TestDb) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let k = key.to_string_lossy().to_string();
    let s = Server::start_with(
        db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
            ("FASTPROOF_SECRETS_KEY_FILE", &k),
            ("FASTPROOF_TM_GATE", "open"),
            ("FASTPROOF_PUBLIC_URL", "https://fastproof.example.com"),
        ],
    )
    .await;
    (s, dir)
}

async fn put_secret(s: &Server, key: &str, name: &str, origin: &str, scope: &str) {
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/{key}/secrets/{name}"),
            Some(ADMIN_TOKEN),
            Some(json!({"value": TOKEN, "purpose": "ci", "origin": origin, "scope": scope})),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
}

async fn put_rerun(s: &Server, key: &str, rerun: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        &format!("/api/v1/projects/{key}/settings"),
        Some(ADMIN_TOKEN),
        Some(json!({"rerun": rerun})),
    )
    .await
}

fn variables() -> Value {
    json!([
        {"name": "BROWSER", "values": ["chromium", "firefox"], "default": "chromium"},
        {"name": "SUITE"}
    ])
}

async fn github(s: &Server, ci: &Ci, key: &str) {
    put_secret(s, key, "gh", &ci.url(), "github:acme/shop:tests.yml").await;
    let (st, b) = put_rerun(
        s,
        key,
        json!({"provider": "github", "url": ci.url(), "repository": "acme/shop",
            "workflow": "tests.yml", "ref": "main", "tokenSecretRef": "gh",
            "refs": ["main", "release/*"], "variables": variables()}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
}

async fn gitlab(s: &Server, ci: &Ci, key: &str) {
    put_secret(s, key, "gl", &ci.url(), "gitlab:acme/shop").await;
    let (st, b) = put_rerun(
        s,
        key,
        json!({"provider": "gitlab", "url": ci.url(), "repository": "acme/shop",
            "tokenSecretRef": "gl", "refs": ["main"], "variables": variables()}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
}

async fn webhook(s: &Server, ci: &Ci, key: &str) {
    put_secret(s, key, "wh", &ci.url(), &format!("webhook:{}", ci.url())).await;
    let (st, b) = put_rerun(
        s,
        key,
        json!({"provider": "webhook", "url": ci.url(), "tokenSecretRef": "wh",
            "variables": variables()}),
    )
    .await;
    assert!(st.is_success(), "{st} {b}");
}

async fn pid(db: &TestDb, key: &str) -> Uuid {
    sqlx::query_scalar("select id from projects where key = $1")
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn case(db: &TestDb, project: Uuid, name: &str) -> i64 {
    let mut conn = db.pool.acquire().await.unwrap();
    let body = CaseBody {
        description: format!("About {name}"),
        preconditions: String::new(),
        steps: vec![CaseStep {
            id: format!("s_{}", Uuid::now_v7().simple()),
            action: "Do it".into(),
            expected: "It works".into(),
            data: String::new(),
            attachments: Vec::new(),
            steps: Vec::new(),
            shared: None,
        }],
        links: Vec::new(),
        parameters: None,
    };
    fastproof_store::tm::testutil::insert_case(&mut conn, project, name, &body, None)
        .await
        .unwrap()
        .number
}

async fn ingest(s: &Server, launch: &str, names: &[&str]) {
    let body: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({"id": format!("{launch}-{n}"), "name": n, "fullName": format!("suite.{n}"),
                "status": "passed", "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64})
        })
        .collect();
    let key = "shop";
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{launch}/results"),
            ADMIN_TOKEN,
            Value::Array(body),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    for _ in 0..100 {
        if s.results(key, launch, "").await.len() == names.len() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("results of {launch} were not ingested");
}

/// A project `shop` with a managed plan of two automated cases (suite.a, suite.b) and one
/// manual case. Returns the plan id.
async fn plan_project(db: &TestDb, s: &Server) -> String {
    s.project("shop").await;
    let p = pid(db, "shop").await;
    let seed = s.launch("shop", None).await;
    let seed_id = seed["id"].as_str().unwrap().to_string();
    ingest(s, &seed_id, &["a", "b", "x"]).await;
    let mut numbers = Vec::new();
    for n in ["a", "b"] {
        let c = case(db, p, &format!("auto {n}")).await;
        sqlx::query(
            "update test_cases set case_id = (select id from cases where project_id = $1 and number = $2) \
             where project_id = $1 and full_name = $3",
        )
        .bind(p)
        .bind(c)
        .bind(format!("suite.{n}"))
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("update cases set automated = true where project_id = $1 and number = $2")
            .bind(p)
            .bind(c)
            .execute(&db.pool)
            .await
            .unwrap();
        numbers.push(c);
    }
    numbers.push(case(db, p, "manual one").await);
    let (st, b) = s
        .post(
            "/api/v1/projects/shop/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Nightly", "caseNumbers": numbers}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["id"].as_str().unwrap().to_string()
}

fn launches_path(plan: &str) -> String {
    format!("/api/v1/projects/shop/test-plans/{plan}/launches")
}

async fn run_ci(s: &Server, token: &str, plan: &str, ci: Value) -> (StatusCode, Value) {
    s.post(
        &launches_path(plan),
        token,
        json!({"name": "ci run", "includeAutomated": true, "includeManual": false, "ci": ci}),
    )
    .await
}

async fn backdate(db: &TestDb) {
    sqlx::query("update plan_dispatches set created_at = created_at - interval '1 minute'")
        .execute(&db.pool)
        .await
        .unwrap();
}

async fn launch_count(s: &Server, plan: &str) -> usize {
    let (_, b) = s
        .get(&format!("{}?limit=100", launches_path(plan)), ADMIN_TOKEN)
        .await;
    b["items"].as_array().unwrap().len()
}

fn form(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn github_gets_one_launch_per_environment_with_the_allowed_variables() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;

    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        &plan,
        json!({"dispatch": true, "ref": "release/1.2", "variables": {"SUITE": "smoke"},
            "environments": [{"name": "stage"},
                {"name": "prod", "variables": {"BROWSER": "firefox", "SUITE": "full"}}]}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let launches = b["launches"].as_array().unwrap();
    assert_eq!(launches.len(), 2);
    assert_eq!(b["launch"]["id"], launches[0]["id"]);
    let dispatches = b["dispatches"].as_array().unwrap();
    assert_eq!(dispatches.len(), 2);
    assert!(dispatches.iter().all(|d| d["status"] == "dispatched"));
    assert_eq!(dispatches[0]["environment"], "stage");
    assert_eq!(dispatches[1]["gitRef"], "release/1.2");
    assert_eq!(
        dispatches[1]["variables"],
        json!({"BROWSER": "firefox", "SUITE": "full"})
    );
    assert_eq!(
        dispatches[0]["variables"],
        json!({"BROWSER": "chromium", "SUITE": "smoke"}),
        "the default fills the rest"
    );
    for (i, l) in launches.iter().enumerate() {
        assert_eq!(l["environment"]["key"], ["stage", "prod"][i], "{l}");
        assert!(l["externalId"].as_str().unwrap().starts_with("plan-"));
        assert_eq!(
            l["manual"],
            Value::Null,
            "includeManual false: no manual run"
        );
    }
    assert_ne!(launches[0]["externalId"], launches[1]["externalId"]);

    let reqs = ci.requests();
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        assert_eq!(
            r.path,
            "/repos/acme/shop/actions/workflows/tests.yml/dispatches"
        );
        assert_eq!(r.auth, format!("Bearer {TOKEN}"));
    }
    let mut seen = Vec::new();
    for r in &reqs {
        let sent: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(sent["ref"], "release/1.2");
        assert_eq!(sent["inputs"]["fastproof_plan_id"], plan);
        let vars: Value =
            serde_json::from_str(sent["inputs"]["fastproof_variables"].as_str().unwrap()).unwrap();
        seen.push((
            sent["inputs"]["fastproof_launch"]
                .as_str()
                .unwrap()
                .to_string(),
            vars,
        ));
    }
    seen.sort_by_key(|(_, v)| v["FASTPROOF_ENVIRONMENT"].to_string());
    let prod = seen
        .iter()
        .find(|(_, v)| v["FASTPROOF_ENVIRONMENT"] == "prod")
        .unwrap();
    assert_eq!(prod.1["BROWSER"], "firefox");
    assert_eq!(prod.1["SUITE"], "full");
    assert_eq!(prod.0, launches[1]["externalId"].as_str().unwrap());

    let (st, tp) = s
        .get(
            &format!(
                "/api/v1/projects/shop/launches/{}/testplan.json",
                launches[0]["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{tp}");
    assert_eq!(tp["tests"].as_array().unwrap().len(), 2);

    backdate(&db).await;
    ci.clear();
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let sent: Value = serde_json::from_str(&ci.requests()[0].body).unwrap();
    assert_eq!(sent["ref"], "main");
    let vars: Value =
        serde_json::from_str(sent["inputs"]["fastproof_variables"].as_str().unwrap()).unwrap();
    assert_eq!(
        vars,
        json!({"BROWSER": "chromium"}),
        "no FASTPROOF_ENVIRONMENT without one"
    );
    assert_eq!(b["launch"]["environment"], Value::Null);
}

#[tokio::test]
async fn github_without_variables_sends_no_variables_input() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    put_secret(&s, "shop", "gh", &ci.url(), "github:acme/shop:tests.yml").await;
    let (st, b) = put_rerun(
        &s,
        "shop",
        json!({"provider": "github", "url": ci.url(), "repository": "acme/shop",
            "workflow": "tests.yml", "tokenSecretRef": "gh"}),
    )
    .await;
    assert!(st.is_success(), "{b}");
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let sent: Value = serde_json::from_str(&ci.requests()[0].body).unwrap();
    assert!(
        sent["inputs"].get("fastproof_variables").is_none(),
        "{sent}"
    );
    assert_eq!(sent["inputs"]["fastproof_plan_id"], plan);
    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        &plan,
        json!({"dispatch": true, "variables": {"A": "1"}}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "no rerun.variables: nothing is allowed: {b}"
    );
    assert_eq!(b["code"], "ci_variable_not_allowed");
}

#[tokio::test]
async fn gitlab_and_webhook_request_shapes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;

    gitlab(&s, &ci, "shop").await;
    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        &plan,
        json!({"dispatch": true, "variables": {"SUITE": "smoke"},
            "environments": [{"name": "stage"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let d = &b["dispatches"][0];
    assert_eq!(d["status"], "dispatched");
    assert!(
        d["ciRef"].as_str().unwrap().ends_with("/pipelines/77"),
        "{d}"
    );
    let r = &ci.requests()[0];
    assert_eq!(r.path, "/api/v4/projects/acme%2Fshop/trigger/pipeline");
    let f = form(&r.body);
    let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("token").as_deref(), Some(TOKEN));
    assert_eq!(get("ref").as_deref(), Some("main"));
    assert_eq!(get("variables[SUITE]").as_deref(), Some("smoke"));
    assert_eq!(get("variables[BROWSER]").as_deref(), Some("chromium"));
    assert_eq!(
        get("variables[FASTPROOF_ENVIRONMENT]").as_deref(),
        Some("stage")
    );
    assert_eq!(
        get("variables[FASTPROOF_PLAN_ID]").as_deref(),
        Some(plan.as_str())
    );
    assert_eq!(
        get("variables[FASTPROOF_LAUNCH]").as_deref(),
        b["launch"]["externalId"].as_str()
    );

    backdate(&db).await;
    ci.clear();
    webhook(&s, &ci, "shop").await;
    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        &plan,
        json!({"dispatch": true, "environments": [{"name": "prod", "variables": {"BROWSER": "firefox"}}]}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["dispatches"][0]["status"], "dispatched");
    let r = &ci.requests()[0];
    assert_eq!(r.event, "plan");
    assert_eq!(r.auth, format!("Bearer {TOKEN}"));
    let sent: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(sent["event"], "plan");
    assert_eq!(sent["mode"], "plan");
    assert_eq!(sent["environment"], "prod");
    assert_eq!(
        sent["variables"],
        json!({"BROWSER": "firefox", "FASTPROOF_ENVIRONMENT": "prod"})
    );
    assert_eq!(sent["planId"], plan);
    assert_eq!(sent["newLaunchExternalId"], b["launch"]["externalId"]);
    assert_eq!(sent["testCount"], 2);
}

#[tokio::test]
async fn variables_refs_and_settings_are_checked_before_anything_starts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;

    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CONFLICT, "no CI settings: {b}");
    assert_eq!(b["code"], "conflict");

    github(&s, &ci, "shop").await;
    let cases: Vec<(Value, &str, Option<&str>)> = vec![
        (
            json!({"variables": {"HOME": "/root"}}),
            "ci_variable_not_allowed",
            Some("HOME"),
        ),
        (
            json!({"variables": {"FASTPROOF_URL": "x"}}),
            "ci_variable_not_allowed",
            Some("FASTPROOF_URL"),
        ),
        (
            json!({"variables": {"BROWSER": "safari"}}),
            "ci_variable_not_allowed",
            Some("BROWSER"),
        ),
        (
            json!({"environments": [{"name": "stage", "variables": {"BROWSER": "ie"}}]}),
            "ci_variable_not_allowed",
            Some("BROWSER"),
        ),
        (json!({"variables": {"SUITE": ""}}), "bad_request", None),
        (json!({"variables": {"SUITE": "a\nb"}}), "bad_request", None),
        (
            json!({"variables": {"SUITE": "x".repeat(257)}}),
            "bad_request",
            None,
        ),
        (json!({"ref": "feature/x"}), "ci_ref_not_allowed", None),
        (json!({"ref": "../main"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/1/2"}), "ci_ref_not_allowed", None),
        (json!({"ref": "main; rm"}), "ci_ref_not_allowed", None),
        (json!({"ref": "-main"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/x.lock"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/a b"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/a~1"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/a:b"}), "ci_ref_not_allowed", None),
        (json!({"ref": "release/a\u{7}"}), "ci_ref_not_allowed", None),
        (json!({"ref": "r".repeat(201)}), "ci_ref_not_allowed", None),
        (
            json!({"environments": [{"name": "a"}, {"name": "a"}]}),
            "bad_request",
            None,
        ),
        (
            json!({"environments": [{"name": "bad name"}]}),
            "bad_request",
            None,
        ),
        (
            json!({"environments": [{"name": "../x"}]}),
            "bad_request",
            None,
        ),
        (
            json!({"environments": (0..11).map(|i| json!({"name": format!("e{i}")})).collect::<Vec<_>>()}),
            "bad_request",
            None,
        ),
    ];
    for (mut ci_body, code, name) in cases {
        ci_body["dispatch"] = json!(true);
        let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, ci_body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{ci_body}: {b}");
        assert_eq!(b["code"], code, "{ci_body}: {b}");
        if let Some(n) = name {
            assert_eq!(b["details"]["name"], n, "{b}");
        }
    }
    assert!(ci.requests().is_empty(), "nothing reached the CI");
    assert_eq!(launch_count(&s, &plan).await, 0, "no launch was started");

    let (st, b) = s
        .post(
            &launches_path(&plan),
            ADMIN_TOKEN,
            json!({"name": "x", "externalId": "fixed", "ci": {"dispatch": true,
                "environments": [{"name": "a"}, {"name": "b"}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");

    let manual = s
        .post(
            "/api/v1/projects/shop/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Manual only", "caseNumbers": [3]}),
        )
        .await
        .1;
    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        manual["id"].as_str().unwrap(),
        json!({"dispatch": true}),
    )
    .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("plan_has_no_automated_cases")),
        "{b}"
    );
    assert_eq!(launch_count(&s, manual["id"].as_str().unwrap()).await, 0);
}

#[tokio::test]
async fn a_ci_refusal_keeps_the_launches_and_hides_the_destination() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    let editor = s.token("ed", "editor", &["shop"]).await;
    let viewer = s.token("vi", "viewer", &["shop"]).await;

    ci.st.force_status.store(422, Ordering::SeqCst);
    let (st, b) = run_ci(&s, &editor, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let d = &b["dispatches"][0];
    assert_eq!(d["status"], "failed");
    assert_eq!(d["error"], "the CI answered HTTP 422");
    assert_eq!(launch_count(&s, &plan).await, 1, "the launch stays");
    assert_eq!(ci.requests().len(), 1, "no retry");

    backdate(&db).await;
    ci.st.force_status.store(0, Ordering::SeqCst);
    sqlx::query("update plan_dispatches set error = 'egress denied: ci.internal.example is not in FASTPROOF_EGRESS_ALLOW'")
        .execute(&db.pool)
        .await
        .unwrap();
    let (_, as_admin) = s
        .get(
            &format!("/api/v1/projects/shop/test-plans/{plan}/dispatches"),
            ADMIN_TOKEN,
        )
        .await;
    assert!(
        as_admin.to_string().contains("ci.internal.example"),
        "{as_admin}"
    );
    let (st, as_viewer) = s
        .get(
            &format!("/api/v1/projects/shop/test-plans/{plan}/dispatches"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{as_viewer}");
    let t = as_viewer.to_string();
    assert!(!t.contains("internal") && !t.contains("EGRESS"), "{t}");
    assert!(t.contains("not reached"), "{t}");

    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let all = audit.to_string();
    assert!(!all.contains(TOKEN), "tokens never reach the audit log");
    let items: Vec<&Value> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == "testPlan.dispatch")
        .collect();
    assert_eq!(items.len(), 1, "{audit}");
    assert_eq!(items[0]["detail"]["ok"], false);
    assert_eq!(
        items[0]["detail"]["variables"],
        json!(["BROWSER"]),
        "names only"
    );
    assert_eq!(items[0]["detail"]["ref"], "main");
    assert_eq!(items[0]["detail"]["target"]["workflow"], "tests.yml");
    assert!(
        !items[0].to_string().contains("chromium"),
        "values are not audited: {}",
        items[0]
    );
}

#[tokio::test]
async fn the_hourly_limit_is_shared_with_reruns_and_a_plan_has_a_cooldown() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;

    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "cooldown: {b}");
    assert_eq!(b["code"], "too_many_requests");
    assert_eq!(ci.requests().len(), 1);
    assert_eq!(
        launch_count(&s, &plan).await,
        1,
        "a refused start leaves no launch"
    );

    backdate(&db).await;
    let p = pid(&db, "shop").await;
    let src: Uuid = sqlx::query_scalar("select id from launches where project_id = $1 limit 1")
        .bind(p)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into launch_reruns (id, project_id, source_launch_id, external_id, provider, mode, status, requested_by) \
         select gen_random_uuid(), $1, $2, 'seed-' || g, 'webhook', 'failed', 'failed', 'seed' from generate_series(1, 58) g",
    )
    .bind(p)
    .bind(src)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update launch_reruns set created_at = now() - interval '5 minutes'")
        .execute(&db.pool)
        .await
        .unwrap();
    let store = fastproof_store::Store::new(db.pool.clone());
    assert_eq!(
        store.ci_triggers_last_hour(p).await.unwrap(),
        59,
        "58 reruns and the plan dispatch count together"
    );
    let (st, b) = run_ci(
        &s,
        ADMIN_TOKEN,
        &plan,
        json!({"dispatch": true, "environments": [{"name": "a"}, {"name": "b"}]}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::TOO_MANY_REQUESTS,
        "two more would pass 60: {b}"
    );
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "the 60th: {b}");
    backdate(&db).await;
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert!(b["message"].as_str().unwrap().contains("hour"), "{b}");

    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{src}/rerun"),
            ADMIN_TOKEN,
            json!({"mode": "failed"}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::TOO_MANY_REQUESTS,
        "a rerun counts plan dispatches too: {b}"
    );
    assert!(b["message"].as_str().unwrap().contains("hour"), "{b}");
}

#[tokio::test]
async fn egress_and_redirects_cannot_send_the_token_elsewhere() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;

    ci.st.redirect.store(true, Ordering::SeqCst);
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["dispatches"][0]["status"], "failed", "{b}");
    assert_eq!(ci.requests().len(), 1, "the redirect was not followed");
    assert!(!b.to_string().contains(TOKEN));
    ci.st.redirect.store(false, Ordering::SeqCst);

    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let k = key.to_string_lossy().to_string();
    let strict = Server::start_with(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", "hooks.example.com"),
            ("FASTPROOF_SECRETS_KEY_FILE", &k),
            ("FASTPROOF_TM_GATE", "open"),
        ],
    )
    .await;
    backdate(&db).await;
    ci.clear();
    let (st, b) = run_ci(&strict, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["dispatches"][0]["status"], "failed", "{b}");
    assert!(
        ci.requests().is_empty(),
        "the host is not on the egress allowlist"
    );
    let editor = strict.token("ed", "editor", &["shop"]).await;
    backdate(&db).await;
    let (st, b) = run_ci(&strict, &editor, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let t = b.to_string();
    assert!(!t.contains("127.0.0.1") && !t.contains("EGRESS"), "{t}");
}

#[tokio::test]
async fn permissions_and_project_ownership() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    s.project("other").await;
    let viewer = s.token("vi", "viewer", &["shop"]).await;
    let uploader = s.token("up", "uploader", &["shop"]).await;
    let tester = s.token("te", "tester", &["shop"]).await;
    let editor = s.token("ed", "editor", &["shop"]).await;
    let stranger = s.token("st", "editor", &["other"]).await;

    let ci_body = json!({"dispatch": true});
    for (who, token) in [
        ("viewer", &viewer),
        ("uploader", &uploader),
        ("stranger", &stranger),
    ] {
        let (st, b) = run_ci(&s, token, &plan, ci_body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{who}: {b}");
    }
    let (st, _) = run_ci(&s, "bogus", &plan, ci_body.clone()).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = run_ci(&s, &tester, &plan, ci_body.clone()).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "runs:execute without launches:rerun: {b}"
    );
    let (st, b) = s
        .post(
            &launches_path(&plan),
            &tester,
            json!({"name": "no ci", "includeAutomated": true}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "runs:execute alone starts without CI: {b}"
    );
    assert!(ci.requests().is_empty(), "nothing was dispatched yet");
    backdate(&db).await;
    let (st, b) = run_ci(&s, &editor, &plan, ci_body.clone()).await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "editor holds both permissions: {b}"
    );

    let a = "/api/v1/projects/shop";
    let o = "/api/v1/projects/other";
    let (st, _) = s
        .get(&format!("{a}/test-plans/{plan}/dispatches"), &stranger)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(&format!("{a}/test-plans/{plan}/dispatches"), "bogus")
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = s
        .get(&format!("{o}/test-plans/{plan}/dispatches"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a plan of another project: {b}");
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, ci_body.clone()).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    let (st, b) = s
        .post(
            &format!("{o}/test-plans/{plan}/launches"),
            ADMIN_TOKEN,
            json!({"name": "x", "ci": {"dispatch": true}}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "start from another project's plan: {b}"
    );
    let (_, dl) = s
        .get(&format!("{a}/test-plans/{plan}/dispatches"), &viewer)
        .await;
    let launch_id = dl["items"][0]["launchId"].as_str().unwrap().to_string();
    let (st, _) = s
        .get(&format!("{o}/launches/{launch_id}/plan-check"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch of another project");
    let (st, _) = s
        .get(&format!("{a}/launches/{launch_id}/plan-check"), &stranger)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(&format!("{a}/launches/{launch_id}/plan-check"), "bogus")
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = s
        .get(
            &format!("{a}/test-plans/{plan}/dispatches?before=junk"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
}

#[tokio::test]
async fn dispatches_are_listed_newest_first_with_a_cursor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    for _ in 0..3 {
        let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
        assert_eq!(st, StatusCode::CREATED, "{b}");
        backdate(&db).await;
    }
    let base = format!("/api/v1/projects/shop/test-plans/{plan}/dispatches");
    let (st, p1) = s.get(&format!("{base}?limit=2"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{p1}");
    assert_eq!(p1["items"].as_array().unwrap().len(), 2);
    let next = p1["next"].as_str().unwrap();
    let (_, p2) = s
        .get(&format!("{base}?limit=2&before={next}"), ADMIN_TOKEN)
        .await;
    assert_eq!(p2["items"].as_array().unwrap().len(), 1);
    assert_eq!(p2["next"], Value::Null);
    let numbers: Vec<i64> = p1["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(p2["items"].as_array().unwrap())
        .map(|d| d["launchNumber"].as_i64().unwrap())
        .collect();
    assert!(numbers.windows(2).all(|w| w[0] > w[1]), "{numbers:?}");
    assert_eq!(p1["items"][0]["requestedBy"], "admin");
    let events: i64 = sqlx::query_scalar(
        "select count(*) from events where type = 'planDispatch.updated' and payload ->> 'planId' = $1",
    )
    .bind(&plan)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        events, 6,
        "dispatching and the outcome of each of three dispatches"
    );
}

#[tokio::test]
async fn the_plan_check_names_extra_results_and_missing_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    let (st, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let id = b["launch"]["id"].as_str().unwrap().to_string();
    let path = format!("/api/v1/projects/shop/launches/{id}/plan-check");

    let (st, b) = s.get(&path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "not finished: {b}");
    ingest(&s, &id, &["a", "x"]).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{id}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let (st, c) = s.get(&path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["planId"], plan);
    assert_eq!(c["inPlan"], 2);
    assert_eq!(c["ran"], 1);
    assert_eq!(c["extra"], 1);
    assert_eq!(c["missing"], 1);
    assert_eq!(c["extraNames"], json!(["suite.x"]));
    assert_eq!(c["warning"], true);

    let plain = s.launch("shop", None).await;
    let pid_ = plain["id"].as_str().unwrap();
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{pid_}/plan-check"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch without a plan");

    backdate(&db).await;
    let (_, b) = run_ci(&s, ADMIN_TOKEN, &plan, json!({"dispatch": true})).await;
    let id2 = b["launch"]["id"].as_str().unwrap().to_string();
    ingest(&s, &id2, &["a", "b"]).await;
    s.post(
        &format!("/api/v1/projects/shop/launches/{id2}/finish"),
        ADMIN_TOKEN,
        json!({}),
    )
    .await;
    let (_, c) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{id2}/plan-check"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        (
            c["ran"].as_i64(),
            c["extra"].as_i64(),
            c["missing"].as_i64(),
            c["warning"].as_bool()
        ),
        (Some(2), Some(0), Some(0), Some(false)),
        "{c}"
    );
}

#[tokio::test]
async fn the_cli_starts_launches_and_exits_one_on_a_failed_dispatch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    let run = |extra: Vec<&str>| {
        let base = s.base.clone();
        let plan = plan.clone();
        let extra: Vec<String> = extra.into_iter().map(str::to_string).collect();
        async move {
            let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
                .args([
                    "plan",
                    "run",
                    "--url",
                    &base,
                    "--project",
                    "shop",
                    "--plan",
                    &plan,
                ])
                .args(&extra)
                .env("FASTPROOF_TOKEN", ADMIN_TOKEN)
                .env_remove("FASTPROOF_LAUNCH")
                .output()
                .await
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
            )
        }
    };
    let (code, out, err) = run(vec![
        "--env",
        "stage",
        "--env",
        "prod",
        "--var",
        "BROWSER=firefox",
        "--ref",
        "release/9",
    ])
    .await;
    assert_eq!(code, Some(0), "{err}");
    let ids: Vec<&str> = out.lines().collect();
    assert_eq!(ids.len(), 2, "{out}");
    assert!(ids.iter().all(|i| i.starts_with("plan-")));
    assert_eq!(ci.requests().len(), 2);
    let sent: Value = serde_json::from_str(&ci.requests()[0].body).unwrap();
    assert_eq!(sent["ref"], "release/9");
    assert!(err.contains("dispatched"), "{err}");

    backdate(&db).await;
    ci.st.force_status.store(500, Ordering::SeqCst);
    let (code, out, err) = run(vec![]).await;
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(out.lines().count(), 1, "the launch id is printed anyway");
    assert!(err.contains("not dispatched"), "{err}");
    backdate(&db).await;
    let (code, _, err) = run(vec!["--var", "HOME=/root"]).await;
    assert_ne!(code, Some(0));
    assert!(
        err.contains("ci_variable_not_allowed") || err.contains("not in"),
        "{err}"
    );
}

#[tokio::test]
async fn parallel_starts_of_one_plan_dispatch_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    let base = s.base.clone();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let (base, plan) = (base.clone(), plan.clone());
        tasks.push(tokio::spawn(async move {
            let c = Server::client_only(&base);
            run_ci(&c, ADMIN_TOKEN, &plan, json!({"dispatch": true}))
                .await
                .0
        }));
    }
    let mut created = 0;
    let mut limited = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::CREATED => created += 1,
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!((created, limited), (1, 3));
    assert_eq!(ci.requests().len(), 1, "one CI call");
    assert_eq!(
        launch_count(&s, &plan).await,
        1,
        "refused starts leave no launch"
    );
}

/// Dispatch latency without the CI's answer time (a local fake CI answers at once); run with
/// `--release --ignored --nocapture` for the contract's target of 300 ms.
#[tokio::test]
#[ignore = "timing, run in a release build"]
async fn dispatch_latency() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    let plan = plan_project(&db, &s).await;
    github(&s, &ci, "shop").await;
    let mut ms = Vec::new();
    for _ in 0..20 {
        backdate(&db).await;
        let t = std::time::Instant::now();
        let (st, b) = run_ci(
            &s,
            ADMIN_TOKEN,
            &plan,
            json!({"dispatch": true, "environments": [{"name": "stage"}]}),
        )
        .await;
        ms.push(t.elapsed().as_millis());
        assert_eq!(st, StatusCode::CREATED, "{b}");
        sqlx::query("update launches set manual_state = null")
            .execute(&db.pool)
            .await
            .unwrap();
    }
    ms.sort_unstable();
    println!(
        "plan dispatch p50 {} ms, max {} ms over 20 starts",
        ms[10], ms[19]
    );
    assert!(ms[10] < 300, "{ms:?}");
}

#[tokio::test]
async fn reserved_variable_names_cannot_be_allowlisted() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ci = Ci::start().await;
    let (s, _d) = start(&db).await;
    s.project("shop").await;
    put_secret(&s, "shop", "gh", &ci.url(), "github:acme/shop:tests.yml").await;
    for name in [
        "CI_DEBUG_TRACE",
        "GITLAB_TOKEN",
        "GITHUB_ENV",
        "RUNNER_DEBUG",
        "ACTIONS_STEP_DEBUG",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "PATH",
        "BASH_ENV",
        "ENV",
    ] {
        let (st, b) = put_rerun(
            &s,
            "shop",
            json!({"provider": "github", "url": ci.url(), "repository": "acme/shop",
                "workflow": "tests.yml", "tokenSecretRef": "gh", "variables": [{"name": name}]}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}: {b}");
    }
}
