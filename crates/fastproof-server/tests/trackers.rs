//! Issue trackers against a local mock of the Jira Cloud, Jira Server/Data Center, GitLab,
//! GitHub and Azure DevOps APIs (request and answer shapes from their public API
//! references): create with templates and screenshots, idempotency, defect links, state sync
//! by polling and by webhook, closing suggests un-muting, rate limits, link existing, the
//! connection test, permissions and secret handling. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method as AxMethod, StatusCode as AxStatus, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const KEY_HEX: &str = "8f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
const GL_TOKEN: &str = "glpat-0123456789abcdef";
const HOOK_SECRET: &str = "hook-secret-0123456789";

#[derive(Clone, Debug)]
struct Rec {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Issue {
    title: String,
    state: String,
    closed: bool,
}

#[derive(Default)]
struct Mock {
    log: Vec<Rec>,
    issues: HashMap<(String, u64), Issue>,
    seq: u64,
    throttle: HashMap<String, u32>,
}

type Shared = Arc<Mutex<Mock>>;

struct Fake {
    addr: std::net::SocketAddr,
    st: Shared,
}

impl Fake {
    async fn start() -> Self {
        let st: Shared = Arc::default();
        let app = axum::Router::new().fallback(handle).with_state(st.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        Self { addr, st }
    }

    fn url(&self, prefix: &str) -> String {
        format!("http://{}{prefix}", self.addr)
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self, method: &str, prefix: &str) -> Vec<Rec> {
        self.st
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|r| r.method == method && r.path.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn set(&self, flavor: &str, n: u64, state: &str, closed: bool) {
        let mut m = self.st.lock().unwrap();
        let i = m.issues.get_mut(&(flavor.to_string(), n)).unwrap();
        i.state = state.into();
        i.closed = closed;
    }

    fn throttle(&self, flavor: &str, times: u32) {
        self.st
            .lock()
            .unwrap()
            .throttle
            .insert(flavor.into(), times);
    }
}

fn reply(status: u16, v: Value) -> Response {
    (AxStatus::from_u16(status).unwrap(), axum::Json(v)).into_response()
}

fn new_issue(m: &mut Mock, flavor: &str, title: &str, state: &str) -> u64 {
    m.seq += 1;
    let n = m.seq;
    m.issues.insert(
        (flavor.into(), n),
        Issue {
            title: title.into(),
            state: state.into(),
            closed: false,
        },
    );
    n
}

fn jira_issue(base: &str, n: u64, i: &Issue) -> Value {
    let (key, name) = if i.closed {
        ("done", "Done")
    } else {
        ("new", "To Do")
    };
    json!({"expand": "renderedFields", "id": (10000 + n).to_string(), "self": format!("{base}/issue/{}", 10000 + n),
        "key": format!("SHOP-{n}"),
        "fields": {"summary": i.title, "resolution": if i.closed { json!({"name": "Fixed"}) } else { Value::Null },
            "status": {"self": format!("{base}/status/1"), "description": "", "name": if i.closed { "Done".to_string() } else { i.state.clone() },
                "id": "1", "statusCategory": {"self": format!("{base}/statuscategory/2"), "id": 2, "key": key, "colorName": "blue-gray", "name": name}}}})
}

async fn handle(
    State(st): State<Shared>,
    method: AxMethod,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let hs: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let mut m = st.lock().unwrap();
    m.log.push(Rec {
        method: method.to_string(),
        path: path.clone(),
        headers: hs.clone(),
        body: body.to_vec(),
    });
    let flavor = path.split('/').nth(1).unwrap_or_default().to_string();
    if let Some(n) = m.throttle.get_mut(&flavor)
        && *n > 0
    {
        *n -= 1;
        return (
            AxStatus::TOO_MANY_REQUESTS,
            [("retry-after", "2")],
            axum::Json(json!({"message": "slow down"})),
        )
            .into_response();
    }
    let auth = hs.get("authorization").cloned().unwrap_or_default();
    let host = format!("http://{}", hs.get("host").cloned().unwrap_or_default());
    let json_body = || serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match (flavor.as_str(), method.as_str()) {
        ("jc" | "js", _) => {
            let ok = if flavor == "jc" {
                auth == format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode("bot@example.com:jira-api-token")
                )
            } else {
                auth == "Bearer jira-pat"
            };
            if !ok {
                return reply(
                    401,
                    json!({"errorMessages": ["You are not authenticated."], "errors": {}}),
                );
            }
            let v = if flavor == "jc" { "3" } else { "2" };
            let base = format!("{host}/{flavor}/rest/api/{v}");
            match (method.as_str(), &seg[1..]) {
                ("GET", ["rest", "api", vv, "project", key]) if *vv == v => {
                    if *key == "SHOP" {
                        reply(
                            200,
                            json!({"id": "10000", "key": "SHOP", "name": "Shop", "projectTypeKey": "software"}),
                        )
                    } else {
                        reply(
                            404,
                            json!({"errorMessages": ["No project could be found with key 'X'."], "errors": {}}),
                        )
                    }
                }
                ("POST", ["rest", "api", vv, "issue"]) if *vv == v => {
                    let b = json_body();
                    let f = &b["fields"];
                    if f["project"]["key"] != "SHOP" || f["issuetype"]["name"].as_str().is_none() {
                        return reply(
                            400,
                            json!({"errorMessages": [], "errors": {"project": "valid project is required"}}),
                        );
                    }
                    let title = f["summary"].as_str().unwrap_or_default().to_string();
                    let n = new_issue(&mut m, &flavor, &title, "To Do");
                    reply(
                        201,
                        json!({"id": (10000 + n).to_string(), "key": format!("SHOP-{n}"), "self": format!("{base}/issue/{}", 10000 + n)}),
                    )
                }
                ("POST", ["rest", "api", vv, "issue", key, "attachments"]) if *vv == v => {
                    if hs.get("x-atlassian-token").map(String::as_str) != Some("no-check") {
                        return reply(403, json!({"errorMessages": ["XSRF check failed"]}));
                    }
                    reply(
                        200,
                        json!([{"self": format!("{base}/attachment/1"), "id": "1", "filename": format!("{key}.png"),
                        "size": body.len(), "mimeType": "image/png", "content": format!("{base}/attachment/content/1")}]),
                    )
                }
                ("GET", ["rest", "api", vv, "issue", key]) if *vv == v => {
                    let n: u64 = key.trim_start_matches("SHOP-").parse().unwrap_or(0);
                    match m.issues.get(&(flavor.clone(), n)) {
                        Some(i) => reply(200, jira_issue(&base, n, i)),
                        None => reply(
                            404,
                            json!({"errorMessages": ["Issue does not exist or you do not have permission to see it."], "errors": {}}),
                        ),
                    }
                }
                _ => reply(404, json!({"errorMessages": ["no route"]})),
            }
        }
        ("gl", _) => {
            if auth != format!("Bearer {GL_TOKEN}") {
                return reply(401, json!({"message": "401 Unauthorized"}));
            }
            if seg.get(4) != Some(&"group%2Fapp") {
                return reply(404, json!({"message": "404 Project Not Found"}));
            }
            let web = |iid: u64| format!("{host}/group/app/-/issues/{iid}");
            let issue = |iid: u64, i: &Issue| {
                json!({"id": 1000 + iid, "iid": iid, "project_id": 4, "title": i.title, "description": "",
                    "state": if i.closed { "closed" } else { "opened" }, "labels": ["fastproof"], "web_url": web(iid),
                    "closed_at": if i.closed { json!("2026-11-23T10:00:00.000Z") } else { Value::Null }})
            };
            match (method.as_str(), &seg[5..]) {
                ("GET", []) => reply(
                    200,
                    json!({"id": 4, "path_with_namespace": "group/app", "web_url": format!("{host}/group/app")}),
                ),
                ("POST", ["uploads"]) => {
                    if !hs
                        .get("content-type")
                        .is_some_and(|c| c.starts_with("multipart/form-data; boundary="))
                    {
                        return reply(400, json!({"message": "file is missing"}));
                    }
                    reply(
                        201,
                        json!({"id": 5, "alt": "shot", "url": "/uploads/66dbcd21ec5d24ed6ea225176098d52b/shot.png",
                        "full_path": "/-/project/4/uploads/66dbcd21ec5d24ed6ea225176098d52b/shot.png",
                        "markdown": "![shot](/uploads/66dbcd21ec5d24ed6ea225176098d52b/shot.png)"}),
                    )
                }
                ("POST", ["issues"]) => {
                    let b = json_body();
                    let title = b["title"].as_str().unwrap_or_default().to_string();
                    let n = new_issue(&mut m, "gl", &title, "opened");
                    let i = m.issues[&("gl".to_string(), n)].clone();
                    reply(201, issue(n, &i))
                }
                ("GET", ["issues", iid]) => {
                    let n: u64 = iid.parse().unwrap_or(0);
                    match m.issues.get(&("gl".to_string(), n)) {
                        Some(i) => reply(200, issue(n, i)),
                        None => reply(404, json!({"message": "404 Not found"})),
                    }
                }
                _ => reply(404, json!({"message": "404 Not Found"})),
            }
        }
        ("gh", _) => {
            if auth != "Bearer ghp-token" {
                return reply(
                    401,
                    json!({"message": "Bad credentials", "documentation_url": "https://docs.github.com/rest"}),
                );
            }
            if hs.get("x-github-api-version").map(String::as_str) != Some("2022-11-28") {
                return reply(400, json!({"message": "missing version"}));
            }
            let issue = |n: u64, i: &Issue| {
                json!({"id": 5000 + n, "node_id": "I_x", "url": format!("{host}/gh/repos/octo/app/issues/{n}"),
                    "html_url": format!("https://github.example.com/octo/app/issues/{n}"), "number": n,
                    "state": if i.closed { "closed" } else { "open" },
                    "state_reason": if i.closed { json!("completed") } else { Value::Null }, "title": i.title,
                    "labels": [{"name": "fastproof"}]})
            };
            match (method.as_str(), &seg[1..]) {
                ("GET", ["repos", "octo", "app"]) => {
                    reply(200, json!({"id": 1, "full_name": "octo/app"}))
                }
                ("POST", ["repos", "octo", "app", "issues"]) => {
                    let b = json_body();
                    let title = b["title"].as_str().unwrap_or_default().to_string();
                    let n = new_issue(&mut m, "gh", &title, "open");
                    let i = m.issues[&("gh".to_string(), n)].clone();
                    reply(201, issue(n, &i))
                }
                ("GET", ["repos", "octo", "app", "issues", n]) => {
                    let n: u64 = n.parse().unwrap_or(0);
                    let Some(i) = m.issues.get(&("gh".to_string(), n)).cloned() else {
                        return reply(404, json!({"message": "Not Found"}));
                    };
                    let etag = format!("W/\"{}-{}\"", n, i.closed);
                    if hs.get("if-none-match") == Some(&etag) {
                        return (AxStatus::NOT_MODIFIED, [("etag", etag)]).into_response();
                    }
                    (AxStatus::OK, [("etag", etag)], axum::Json(issue(n, &i))).into_response()
                }
                _ => reply(404, json!({"message": "Not Found"})),
            }
        }
        ("az", _) => {
            let pat = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(":azure-pat")
            );
            if auth != pat {
                return (AxStatus::UNAUTHORIZED, "Unauthorized").into_response();
            }
            if uri.query().is_none_or(|q| !q.contains("api-version=7.1")) {
                return reply(
                    400,
                    json!({"message": "No api-version was supplied", "typeKey": "VssVersionNotSpecifiedException"}),
                );
            }
            let item = |id: u64, i: &Issue, links: bool| {
                let mut v = json!({"id": id, "rev": 1, "fields": {"System.State": i.state, "System.Title": i.title},
                    "url": format!("{host}/az/example/_apis/wit/workItems/{id}")});
                if links {
                    v["fields"]["System.WorkItemType"] = json!("Bug");
                    v["_links"] = json!({"html": {"href": format!("{host}/az/example/Shop%20Web/_workitems/edit/{id}")}});
                }
                v
            };
            match (method.as_str(), &seg[1..]) {
                ("GET", ["example", "_apis", "projects", p])
                    if *p == "Shop%20Web" || *p == "Shop Web" =>
                {
                    reply(
                        200,
                        json!({"id": "eb6e4656-77fc-42a1-9181-4c6d8e9da5d1", "name": "Shop Web", "state": "wellFormed"}),
                    )
                }
                ("POST", ["example", _, "_apis", "wit", "attachments"]) => {
                    if hs.get("content-type").map(String::as_str)
                        != Some("application/octet-stream")
                    {
                        return reply(400, json!({"message": "bad content type"}));
                    }
                    reply(
                        201,
                        json!({"id": "6b2266bf-a155-4582-a475-ca4da68193ef",
                        "url": format!("{host}/az/example/_apis/wit/attachments/6b2266bf-a155-4582-a475-ca4da68193ef?fileName=shot.png")}),
                    )
                }
                ("POST", ["example", _, "_apis", "wit", "workitems", t]) if t.starts_with('$') => {
                    if hs.get("content-type").map(String::as_str)
                        != Some("application/json-patch+json")
                    {
                        return reply(400, json!({"message": "json patch expected"}));
                    }
                    let ops = json_body();
                    let title = ops
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|o| o["path"] == "/fields/System.Title")
                        .and_then(|o| o["value"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    let n = new_issue(&mut m, "az", &title, "New");
                    let i = m.issues[&("az".to_string(), n)].clone();
                    reply(200, item(n, &i, true))
                }
                ("GET", ["example", _, "_apis", "wit", "workitems", id]) => {
                    let n: u64 = id.parse().unwrap_or(0);
                    match m.issues.get(&("az".to_string(), n)) {
                        Some(i) => reply(200, item(n, i, false)),
                        None => reply(
                            404,
                            json!({"message": format!("TF401232: Work item {n} does not exist")}),
                        ),
                    }
                }
                _ => reply(404, json!({"message": "no route"})),
            }
        }
        _ => reply(404, json!({})),
    }
}

async fn start(db: &TestDb) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let s = Server::start_cfg(
        db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
            ("FASTPROOF_SECRETS_KEY_FILE", key.to_str().unwrap()),
            ("FASTPROOF_PUBLIC_URL", "https://fastproof.example.com"),
        ],
        |c| {
            c.integrations.tracker_tick = Duration::from_millis(100);
            c.integrations.tracker_poll = Duration::from_secs(1);
        },
    )
    .await;
    (s, dir)
}

