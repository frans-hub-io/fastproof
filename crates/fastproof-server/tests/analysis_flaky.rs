//! Flaky causes over HTTP (contract `76` §7.8, §8, §13 "H"): the `flakyHints` step of the
//! analysis role after launches in which a test flips on one host, the hints in the flaky list
//! and at `GET .../flaky/{seriesId}/hints` with their permissions and IDOR checks, the daily job,
//! the capability and the `flaky.hintMinScore` setting. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb, paused};
use fastproof_server::analysis::flaky_hints::{Batch, daily_within, recompute_with};
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

fn res(id: &str, name: &str, status: &str, host: &str) -> Value {
    let mut v = json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"), "status": status,
        "labels": [{"name": "host", "value": host}]});
    if status == "failed" {
        v["error"] = json!({"message": "expected the basket to hold 2 items but it held 3",
            "trace": "java.lang.AssertionError: expected 2 but was 3\n\tat com.example.Suite.basket(Suite.java:42)"});
    }
    v
}

/// A launch with results, finished and analysed; returns its id and the step outcome.
async fn run(
    s: &Server,
    state: &fastproof_server::state::AppState,
    project: &str,
    items: Vec<Value>,
) -> (String, Value) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": "main"}}),
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
            return (l, a["steps"]["flakyHints"].clone());
        }
        assert!(
            std::time::Instant::now() < deadline,
            "launch {l} was not analysed: {a}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn flaky_causes_over_http() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
        ],
        paused(ROLE_JOBS),
    )
    .await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("fl").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["fl"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;
    let (_, c) = s
        .get("/api/v1/projects/fl/analysis/capabilities", &viewer)
        .await;
    assert_eq!(c["features"]["flakyHints"], true, "{c}");

    // The setting's range.
    for (v, want) in [(0.01, StatusCode::BAD_REQUEST), (0.3, StatusCode::OK)] {
        let (st, b) = s
            .call(
                Method::PUT,
                "/api/v1/projects/fl/settings",
                Some(ADMIN_TOKEN),
                Some(json!({"analysis": {"flaky": {"hintMinScore": v}}})),
            )
            .await;
        assert_eq!(st, want, "{v}: {b}");
    }

    // A test that fails on host ci-3 four times out of five and passes elsewhere.
    let mut steps = Vec::new();
    for i in 0..15 {
        let host = ["ci-1", "ci-2", "ci-3"][i % 3];
        let status = if host == "ci-3" && i != 5 {
            "failed"
        } else {
            "passed"
        };
        let (_, step) = run(
            &s,
            &state,
            "fl",
            vec![
                res(&format!("a{i}"), "basket", status, host),
                res(&format!("b{i}"), "steady", "passed", host),
            ],
        )
        .await;
        steps.push(step);
    }
    assert!(steps.iter().all(|x| x["status"] == "done"), "{steps:#?}");
    assert!(
        steps.iter().any(|x| x["detail"]["series"] == 1),
        "the flipping series was computed after a launch: {steps:#?}"
    );
    assert!(steps.iter().all(|x| x["detail"]["backend"] == "postgres"));

    let (st, list) = s.get("/api/v1/projects/fl/flaky", &viewer).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "basket")
        .unwrap_or_else(|| panic!("{list}"))
        .clone();
    assert_eq!(item["hints"][0]["kind"], "environment", "{item}");
    assert_eq!(item["infraShare"], 0.0);
    let series = item["seriesId"].as_str().unwrap().to_string();

    let path = format!("/api/v1/projects/fl/flaky/{series}/hints");
    let (st, h) = s.get(&path, &viewer).await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(h["seriesId"], series.as_str());
    assert_eq!(h["backend"], "postgres");
    assert_eq!(h["windowRuns"], 15);
    assert_eq!(h["failures"], 4);
    let e = &h["hints"][0];
    assert_eq!(
        (
            e["kind"].as_str(),
            e["evidence"]["dimension"].as_str(),
            e["evidence"]["value"].as_str()
        ),
        (Some("environment"), Some("host"), Some("ci-3")),
        "{h}"
    );
    assert_eq!(e["actions"], json!(["mute", "issue"]));
    assert!(e["summary"].as_str().unwrap().contains("ci-3"));
    assert!(e["evidence"]["examples"][0]["resultId"].is_string());

    // Permissions and ownership: unauthenticated 401, another project's token 403, the series
    // through another project 404, a malformed id 404 (like an unknown one).
    let (st, _) = s.call(Method::GET, &path, None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = s.get(&path, &outsider).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(
            "/api/v1/projects/missing/flaky/00000000-0000-0000-0000-000000000000/hints",
            &outsider,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "same answer whether the project exists or not"
    );
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/other/flaky/{series}/hints"),
            &outsider,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    let (st, _) = s
        .get("/api/v1/projects/fl/flaky/not-a-uuid/hints", &viewer)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/fl/flaky/{}/hints", uuid::Uuid::now_v7()),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");

    // The steady test has no hints row: an empty answer, not an error.
    let steady: String = sqlx::query_scalar(
        "select s.id::text from series s join test_cases tc on tc.id = s.test_case_id \
         where tc.display_name = 'steady'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/fl/flaky/{steady}/hints"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (h["computedAt"].clone(), h["hints"].clone()),
        (Value::Null, json!([]))
    );

    // The daily job recomputes series whose hints are missing.
    sqlx::query("delete from flaky_hints")
        .execute(&db.pool)
        .await
        .unwrap();
    let (_, h) = s.get(&path, &viewer).await;
    assert_eq!(h["computedAt"], Value::Null);
    fastproof_server::analysis::flaky_hints::daily(&state)
        .await
        .unwrap();
    let (_, h) = s.get(&path, &viewer).await;
    assert!(h["computedAt"].is_string(), "{h}");
    assert_eq!(h["hints"][0]["kind"], "environment");

    // Budgets (security review): an expired deadline computes nothing and says so; a
    // ClickHouse that fails is asked once per batch and every series is still computed on
    // PostgreSQL; a series of another project (or none) is not computed.
    let project = state.store.project("fl").await.unwrap().unwrap();
    let settings = state.store.project_settings(project.id).await.unwrap();
    let ids: Vec<uuid::Uuid> = vec![
        series.parse().unwrap(),
        steady.parse().unwrap(),
        uuid::Uuid::now_v7(),
    ];
    let mut b = Batch::default();
    recompute_with(
        &state,
        &project,
        &settings,
        &ids,
        Instant::now(),
        true,
        &mut b,
        |_| async { Ok(None) },
    )
    .await;
    assert!(b.out_of_time, "{b:?}");
    assert_eq!(b.series, 0, "{b:?}");
    let calls = AtomicUsize::new(0);
    let mut b = Batch::default();
    recompute_with(
        &state,
        &project,
        &settings,
        &ids,
        Instant::now() + Duration::from_secs(60),
        true,
        &mut b,
        |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(anyhow::anyhow!("clickhouse is down")) }
        },
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "ClickHouse asked once");
    assert!(b.clickhouse_down && !b.out_of_time, "{b:?}");
    assert_eq!((b.series, b.clickhouse, b.failed), (2, 0, 0), "{b:?}");

    // The daily job shares its time between the projects: none without time, every project
    // with Analysis on otherwise.
    sqlx::query("delete from flaky_hints")
        .execute(&db.pool)
        .await
        .unwrap();
    let r = daily_within(&state, Duration::ZERO, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!((r.projects, r.series), (0, 0), "{r:?}");
    let (_, h) = s.get(&path, &viewer).await;
    assert_eq!(h["computedAt"], Value::Null);
    for day in 0..2 {
        sqlx::query("delete from flaky_hints")
            .execute(&db.pool)
            .await
            .unwrap();
        let r = daily_within(
            &state,
            Duration::from_secs(60),
            chrono::Utc::now() + chrono::Duration::days(day),
        )
        .await
        .unwrap();
        assert!(r.projects >= 2, "{r:?}");
        assert!(
            r.series >= 1 && r.failed == 0 && r.out_of_time == 0,
            "{r:?}"
        );
        let (_, h) = s.get(&path, &viewer).await;
        assert_eq!(h["hints"][0]["kind"], "environment", "{h}");
    }

    // A project that opted out of Analysis gets no step.
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/fl/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"analysis": {"enabled": false}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, step) = run(&s, &state, "fl", vec![res("z", "basket", "failed", "ci-3")]).await;
    assert_eq!(step["status"], "skipped", "{step}");
}

