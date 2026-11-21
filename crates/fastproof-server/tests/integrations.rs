//! Integrations on a real PostgreSQL against local fakes: quality gates and `fastproof gate`,
//! GitLab and GitHub comments and statuses, signed notifications with retries, the egress
//! allowlist, secrets, MCP over HTTP and FastMock correlation. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`; the real FastMock test also needs
//! `FASTPROOF_TEST_FASTMOCK_URL` (admin URL) and `FASTPROOF_TEST_FASTMOCK_TOKEN`.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method as AxMethod, StatusCode as AxStatus, Uri};
use axum::response::{IntoResponse, Response};
use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const KEY_HEX: &str = "8f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

#[derive(Clone, Debug)]
struct Rec {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
    at: std::time::Instant,
}

#[derive(Default)]
struct FakeState {
    log: Mutex<Vec<Rec>>,
    notes: Mutex<Vec<(u64, String)>>,
    fail_hook: AtomicUsize,
    throttle_teams: AtomicUsize,
    journal: Mutex<Vec<Value>>,
}

/// A fake GitLab, GitHub, webhook receiver, Slack, Teams Workflows, Telegram and FastMock
/// admin API.
struct Fake {
    addr: SocketAddr,
    st: Arc<FakeState>,
}

impl Fake {
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

    fn requests(&self, path_prefix: &str) -> Vec<Rec> {
        self.st
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.starts_with(path_prefix))
            .cloned()
            .collect()
    }
}

async fn handle(
    State(st): State<Arc<FakeState>>,
    method: AxMethod,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let rec = Rec {
        method: method.to_string(),
        path: path.clone(),
        query: uri.query().unwrap_or_default().to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect(),
        body: String::from_utf8_lossy(&body).to_string(),
        at: std::time::Instant::now(),
    };
    st.log.lock().unwrap().push(rec.clone());
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    let json_body = |v: Value| (AxStatus::OK, axum::Json(v)).into_response();
    let comment_list = || {
        let notes = st.notes.lock().unwrap();
        json_body(Value::Array(
            notes
                .iter()
                .map(|(id, b)| json!({"id": id, "body": b, "system": false}))
                .collect(),
        ))
    };
    let new_comment = || {
        let b: Value = serde_json::from_str(&rec.body).unwrap_or_default();
        let mut notes = st.notes.lock().unwrap();
        let id = 100 + notes.len() as u64;
        notes.push((id, b["body"].as_str().unwrap_or_default().to_string()));
        (AxStatus::CREATED, axum::Json(json!({"id": id}))).into_response()
    };
    let edit_comment = |id: &str| {
        let b: Value = serde_json::from_str(&rec.body).unwrap_or_default();
        let id: u64 = id.parse().unwrap_or(0);
        let mut notes = st.notes.lock().unwrap();
        match notes.iter_mut().find(|n| n.0 == id) {
            Some(n) => {
                n.1 = b["body"].as_str().unwrap_or_default().to_string();
                json_body(json!({"id": id}))
            }
            None => AxStatus::NOT_FOUND.into_response(),
        }
    };
    match (method.as_str(), seg.as_slice()) {
        ("GET", ["api", "v4", "projects", _, "merge_requests", _, "notes"]) => comment_list(),
        ("POST", ["api", "v4", "projects", _, "merge_requests", _, "notes"]) => new_comment(),
        ("PUT", ["api", "v4", "projects", _, "merge_requests", _, "notes", id]) => edit_comment(id),
        ("POST", ["api", "v4", "projects", _, "statuses", _]) => {
            (AxStatus::CREATED, axum::Json(json!({}))).into_response()
        }
        ("GET", ["repos", _, _, "issues", _, "comments"]) => comment_list(),
        ("POST", ["repos", _, _, "issues", _, "comments"]) => new_comment(),
        ("PATCH", ["repos", _, _, "issues", "comments", id]) => edit_comment(id),
        ("POST", ["repos", _, _, "statuses", _]) => {
            (AxStatus::CREATED, axum::Json(json!({}))).into_response()
        }
        ("POST", ["hook"]) => {
            if st
                .fail_hook
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                AxStatus::SERVICE_UNAVAILABLE.into_response()
            } else {
                AxStatus::NO_CONTENT.into_response()
            }
        }
        ("POST", ["gone"]) => AxStatus::NOT_FOUND.into_response(),
        ("POST", ["redirect"]) => {
            (AxStatus::FOUND, [("location", "http://203.0.113.7/stolen")]).into_response()
        }
        (_, ["moved", rest @ ..]) => {
            let host = rec
                .headers
                .get("host")
                .cloned()
                .unwrap_or_default()
                .replace("127.0.0.1", "localhost");
            let q = if rec.query.is_empty() {
                String::new()
            } else {
                format!("?{}", rec.query)
            };
            let to = format!("http://{host}/{}{q}", rest.join("/"));
            (AxStatus::FOUND, [("location", to)]).into_response()
        }
        ("POST", ["slack"]) => "ok".into_response(),
        ("POST", [hook @ ("teams" | "teams-mr")]) => {
            if *hook == "teams"
                && st
                    .throttle_teams
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
            {
                (AxStatus::TOO_MANY_REQUESTS, [("retry-after", "1")]).into_response()
            } else if serde_json::from_str::<Value>(&rec.body)
                .is_ok_and(|v| v["type"] == "message" && v["attachments"].is_array())
                && rec.query.contains("sig=")
            {
                AxStatus::ACCEPTED.into_response()
            } else {
                AxStatus::BAD_REQUEST.into_response()
            }
        }
        ("POST", ["teams-closed"]) => AxStatus::UNAUTHORIZED.into_response(),
        ("POST", [bot, "sendMessage"]) if bot.starts_with("bot") => json_body(json!({"ok": true})),
        ("GET", ["__admin", "requests"]) => {
            let offset: usize = rec
                .query
                .split('&')
                .find_map(|kv| kv.strip_prefix("offset="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let all = st.journal.lock().unwrap().clone();
            let page: Vec<Value> = all.into_iter().skip(offset).collect();
            json_body(
                json!({"requests": page, "total": page.len(), "offset": offset, "limit": 500}),
            )
        }
        _ => AxStatus::NOT_FOUND.into_response(),
    }
}

/// A plaintext SMTP sink that keeps every message.
async fn smtp_sink() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mails = Arc::new(Mutex::new(Vec::new()));
    let m = mails.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l.accept().await else {
                return;
            };
            let m = m.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 sink ESMTP\r\n").await.unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim_end().to_ascii_uppercase();
                    if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
                        w.write_all(b"250-sink\r\n250 8BITMIME\r\n").await.unwrap();
                    } else if cmd == "DATA" {
                        w.write_all(b"354 go\r\n").await.unwrap();
                        let mut data = String::new();
                        loop {
                            line.clear();
                            r.read_line(&mut line).await.unwrap();
                            if line == ".\r\n" {
                                break;
                            }
                            data.push_str(&line);
                        }
                        m.lock().unwrap().push(data);
                        w.write_all(b"250 queued\r\n").await.unwrap();
                    } else if cmd == "QUIT" {
                        w.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else {
                        w.write_all(b"250 ok\r\n").await.unwrap();
                    }
                }
            });
        }
    });
    (addr, mails)
}

struct Env {
    _dir: tempfile::TempDir,
    vars: Vec<(String, String)>,
}

fn env(fake: &Fake, extra: &[(&str, &str)]) -> Env {
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
        (
            "FASTPROOF_PUBLIC_URL".into(),
            "https://fastproof.example.com".into(),
        ),
        ("FASTPROOF_TELEGRAM_API_URL".into(), fake.url()),
    ];
    for (k, v) in extra {
        vars.retain(|(x, _)| x != k);
        vars.push((k.to_string(), v.to_string()));
    }
    Env { _dir: dir, vars }
}