async fn secret(
    s: &Server,
    project: &str,
    name: &str,
    value: &str,
    purpose: &str,
    origin: Option<&str>,
) {
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/{project}/secrets/{name}"),
            Some(ADMIN_TOKEN),
            Some(json!({"value": value, "purpose": purpose, "origin": origin})),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
}

fn trackers(fake: &Fake) -> Value {
    json!([
        {"id": "jc", "type": "jira", "urlTemplate": format!("{}/browse/{{key}}", fake.url("/jc")), "keyRegex": "SHOP-[0-9]+",
         "api": {"url": fake.url("/jc"), "tokenSecretRef": "jc", "project": "SHOP", "labels": ["fastproof"],
                 "attachScreenshots": true}},
        {"id": "js", "type": "jira", "urlTemplate": format!("{}/browse/{{key}}", fake.url("/js")), "keyRegex": "SHOP-[0-9]+",
         "api": {"url": fake.url("/js"), "deployment": "server", "tokenSecretRef": "js", "project": "SHOP", "issueType": "Bug"}},
        {"id": "gl", "type": "gitlab", "urlTemplate": format!("{}/group/app/-/issues/{{key}}", fake.url("/gl")), "keyRegex": "#?[0-9]+",
         "api": {"url": fake.url("/gl"), "tokenSecretRef": "gl", "project": "group/app", "labels": ["fastproof"],
                 "attachScreenshots": true, "titleTemplate": "[{{project}}] {{name}}"}},
        {"id": "gh", "type": "github", "urlTemplate": "https://github.example.com/octo/app/issues/{key}", "keyRegex": "#?[0-9]+",
         "api": {"url": fake.url("/gh"), "tokenSecretRef": "gh", "project": "octo/app", "sync": "webhook",
                 "webhookSecretRef": "gh-hook"}},
        {"id": "az", "type": "azure", "urlTemplate": format!("{}/example/Shop%20Web/_workitems/edit/{{key}}", fake.url("/az")),
         "keyRegex": "[0-9]+",
         "api": {"url": fake.url("/az/example"), "tokenSecretRef": "az", "project": "Shop Web", "labels": ["fastproof", "ui"],
                 "attachScreenshots": true}}
    ])
}

