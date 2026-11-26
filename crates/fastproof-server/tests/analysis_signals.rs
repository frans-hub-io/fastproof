//! Signals about test attempts (contract §7.7, §13 "K") over HTTP: push validation and limits
//! (kinds, addressing, time window, `detail`, sizes, per-attempt cap, duplicates, rate),
//! permissions and project isolation, matching at push time, by the launch's analysis step and
//! by the resolution job, deletion of signals that never match, FastMock phase 1 (a fake
//! journal always; a real FastMock binary with `FASTPROOF_TEST_FASTMOCK_BIN`), the FastMock
//! egress allowlist, the boost of a `systemIssue` suggestion by a grid signal, and that signals
//! never change a defect. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// The analysis role's jobs, paused so the tests run their work themselves.
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
    "analysis.tasks",
];

const TRACE_A: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const TRACE_B: &str = "0af7651916cd43dd8448eb211c80319c";

fn env<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut v = vec![
        ("FASTPROOF_ANALYSIS", "on"),
        ("FASTPROOF_ANALYSIS_WORKERS", "0"),
        ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
        ("FASTPROOF_ANALYSIS_GATE", "open"),
        ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
    ];
    for (k, val) in extra {
        v.retain(|(x, _)| x != k);
        v.push((k, val));
    }
    v
}

async fn start(db: &TestDb, extra: &[(&str, &str)]) -> Server {
    Server::start_with_paused(db, &env(extra), ROLE_JOBS).await
}

fn traceparent(trace: &str) -> String {
    format!("00-{trace}-00f067aa0ba902b7-01")
}

/// A failed attempt: client id, name, optional trace id, start in ms.
fn failed(id: &str, name: &str, trace: Option<&str>, start_ms: i64) -> Value {
    let mut r = json!({"id": id, "name": name, "fullName": format!("com.example.Shop#{name}"),
        "status": "failed", "start": start_ms, "stop": start_ms + 2000,
        "error": {"message": format!("{name} failed: connection refused by vault"),
                  "trace": format!("java.lang.IllegalStateException: {name}\n\tat com.example.{name}.run({name}.java:1)")}});
    if let Some(t) = trace {
        r["traceparent"] = json!(traceparent(t));
    }
    r
}

async fn new_launch(s: &Server, project: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    b["id"].as_str().unwrap().to_string()
}

/// Posts results and waits until the launch holds `total` results.
async fn post_results(s: &Server, project: &str, lid: &str, items: Vec<Value>, total: usize) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while s.results(project, lid, "").await.len() < total {
        assert!(Instant::now() < deadline, "results not stored");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn finish(s: &Server, project: &str, lid: &str) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{b}");
}

/// Runs the analysis role until the launch is analysed; returns its status.
async fn analysed(s: &Server, project: &str, lid: &str) -> Value {
    let state = s.running.as_ref().unwrap().state.clone();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        fastproof_server::analysis::run_once(&state).await.unwrap();
        let (_, a) = s
            .get(
                &format!("/api/v1/projects/{project}/launches/{lid}/analysis"),
                ADMIN_TOKEN,
            )
            .await;
        if a["state"] == "done" {
            return a;
        }
        assert!(Instant::now() < deadline, "not analysed: {a}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn result_id(s: &Server, project: &str, lid: &str, name: &str) -> String {
    s.results(project, lid, "")
        .await
        .into_iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no result {name}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn push(s: &Server, project: &str, token: &str, items: Vec<Value>) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/signals"),
        token,
        json!({"items": items}),
    )
    .await
}

async fn signals_of(s: &Server, project: &str, token: &str, result: &str) -> (StatusCode, Value) {
    s.get(
        &format!("/api/v1/projects/{project}/results/{result}/signals"),
        token,
    )
    .await
}

fn kinds(v: &Value) -> Vec<(String, String, String, i64)> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["source"].as_str().unwrap().to_string(),
                i["kind"].as_str().unwrap().to_string(),
                i["confidence"].as_str().unwrap().to_string(),
                i["attempt"].as_i64().unwrap_or(-1),
            )
        })
        .collect()
}