async fn start(db: &TestDb, e: &Env) -> Server {
    let extra: Vec<(&str, &str)> = e
        .vars
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    Server::start_cfg(db, &extra, |c| {
        c.integrations.poll = Duration::from_millis(100);
        c.integrations.settle = Duration::ZERO;
        c.integrations.delivery_backoff = Duration::from_millis(50);
        c.integrations.delivery_max_attempts = 4;
    })
    .await
}

async fn settings(db: &TestDb, project: &str, v: Value) {
    let id: uuid::Uuid = sqlx::query_scalar("select id from projects where key = $1")
        .bind(project)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(id, v)
        .await
        .unwrap();
}

async fn put_secret(
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

async fn launch(s: &Server, project: &str, body: Value) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            body,
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    b["id"].as_str().unwrap().to_string()
}

/// Posts results `(name, status)` into a launch.
async fn results(s: &Server, project: &str, launch: &str, items: &[(&str, &str)]) {
    let body: Vec<Value> = items
        .iter()
        .map(|(n, st)| {
            let mut r = json!({"id": format!("{launch}-{n}"), "name": n, "fullName": format!("suite.{n}"),
                "status": st, "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64});
            if *st == "failed" {
                r["error"] = json!({"message": format!("{n} failed: expected 1 got 2")});
            }
            r
        })
        .collect();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{launch}/results"),
            ADMIN_TOKEN,
            Value::Array(body),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
}

async fn finish(s: &Server, project: &str, launch: &str) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{launch}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
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

async fn deliveries(s: &Server, project: &str) -> Vec<Value> {
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/{project}/notifications/deliveries"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["items"].as_array().unwrap().clone()
}

async fn gate_done(db: &TestDb, launch: &str) -> bool {
    let id: uuid::Uuid = launch.parse().unwrap();
    sqlx::query_scalar::<_, String>("select state from launch_gates where launch_id = $1")
        .bind(id)
        .fetch_optional(&db.pool)
        .await
        .unwrap()
        .as_deref()
        == Some("done")
}

async fn gate_cli(s: &Server, args: &[&str]) -> i32 {
    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .arg("gate")
        .args(["--url", &s.base])
        .args(args)
        .env("FASTPROOF_TOKEN", ADMIN_TOKEN)
        .env_remove("FASTPROOF_LAUNCH")
        .env_remove("GITLAB_CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("JENKINS_URL")
        .output()
        .await
        .unwrap();
    eprintln!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.status.code().unwrap_or(-1)
}

#[tokio::test]
async fn gates_baselines_and_cli_exit_codes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    settings(
        &db,
        "shop",
        json!({"gates": [
            {"name": "strict", "branches": ["release/*"], "rules": [{"metric": "failed", "op": "lte", "value": 0}]},
            {"name": "no-new", "rules": [
                {"metric": "newFailures", "op": "lte", "value": 0, "baseline": "main"},
                {"metric": "passRate", "op": "gte", "value": 50}
            ]}
        ]}),
    )
    .await;
    let base = launch(
        &s,
        "shop",
        json!({"name": "main run", "externalId": "m1", "vcs": {"branch": "main"}}),
    )
    .await;
    results(
        &s,
        "shop",
        &base,
        &[("a", "passed"), ("b", "failed"), ("c", "passed")],
    )
    .await;
    finish(&s, "shop", &base).await;

    let feat = launch(
        &s,
        "shop",
        json!({"name": "feature run", "externalId": "f1", "vcs": {"branch": "feature-x"}}),
    )
    .await;
    results(
        &s,
        "shop",
        &feat,
        &[
            ("a", "passed"),
            ("b", "failed"),
            ("c", "passed"),
            ("d", "passed"),
        ],
    )
    .await;
    let (st, g) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{feat}/gate"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    assert_eq!(g["status"], "pending", "{g}");
    assert_eq!(
        gate_cli(
            &s,
            &[
                "--project",
                "shop",
                "--launch",
                "f1",
                "--wait",
                "300ms",
                "--poll",
                "100ms"
            ]
        )
        .await,
        2
    );
    finish(&s, "shop", &feat).await;
    let (_, g) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{feat}/gate"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(g["status"], "passed", "{g}");
    assert_eq!(
        g["results"].as_array().unwrap().len(),
        2,
        "strict does not apply: {g}"
    );
    assert_eq!(g["results"][0]["actual"], 0.0, "b failed on main too: {g}");
    assert_eq!(g["results"][0]["baselineLaunchId"], base.as_str());
    assert_eq!(g["results"][1]["actual"], 75.0);
    assert_eq!(
        gate_cli(&s, &["--project", "shop", "--launch", "f1"]).await,
        0
    );

    let rel = launch(
        &s,
        "shop",
        json!({"name": "release run", "externalId": "r1", "vcs": {"branch": "release/1"}}),
    )
    .await;
    results(
        &s,
        "shop",
        &rel,
        &[("a", "failed"), ("b", "failed"), ("c", "passed")],
    )
    .await;
    finish(&s, "shop", &rel).await;
    let (_, g) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{rel}/gate"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(g["status"], "failed", "{g}");
    let new = g["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule"]["metric"] == "newFailures")
        .unwrap();
    assert_eq!(new["actual"], 1.0, "only a is new against main: {g}");
    assert_eq!(
        gate_cli(&s, &["--project", "shop", "--launch", "r1", "--wait", "5s"]).await,
        1
    );
    assert_eq!(gate_cli(&s, &["--project", "shop", "--id", &rel]).await, 1);
    assert_eq!(
        gate_cli(&s, &["--project", "shop", "--launch", "nope"]).await,
        2
    );
    assert_eq!(
        gate_cli(&s, &["--project", "missing", "--launch", "r1"]).await,
        2
    );

    let viewer = s.token("v", "viewer", &["shop"]).await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{rel}/gate"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    s.project("elsewhere").await;
    let other = s.token("o", "viewer", &["elsewhere"]).await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{rel}/gate"),
            &other,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    settings(&db, "shop", json!({"gates": [{"name": "x", "rules": []}]})).await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/shop/launches/{rel}/gate"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    wait_for("gates stored", || gate_done(&db, &rel)).await;
    let text = s
        .http
        .get(format!("{}/metrics", s.ops))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("fastproof_launch_duration_seconds"), "{text}");
}