async fn setup(s: &Server, fake: &Fake) -> (String, Vec<Value>) {
    s.project("ti").await;
    let o = fake.origin();
    secret(
        s,
        "ti",
        "jc",
        "bot@example.com:jira-api-token",
        "tracker",
        Some(&o),
    )
    .await;
    secret(s, "ti", "js", "jira-pat", "tracker", Some(&o)).await;
    secret(s, "ti", "gl", GL_TOKEN, "tracker", Some(&o)).await;
    secret(s, "ti", "gh", "ghp-token", "tracker", Some(&o)).await;
    secret(s, "ti", "az", "azure-pat", "tracker", Some(&o)).await;
    secret(s, "ti", "gh-hook", HOOK_SECRET, "webhook", None).await;
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ti/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"issueTrackers": trackers(fake)})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");

    let (_, l) = s
        .post(
            "/api/v1/projects/ti/launches",
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": "main"}}),
        )
        .await;
    let lid = l["id"].as_str().unwrap().to_string();
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/ti/launches/{lid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "image/png")
        .header("x-fastproof-attachment-name", "checkout failure.png")
        .body(PNG)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let aid = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mk = |id: &str, name: &str, att: bool| {
        let mut v = json!({"id": id, "name": name, "fullName": format!("shop.Checkout#{name}"), "status": "failed",
            "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64,
            "error": {"message": "expected 200 but was 500 @admin see http://evil.example.net ```",
                      "trace": "java.lang.AssertionError\n\tat shop.Checkout.pays(Checkout.java:12)"}});
        if att {
            v["attachments"] = json!([{"id": aid, "name": "checkout failure.png"}]);
        }
        v
    };
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/ti/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([
                mk("1", "pays", true),
                mk("2", "refunds", false),
                mk("3", "voids", false)
            ]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let results = s.results("ti", &lid, "").await;
    assert_eq!(results.len(), 3);
    (lid, results)
}