fn codes(v: &Value) -> Vec<(u64, String)> {
    v["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["index"].as_u64().unwrap(),
                r["code"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// `(id, defect)` of every result of a launch, to show signals never change a defect.
async fn defects(db: &TestDb, lid: &str) -> Vec<(String, Option<Value>)> {
    sqlx::query_as("select id::text, defect from results where launch_id = $1::uuid order by id")
        .bind(lid)
        .fetch_all(&db.pool)
        .await
        .unwrap()
}

async fn count(db: &TestDb, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&db.pool).await.unwrap()
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[tokio::test]
async fn push_validates_limits_permissions_and_matches_attempts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("sig").await;
    s.project("other").await;
    let editor = s.token("ed", "editor", &["sig"]).await;
    let viewer = s.token("v", "viewer", &["sig"]).await;
    let outsider = s.token("out", "admin", &["other"]).await;
    let start_ms = now().timestamp_millis() - 10_000;
    let l = new_launch(&s, "sig").await;
    post_results(
        &s,
        "sig",
        &l,
        vec![
            failed("c-grid", "Grid", Some(TRACE_A), start_ms),
            failed("c-ci", "Ci", None, start_ms),
        ],
        2,
    )
    .await;
    let ol = new_launch(&s, "other").await;
    let grid = result_id(&s, "sig", &l, "Grid").await;
    let ci = result_id(&s, "sig", &l, "Ci").await;
    let before = defects(&db, &l).await;
    let at = now().to_rfc3339();
    let old = (now() - chrono::Duration::days(8)).to_rfc3339();

    let items = vec![
        json!({"traceId": TRACE_A.to_ascii_uppercase(), "source": "grid", "kind": "node_lost", "at": at,
               "detail": {"node": "node-7", "sessionId": "s-1", "memoryMb": 2048, "retried": false}}),
        json!({"launchId": l, "resultClientId": "c-ci", "source": "ci", "kind": "runner_oom", "at": at}),
        json!({"traceId": TRACE_B, "source": "k8s", "kind": "oom_killed", "at": at}),
        json!({"traceId": TRACE_A, "source": "grid", "kind": "runner_oom", "at": at}),
        json!({"traceId": TRACE_A, "source": "mock", "kind": "unmatched", "at": at}),
        json!({"source": "grid", "kind": "evicted", "at": at}),
        json!({"launchId": ol, "resultClientId": "c-ci", "source": "ci", "kind": "pod_evicted", "at": at}),
        json!({"traceId": TRACE_A, "source": "grid", "kind": "evicted", "at": old}),
        json!({"traceId": TRACE_A, "source": "grid", "kind": "browser_oom", "at": at, "detail": {"log": "x".repeat(5000)}}),
        json!({"traceId": TRACE_A, "source": "grid", "kind": "node_lost", "at": at,
               "detail": {"node": "node-7", "sessionId": "s-1", "memoryMb": 2048, "retried": false}}),
        json!({"launchId": "00000000-0000-7000-8000-000000000000", "resultClientId": "c-ci", "source": "ci", "kind": "job_cancelled", "at": at}),
    ];
    let (st, b) = push(&s, "sig", &editor, items.clone()).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["accepted"], 3, "{b}");
    assert_eq!(b["matched"], 2, "{b}");
    assert_eq!(b["pending"], 1, "{b}");
    assert_eq!(
        codes(&b),
        vec![
            (3, "unknown_kind".into()),
            (4, "unknown_kind".into()),
            (5, "bad_address".into()),
            (6, "bad_address".into()),
            (7, "too_old".into()),
            (8, "detail_too_large".into()),
            (9, "duplicate".into()),
            (10, "bad_address".into()),
        ],
        "a launch of another project is the same as an unknown one"
    );
    let (st, b) = push(&s, "sig", &editor, vec![items[0].clone()]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(codes(&b), vec![(0, "duplicate".into())], "stored once");

    // The result's signals: viewers read them; another project neither reads nor finds them.
    let (st, v) = signals_of(&s, "sig", &viewer, &grid).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        kinds(&v),
        vec![("grid".into(), "node_lost".into(), "exact".into(), 1)]
    );
    assert_eq!(v["items"][0]["detail"]["node"], "node-7");
    assert_eq!(v["items"][0]["detail"]["memoryMb"], 2048);
    assert_eq!(v["items"][0]["resultId"], grid.as_str());
    let (_, v) = signals_of(&s, "sig", &viewer, &ci).await;
    assert_eq!(
        kinds(&v),
        vec![("ci".into(), "runner_oom".into(), "exact".into(), 1)]
    );
    let (st, _) = signals_of(&s, "sig", &outsider, &grid).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = signals_of(&s, "other", &outsider, &grid).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a result of another project");
    let (st, _) = signals_of(&s, "nope", &viewer, &grid).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Pushing needs `launches:write` in the project; the same 403 for unknown projects.
    let (st, b) = push(&s, "sig", &viewer, vec![items[1].clone()]).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    let (st, _) = push(&s, "sig", &outsider, vec![items[1].clone()]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = push(&s, "nope", &editor, vec![items[1].clone()]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .call(
            Method::POST,
            "/api/v1/projects/sig/signals",
            None,
            Some(json!({"items": [items[1].clone()]})),
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // The other project's own push cannot reach this project's launch.
    let (st, b) = push(&s, "other", &outsider, vec![items[1].clone()]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(codes(&b), vec![(0, "bad_address".into())]);

    // Whole-request checks.
    let (st, b) = push(&s, "sig", &editor, vec![]).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let many: Vec<Value> = (0..1001)
        .map(|i| json!({"traceId": TRACE_B, "source": "grid", "kind": "evicted", "at": (now() - chrono::Duration::seconds(i)).to_rfc3339()}))
        .collect();
    let (st, b) = push(&s, "sig", &editor, many).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = push(
        &s,
        "sig",
        &editor,
        vec![json!({"traceId": TRACE_B, "source": "grid", "kind": "evicted", "at": at, "detail": {"nested": {"a": 1}}})],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert!(b["message"].as_str().unwrap().contains("flat"), "{b}");
    let (st, b) = push(
        &s,
        "sig",
        &editor,
        vec![
            json!({"traceId": TRACE_B, "source": "grid", "kind": "evicted", "at": at, "extra": 1}),
        ],
    )
    .await;
    assert!(st.is_client_error(), "unknown fields are refused: {st} {b}");
    let big: Vec<Value> = (0..300)
        .map(|i| json!({"traceId": TRACE_B, "source": "grid", "kind": "evicted",
            "at": (now() - chrono::Duration::seconds(i)).to_rfc3339(), "detail": {"pad": "p".repeat(3800)}}))
        .collect();
    let (st, b) = push(&s, "sig", &editor, big).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{b}");
    assert_eq!(b["code"], "payload_too_large", "{b}");

    // At most 50 signals per attempt.
    let burst: Vec<Value> = (0..50)
        .map(|i| {
            json!({"traceId": TRACE_A, "source": "grid", "kind": "session_crashed",
            "at": (now() - chrono::Duration::seconds(i + 1)).to_rfc3339()})
        })
        .collect();
    let (st, b) = push(&s, "sig", &editor, burst).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["accepted"], 49, "{b}");
    assert_eq!(codes(&b), vec![(49, "attempt_full".into())]);
    let (_, v) = signals_of(&s, "sig", &viewer, &grid).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 50);

    // Signals never change a defect, and the analysis of the launch keeps it that way.
    finish(&s, "sig", &l).await;
    let a = analysed(&s, "sig", &l).await;
    assert_eq!(
        a["steps"]["signals"]["status"], "skipped",
        "no FastMock and nothing pending for this launch: {a}"
    );
    assert_eq!(defects(&db, &l).await, before);
}

#[tokio::test]
async fn pushes_are_rate_limited_per_project_and_need_analysis() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[("FASTPROOF_ANALYSIS_SIGNALS_PER_MINUTE", "3")]).await;
    for p in ["ra", "rb", "rc", "rd"] {
        s.project(p).await;
    }
    let at = |i: i64| (now() - chrono::Duration::seconds(i)).to_rfc3339();
    let item =
        |i: i64| json!({"traceId": TRACE_B, "source": "grid", "kind": "evicted", "at": at(i)});
    let (st, b) = push(&s, "ra", ADMIN_TOKEN, vec![item(1), item(2)]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["pending"], 2);
    let r = s
        .req(
            Method::POST,
            "/api/v1/projects/ra/signals",
            Some(ADMIN_TOKEN),
        )
        .json(&json!({"items": [item(3), item(4)]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["code"], "rate_limited");
    let (st, _) = push(&s, "rb", ADMIN_TOKEN, vec![item(1), item(2)]).await;
    assert_eq!(st, StatusCode::OK, "projects are separate");
    let five: Vec<Value> = (1..=5).map(item).collect();
    let (st, b) = push(&s, "rc", ADMIN_TOKEN, five.clone()).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "a push larger than the budget takes all of it: {b}"
    );
    assert_eq!(b["accepted"], 5);
    let (st, _) = push(&s, "rc", ADMIN_TOKEN, vec![item(9)]).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);

    // A project that opted out of Analysis, and an instance without it, take no signals.
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/rd/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"analysis": {"enabled": false}})),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let (st, b) = push(&s, "rd", ADMIN_TOKEN, vec![item(1)]).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "analysis_disabled");
    let off = Server::start_with(&db, &[("FASTPROOF_ANALYSIS", "off")]).await;
    let (st, b) = push(&off, "rb", ADMIN_TOKEN, vec![item(1)]).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "analysis_disabled");
}