#[tokio::test]
async fn vcs_comments_are_created_then_updated() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    for (project, provider) in [("gl", "gitlab"), ("gh", "github")] {
        s.project(project).await;
        put_secret(
            &s,
            project,
            "vcs-token",
            &format!("{provider}-token-123"),
            "vcs",
            Some(&fake.url()),
        )
        .await;
        settings(
            &db,
            project,
            json!({"vcs": {"provider": provider, "url": fake.url(), "repository": "group/app", "tokenSecretRef": "vcs-token"},
                   "gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}]}),
        )
        .await;
        let mut ids = Vec::new();
        for (ext, statuses) in [("p1", ["passed", "failed"]), ("p2", ["passed", "passed"])] {
            let id = launch(
                &s,
                project,
                json!({"name": "app pipeline", "externalId": ext,
                       "ci": {"provider": provider, "mergeRequest": "5"},
                       "vcs": {"repository": format!("{}/group/app", fake.url()), "branch": "feature",
                               "commit": "0123456789abcdef0123456789abcdef01234567"}}),
            )
            .await;
            results(&s, project, &id, &[("x", statuses[0]), ("y", statuses[1])]).await;
            finish(&s, project, &id).await;
            wait_for("vcs delivery", || async {
                deliveries(&s, project)
                    .await
                    .iter()
                    .filter(|d| d["status"] == "delivered")
                    .count()
                    == ids.len() + 1
            })
            .await;
            ids.push(id);
        }
        let notes = fake.st.notes.lock().unwrap().clone();
        fake.st.notes.lock().unwrap().clear();
        assert_eq!(notes.len(), 1, "{provider}: {notes:?}");
        assert!(notes[0].1.contains("<!-- fastproof:"), "{notes:?}");
        assert!(
            notes[0].1.contains("Quality gate: passed"),
            "updated in place: {notes:?}"
        );
        assert!(notes[0].1.contains(&format!(
            "https://fastproof.example.com/p/{project}/launches/{}",
            ids[1]
        )));
        let (status_prefix, created, updated) = if provider == "gitlab" {
            (
                "/api/v4/projects/group%2Fapp/statuses/",
                "/api/v4/projects/group%2Fapp/merge_requests/5/notes",
                "PUT",
            )
        } else {
            (
                "/repos/group/app/statuses/",
                "/repos/group/app/issues/5/comments",
                "PATCH",
            )
        };
        let statuses = fake.requests(status_prefix);
        assert_eq!(statuses.len(), 2);
        let first: Value = serde_json::from_str(&statuses[0].body).unwrap();
        let second: Value = serde_json::from_str(&statuses[1].body).unwrap();
        if provider == "gitlab" {
            assert_eq!(
                (first["state"].as_str(), second["state"].as_str()),
                (Some("failed"), Some("success"))
            );
            assert_eq!(
                statuses[0].headers["authorization"],
                "Bearer gitlab-token-123"
            );
            assert!(!statuses[0].headers.contains_key("private-token"));
        } else {
            assert_eq!(
                (first["state"].as_str(), second["state"].as_str()),
                (Some("failure"), Some("success"))
            );
            assert_eq!(
                statuses[0].headers["authorization"],
                "Bearer github-token-123"
            );
        }
        let posts: Vec<Rec> = fake
            .requests(created)
            .into_iter()
            .filter(|r| r.method == "POST")
            .collect();
        assert_eq!(posts.len(), 1, "{provider}");
        assert_eq!(
            fake.requests("/")
                .iter()
                .filter(|r| r.method == updated)
                .count(),
            1
        );
        fake.st.log.lock().unwrap().clear();
    }
}

/// A launch with a merge request and a commit on `repository`.
async fn mr_launch(
    s: &Server,
    project: &str,
    name: &str,
    repository: &str,
    branch: &str,
) -> String {
    let id = launch(
        s,
        project,
        json!({"name": name, "ci": {"provider": "gitlab", "mergeRequest": "5"},
               "vcs": {"repository": repository, "branch": branch,
                       "commit": "0123456789abcdef0123456789abcdef01234567"}}),
    )
    .await;
    results(s, project, &id, &[("x", "passed")]).await;
    finish(s, project, &id).await;
    id
}

async fn settled_delivery(s: &Server, project: &str, launch: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let found = deliveries(s, project)
            .await
            .into_iter()
            .find(|d| d["launchId"] == launch);
        if let Some(d) = found
            && matches!(d["status"].as_str(), Some("delivered" | "failed"))
        {
            return d;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: delivery of {launch}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn vcs_token_stays_on_the_pinned_repository() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("gl").await;
    put_secret(&s, "gl", "vcs-token", "glpat-123", "vcs", Some(&fake.url())).await;
    settings(
        &db,
        "gl",
        json!({"vcs": {"provider": "gitlab", "url": fake.url(), "repository": "group/app",
                       "tokenSecretRef": "vcs-token", "sourceBranches": ["feature/*"]}}),
    )
    .await;

    let elsewhere = mr_launch(
        &s,
        "gl",
        "app",
        &format!("{}/victim/secret-app", fake.url()),
        "feature/x",
    )
    .await;
    let d = settled_delivery(&s, "gl", &elsewhere).await;
    assert_eq!(d["status"], "failed", "{d}");
    assert!(
        d["lastError"]
            .as_str()
            .unwrap()
            .contains("is not vcs.repository"),
        "{d}"
    );
    assert!(fake.requests("/api/v4/projects/victim").is_empty());
    assert!(fake.requests("/api/v4/").is_empty(), "nothing was called");

    let name = "app [click](https://evil.example.com/x) ![t](https://evil.example.com/t.png) @all <b>hi</b>";
    let pinned = mr_launch(
        &s,
        "gl",
        name,
        &format!("{}/group/app.git", fake.url()),
        "feature/y",
    )
    .await;
    assert_eq!(
        settled_delivery(&s, "gl", &pinned).await["status"],
        "delivered"
    );
    let notes = fake.st.notes.lock().unwrap().clone();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let (marker, body) = notes[0].1.split_once('\n').unwrap();
    assert!(
        marker.starts_with("<!-- fastproof:")
            && marker.ends_with(" -->")
            && !marker[4..marker.len() - 4].contains('>'),
        "the name stays inside the hidden marker: {marker}"
    );
    for bad in [
        "](https://evil",
        "![",
        "@all",
        "<b>",
        "https://evil",
        "[click]",
    ] {
        assert!(!body.contains(bad), "{bad} in {body}");
    }

    let other_branch = mr_launch(
        &s,
        "gl",
        "app",
        &format!("{}/group/app", fake.url()),
        "main",
    )
    .await;
    wait_for("gate evaluated", || gate_done(&db, &other_branch)).await;
    assert!(
        deliveries(&s, "gl")
            .await
            .iter()
            .all(|d| d["launchId"] != other_branch.as_str()),
        "a branch outside vcs.sourceBranches posts nothing"
    );
}

#[tokio::test]
async fn vcs_calls_do_not_follow_redirects() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[("FASTPROOF_EGRESS_ALLOW", "127.0.0.1, localhost")]);
    let s = start(&db, &e).await;
    s.project("gl").await;
    put_secret(&s, "gl", "vcs-token", "glpat-456", "vcs", Some(&fake.url())).await;
    settings(
        &db,
        "gl",
        json!({"vcs": {"provider": "gitlab", "url": format!("{}/moved", fake.url()),
                       "repository": "group/app", "tokenSecretRef": "vcs-token"}}),
    )
    .await;
    let id = mr_launch(
        &s,
        "gl",
        "app",
        &format!("{}/group/app", fake.url()),
        "feature",
    )
    .await;
    let d = settled_delivery(&s, "gl", &id).await;
    assert_eq!(d["status"], "failed", "{d}");
    assert!(d["lastError"].as_str().unwrap().contains("HTTP 302"), "{d}");
    let log = fake.st.log.lock().unwrap().clone();
    assert!(!log.is_empty());
    for r in &log {
        assert!(r.path.starts_with("/moved/"), "followed to {}", r.path);
        assert!(
            !r.headers["host"].starts_with("localhost"),
            "the token went to another origin: {r:?}"
        );
    }
}

#[tokio::test]
async fn secrets_are_bound_to_purpose_and_origin() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[("FASTPROOF_EGRESS_ALLOW", "127.0.0.1, localhost")]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    let l = launch(&s, "shop", json!({"name": "n"})).await;
    let r = mock_result(&s, "shop", &l, "r", None, 1_795_249_079_000).await;
    let path = format!("/api/v1/projects/shop/results/{r}/mock-interactions");
    put_secret(
        &s,
        "shop",
        "fm",
        "fm-token-9",
        "fastmock",
        Some(&fake.url()),
    )
    .await;
    put_secret(&s, "shop", "hook-key", "hmac-key-9", "webhook", None).await;
    let moved = fake.url().replace("127.0.0.1", "localhost");

    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": moved, "tokenSecretRef": "fm"}}),
    )
    .await;
    let (st, b) = s.get(&path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert!(
        b["message"]
            .as_str()
            .unwrap()
            .contains("may only be sent to"),
        "{b}"
    );
    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": fake.url(), "tokenSecretRef": "hook-key"}}),
    )
    .await;
    let (st, b) = s.get(&path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert!(
        b["message"].as_str().unwrap().contains("is for webhook"),
        "{b}"
    );
    assert!(
        fake.requests("/__admin").is_empty(),
        "no token left the server"
    );

    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": fake.url(), "tokenSecretRef": "fm"}}),
    )
    .await;
    let (st, mut doc) = s.get("/api/v1/projects/shop/settings", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    doc["fastmock"]["url"] = json!(moved);
    let put = |d: Value| {
        let s = &s;
        async move {
            s.call(
                Method::PUT,
                "/api/v1/projects/shop/settings",
                Some(ADMIN_TOKEN),
                Some(d),
            )
            .await
        }
    };
    let (st, b) = put(doc.clone()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert!(
        b["message"]
            .as_str()
            .unwrap()
            .contains("may only be sent to"),
        "{b}"
    );
    put_secret(&s, "shop", "fm", "fm-token-9", "fastmock", Some(&moved)).await;
    let (st, b) = put(doc.clone()).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let change = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "project.settings.urls")
        .unwrap_or_else(|| panic!("{audit}"))
        .clone();
    let text = change.to_string();
    assert!(
        text.contains("fastmock.url")
            && text.contains(&format!("{}/", fake.url()))
            && text.contains(&format!("{moved}/")),
        "{text}"
    );
    assert!(!text.contains("fm-token-9"));
}

