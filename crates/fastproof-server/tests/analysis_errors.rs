//! Unique errors over HTTP (contract `76` §7.4, §8, §13 "E"): the `errors` step of the analysis
//! role through the life of an error (new, resolved, regressed), the `errorRegressed` delivery
//! and its rate per channel, the clusters' `isNew`/`newOnBranch`/`errorState`, the list, the
//! detail, the timeline and the patch with their permissions and IDOR checks, and the gate on
//! the new channel event. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb, paused};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const ROLE_JOBS: &[&str] = &[
    "analysis.consume",
    "analysis.prune",
    "analysis.tasks.run",
    "analysis.memory.fold",
    "analysis.memory.seed",
    "analysis.backtest.nightly",
    "analysis.errors.issues",
    "analysis.signals.resolve",
    "analysis.flaky.daily",
    "analysis.ch.backfill",
    "analysis.ch.rollup",
];

/// A webhook receiver that records the bodies it gets.
struct Hook {
    addr: SocketAddr,
    got: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Hook {
    async fn start() -> Self {
        let got: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let g = got.clone();
        let app = axum::Router::new().fallback(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let g = g.clone();
                async move {
                    let event = headers
                        .get("x-fastproof-event")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    g.lock()
                        .unwrap()
                        .push((event, serde_json::from_slice(&body).unwrap_or_default()));
                    axum::http::StatusCode::NO_CONTENT
                }
            },
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        Self { addr, got }
    }

    fn events(&self, name: &str) -> Vec<Value> {
        self.got
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e == name)
            .map(|(_, b)| b.clone())
            .collect()
    }
}

fn res(id: &str, msg: &str) -> Value {
    json!({"id": id, "name": format!("test {id}"), "fullName": format!("com.example.Suite#t{id}"), "status": "failed",
        "error": {"message": msg, "trace": format!("java.lang.IllegalStateException: {msg}\n\tat com.example.Svc.run(Svc.java:1)")}})
}

fn pass(id: &str) -> Value {
    json!({"id": id, "name": format!("test {id}"), "fullName": format!("com.example.Suite#t{id}"), "status": "passed"})
}