#[tokio::test]
async fn late_signals_are_resolved_and_unmatched_ones_deleted() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("late").await;
    let l = new_launch(&s, "late").await;
    let at = now().to_rfc3339();
    let start_ms = now().timestamp_millis() - 10_000;

    // Pushed before their attempts exist: pending.
    let (st, b) = push(
        &s,
        "late",
        ADMIN_TOKEN,
        vec![
            json!({"traceId": TRACE_A, "source": "grid", "kind": "browser_oom", "at": at}),
            json!({"launchId": l, "resultClientId": "c-step", "source": "k8s", "kind": "pod_evicted", "at": at}),
            json!({"traceId": TRACE_B, "source": "grid", "kind": "startup_timeout", "at": at}),
        ],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (b["accepted"].as_u64(), b["pending"].as_u64()),
        (Some(3), Some(3)),
        "{b}"
    );

    // The resolution job finds the traced attempt once it arrives.
    post_results(
        &s,
        "late",
        &l,
        vec![failed("c-job", "Job", Some(TRACE_A), start_ms)],
        1,
    )
    .await;
    fastproof_server::analysis::signals::resolve(&state)
        .await
        .unwrap();
    let job = result_id(&s, "late", &l, "Job").await;
    let (_, v) = signals_of(&s, "late", ADMIN_TOKEN, &job).await;
    assert_eq!(
        kinds(&v),
        vec![("grid".into(), "browser_oom".into(), "exact".into(), 1)]
    );

    // The launch's analysis step resolves the signal pushed with its launch and client id; a
    // second attempt of the result is attempt 2.
    let mut retry = failed("c-step-2", "Step", None, start_ms + 3000);
    retry["retry"] = json!(1);
    post_results(
        &s,
        "late",
        &l,
        vec![failed("c-step", "Step", None, start_ms), retry],
        2,
    )
    .await;
    let step = result_id(&s, "late", &l, "Step").await;
    finish(&s, "late", &l).await;
    let a = analysed(&s, "late", &l).await;
    assert_eq!(a["steps"]["signals"]["status"], "done", "{a}");
    assert_eq!(a["steps"]["signals"]["detail"]["resolved"], 1, "{a}");
    let (_, v) = signals_of(&s, "late", ADMIN_TOKEN, &step).await;
    assert_eq!(
        kinds(&v),
        vec![("k8s".into(), "pod_evicted".into(), "exact".into(), 1)]
    );
    let (st, b) = push(
        &s,
        "late",
        ADMIN_TOKEN,
        vec![json!({"launchId": l, "resultClientId": "c-step-2", "source": "ci", "kind": "job_cancelled", "at": at})],
    )
    .await;
    assert_eq!(
        (st, b["matched"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{b}"
    );
    let (_, v) = signals_of(&s, "late", ADMIN_TOKEN, &step).await;
    assert_eq!(
        kinds(&v)[1],
        ("ci".into(), "job_cancelled".into(), "exact".into(), 2)
    );

    // The readers of suggestions and flaky hints see the matched signals of the project only.
    let (pid, series): (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("select project_id, series_id from results where id = $1::uuid")
            .bind(&step)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let by_series = state
        .store
        .signals_for_series(pid, series, now() - chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(by_series.len(), 2);
    assert!(
        by_series
            .iter()
            .all(|(r, s)| r.to_string() == step && s.result_id == Some(*r))
    );
    assert!(
        state
            .store
            .signals_for_series(
                uuid::Uuid::now_v7(),
                series,
                now() - chrono::Duration::days(1)
            )
            .await
            .unwrap()
            .is_empty()
    );
    let ids: Vec<uuid::Uuid> = [&job, &step].iter().map(|r| r.parse().unwrap()).collect();
    assert_eq!(
        state
            .store
            .signals_for_results(pid, &ids)
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(
        state
            .store
            .signals_for_results(uuid::Uuid::now_v7(), &ids)
            .await
            .unwrap()
            .is_empty()
    );

    // A signal that never matches is deleted after two days.
    assert_eq!(
        count(
            &db,
            "select count(*) from attempt_signals where not resolved"
        )
        .await,
        1
    );
    sqlx::query(
        "update attempt_signals set created_at = now() - interval '49 hours' where not resolved",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    fastproof_server::analysis::signals::resolve(&state)
        .await
        .unwrap();
    assert_eq!(
        count(
            &db,
            "select count(*) from attempt_signals where not resolved"
        )
        .await,
        0
    );
    assert_eq!(count(&db, "select count(*) from attempt_signals").await, 3);
}

/// The fake's journal and the queries it was asked.
type FakeState = (Arc<Vec<Value>>, Arc<Mutex<Vec<String>>>);

/// A FastMock admin API that answers `GET /__admin/requests` from a fixed journal (newest
/// first, paged) and records the queries.
struct FakeFastMock {
    url: String,
    queries: Arc<Mutex<Vec<String>>>,
}

impl FakeFastMock {
    async fn start(journal: Vec<Value>) -> Self {
        use axum::extract::{RawQuery, State};
        let queries = Arc::new(Mutex::new(Vec::new()));
        let st = (Arc::new(journal), queries.clone());
        let app = axum::Router::new()
            .route(
                "/__admin/requests",
                axum::routing::get(
                    |State((journal, queries)): State<FakeState>,
                     RawQuery(q): RawQuery| async move {
                        let q = q.unwrap_or_default();
                        queries.lock().unwrap().push(q.clone());
                        let offset: usize = q
                            .split('&')
                            .find_map(|kv| kv.strip_prefix("offset="))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        let page: Vec<Value> =
                            journal.iter().skip(offset).take(500).cloned().collect();
                        axum::Json(json!({"requests": page}))
                    },
                ),
            )
            .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url: format!("http://{addr}"),
            queries,
        }
    }
}

fn entry(
    at: chrono::DateTime<chrono::Utc>,
    method: &str,
    path: &str,
    status: u16,
    stub: Option<&str>,
    ms: u64,
    trace: Option<&str>,
) -> Value {
    let mut e = json!({"id": 1, "at": at.to_rfc3339(), "source": "stubs", "method": method, "path": path,
        "query": "", "headers": {}, "body": "", "status": status, "responseBody": "", "matched": stub.is_some(),
        "durationMs": ms});
    if let Some(st) = stub {
        e["stubId"] = json!(st);
    }
    if let Some(t) = trace {
        e["traceId"] = json!(t);
    }
    e
}

async fn fastmock_settings(db: &TestDb, project: &str, url: &str) {
    let id: uuid::Uuid = sqlx::query_scalar("select id from projects where key = $1")
        .bind(project)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(id, json!({"fastmock": {"url": url}}))
        .await
        .unwrap();
}

#[tokio::test]
async fn fastmock_phase_one_writes_mock_signals_within_the_egress_allowlist() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let t0 = now() - chrono::Duration::seconds(30);
    let ms = |d: i64| t0 + chrono::Duration::milliseconds(d);
    let pay = "7f6c1a1e-8f43-4c55-8f0e-1d2c3b4a5f60";
    let users = "3ff71b1a-3e73-433e-ab1d-fc0be57d70ad";
    let journal = vec![
        entry(
            ms(1900),
            "GET",
            "/report",
            200,
            Some("8b0e7b7e-1111-4c55-8f0e-1d2c3b4a5f60"),
            2600,
            None,
        ),
        entry(ms(1500), "GET", "/nowhere", 404, None, 1, Some(TRACE_A)),
        entry(ms(1200), "GET", "/users/2", 404, None, 1, Some(TRACE_A)),
        entry(ms(1000), "POST", "/pay", 502, Some(pay), 12, Some(TRACE_A)),
        entry(
            ms(800),
            "GET",
            "/users/1",
            200,
            Some(users),
            3,
            Some(TRACE_A),
        ),
    ];
    let fake = FakeFastMock::start(journal).await;
    let s = start(&db, &[]).await;
    s.project("fm").await;
    s.project("far").await;
    fastmock_settings(&db, "fm", &fake.url).await;
    fastmock_settings(&db, "far", "http://fastmock.example.net:9090").await;
    let l = new_launch(&s, "fm").await;
    let start_ms = t0.timestamp_millis();
    post_results(
        &s,
        "fm",
        &l,
        vec![
            failed("c-pay", "Pay", Some(TRACE_A), start_ms),
            failed("c-report", "Report", None, start_ms),
            json!({"id": "c-ok", "name": "Ok", "fullName": "com.example.Shop#Ok", "status": "passed",
                   "start": start_ms, "stop": start_ms + 2000}),
        ],
        3,
    )
    .await;
    let before = defects(&db, &l).await;
    finish(&s, "fm", &l).await;
    let a = analysed(&s, "fm", &l).await;
    let step = &a["steps"]["signals"];
    assert_eq!(step["status"], "done", "{a}");
    assert_eq!(
        step["detail"]["fastmock"]["attempts"], 2,
        "failed attempts only: {a}"
    );
    assert_eq!(step["detail"]["fastmock"]["read"], 2, "{a}");
    assert!(!fake.queries.lock().unwrap().is_empty());

    let pay_id = result_id(&s, "fm", &l, "Pay").await;
    let (_, v) = signals_of(&s, "fm", ADMIN_TOKEN, &pay_id).await;
    let got: BTreeSet<(String, String, String, i64)> = kinds(&v).into_iter().collect();
    assert_eq!(
        got,
        BTreeSet::from([
            ("mock".into(), "stub_error".into(), "exact".into(), 1),
            ("mock".into(), "unmatched".into(), "exact".into(), 1),
            ("mock".into(), "unexpected_calls".into(), "exact".into(), 1),
        ]),
        "{v}"
    );
    let err = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "stub_error")
        .unwrap();
    assert_eq!(err["detail"]["path"], "/pay");
    assert_eq!(err["detail"]["status"], 502);
    assert_eq!(err["detail"]["stub"], pay);
    let report = result_id(&s, "fm", &l, "Report").await;
    let (_, v) = signals_of(&s, "fm", ADMIN_TOKEN, &report).await;
    let got: BTreeSet<(String, String, String, i64)> = kinds(&v).into_iter().collect();
    assert!(
        got.contains(&("mock".into(), "slow".into(), "window".into(), 1)),
        "by time window: {v}"
    );

    // Analysing the launch again refreshes the rows instead of adding more.
    let n = count(
        &db,
        "select count(*) from attempt_signals where source = 'mock'",
    )
    .await;
    let state = s.running.as_ref().unwrap().state.clone();
    let lid: uuid::Uuid = l.parse().unwrap();
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'fm'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        state
            .store
            .enqueue_analysis_launch(
                pid,
                lid,
                fastproof_store::analysis::types::QueueReason::Recompute,
                Duration::ZERO
            )
            .await
            .unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while fastproof_server::analysis::run_once(&state)
        .await
        .unwrap()
        .processed
        == 0
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        count(
            &db,
            "select count(*) from attempt_signals where source = 'mock'"
        )
        .await,
        n
    );
    assert_eq!(
        defects(&db, &l).await,
        before,
        "signals never change a defect"
    );

    // FastMock outside FASTPROOF_EGRESS_ALLOW: the step fails without the host in its message.
    let far = new_launch(&s, "far").await;
    post_results(
        &s,
        "far",
        &far,
        vec![failed("c-far", "Far", Some(TRACE_B), start_ms)],
        1,
    )
    .await;
    finish(&s, "far", &far).await;
    let a = analysed(&s, "far", &far).await;
    let step = &a["steps"]["signals"];
    assert_eq!(step["status"], "failed", "{a}");
    let text = step.to_string();
    assert!(text.contains("HTTP 502"), "{text}");
    assert!(
        !text.contains("example.net"),
        "the FastMock host stays out of the status: {text}"
    );
    assert_eq!(
        count(&db, "select count(*) from attempt_signals s join projects p on p.id = s.project_id where p.key = 'far'").await,
        0
    );
}

/// Security review: time windows of parallel attempts overlap, so every attempt gets the same
/// journal entries back; the step keeps at most `MAX_KEPT_ENTRIES` over the launch (each entry
/// bounded) and is `partial` beyond, instead of holding attempts × entries in memory.
#[tokio::test]
async fn fastmock_reads_stop_at_the_kept_entry_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let t0 = now() - chrono::Duration::seconds(30);
    let long = format!("/assets/{}", "a".repeat(1000));
    let journal: Vec<Value> = (0..500)
        .map(|i| {
            entry(
                t0 + chrono::Duration::milliseconds(100 + i),
                "GET",
                &long,
                200,
                Some("8b0e7b7e-1111-4c55-8f0e-1d2c3b4a5f60"),
                1,
                None,
            )
        })
        .collect();
    let fake = FakeFastMock::start(journal).await;
    let s = start(&db, &[]).await;
    s.project("fm").await;
    fastmock_settings(&db, "fm", &fake.url).await;
    let l = new_launch(&s, "fm").await;
    let start_ms = t0.timestamp_millis();
    let attempts = fastproof_server::analysis::signals::MAX_KEPT_ENTRIES / 500 + 1;
    let items: Vec<Value> = (0..attempts)
        .map(|i| failed(&format!("c-{i}"), &format!("T{i}"), None, start_ms))
        .collect();
    post_results(&s, "fm", &l, items, attempts).await;
    finish(&s, "fm", &l).await;
    let a = analysed(&s, "fm", &l).await;
    let step = &a["steps"]["signals"];
    assert_eq!(step["status"], "partial", "{a}");
    let fm = &step["detail"]["fastmock"];
    assert_eq!(fm["attempts"], attempts, "{a}");
    assert_eq!(
        fm["read"],
        attempts - 1,
        "stops once the budget is reached: {a}"
    );
    assert_eq!(fm["truncated"], true, "{a}");
}