fn ch_vars(db_name: &str) -> Option<Vec<(String, String)>> {
    let url = std::env::var("FASTPROOF_TEST_CLICKHOUSE_URL").ok()?;
    let mut v = vec![
        ("FASTPROOF_SINKS".to_string(), "clickhouse".to_string()),
        ("FASTPROOF_CLICKHOUSE_URL".into(), url),
        ("FASTPROOF_CLICKHOUSE_DATABASE".into(), db_name.into()),
        ("FASTPROOF_SINK_FLUSH_INTERVAL_MS".into(), "200".into()),
    ];
    for (k, t) in [
        (
            "FASTPROOF_TEST_CLICKHOUSE_USER",
            "FASTPROOF_CLICKHOUSE_USER",
        ),
        (
            "FASTPROOF_TEST_CLICKHOUSE_PASSWORD_FILE",
            "FASTPROOF_CLICKHOUSE_PASSWORD_FILE",
        ),
    ] {
        if let Ok(x) = std::env::var(k) {
            v.push((t.into(), x));
        }
    }
    Some(v)
}

/// The daily job on the ClickHouse window (`clickhouse::series_history`): runs still in
/// PostgreSQL get their host and their pushed signals merged in, so the environment and the
/// infrastructure causes are found as on PostgreSQL. Skipped without
/// `FASTPROOF_TEST_CLICKHOUSE_URL`.
#[tokio::test]
async fn flaky_causes_on_the_clickhouse_window() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ch_db = format!("fptest_fl_{:08x}", rand::random::<u32>());
    let Some(vars) = ch_vars(&ch_db) else {
        eprintln!("FASTPROOF_TEST_CLICKHOUSE_URL is not set: skipped");
        return;
    };
    let mut extra: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    extra.extend([
        ("FASTPROOF_ANALYSIS", "on"),
        ("FASTPROOF_ANALYSIS_WORKERS", "0"),
        ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
    ]);
    let s = Server::start_cfg(&db, &extra, paused(ROLE_JOBS)).await;
    let state = s.running.as_ref().unwrap().state.clone();
    assert!(state.clickhouse.is_some());
    s.project("fl").await;
    let viewer = s.token("v", "viewer", &["fl"]).await;

    let mut failing = Vec::new();
    for i in 0..15 {
        let host = ["ci-1", "ci-2", "ci-3"][i % 3];
        let status = if host == "ci-3" && i != 5 {
            "failed"
        } else {
            "passed"
        };
        let (l, step) = run(
            &s,
            &state,
            "fl",
            vec![
                res(&format!("a{i}"), "basket", status, host),
                res(&format!("b{i}"), "steady", "passed", host),
            ],
        )
        .await;
        assert_eq!(step["status"], "done", "{step}");
        if status == "failed" {
            failing.push((l, format!("a{i}")));
        }
    }
    let at = chrono::Utc::now().to_rfc3339();
    let items: Vec<Value> = failing[..2]
        .iter()
        .map(|(l, id)| {
            json!({"launchId": l, "resultClientId": id, "source": "grid", "kind": "node_lost", "at": at})
        })
        .collect();
    let (st, b) = s
        .post(
            "/api/v1/projects/fl/signals",
            ADMIN_TOKEN,
            json!({"items": items}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["matched"], 2, "{b}");

    let ch = state.clickhouse.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let n: i64 = ch
            .exec(
                &format!("SELECT count() FROM {ch_db}.results FINAL WHERE project = 'fl'"),
                &[],
                None,
            )
            .await
            .map(|t| t.trim().parse().unwrap_or(-1))
            .unwrap_or(-1);
        if n == 30 {
            break;
        }
        assert!(Instant::now() < deadline, "results in ClickHouse: {n}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    sqlx::query("delete from flaky_hints")
        .execute(&db.pool)
        .await
        .unwrap();
    fastproof_server::analysis::flaky_hints::daily(&state)
        .await
        .unwrap();
    let (_, list) = s.get("/api/v1/projects/fl/flaky", &viewer).await;
    let series = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "basket")
        .unwrap_or_else(|| panic!("{list}"))["seriesId"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/fl/flaky/{series}/hints"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(h["backend"], "clickhouse", "{h}");
    assert_eq!(
        (&h["windowRuns"], &h["failures"]),
        (&json!(15), &json!(4)),
        "{h}"
    );
    let hint = |kind: &str| {
        h["hints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["kind"] == kind)
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} hint: {h}"))
    };
    let env = hint("environment");
    assert_eq!(
        (&env["evidence"]["dimension"], &env["evidence"]["value"]),
        (&json!("host"), &json!("ci-3")),
        "{h}"
    );
    assert_eq!(
        hint("infra")["evidence"]["signals"]["grid:node_lost"],
        2,
        "{h}"
    );
    assert_eq!(h["infraShare"].as_f64(), Some(0.5), "{h}");
    ch.exec(&format!("DROP DATABASE {ch_db}"), &[], None)
        .await
        .unwrap();
}