fn rid(results: &[Value], name: &str) -> String {
    results.iter().find(|r| r["name"] == name).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    s.post("/api/v1/projects/ti/tracker-issues", token, body)
        .await
}

async fn wait_for<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !f().await {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn issues_are_created_in_every_tracker_and_synced_back() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let (s, _dir) = start(&db).await;
    let (lid, results) = setup(&s, &fake).await;
    let pays = rid(&results, "pays");
    let fp = results[0]["fingerprint"].as_str().unwrap().to_string();

    let (st, dry) = create(
        &s,
        ADMIN_TOKEN,
        json!({"tracker": "jc", "requestId": uuid::Uuid::now_v7(), "resultIds": [pays], "dryRun": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dry}");
    assert!(dry["issue"].is_null());
    assert_eq!(dry["preview"]["markup"], "adf");
    assert_eq!(
        dry["preview"]["screenshots"],
        json!(["checkout failure.png"])
    );
    assert!(
        fake.requests("POST", "/jc").is_empty(),
        "a dry run sends nothing"
    );

    let mut created = HashMap::new();
    for t in ["jc", "js", "gl", "gh", "az"] {
        let req = uuid::Uuid::now_v7();
        let body = json!({"tracker": t, "requestId": req, "resultIds": [pays], "note": "Seen on @release day"});
        let (st, b) = create(&s, ADMIN_TOKEN, body.clone()).await;
        assert_eq!(st, StatusCode::OK, "{t}: {b}");
        let issue = &b["issue"];
        assert_eq!(issue["trackerId"], t);
        assert_eq!(issue["category"], "open", "{t}: {b}");
        assert!(
            issue["url"].as_str().unwrap().starts_with("http"),
            "{t}: {b}"
        );
        assert_eq!(b["updatedResults"], 1, "{t}");
        assert_eq!(b["repeated"], false);
        let (st, again) = create(&s, ADMIN_TOKEN, body).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(again["repeated"], true, "{t}");
        assert_eq!(again["issue"]["id"], issue["id"]);
        created.insert(t, b);
    }
    assert_eq!(created["jc"]["issue"]["key"], "SHOP-1");
    assert_eq!(created["jc"]["attached"], 1);
    assert_eq!(created["gl"]["attached"], 1);
    assert_eq!(created["az"]["attached"], 1);
    assert_eq!(created["gh"]["attached"], 0);
    assert_eq!(
        created["js"]["attached"], 0,
        "js does not attach by default"
    );
    assert_eq!(created["gl"]["issue"]["title"], "[ti] pays");

    let jc = fake.requests("POST", "/jc/rest/api/3/issue");
    assert_eq!(jc.len(), 2, "create and attachment");
    let cloud: Value = serde_json::from_slice(&jc[0].body).unwrap();
    let d = &cloud["fields"]["description"];
    assert_eq!(d["type"], "doc");
    let text = d.to_string();
    assert!(text.contains("codeBlock"), "{text}");
    assert!(text.contains("Seen on @release day"), "{text}");
    assert_eq!(cloud["fields"]["labels"], json!(["fastproof"]));
    assert_eq!(cloud["fields"]["issuetype"]["name"], "Bug");
    assert!(jc[1].path.ends_with("/issue/SHOP-1/attachments"));
    assert!(String::from_utf8_lossy(&jc[1].body).contains("filename=\"checkout_failure.png\""));

    let js = fake.requests("POST", "/js/rest/api/2/issue");
    let server: Value = serde_json::from_slice(&js[0].body).unwrap();
    let wiki = server["fields"]["description"].as_str().unwrap();
    assert!(wiki.contains("{noformat}"), "{wiki}");
    assert!(wiki.contains("Checkout\\#pays"), "{wiki}");
    assert!(wiki.contains("Seen on \\@release day"), "{wiki}");

    let gl = fake.requests("POST", "/gl/api/v4/projects/group%2Fapp/issues");
    let glb: Value = serde_json::from_slice(&gl[0].body).unwrap();
    let md = glb["description"].as_str().unwrap();
    assert!(md.contains("````text"), "{md}");
    assert!(md.contains("![checkout_failure.png](/uploads/"), "{md}");
    assert!(
        md.contains("<https://fastproof.example.com/p/ti/results/"),
        "{md}"
    );
    assert_eq!(glb["labels"], "fastproof");

    let gh = fake.requests("POST", "/gh/repos/octo/app/issues");
    let ghb: Value = serde_json::from_slice(&gh[0].body).unwrap();
    assert!(ghb["body"].as_str().unwrap().contains("\\@\u{200B}release"));

    let az = fake.requests("POST", "/az/example/Shop%20Web/_apis/wit/workitems/$Bug");
    assert_eq!(az.len(), 1);
    let ops: Value = serde_json::from_slice(&az[0].body).unwrap();
    let ops = ops.as_array().unwrap();
    let repro = ops
        .iter()
        .find(|o| o["path"] == "/fields/Microsoft.VSTS.TCM.ReproSteps")
        .unwrap()["value"]
        .as_str()
        .unwrap();
    assert!(repro.contains("<pre>"), "{repro}");
    assert!(!repro.contains("<img"), "{repro}");
    assert!(
        ops.iter()
            .any(|o| o["path"] == "/relations/-" && o["value"]["rel"] == "AttachedFile")
    );
    assert!(
        ops.iter()
            .any(|o| o["path"] == "/fields/System.Tags" && o["value"] == "fastproof; ui")
    );

    let (_, detail) = s
        .get(&format!("/api/v1/projects/ti/results/{pays}"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        detail["defect"]["issueKey"], "5",
        "the last create (Azure) is on the defect: {detail}"
    );

    let urls: Vec<Value> = created
        .values()
        .map(|b| b["issue"]["url"].clone())
        .collect();
    let (st, found) = s
        .post(
            "/api/v1/projects/ti/tracker-issues/lookup",
            ADMIN_TOKEN,
            json!({"urls": urls}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{found}");
    assert_eq!(found["items"].as_array().unwrap().len(), 5);
    let (_, by_result) = s
        .get(
            &format!("/api/v1/projects/ti/tracker-issues?resultId={pays}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(by_result["items"].as_array().unwrap().len(), 5);

    let (st, cl) = create(
        &s,
        ADMIN_TOKEN,
        json!({"tracker": "gl", "requestId": uuid::Uuid::now_v7(), "launchId": lid, "fingerprint": fp}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cl}");
    assert_eq!(cl["updatedResults"], 3);
    let cl_id = cl["issue"]["id"].as_str().unwrap().to_string();
    let last = fake.requests("POST", "/gl/api/v4/projects/group%2Fapp/issues");
    let body: Value = serde_json::from_slice(&last.last().unwrap().body).unwrap();
    let md = body["description"].as_str().unwrap();
    assert!(
        md.contains("Results with this failure in the launch: 3"),
        "{md}"
    );
    assert!(md.contains("- shop\\.Checkout\\#\u{200B}refunds"), "{md}");

    let (st, b) = s
        .post(
            "/api/v1/projects/ti/triage",
            ADMIN_TOKEN,
            json!({"launchId": lid, "fingerprint": fp, "mute": {"reason": "known bug"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let gl_iid: u64 = cl["issue"]["key"]
        .as_str()
        .unwrap()
        .trim_start_matches('#')
        .parse()
        .unwrap();
    fake.set("gl", gl_iid, "closed", true);
    wait_for("the closed GitLab issue is read back", || async {
        let (_, b) = s
            .get(
                &format!("/api/v1/projects/ti/tracker-issues/{cl_id}"),
                ADMIN_TOKEN,
            )
            .await;
        b["category"] == "closed"
    })
    .await;
    let (_, issue) = s
        .get(
            &format!("/api/v1/projects/ti/tracker-issues/{cl_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(issue["unmuteSuggested"], true, "{issue}");
    assert_eq!(issue["mutedTests"], 3);
    assert_eq!(issue["canUnmute"], true);
    assert_eq!(issue["links"].as_array().unwrap().len(), 1);
    let (_, suggested) = s
        .get(
            "/api/v1/projects/ti/tracker-issues?unmuteSuggested=true",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(suggested["items"].as_array().unwrap().len(), 1);

    let viewer = s.token("viewer", "viewer", &["ti"]).await;
    let editor = s.token("editor", "editor", &["ti"]).await;
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/ti/tracker-issues/{cl_id}/unmute"),
            &viewer,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/ti/tracker-issues/{cl_id}/unmute"),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["tests"], 3);
    assert_eq!(out["results"], 3);
    let after = s.results("ti", &lid, "").await;
    assert!(after.iter().all(|r| r["muted"] == false));
    let (_, issue) = s
        .get(
            &format!("/api/v1/projects/ti/tracker-issues/{cl_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(issue["unmuteSuggested"], false);
    assert_eq!(issue["mutedTests"], 0);

    let jira_id = created["jc"]["issue"]["id"].as_str().unwrap().to_string();
    fake.set("jc", 1, "Done", true);
    wait_for("the Jira Cloud issue is closed", || async {
        let (_, b) = s
            .get(
                &format!("/api/v1/projects/ti/tracker-issues/{jira_id}"),
                ADMIN_TOKEN,
            )
            .await;
        b["category"] == "closed" && b["state"] == "Done"
    })
    .await;

    let audit: Vec<String> = sqlx::query_scalar(
        "select action from platform_audit where action like 'tracker.%' order by id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        audit.iter().any(|a| a == "tracker.issue.create"),
        "{audit:?}"
    );
    assert!(
        audit.iter().any(|a| a == "tracker.issue.unmute"),
        "{audit:?}"
    );
}

#[tokio::test]
async fn webhooks_link_limits_permissions_and_secrets() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let (s, _dir) = start(&db).await;
    let (_lid, results) = setup(&s, &fake).await;
    let pays = rid(&results, "pays");
    let voids = rid(&results, "voids");

    let viewer = s.token("viewer", "viewer", &["ti"]).await;
    let editor = s.token("editor", "editor", &["ti"]).await;
    let body = json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7(), "resultIds": [pays]});
    let (st, _) = create(&s, &viewer, body.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .req(Method::POST, "/api/v1/projects/ti/tracker-issues", None)
        .json(&body)
        .send()
        .await
        .map(|r| (r.status(), ()))
        .unwrap();
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, gh) = create(&s, &editor, body).await;
    assert_eq!(st, StatusCode::OK, "{gh}");
    let gh_id = gh["issue"]["id"].as_str().unwrap().to_string();
    let (st, list) = s.get("/api/v1/projects/ti/tracker-issues", &viewer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    for bad in [
        json!({"tracker": "nope", "requestId": uuid::Uuid::now_v7(), "resultIds": [pays]}),
        json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7()}),
        json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7(), "resultIds": [pays], "title": " "}),
        json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7(), "launchId": uuid::Uuid::now_v7(), "fingerprint": "x"}),
    ] {
        let (st, b) = create(&s, ADMIN_TOKEN, bad.clone()).await;
        assert!(
            matches!(st, StatusCode::BAD_REQUEST | StatusCode::CONFLICT),
            "{bad}: {st} {b}"
        );
    }
    let (st, _) = create(
        &s,
        ADMIN_TOKEN,
        json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7(), "resultIds": [uuid::Uuid::now_v7()]}),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let hook = "/api/v1/hooks/trackers/ti/gh".to_string();
    let payload = json!({"action": "closed", "issue": {"number": 1, "state": "open"}}).to_string();
    let sign = |secret: &str, body: &str| {
        let k = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
        format!(
            "sha256={}",
            hex::encode(ring::hmac::sign(&k, body.as_bytes()).as_ref())
        )
    };
    for (path, sig) in [
        (hook.clone(), sign("wrong", &payload)),
        (hook.clone(), String::new()),
        (
            "/api/v1/hooks/trackers/ti/gl".to_string(),
            sign(HOOK_SECRET, &payload),
        ),
        (
            "/api/v1/hooks/trackers/nope/gh".to_string(),
            sign(HOOK_SECRET, &payload),
        ),
    ] {
        let r = s
            .req(Method::POST, &path, None)
            .header("x-hub-signature-256", sig)
            .header("x-github-event", "issues")
            .body(payload.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    let gets_before = fake.requests("GET", "/gh/repos/octo/app/issues/1").len();
    fake.set("gh", 1, "closed", true);
    let r = s
        .req(Method::POST, &hook, None)
        .header("x-hub-signature-256", sign(HOOK_SECRET, &payload))
        .header("x-github-event", "issues")
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    wait_for("the webhook triggers a read", || async {
        let (_, b) = s
            .get(
                &format!("/api/v1/projects/ti/tracker-issues/{gh_id}"),
                ADMIN_TOKEN,
            )
            .await;
        b["category"] == "closed" && b["state"] == "closed (completed)"
    })
    .await;
    assert!(fake.requests("GET", "/gh/repos/octo/app/issues/1").len() > gets_before);
    let first_get = fake.requests("GET", "/gh/repos/octo/app/issues/1");
    assert!(first_get.iter().all(|r| r.headers.get("authorization").map(String::as_str) == Some("Bearer ghp-token")));

    tokio::time::sleep(Duration::from_secs(5)).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/ti/tracker-issues/{gh_id}/sync"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let conditional = fake.requests("GET", "/gh/repos/octo/app/issues/1");
    assert!(
        conditional
            .last()
            .unwrap()
            .headers
            .contains_key("if-none-match"),
        "the second read is conditional"
    );
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/ti/tracker-issues/{gh_id}/sync"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");

    fake.throttle("gh", 1);
    let calls = fake.requests("POST", "/gh").len();
    let body = json!({"tracker": "gh", "requestId": uuid::Uuid::now_v7(), "resultIds": [voids]});
    let r = s
        .req(
            Method::POST,
            "/api/v1/projects/ti/tracker-issues",
            Some(ADMIN_TOKEN),
        )
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["retry-after"], "2");
    let (st, _) = create(&s, ADMIN_TOKEN, body.clone()).await;
    assert_eq!(
        st,
        StatusCode::TOO_MANY_REQUESTS,
        "paused without calling the tracker"
    );
    assert_eq!(fake.requests("POST", "/gh").len(), calls + 1);
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let (st, b) = create(&s, ADMIN_TOKEN, body).await;
    assert_eq!(st, StatusCode::OK, "{b}");

    fake.st.lock().unwrap().issues.insert(
        ("az".into(), 77),
        Issue {
            title: "Existing bug".into(),
            state: "Active".into(),
            closed: false,
        },
    );
    let (st, linked) = s
        .post(
            "/api/v1/projects/ti/tracker-issues/link",
            &editor,
            json!({"tracker": "az", "issueKey": "#77", "resultIds": [voids]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{linked}");
    assert_eq!(linked["issue"]["key"], "77");
    assert_eq!(linked["issue"]["state"], "Active");
    assert_eq!(linked["issue"]["title"], "Existing bug");
    assert!(
        linked["issue"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/_workitems/edit/77")
    );
    let (st, b) = s
        .post(
            "/api/v1/projects/ti/tracker-issues/link",
            &editor,
            json!({"tracker": "az", "issueKey": "78", "resultIds": [voids]}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    let (st, _) = s
        .post(
            "/api/v1/projects/ti/tracker-issues/link",
            &editor,
            json!({"tracker": "az", "issueKey": "../x", "resultIds": [voids]}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, t) = s
        .post(
            "/api/v1/projects/ti/issue-trackers/gl/test",
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["ok"], true);
    assert_eq!(t["project"], "group/app");
    let (st, _) = s
        .post(
            "/api/v1/projects/ti/issue-trackers/gl/test",
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    secret(
        &s,
        "ti",
        "gl",
        "glpat-wrong",
        "tracker",
        Some(&fake.origin()),
    )
    .await;
    let (_, t) = s
        .post(
            "/api/v1/projects/ti/issue-trackers/gl/test",
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(t["ok"], false);
    let err = t["error"].as_str().unwrap();
    assert!(err.contains("401"), "{err}");
    assert!(
        !err.contains("glpat") && !err.contains("127.0.0.1"),
        "{err}"
    );

    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ti/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"issueTrackers": [{"id": "gl", "type": "gitlab",
                "urlTemplate": "https://gitlab.example.net/g/a/-/issues/{key}", "keyRegex": "[0-9]+",
                "api": {"url": "https://gitlab.example.net", "tokenSecretRef": "gl", "project": "g/a"}}]})),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "a tracker secret cannot move to another origin: {b}"
    );
    for bad in [
        json!({"type": "gitlab", "urlTemplate": "https://x.example.com/{key}", "keyRegex": "[0-9]+",
            "api": {"url": "https://x.example.com", "tokenSecretRef": "gl", "project": "g/a"}}),
        json!({"id": "x", "type": "jira", "urlTemplate": "https://x.example.com/browse/{key}", "keyRegex": "[A-Z]+-[0-9]+",
            "api": {"url": "https://x.example.com", "tokenSecretRef": "t", "project": "shop"}}),
        json!({"id": "x", "type": "github", "urlTemplate": "https://x.example.com/{key}", "keyRegex": "[0-9]+",
            "api": {"url": "https://x.example.com", "tokenSecretRef": "t", "project": "o/r", "sync": "webhook"}}),
        json!({"id": "x", "type": "github", "urlTemplate": "https://x.example.com/{key}", "keyRegex": "[0-9]+",
            "api": {"url": "https://x.example.com", "tokenSecretRef": "t", "project": "o/r", "bodyTemplate": "{{secret}}"}}),
    ] {
        let (st, b) = s
            .call(
                Method::PUT,
                "/api/v1/projects/ti/settings",
                Some(ADMIN_TOKEN),
                Some(json!({"issueTrackers": [bad]})),
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    }
    let (_, readable) = s.get("/api/v1/projects/ti/settings", &viewer).await;
    let t0 = &readable["issueTrackers"][0];
    assert_eq!(t0["api"]["url"], "", "readers do not see the API URL: {t0}");
    assert_eq!(t0["api"]["redacted"], true);

    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/ti/tracker-issues/{gh_id}"),
            Some(&viewer),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/ti/tracker-issues/{gh_id}"),
            Some(&editor),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/ti/tracker-issues/{gh_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