/// A FastMock process started from `FASTPROOF_TEST_FASTMOCK_BIN`, stopped when dropped.
struct FastMockProcess {
    child: std::process::Child,
    admin: String,
    data: String,
    token: String,
    _dir: tempfile::TempDir,
}

impl Drop for FastMockProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl FastMockProcess {
    async fn start(bin: &str) -> Self {
        use sha2::Digest;
        let token = format!("fm-{}", uuid::Uuid::now_v7().simple());
        let hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));
        let (data_port, admin_port) = (free_port(), free_port());
        let dir = tempfile::tempdir().unwrap();
        let log = std::fs::File::create(dir.path().join("fastmock.log")).unwrap();
        let child = std::process::Command::new(bin)
            .arg("serve")
            .env("FASTMOCK_STORAGE", "memory")
            .env("FASTMOCK_LISTEN", format!("127.0.0.1:{data_port}"))
            .env("FASTMOCK_ADMIN_LISTEN", format!("127.0.0.1:{admin_port}"))
            .env("FASTMOCK_ADMIN_TOKENS", format!("fastproof:admin:{hash}"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let p = Self {
            child,
            admin: format!("http://127.0.0.1:{admin_port}"),
            data: format!("http://127.0.0.1:{data_port}"),
            token,
            _dir: dir,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let ok = reqwest::get(format!("{}/readyz", p.admin))
                .await
                .is_ok_and(|r| r.status().is_success());
            if ok {
                return p;
            }
            assert!(Instant::now() < deadline, "FastMock did not start");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

#[tokio::test]
async fn fastmock_phase_one_against_a_real_fastmock() {
    let Ok(bin) = std::env::var("FASTPROOF_TEST_FASTMOCK_BIN") else {
        eprintln!("FASTPROOF_TEST_FASTMOCK_BIN is not set: skipped");
        return;
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    let fm = FastMockProcess::start(&bin).await;
    let http = reqwest::Client::new();
    for stub in [
        json!({"name": "users", "request": {"method": "GET", "urlPathTemplate": "/users/{id}",
            "headers": {"x-tenant": {"equalTo": "t1"}}}, "response": {"status": 200, "body": "ok"}}),
        json!({"name": "pay", "request": {"method": "POST", "urlPath": "/pay"}, "response": {"status": 503, "body": "down"}}),
    ] {
        let r = http
            .post(format!("{}/__admin/stubs", fm.admin))
            .bearer_auth(&fm.token)
            .json(&stub)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    }
    let started = now().timestamp_millis();
    let tp = traceparent(TRACE_A);
    for (method, path, tenant) in [
        ("GET", "/users/1", true),
        ("GET", "/users/2", false),
        ("POST", "/pay", false),
        ("GET", "/nowhere", false),
    ] {
        let mut rb = http
            .request(method.parse().unwrap(), format!("{}{path}", fm.data))
            .header("traceparent", &tp);
        if tenant {
            rb = rb.header("x-tenant", "t1");
        }
        rb.send().await.unwrap();
    }

    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("secrets.key");
    std::fs::write(
        &key_file,
        format!(
            "{}\n",
            hex::encode(uuid::Uuid::now_v7().as_bytes().repeat(2))
        ),
    )
    .unwrap();
    let key_path = key_file.to_string_lossy().to_string();
    let s = start(&db, &[("FASTPROOF_SECRETS_KEY_FILE", &key_path)]).await;
    s.project("real").await;
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/real/secrets/fm",
            Some(ADMIN_TOKEN),
            Some(json!({"value": fm.token, "purpose": "fastmock", "origin": fm.admin})),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    let id: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'real'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(
            id,
            json!({"fastmock": {"url": fm.admin, "tokenSecretRef": "fm"}}),
        )
        .await
        .unwrap();
    let l = new_launch(&s, "real").await;
    post_results(
        &s,
        "real",
        &l,
        vec![failed("c-real", "Checkout", Some(TRACE_A), started - 1000)],
        1,
    )
    .await;
    finish(&s, "real", &l).await;
    let a = analysed(&s, "real", &l).await;
    assert_eq!(a["steps"]["signals"]["status"], "done", "{a}");
    let rid = result_id(&s, "real", &l, "Checkout").await;
    let (_, v) = signals_of(&s, "real", ADMIN_TOKEN, &rid).await;
    let got: BTreeSet<(String, String, String, i64)> = kinds(&v).into_iter().collect();
    assert_eq!(
        got,
        BTreeSet::from([
            ("mock".into(), "stub_error".into(), "exact".into(), 1),
            ("mock".into(), "unmatched".into(), "exact".into(), 1),
            ("mock".into(), "unexpected_calls".into(), "exact".into(), 1),
        ]),
        "near miss on /users/{{id}}, 503 from the pay stub, an unknown route: {v}"
    );
}

#[tokio::test]
async fn a_pushed_grid_signal_boosts_the_system_issue_suggestion() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("boost").await;
    let editor = s.token("ed", "editor", &["boost"]).await;
    let (_, types) = s
        .get("/api/v1/projects/boost/defect-types", ADMIN_TOKEN)
        .await;
    let si = types["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == "systemIssue" && t["builtIn"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let start_ms = now().timestamp_millis() - 60_000;

    // A person decided this failure was a system issue.
    let first = new_launch(&s, "boost").await;
    post_results(
        &s,
        "boost",
        &first,
        vec![failed("c-1", "Vault", None, start_ms)],
        1,
    )
    .await;
    finish(&s, "boost", &first).await;
    let r = result_id(&s, "boost", &first, "Vault").await;
    let (st, b) = s
        .post(
            "/api/v1/projects/boost/triage",
            &editor,
            json!({"resultIds": [r], "defectTypeId": si}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    analysed(&s, "boost", &first).await;
    fastproof_server::analysis::suggestions::fold(&state)
        .await
        .unwrap();

    // The same failure again, and the grid says the node was lost during the attempt.
    let second = new_launch(&s, "boost").await;
    post_results(
        &s,
        "boost",
        &second,
        vec![failed("c-2", "Vault", Some(TRACE_A), start_ms + 30_000)],
        1,
    )
    .await;
    let (st, b) = push(
        &s,
        "boost",
        &editor,
        vec![json!({"traceId": TRACE_A, "source": "grid", "kind": "node_lost", "at": now().to_rfc3339()})],
    )
    .await;
    assert_eq!(
        (st, b["matched"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{b}"
    );
    let before = defects(&db, &second).await;
    finish(&s, "boost", &second).await;
    let a = analysed(&s, "boost", &second).await;
    assert_eq!(a["steps"]["suggestions"]["status"], "done", "{a}");
    let (st, v) = s
        .get(
            &format!("/api/v1/projects/boost/launches/{second}/suggestions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let cluster = &v["clusters"][0];
    assert_eq!(
        cluster["signals"],
        json!([{"source": "grid", "kind": "node_lost", "confidence": "exact", "count": 1}]),
        "{v}"
    );
    let top = &cluster["suggestions"][0];
    assert_eq!(top["defectTypeId"], si.as_str(), "{v}");
    let grid = top["why"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["signal"] == "grid")
        .unwrap_or_else(|| panic!("no grid contribution: {top}"));
    assert!(
        (grid["weight"].as_f64().unwrap() - 0.30).abs() < 1e-9,
        "{grid}"
    );
    assert_eq!(
        defects(&db, &second).await,
        before,
        "a suggestion is not a decision"
    );
}