#[tokio::test]
async fn settings_hide_targets_from_readers() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    settings(
        &db,
        "shop",
        json!({"notifications": [
                {"id": "sl", "kind": "slack", "on": ["gateFailed"],
                 "target": "https://hooks.example.com/services/T0/B0/token-part-321"},
                {"id": "mail", "kind": "email", "on": ["gateFailed"], "target": "qa@example.com"}],
               "fastmock": {"url": "https://fastmock.example.com:9090"}}),
    )
    .await;
    let viewer = s.token("v", "viewer", &["shop"]).await;
    let (st, b) = s.get("/api/v1/projects/shop/settings", &viewer).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let text = b.to_string();
    for hidden in ["token-part-321", "qa@example.com", "fastmock.example.com"] {
        assert!(
            !text.contains(hidden),
            "{hidden} visible to a viewer: {text}"
        );
    }
    assert_eq!(b["notifications"][0]["id"], "sl");
    let (st, b) = s.get("/api/v1/projects/shop/settings", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert!(b.to_string().contains("token-part-321"), "{b}");
}

#[tokio::test]
async fn notifications_are_signed_retried_and_listed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let (smtp, mails) = smtp_sink().await;
    let smtp_url = format!("smtp+insecure://{smtp}");
    let e = env(
        &fake,
        &[
            ("FASTPROOF_SMTP_URL", &smtp_url),
            ("FASTPROOF_SMTP_FROM", "FastProof <fastproof@example.com>"),
        ],
    );
    let s = start(&db, &e).await;
    s.project("shop").await;
    put_secret(&s, "shop", "hook-key", "hmac-key-456", "webhook", None).await;
    put_secret(&s, "shop", "tg", "123456:ABC-def_ghi", "telegram", None).await;
    put_secret(
        &s,
        "shop",
        "slack-url",
        &format!("{}/slack", fake.url()),
        "slack",
        Some(&fake.url()),
    )
    .await;
    fake.st.fail_hook.store(2, Ordering::SeqCst);
    settings(
        &db,
        "shop",
        json!({"gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}],
               "notifications": [
            {"id": "hook", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/hook", fake.url()), "secretRef": "hook-key"},
            {"id": "slack", "kind": "slack", "on": ["gateFailed"], "targetSecretRef": "slack-url"},
            {"id": "tg", "kind": "telegram", "on": ["newFailures"], "target": "-100200", "secretRef": "tg"},
            {"id": "mail", "kind": "email", "on": ["gateFailed"], "target": "qa@example.com"},
            {"id": "gone", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/gone", fake.url())},
            {"id": "main-only", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/hook", fake.url()),
             "filter": {"branches": ["main"]}}
        ]}),
    )
    .await;
    let id = launch(
        &s,
        "shop",
        json!({"name": "nightly", "vcs": {"branch": "dev"}}),
    )
    .await;
    results(&s, "shop", &id, &[("a", "passed"), ("b", "failed")]).await;
    finish(&s, "shop", &id).await;
    wait_for("deliveries settle", || async {
        let d = deliveries(&s, "shop").await;
        d.len() == 5
            && d.iter()
                .all(|x| x["status"] == "delivered" || x["status"] == "failed")
    })
    .await;
    let d = deliveries(&s, "shop").await;
    let by = |id: &str| {
        d.iter()
            .find(|x| x["notificationId"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by("hook")["status"], "delivered");
    assert_eq!(by("hook")["attempts"], 3, "two 503s then success");
    assert_eq!(by("hook")["responseStatus"], 204);
    assert_eq!(by("gone")["status"], "failed");
    assert_eq!(by("gone")["attempts"], 1, "a 404 is not retried");
    for n in ["slack", "tg", "mail"] {
        assert_eq!(by(n)["status"], "delivered", "{n}: {d:?}");
    }
    assert!(
        d.iter()
            .all(|x| x.get("payload").is_none() && x.get("target").is_none())
    );

    let hooks = fake.requests("/hook");
    assert_eq!(hooks.len(), 3);
    let body = hooks[2].body.as_bytes();
    let sig = &hooks[2].headers["x-fastproof-signature"];
    let ts: i64 = hooks[2].headers["x-fastproof-timestamp"].parse().unwrap();
    assert!((chrono::Utc::now().timestamp() - ts).abs() < 120, "{ts}");
    assert_eq!(
        *sig,
        fastproof_server::integrations::notify::signature(b"hmac-key-456", ts, body)
    );
    let mut signed = format!("{ts}.").into_bytes();
    signed.extend_from_slice(body);
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"hmac-key-456");
    assert_eq!(
        *sig,
        format!(
            "sha256={}",
            hex::encode(ring::hmac::sign(&key, &signed).as_ref())
        ),
        "the signature covers <timestamp>.<body>"
    );
    assert_eq!(
        hooks[0].body, hooks[2].body,
        "the body is stable across retries"
    );
    let v: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(v["event"], "launchFinished");
    assert_eq!(v["project"], "shop");
    assert_eq!(v["launch"]["counts"]["failed"], 1);
    assert_eq!(v["gate"]["status"], "failed");
    assert_eq!(hooks[2].headers["x-fastproof-event"], "launchFinished");
    assert!(
        !fake.requests("/gone")[0]
            .headers
            .contains_key("x-fastproof-signature")
    );

    let tg = fake.requests("/bot123456:ABC-def_ghi/sendMessage");
    assert_eq!(tg.len(), 1);
    let tv: Value = serde_json::from_str(&tg[0].body).unwrap();
    assert_eq!(tv["chat_id"], "-100200");
    assert!(tv["text"].as_str().unwrap().contains("New failures (1)"));
    let sl: Value = serde_json::from_str(&fake.requests("/slack")[0].body).unwrap();
    assert!(sl["text"].as_str().unwrap().contains("quality gate failed"));
    let mail = mails.lock().unwrap().join("\n");
    assert!(
        mail.contains("qa@example.com") && mail.contains("quality gate failed"),
        "{mail}"
    );

    let (st, b) = s
        .get(
            "/api/v1/projects/shop/notifications/deliveries?status=failed",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (st, _) = s
        .get(
            "/api/v1/projects/shop/notifications/deliveries?status=weird",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .call(
            Method::GET,
            "/api/v1/projects/shop/notifications/deliveries",
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn egress_is_denied_outside_the_allowlist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let port = fake.addr.port().to_string();
    let allow = format!("localhost:{port}, 127.0.0.1:{port}, *.example.org");
    let e = env(&fake, &[("FASTPROOF_EGRESS_ALLOW", &allow)]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    let wild = format!("http://localhost.example.org:{port}/hook");
    settings(
        &db,
        "shop",
        json!({"notifications": [
            {"id": "other-host", "kind": "webhook", "on": ["launchFinished"], "target": "http://hooks.example.com/x"},
            {"id": "other-port", "kind": "webhook", "on": ["launchFinished"], "target": format!("http://127.0.0.1:{}/hook", fake.addr.port() + 1)},
            {"id": "redirect", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/redirect", fake.url())},
            {"id": "private", "kind": "webhook", "on": ["launchFinished"], "target": "http://10.0.0.1/hook"},
            {"id": "wildcard-to-internal", "kind": "webhook", "on": ["launchFinished"], "target": wild},
            {"id": "named-loopback", "kind": "webhook", "on": ["launchFinished"], "target": format!("http://localhost:{port}/hook")}
        ]}),
    )
    .await;
    let id = launch(&s, "shop", json!({"name": "n"})).await;
    finish(&s, "shop", &id).await;
    wait_for("deliveries settle", || async {
        let d = deliveries(&s, "shop").await;
        d.len() == 6
            && d.iter()
                .all(|x| x["status"] == "delivered" || x["status"] == "failed")
    })
    .await;
    let d = deliveries(&s, "shop").await;
    let by = |id: &str| {
        d.iter()
            .find(|x| x["notificationId"] == id)
            .unwrap()
            .clone()
    };
    for (n, text) in [
        ("other-host", "not in FASTPROOF_EGRESS_ALLOW"),
        ("other-port", "not in FASTPROOF_EGRESS_ALLOW"),
        ("redirect", "203.0.113.7 is not in FASTPROOF_EGRESS_ALLOW"),
        ("private", "not in FASTPROOF_EGRESS_ALLOW"),
    ] {
        let x = by(n);
        assert_eq!(x["status"], "failed", "{n}: {x}");
        assert_eq!(x["attempts"], 1, "{n}: denials are not retried");
        assert!(x["lastError"].as_str().unwrap().contains(text), "{n}: {x}");
    }
    let w = by("wildcard-to-internal");
    assert_eq!(w["status"], "failed", "{w}");
    assert!(
        w["lastError"]
            .as_str()
            .unwrap()
            .contains("internal address")
            || w["lastError"].as_str().unwrap().contains("cannot resolve"),
        "{w}"
    );
    assert_eq!(
        by("named-loopback")["status"],
        "delivered",
        "explicit hosts may be internal"
    );
    assert_eq!(fake.requests("/redirect").len(), 1);
    assert!(fake.requests("/stolen").is_empty());
}

#[tokio::test]
async fn secrets_are_write_only() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    let path = "/api/v1/projects/shop/secrets/slack-url";
    let value = "https://hooks.example.com/services/T000/B000/very-secret-value-789";
    let (st, _) = s
        .call(
            Method::PUT,
            path,
            Some(ADMIN_TOKEN),
            Some(json!({"value": value, "purpose": "slack"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = s
        .call(
            Method::PUT,
            path,
            Some(ADMIN_TOKEN),
            Some(
                json!({"value": value, "purpose": "slack", "origin": "https://hooks.example.com/"}),
            ),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, list) = s.get("/api/v1/projects/shop/secrets", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"][0]["name"], "slack-url");
    assert_eq!(list["items"][0]["purpose"], "slack");
    assert_eq!(list["items"][0]["origin"], "https://hooks.example.com");
    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    for text in [list.to_string(), audit.to_string()] {
        assert!(!text.contains("very-secret-value-789"), "{text}");
    }
    assert!(audit.to_string().contains("secret.create"));
    let stored: Vec<u8> =
        sqlx::query_scalar("select ciphertext from project_secrets where name = 'slack-url'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("very-secret"));
    let editor = s.token("ed", "editor", &["shop"]).await;
    let (st, _) = s
        .call(
            Method::PUT,
            path,
            Some(&editor),
            Some(json!({"value": "x", "purpose": "webhook"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.get("/api/v1/projects/shop/secrets", &editor).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for (name, body) in [
        ("-bad", json!({"value": "x", "purpose": "webhook"})),
        ("ok", json!({"value": "", "purpose": "webhook"})),
        (
            "ok",
            json!({"value": "x".repeat(9000), "purpose": "webhook"}),
        ),
        (
            "ok",
            json!({"value": "x", "purpose": "webhook", "extra": 1}),
        ),
        ("ok", json!({"value": "x"})),
        ("ok", json!({"value": "x", "purpose": "anything"})),
        ("ok", json!({"value": "x", "purpose": "vcs"})),
        (
            "ok",
            json!({"value": "x", "purpose": "fastmock", "origin": "fastmock.example.com"}),
        ),
        (
            "ok",
            json!({"value": "x", "purpose": "vcs", "origin": "https://gitlab.example.com/api"}),
        ),
        ("ok", json!({"value": "not a url", "purpose": "slack"})),
        (
            "ok",
            json!({"value": value, "purpose": "slack", "origin": "https://evil.example.net"}),
        ),
    ] {
        let (st, b) = s
            .call(
                Method::PUT,
                &format!("/api/v1/projects/shop/secrets/{name}"),
                Some(ADMIN_TOKEN),
                Some(body),
            )
            .await;
        assert!(
            matches!(
                st,
                StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "{name}: {st} {b}"
        );
    }
    let (st, _) = s.call(Method::DELETE, path, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = s.call(Method::DELETE, path, Some(ADMIN_TOKEN), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let dbg = format!("{:?}", s.running.as_ref().unwrap().state.cfg);
    assert!(!dbg.contains(KEY_HEX), "{dbg}");

    let mut s2 = Server::start(&db).await;
    let (st, b) = s2
        .call(
            Method::PUT,
            path,
            Some(ADMIN_TOKEN),
            Some(json!({"value": "x", "purpose": "webhook"})),
        )
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    s2.stop().await;
}

async fn mcp(s: &Server, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut rb = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .json(&body);
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    let r = rb.send().await.unwrap();
    let st = r.status();
    let text = r.text().await.unwrap();
    let json_text = text
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .map(|d| d.trim().to_string())
        .unwrap_or(text);
    (
        st,
        serde_json::from_str(&json_text).unwrap_or(Value::String(json_text)),
    )
}

async fn tool(s: &Server, token: &str, name: &str, args: Value) -> Value {
    let (st, b) = mcp(
        s,
        Some(token),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["result"].clone()
}

fn tool_json(r: &Value) -> Value {
    serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap_or(Value::Null)
}

#[tokio::test]
async fn mcp_tools_over_http() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    settings(
        &db,
        "shop",
        json!({"issueTrackers": [{"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}", "keyRegex": "[A-Z]+-[0-9]+"}]}),
    )
    .await;
    let l = launch(&s, "shop", json!({"name": "n"})).await;
    results(&s, "shop", &l, &[("a", "passed"), ("b", "failed")]).await;
    let res = s.results("shop", &l, "&status=failed").await;
    let rid = res[0]["id"].as_str().unwrap().to_string();

    let (st, _) = mcp(
        &s,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let viewer = s.token("v", "viewer", &["shop"]).await;
    let editor = s.token("e", "editor", &["shop"]).await;
    let (st, init) = mcp(
        &s,
        Some(&viewer),
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18",
            "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "fastproof", "{init}");
    let (st, list) = mcp(
        &s,
        Some(&viewer),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let mut names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "compare_launches",
            "create_rule_draft",
            "fastmock_interactions",
            "flaky_tests",
            "get_launch",
            "get_result",
            "launch_failures",
            "list_launches",
            "list_projects",
            "result_logs",
            "similar_failures",
            "test_history",
            "triage"
        ]
    );

    let r = tool(&s, &viewer, "list_projects", json!({})).await;
    assert_eq!(tool_json(&r)["items"][0]["key"], "shop");
    let r = tool(&s, &viewer, "list_launches", json!({"project": "shop"})).await;
    assert_eq!(tool_json(&r)["items"][0]["id"], l.as_str());
    let r = tool(
        &s,
        &viewer,
        "launch_failures",
        json!({"project": "shop", "launchId": l}),
    )
    .await;
    assert_eq!(tool_json(&r)["items"][0]["count"], 1, "{r}");
    let r = tool(
        &s,
        &viewer,
        "result_logs",
        json!({"project": "shop", "resultId": rid}),
    )
    .await;
    assert!(
        tool_json(&r)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("b failed"),
        "{r}"
    );
    let r = tool(
        &s,
        &viewer,
        "get_result",
        json!({"project": "shop", "resultId": rid}),
    )
    .await;
    assert_eq!(tool_json(&r)["status"], "failed");
    let r = tool(&s, &viewer, "list_launches", json!({"project": "other"})).await;
    assert_eq!(r["isError"], true, "{r}");
    let r = tool(
        &s,
        &viewer,
        "list_launches",
        json!({"project": "shop", "limit": 1000}),
    )
    .await;
    assert_eq!(r["isError"], true);

    let r = tool(
        &s,
        &viewer,
        "triage",
        json!({"project": "shop", "resultIds": [rid], "comment": "x"}),
    )
    .await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("forbidden"),
        "{r}"
    );
    let r = tool(
        &s,
        &editor,
        "triage",
        json!({"project": "shop", "resultIds": [rid], "comment": "flaky backend", "issue": {"key": "PAY-12"},
               "source": "user"}),
    )
    .await;
    assert_eq!(
        r["isError"], true,
        "unknown fields like source are rejected: {r}"
    );
    let r = tool(
        &s,
        &editor,
        "triage",
        json!({"project": "shop", "resultIds": [rid], "comment": "flaky backend", "issue": {"key": "PAY-12"}}),
    )
    .await;
    assert_ne!(r["isError"], true, "{r}");
    assert_eq!(tool_json(&r)["updated"], 1);
    let defect: Value = sqlx::query_scalar("select defect from results where id = $1")
        .bind(rid.parse::<uuid::Uuid>().unwrap())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(defect["source"], "ai");
    assert_eq!(defect["issueUrl"], "https://jira.example.com/browse/PAY-12");
    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let a = audit.to_string();
    assert!(
        a.contains("triage.apply") && a.contains("\"source\":\"ai\""),
        "{a}"
    );
    let draft = json!({"project": "shop", "name": "Payment timeouts", "match": {"messageRegex": "timeout"},
        "action": {"comment": "known gateway timeout"}});
    let r = tool(&s, &viewer, "create_rule_draft", draft.clone()).await;
    assert_eq!(r["isError"], true, "a viewer cannot write rules: {r}");
    let uploader = s.token("u", "uploader", &["shop"]).await;
    let r = tool(&s, &uploader, "create_rule_draft", draft.clone()).await;
    assert_eq!(r["isError"], true, "rules:write is required: {r}");
    let r = tool(&s, &editor, "create_rule_draft", draft).await;
    assert_ne!(r["isError"], true, "{r}");
    let created = tool_json(&r);
    assert_eq!(created["name"], "Payment timeouts", "{created}");
    assert_eq!(created["enabled"], false, "drafts are disabled: {created}");
    let (st, rules) = s.get("/api/v1/projects/shop/rules", &editor).await;
    assert_eq!(st, StatusCode::OK, "{rules}");
    assert_eq!(rules["items"].as_array().unwrap().len(), 1, "{rules}");
    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let created_by_ai = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["action"] == "rule.create" && e.to_string().contains("\"source\":\"ai\""));
    assert!(created_by_ai, "{audit}");
    let (st, b) = mcp(
        &s,
        Some(&viewer),
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "rm_rf", "arguments": {}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(b["error"].is_object(), "{b}");
}

async fn mock_result(
    s: &Server,
    project: &str,
    launch: &str,
    id: &str,
    traceparent: Option<&str>,
    start_ms: i64,
) -> String {
    let mut r = json!({"id": id, "name": id, "fullName": format!("mock.{id}"), "status": "failed",
        "start": start_ms, "stop": start_ms + 2000, "error": {"message": "boom"},
        "labels": [{"name": "fastmock.space", "value": "payments"}]});
    if let Some(t) = traceparent {
        r["traceparent"] = json!(t);
    }
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{launch}/results"),
            ADMIN_TOKEN,
            json!([r]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let all = s.results(project, launch, "").await;
    all.iter().find(|x| x["name"] == id).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn mock_interactions_against_a_fake_journal() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    let l = launch(&s, "shop", json!({"name": "n"})).await;
    let start_ms = 1_795_249_079_000i64;
    let at = |offset_ms: i64| {
        chrono::DateTime::from_timestamp_millis(start_ms + offset_ms)
            .unwrap()
            .to_rfc3339()
    };
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    *fake.st.journal.lock().unwrap() = vec![
        json!({"id": 3, "at": at(60_000), "source": "stubs", "method": "GET", "path": "/late", "query": "", "headers": {}, "body": "", "status": 200, "responseBody": "", "matched": true, "durationMs": 1}),
        json!({"id": 2, "at": at(1500), "source": "stubs", "method": "POST", "path": "/pay", "query": "a=1", "headers": {}, "body": "", "status": 502, "responseBody": "", "stubId": "7f6c1a1e-8f43-4c55-8f0e-1d2c3b4a5f60", "matched": true, "durationMs": 12, "traceId": trace, "space": "payments"}),
        json!({"id": 1, "at": at(500), "source": "stubs", "method": "GET", "path": "/user", "query": "", "headers": {}, "body": "", "status": 404, "responseBody": "", "matched": false, "durationMs": 3}),
    ];
    let traced = mock_result(
        &s,
        "shop",
        &l,
        "traced",
        Some(&format!("00-{trace}-00f067aa0ba902b7-01")),
        start_ms,
    )
    .await;
    let plain = mock_result(&s, "shop", &l, "plain", None, start_ms).await;

    let path = |r: &str| format!("/api/v1/projects/shop/results/{r}/mock-interactions");
    let (st, b) = s.get(&path(&traced), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::CONFLICT, "not configured: {b}");
    put_secret(
        &s,
        "shop",
        "fm",
        "fm-token-1",
        "fastmock",
        Some(&fake.url()),
    )
    .await;
    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": fake.url(), "tokenSecretRef": "fm"}}),
    )
    .await;

    let (st, b) = s.get(&path(&traced), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["source"], "traceId");
    assert_eq!(b["items"].as_array().unwrap().len(), 1, "{b}");
    assert_eq!(b["items"][0]["url"], "/pay?a=1");
    assert_eq!(b["items"][0]["status"], 502);
    assert_eq!(
        b["items"][0]["matchedStub"],
        "7f6c1a1e-8f43-4c55-8f0e-1d2c3b4a5f60"
    );
    assert_eq!(b["space"], "payments");
    let q = fake.requests("/__admin/requests");
    assert_eq!(q[0].headers["authorization"], "Bearer fm-token-1");
    assert!(
        q[0].query.contains("space=payments") && q[0].query.contains("since="),
        "{}",
        q[0].query
    );

    let (st, b) = s.get(&path(&plain), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["source"], "timeWindow");
    let urls: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["url"].as_str().unwrap())
        .collect();
    assert_eq!(urls, vec!["/user", "/pay?a=1"]);
    assert_eq!(b["items"][0]["nearMiss"], true);
    assert!(b["confidence"].as_f64().unwrap() < 0.95);

    let (st, _) = s
        .get(&format!("{}?attempt=9", path(&plain)), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .get(&format!("{}?attempt=0", path(&plain)), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    s.project("elsewhere").await;
    let other = s.token("o", "viewer", &["elsewhere"]).await;
    let (st, _) = s.get(&path(&plain), &other).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let viewer = s.token("v", "viewer", &["shop"]).await;
    let r = tool(
        &s,
        &viewer,
        "fastmock_interactions",
        json!({"project": "shop", "resultId": traced}),
    )
    .await;
    assert_eq!(tool_json(&r)["source"], "traceId", "{r}");

    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": "http://fastmock.example.net:9090"}}),
    )
    .await;
    let (st, b) = s.get(&path(&traced), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{b}");
    assert!(
        b["message"].as_str().unwrap().contains("egress denied"),
        "{b}"
    );
}

#[tokio::test]
async fn mock_interactions_against_a_real_fastmock() {
    let (Ok(admin), Ok(token)) = (
        std::env::var("FASTPROOF_TEST_FASTMOCK_URL"),
        std::env::var("FASTPROOF_TEST_FASTMOCK_TOKEN"),
    ) else {
        eprintln!("FASTPROOF_TEST_FASTMOCK_URL / _TOKEN are not set: skipped");
        return;
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    let data = std::env::var("FASTPROOF_TEST_FASTMOCK_DATA_URL").unwrap_or_else(|_| admin.clone());
    let trace = format!("{:032x}", rand::random::<u128>());
    let started = chrono::Utc::now().timestamp_millis();
    let r = reqwest::Client::new()
        .get(format!("{data}/fastproof-correlation-probe"))
        .header("traceparent", format!("00-{trace}-00f067aa0ba902b7-01"))
        .send()
        .await
        .unwrap();
    assert!(r.status().as_u16() > 0);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let fake = Fake::start().await;
    let host = url::Url::parse(&admin).unwrap();
    let allow = host.host_str().unwrap().to_string();
    let e = env(&fake, &[("FASTPROOF_EGRESS_ALLOW", &allow)]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    put_secret(&s, "shop", "fm", &token, "fastmock", Some(&admin)).await;
    settings(
        &db,
        "shop",
        json!({"fastmock": {"url": admin, "tokenSecretRef": "fm"}}),
    )
    .await;
    let l = launch(&s, "shop", json!({"name": "n"})).await;
    let mut res = json!({"id": "probe", "name": "probe", "fullName": "mock.probe", "status": "failed",
        "start": started - 1000, "stop": started + 1500, "traceparent": format!("00-{trace}-00f067aa0ba902b7-01")});
    res["error"] = json!({"message": "boom"});
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/shop/launches/{l}/results"),
            ADMIN_TOKEN,
            json!([res]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let rid = s.results("shop", &l, "").await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/shop/results/{rid}/mock-interactions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["source"], "traceId", "{b}");
    assert_eq!(b["items"][0]["url"], "/fastproof-correlation-probe", "{b}");
    assert_eq!(b["items"][0]["traceId"], trace.as_str());
}

async fn put_settings(s: &Server, project: &str, token: &str, doc: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        &format!("/api/v1/projects/{project}/settings"),
        Some(token),
        Some(doc),
    )
    .await
}

/// The Workflows envelope and the required parts of an Adaptive Card 1.5.
fn assert_teams_message(body: &str) -> Value {
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["type"], "message", "{v}");
    let att = &v["attachments"][0];
    assert_eq!(v["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(
        att["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    assert!(att["contentUrl"].is_null());
    let card = att["content"].clone();
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(card["version"], "1.5");
    assert_eq!(
        card["$schema"],
        "http://adaptivecards.io/schemas/adaptive-card.json"
    );
    assert!(!card["body"].as_array().unwrap().is_empty());
    for a in card["actions"].as_array().into_iter().flatten() {
        assert_eq!(a["type"], "Action.OpenUrl");
        assert!(a["title"].is_string() && a["url"].is_string(), "{a}");
    }
    assert!(body.len() <= fastproof_server::integrations::teams::MAX_PAYLOAD_BYTES);
    card
}

#[tokio::test]
async fn teams_cards_are_routed_by_filters_and_throttling_is_honoured() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    put_secret(
        &s,
        "shop",
        "teams-nightly",
        &format!(
            "{}/teams?api-version=1&sp=%2Ftriggers%2Fmanual%2Frun&sv=1.0&sig=nightly-sig-123",
            fake.url()
        ),
        "teams",
        None,
    )
    .await;
    put_secret(
        &s,
        "shop",
        "teams-mr",
        &format!("{}/teams-mr?sig=mr-sig-456", fake.url()),
        "teams",
        Some(&fake.url()),
    )
    .await;
    let (_, list) = s.get("/api/v1/projects/shop/secrets", ADMIN_TOKEN).await;
    let nightly = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "teams-nightly")
        .unwrap()
        .clone();
    assert_eq!(nightly["purpose"], "teams");
    assert_eq!(
        nightly["origin"],
        fake.url(),
        "the origin is taken from the URL"
    );
    fake.st.throttle_teams.store(1, Ordering::SeqCst);
    settings(
        &db,
        "shop",
        json!({"gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}],
               "notifications": [
            {"id": "nightly", "kind": "teams", "on": ["launchFinished"], "targetSecretRef": "teams-nightly",
             "filter": {"launchNames": ["nightly-*"], "attributes": ["trigger=schedule"]}},
            {"id": "mr", "kind": "teams", "on": ["gateFailed"], "targetSecretRef": "teams-mr",
             "filter": {"attributes": ["mr=*"], "branches": ["feature/*"]}}
        ]}),
    )
    .await;
    let night = launch(
        &s,
        "shop",
        json!({"name": "nightly-full", "vcs": {"branch": "main"},
               "attributes": [{"key": "trigger", "value": "schedule"}]}),
    )
    .await;
    results(&s, "shop", &night, &[("a", "passed"), ("b", "failed")]).await;
    finish(&s, "shop", &night).await;
    let mr = launch(
        &s,
        "shop",
        json!({"name": "mr-checks", "vcs": {"branch": "feature/login"},
               "attributes": [{"key": "mr", "value": "42"}]}),
    )
    .await;
    results(&s, "shop", &mr, &[("c", "failed")]).await;
    finish(&s, "shop", &mr).await;
    let other = launch(
        &s,
        "shop",
        json!({"name": "nightly-full", "vcs": {"branch": "main"},
               "attributes": [{"key": "trigger", "value": "push"}]}),
    )
    .await;
    finish(&s, "shop", &other).await;
    wait_for("teams deliveries settle", || async {
        let d = deliveries(&s, "shop").await;
        d.len() == 2 && d.iter().all(|x| x["status"] == "delivered")
    })
    .await;
    wait_for("the unmatched launch is evaluated", || {
        gate_done(&db, &other)
    })
    .await;
    let d = deliveries(&s, "shop").await;
    assert_eq!(d.len(), 2, "the push launch matched no filter: {d:?}");
    let by = |id: &str| {
        d.iter()
            .find(|x| x["notificationId"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by("nightly")["launchId"], night.as_str());
    assert_eq!(by("nightly")["kind"], "teams");
    assert_eq!(by("nightly")["attempts"], 2, "a 429 is retried");
    assert_eq!(by("nightly")["responseStatus"], 202);
    assert_eq!(by("mr")["launchId"], mr.as_str());
    assert_eq!(by("mr")["attempts"], 1);
    let listed = serde_json::to_string(&d).unwrap();
    assert!(!listed.contains("sig="), "{listed}");

    let calls = fake.requests("/teams");
    let night_calls: Vec<&Rec> = calls.iter().filter(|r| r.path == "/teams").collect();
    assert_eq!(night_calls.len(), 2);
    assert!(
        night_calls[1].at.duration_since(night_calls[0].at) >= Duration::from_millis(900),
        "Retry-After: 1 delays the retry beyond the 50 ms backoff"
    );
    assert_eq!(night_calls[0].body, night_calls[1].body);
    assert!(night_calls[1].query.contains("sig=nightly-sig-123"));
    assert_eq!(night_calls[1].headers["content-type"], "application/json");
    let card = assert_teams_message(&night_calls[1].body);
    let text = card.to_string();
    assert!(text.contains("shop / nightly-full #1"), "{text}");
    assert!(text.contains("Quality gate failed"), "{text}");
    assert!(text.contains("2 total · 1 passed · 1 failed"), "{text}");
    assert!(text.contains("Pass rate"), "{text}");
    assert!(text.contains("New failures (1)"), "{text}");
    let launch_url = format!("https://fastproof.example.com/p/shop/launches/{night}");
    assert_eq!(card["actions"][0]["title"], "Open launch");
    assert_eq!(card["actions"][0]["url"], launch_url.as_str());
    assert_eq!(card["actions"][1]["title"], "Open failures");
    assert_eq!(
        card["actions"][1]["url"],
        format!("{launch_url}?tab=failures")
    );
    assert!(
        text.contains("https://fastproof.example.com/p/shop/results/"),
        "new failures link to their results: {text}"
    );
    let mr_call = calls.iter().find(|r| r.path == "/teams-mr").unwrap();
    let mr_card = assert_teams_message(&mr_call.body);
    assert!(mr_card.to_string().contains("feature/login"));
}

#[tokio::test]
async fn teams_needs_a_teams_secret_on_its_origin() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    let teams_url = format!("{}/teams?sig=s3cr3t-sig", fake.url());
    for (body, want) in [
        (
            json!({"value": "http://teams.example.com/w?sig=x", "purpose": "teams"}),
            "https",
        ),
        (
            json!({"value": "not a url", "purpose": "teams"}),
            "workflow URL",
        ),
        (
            json!({"value": teams_url, "purpose": "teams", "origin": "https://other.example.com"}),
            "origin is not the origin",
        ),
    ] {
        let (st, b) = s
            .call(
                Method::PUT,
                "/api/v1/projects/shop/secrets/t",
                Some(ADMIN_TOKEN),
                Some(body),
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
        assert!(b["message"].as_str().unwrap().contains(want), "{want}: {b}");
    }
    put_secret(&s, "shop", "teams-url", &teams_url, "teams", None).await;
    put_secret(
        &s,
        "shop",
        "slack-url",
        &format!("{}/slack", fake.url()),
        "slack",
        None,
    )
    .await;
    let (_, base) = s.get("/api/v1/projects/shop/settings", ADMIN_TOKEN).await;
    let with = |n: Value| {
        let mut d = base.clone();
        d["notifications"] = json!([n]);
        d
    };
    for (n, want) in [
        (
            json!({"id": "t", "kind": "teams", "on": ["launchFinished"], "target": teams_url}),
            "target is not accepted",
        ),
        (
            json!({"id": "t", "kind": "teams", "on": ["launchFinished"]}),
            "need targetSecretRef",
        ),
        (
            json!({"id": "t", "kind": "teams", "on": ["launchFinished"], "targetSecretRef": "slack-url"}),
            "is for slack",
        ),
        (
            json!({"id": "t", "kind": "slack", "on": ["launchFinished"], "targetSecretRef": "teams-url"}),
            "is for teams",
        ),
        (
            json!({"id": "t", "kind": "teams", "on": ["launchFinished"], "targetSecretRef": "teams-url",
                   "filter": {"attributes": ["no-equals-sign"]}}),
            "key=value",
        ),
    ] {
        let (st, b) = put_settings(&s, "shop", ADMIN_TOKEN, with(n.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{n}: {b}");
        assert!(b["message"].as_str().unwrap().contains(want), "{want}: {b}");
    }
    let (st, b) = put_settings(
        &s,
        "shop",
        ADMIN_TOKEN,
        with(json!({"id": "t", "kind": "teams", "on": ["launchFinished"], "targetSecretRef": "teams-url",
                    "filter": {"launchNames": ["nightly-*"], "attributes": ["trigger=schedule"]}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["notifications"][0]["filter"]["launchNames"][0],
        "nightly-*"
    );
    let viewer = s.token("v", "viewer", &["shop"]).await;
    let (st, b) = s.get("/api/v1/projects/shop/settings", &viewer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["notifications"][0]["redacted"], true);
    assert!(!b.to_string().contains("s3cr3t-sig"));

    settings(
        &db,
        "shop",
        json!({"notifications": [{"id": "t", "kind": "slack", "on": ["launchFinished"], "targetSecretRef": "teams-url"}]}),
    )
    .await;
    let id = launch(&s, "shop", json!({"name": "n"})).await;
    finish(&s, "shop", &id).await;
    wait_for("the misbound delivery fails", || async {
        deliveries(&s, "shop")
            .await
            .first()
            .is_some_and(|d| d["status"] == "failed")
    })
    .await;
    let d = deliveries(&s, "shop").await;
    assert!(
        d[0]["lastError"].as_str().unwrap().contains("is for teams"),
        "{d:?}"
    );
    assert!(fake.requests("/teams").is_empty(), "nothing was sent");
}

#[tokio::test]
async fn test_notifications_are_sent_on_request() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    let e = env(&fake, &[]);
    let s = start(&db, &e).await;
    s.project("shop").await;
    put_secret(
        &s,
        "shop",
        "teams-url",
        &format!("{}/teams?sig=t-sig", fake.url()),
        "teams",
        None,
    )
    .await;
    put_secret(
        &s,
        "shop",
        "closed",
        &format!("{}/teams-closed?sig=c-sig", fake.url()),
        "teams",
        None,
    )
    .await;
    put_secret(&s, "shop", "hook-key", "hmac-key-789", "webhook", None).await;
    settings(
        &db,
        "shop",
        json!({"notifications": [
            {"id": "teams", "kind": "teams", "on": ["gateFailed"], "targetSecretRef": "teams-url"},
            {"id": "closed", "kind": "teams", "on": ["gateFailed"], "targetSecretRef": "closed"},
            {"id": "hook", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/hook", fake.url()), "secretRef": "hook-key"},
            {"id": "gone", "kind": "webhook", "on": ["launchFinished"], "target": format!("{}/gone", fake.url())}
        ]}),
    )
    .await;
    let test = |id: &str, token: Option<&str>| {
        let s = &s;
        let path = format!("/api/v1/projects/shop/notifications/{id}/test");
        let token = token.map(str::to_string);
        async move { s.call(Method::POST, &path, token.as_deref(), None).await }
    };

    let (st, b) = test("teams", Some(ADMIN_TOKEN)).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b,
        json!({"delivered": true, "responseStatus": 202, "error": null})
    );
    let sent = fake.requests("/teams");
    assert_eq!(sent.len(), 1);
    let card = assert_teams_message(&sent[0].body);
    let text = card.to_string();
    assert!(text.contains("Test notification"), "{text}");
    assert!(text.contains("shop / Sample launch #0"), "{text}");
    assert_eq!(
        card["actions"][0]["url"],
        "https://fastproof.example.com/p/shop"
    );

    let (st, b) = test("teams", Some(ADMIN_TOKEN)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(fake.requests("/teams").len(), 1);

    let (st, b) = test("hook", Some(ADMIN_TOKEN)).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["delivered"], true);
    let hook = &fake.requests("/hook")[0];
    let v: Value = serde_json::from_str(&hook.body).unwrap();
    assert_eq!(v["test"], true);
    assert_eq!(v["project"], "shop");
    let ts: i64 = hook.headers["x-fastproof-timestamp"].parse().unwrap();
    assert_eq!(
        hook.headers["x-fastproof-signature"],
        fastproof_server::integrations::notify::signature(
            b"hmac-key-789",
            ts,
            hook.body.as_bytes()
        )
    );

    let (st, b) = test("gone", Some(ADMIN_TOKEN)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["delivered"], false);
    assert_eq!(b["responseStatus"], 404);
    assert_eq!(b["error"], "HTTP 404");
    let (_, b) = test("closed", Some(ADMIN_TOKEN)).await;
    assert_eq!(b["delivered"], false, "{b}");
    assert!(b["error"].as_str().unwrap().contains("Anyone"), "{b}");
    assert!(!b.to_string().contains("c-sig"));

    let (st, _) = test("missing", Some(ADMIN_TOKEN)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let viewer = s.token("v", "viewer", &["shop"]).await;
    let (st, b) = test("hook", Some(&viewer)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    let (st, _) = test("hook", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(
        fake.requests("/hook").len(),
        1,
        "refused callers send nothing"
    );
    assert!(
        deliveries(&s, "shop").await.is_empty(),
        "tests are not queued as deliveries"
    );
    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let tests: Vec<&Value> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["action"] == "notification.test")
        .collect();
    assert_eq!(tests.len(), 4, "{audit}");
}