/// A launch of a branch with results, finished and analysed.
async fn run(
    s: &Server,
    state: &fastproof_server::state::AppState,
    project: &str,
    branch: &str,
    commit: &str,
    items: Vec<Value>,
) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": branch, "commit": commit}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let l = b["id"].as_str().unwrap().to_string();
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{l}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{l}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success());
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        fastproof_server::analysis::run_once(state).await.unwrap();
        let (_, a) = s
            .get(
                &format!("/api/v1/projects/{project}/launches/{l}/analysis"),
                ADMIN_TOKEN,
            )
            .await;
        if a["state"] == "done" {
            assert_eq!(a["steps"]["errors"]["status"], "done", "{a}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "launch {l} was not analysed: {a}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    l
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn errors_live_regress_notify_and_answer_over_http() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let hook = Hook::start().await;
    let s = Server::start_cfg(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
            ("FASTPROOF_ANALYSIS_GATE", "open"),
            ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
            ("FASTPROOF_PUBLIC_URL", "https://fastproof.example.com"),
        ],
        |c| {
            paused(ROLE_JOBS)(c);
            c.integrations.poll = Duration::from_millis(100);
            c.integrations.settle = Duration::ZERO;
            c.integrations.delivery_backoff = Duration::from_millis(50);
        },
    )
    .await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("er").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["er"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/er/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"analysis": {"resolveAfterLaunches": 1},
                "notifications": [{"id": "hook", "kind": "webhook", "on": ["errorRegressed"],
                    "target": format!("http://{}/hook", hook.addr)}]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, c) = s
        .get("/api/v1/projects/er/analysis/capabilities", &viewer)
        .await;
    assert_eq!(c["features"]["errors"], true, "{c}");

    // New on its first launch, on main.
    let l1 = run(
        &s,
        &state,
        "er",
        "main",
        "aaa111",
        vec![res("1", "pool exhausted"), pass("2")],
    )
    .await;
    let (st, list) = s.get("/api/v1/projects/er/errors", &viewer).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let e = &list["items"][0];
    assert_eq!(
        (e["state"].as_str(), e["launches"].as_i64()),
        (Some("new"), Some(1))
    );
    assert_eq!(e["branches"], json!(["main"]));
    assert_eq!(e["spark"].as_array().unwrap().len(), 14);
    assert_eq!(e["spark"][13], 1);
    let fp = e["fingerprint"].as_str().unwrap().to_string();
    let (_, cl) = s
        .get(
            &format!("/api/v1/projects/er/launches/{l1}/clusters"),
            &viewer,
        )
        .await;
    let c0 = &cl["items"][0];
    assert_eq!(
        (
            c0["isNew"].as_bool(),
            c0["newOnBranch"].as_bool(),
            c0["errorState"].as_str()
        ),
        (Some(true), Some(true), Some("new")),
        "{cl}"
    );

    // Resolved after one launch without it (resolveAfterLaunches = 1), then regressed: one
    // delivery; the second regression within 10 minutes is not sent again.
    run(
        &s,
        &state,
        "er",
        "main",
        "bbb222",
        vec![pass("1"), pass("2")],
    )
    .await;
    let (_, d) = s
        .get(&format!("/api/v1/projects/er/errors/{fp}"), &viewer)
        .await;
    assert_eq!(d["state"], "resolved", "{d}");
    assert!(d["mttrSeconds"].as_i64().is_some(), "{d}");
    let l3 = run(
        &s,
        &state,
        "er",
        "main",
        "ccc333",
        vec![res("1", "pool exhausted")],
    )
    .await;
    wait_for("the errorRegressed webhook", || {
        !hook.events("errorRegressed").is_empty()
    })
    .await;
    let got = hook.events("errorRegressed");
    assert_eq!(got[0]["count"], 1, "{}", got[0]);
    assert_eq!(got[0]["items"][0]["fingerprint"], fp.as_str());
    assert_eq!(got[0]["launch"]["id"], l3.as_str());
    assert_eq!(
        got[0]["items"][0]["url"],
        format!("https://fastproof.example.com/p/er/errors/{fp}")
    );
    run(&s, &state, "er", "main", "ddd444", vec![pass("1")]).await;
    run(
        &s,
        &state,
        "er",
        "main",
        "eee555",
        vec![res("1", "pool exhausted")],
    )
    .await;
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from integration_deliveries where events @> array['errorRegressed']",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        queued, 1,
        "at most one errorRegressed delivery per channel every 10 minutes"
    );
    let (_, d) = s
        .get(&format!("/api/v1/projects/er/errors/{fp}"), &viewer)
        .await;
    assert_eq!(
        (d["state"].as_str(), d["regressedCount"].as_i64()),
        (Some("regressed"), Some(2)),
        "{d}"
    );
    assert_eq!(d["commits"]["first"], "eee555", "{d}");
    assert_eq!(d["commits"]["lastGood"], "ddd444", "{d}");
    assert_eq!(d["window"]["backend"], "postgres");
    assert_eq!(d["timeline"].as_array().unwrap().len(), 31, "{d}");
    assert_eq!(d["branchList"][0]["branch"], "main");
    assert_eq!(d["testList"][0]["occurrences"], 3, "{d}");

    // Filters, the timeline and bad input.
    let (_, r) = s
        .get(
            "/api/v1/projects/er/errors?state=regressed&branch=main&q=exhaust",
            &viewer,
        )
        .await;
    assert_eq!(r["items"].as_array().unwrap().len(), 1, "{r}");
    let (_, r) = s
        .get("/api/v1/projects/er/errors?state=resolved", &viewer)
        .await;
    assert!(r["items"].as_array().unwrap().is_empty());
    let (st, _) = s.get("/api/v1/projects/er/errors?q=ab", &viewer).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .get("/api/v1/projects/er/errors?state=gone", &viewer)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s.get("/api/v1/projects/er/errors?limit=501", &viewer).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, t) = s
        .get(
            &format!("/api/v1/projects/er/errors/{fp}/timeline"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    let total: i64 = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["occurrences"].as_i64().unwrap())
        .sum();
    assert_eq!(total, 3);
    let (st, t) = s
        .get(
            &format!("/api/v1/projects/er/errors/{fp}/timeline?from=2020-01-01T00:00:00Z"),
            &viewer,
        )
        .await;
    assert_eq!(
        (st, t["window"]["partial"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/er/errors/{fp}/timeline?from=2030-01-01T00:00:00Z&to=2029-01-01T00:00:00Z"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Permissions and other projects (IDOR): reads need launches:read in the project, patches
    // results:triage; an error of another project is not found there.
    let (st, _) = s.get("/api/v1/projects/er/errors", &outsider).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(&format!("/api/v1/projects/er/errors/{fp}"), &outsider)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.get("/api/v1/projects/nope/errors", &outsider).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "the same answer whether the project exists or not"
    );
    for path in [
        format!("/api/v1/projects/other/errors/{fp}"),
        format!("/api/v1/projects/other/errors/{fp}/timeline"),
        "/api/v1/projects/er/errors/zz".to_string(),
    ] {
        let (st, _) = s.get(&path, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/other/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"state": "ignored"})),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(&viewer),
            Some(json!({"state": "ignored"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"state": "ignored", "extra": 1})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"aliasOf": fp})),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("alias_cycle")),
        "{b}"
    );
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"state": "ignored"})),
        )
        .await;
    assert_eq!(
        (st, b["state"].as_str()),
        (StatusCode::OK, Some("ignored")),
        "{b}"
    );
    let audit: Value = sqlx::query_scalar(
        "select detail from platform_audit where action = 'error.update' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(audit["fingerprint"], fp.as_str());
    assert_eq!(audit["state"], "ignored");
    // Ignored errors do not change by themselves and send nothing.
    run(&s, &state, "er", "main", "fff666", vec![pass("1")]).await;
    run(
        &s,
        &state,
        "er",
        "main",
        "ggg777",
        vec![res("1", "pool exhausted")],
    )
    .await;
    let (_, d) = s
        .get(&format!("/api/v1/projects/er/errors/{fp}"), &viewer)
        .await;
    assert_eq!(d["state"], "ignored");
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp}"),
            Some(ADMIN_TOKEN),
            Some(json!({"state": "active"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["state"], "regressed",
        "activated: computed again from its occurrences"
    );

    // A second error on another branch: new there, merged into the first.
    let l8 = run(
        &s,
        &state,
        "er",
        "feature/x",
        "hhh888",
        vec![res("3", "disk full on volume")],
    )
    .await;
    let (_, cl) = s
        .get(
            &format!("/api/v1/projects/er/launches/{l8}/clusters"),
            &viewer,
        )
        .await;
    let fp2 = cl["items"][0]["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(cl["items"][0]["isNew"], true);
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp2}"),
            Some(ADMIN_TOKEN),
            Some(json!({"aliasOf": fp})),
        )
        .await;
    assert_eq!(
        (st, b["aliasOf"].as_str()),
        (StatusCode::OK, Some(fp.as_str())),
        "{b}"
    );
    let (_, list) = s.get("/api/v1/projects/er/errors", &viewer).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["aliases"], 1);
    let (_, cl) = s
        .get(
            &format!("/api/v1/projects/er/launches/{l8}/clusters"),
            &viewer,
        )
        .await;
    assert_eq!(
        cl["items"][0]["fingerprint"],
        fp.as_str(),
        "clusters group by the canonical error"
    );
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/er/errors/{fp2}"),
            Some(ADMIN_TOKEN),
            Some(json!({"aliasOf": null})),
        )
        .await;
    assert_eq!(
        (st, b["aliasOf"].clone()),
        (StatusCode::OK, Value::Null),
        "{b}"
    );
    assert_eq!(b["state"], "new");
}

#[tokio::test]
async fn the_error_regressed_channel_waits_for_the_analysis_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_ANALYSIS_GATE", "closed"),
        ],
        ROLE_JOBS,
    )
    .await;
    s.project("gt").await;
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/gt/settings",
            Some(ADMIN_TOKEN),
            Some(
                json!({"notifications": [{"id": "h", "kind": "webhook", "on": ["errorRegressed"],
                "target": "https://hooks.example.com/x"}]}),
            ),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("upgrade_in_progress")),
        "{b}"
    );
    assert_eq!(b["details"]["area"], "analysis");
    // The errors routes themselves are plain reads and patches: they answer with the gate closed.
    let (st, b) = s.get("/api/v1/projects/gt/errors", ADMIN_TOKEN).await;
    assert_eq!((st, b["items"].clone()), (StatusCode::OK, json!([])));
}
