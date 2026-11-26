//! Failure analytics over HTTP (contract `76` §7.9, §8, §13 "C"): the PostgreSQL fallback of
//! every project endpoint with its window, validation, permissions and IDOR checks, the 409 of
//! the ClickHouse-only views; with ClickHouse (`FASTPROOF_TEST_CLICKHOUSE_URL`): the triage
//! mirror of the sink, the backfill and the rollup, the ClickHouse answers equal to the
//! PostgreSQL ones on one dataset within the PostgreSQL window, the reads other areas call,
//! portfolio and cross-project isolation, and launch deletion. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{ADMIN_TOKEN, Server, TestDb, paused};
use fastproof_server::state::AppState;
use fastproof_store::analysis::types::{FailureGroupBy, FailureQuery};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

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

const ENDPOINTS: &[&str] = &[
    "failures",
    "durations",
    "retries",
    "flaky-trend",
    "mttr",
    "slow-steps",
];

fn failed(id: &str, name: &str, msg: &str, retry: u32) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.{name}"), "status": "failed",
        "retry": retry, "start": 1_000, "stop": 1_400,
        "labels": [{"name": "suite", "value": "checkout"}],
        "steps": [{"name": "open cart", "status": "passed", "start": 1_000, "stop": 1_100},
                  {"name": "pay", "status": "failed", "durationMs": 250}],
        "error": {"message": msg, "trace": format!("java.lang.IllegalStateException: {msg}\n\tat com.example.Svc.run(Svc.java:1)")}})
}

fn passed(id: &str, name: &str, retry: u32) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.{name}"), "status": "passed",
        "retry": retry, "start": 2_000, "stop": 2_050,
        "steps": [{"name": "open cart", "status": "passed", "durationMs": 30}]})
}

/// A finished launch of the project with these results; returns its id.
async fn launch(s: &Server, project: &str, branch: &str, items: Vec<Value>) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "nightly", "vcs": {"branch": branch, "commit": "abc123"},
                "environment": {"os": "linux"}}),
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
    l
}

async fn type_id(s: &Server, project: &str, group: &str) -> String {
    let (_, b) = s
        .get(
            &format!("/api/v1/projects/{project}/defect-types"),
            ADMIN_TOKEN,
        )
        .await;
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == group && t["builtIn"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Launches of the project at fixed ages (hours ago) written straight to PostgreSQL: the
/// durations comparison needs runs spread over days. A series runs 100 ms before the last
/// week and 300 ms in it. Returns the result ids.
async fn old_runs(db: &TestDb, project: &str, number_base: i64) -> Vec<Uuid> {
    let pid: Uuid = sqlx::query_scalar("select id from projects where key = $1")
        .bind(project)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let series = Uuid::now_v7();
    let mut ids = Vec::new();
    for i in 0..24i64 {
        let lid = Uuid::now_v7();
        let hours = 1 + i * 12;
        let at: DateTime<Utc> = sqlx::query_scalar(
            "insert into launches (id, project_id, number, name, status, created_at, started_at, finished_at, \
               branch, env_key) values ($1, $2, $3, 'old', 'finished', now() - make_interval(hours => $4::int), \
               now() - make_interval(hours => $4::int), now() - make_interval(hours => $4::int), 'main', 'linux') \
             returning created_at",
        )
        .bind(lid)
        .bind(pid)
        .bind(number_base + i)
        .bind(hours as i32)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let rid = Uuid::now_v7();
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, status, duration_ms) values ($1, $2, $3, $4, $5, $5, 'Slow', 'com.example.Slow', 'passed', $6)",
        )
        .bind(rid)
        .bind(at)
        .bind(pid)
        .bind(lid)
        .bind(series)
        .bind(if hours < 7 * 24 { 300i64 } else { 100 })
        .execute(&db.pool)
        .await
        .unwrap();
        ids.push(rid);
    }
    ids
}

/// The dataset: two launches through the API (failures with two messages, a retried flaky
/// test, steps), a person's triage, and the spread-out runs.
async fn seed(s: &Server, db: &TestDb, project: &str, base: i64) -> (Vec<String>, Vec<Uuid>) {
    let mut launches = Vec::new();
    for b in ["main", "release"] {
        launches.push(
            launch(
                s,
                project,
                b,
                vec![
                    failed("1", "Cart", "total is wrong", 0),
                    failed("2", "Vault", "connection refused by vault", 0),
                    failed("3", "Vault2", "connection refused by vault", 0),
                    failed("4", "Flaky", "timeout after 5s", 0),
                    passed("4b", "Flaky", 1),
                    passed("5", "Ok", 0),
                ],
            )
            .await,
        );
    }
    let pb = type_id(s, project, "productBug").await;
    let results = s.results(project, &launches[0], "").await;
    let cart: Vec<&Value> = results.iter().filter(|r| r["name"] == "Cart").collect();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/triage"),
            ADMIN_TOKEN,
            json!({"resultIds": [cart[0]["id"]], "defectTypeId": pb}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let old = old_runs(db, project, base).await;
    (launches, old)
}

#[tokio::test]
async fn postgres_fallback_validation_and_permissions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
        ],
        paused(ROLE_JOBS),
    )
    .await;
    s.project("an").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["an"]).await;
    let outsider = s.token("o", "admin", &["other"]).await;
    let (launches, _) = seed(&s, &db, "an", 1000).await;
    let olaunch = launch(
        &s,
        "other",
        "main",
        vec![failed("9", "Else", "elsewhere", 0)],
    )
    .await;
    let other_result = s.results("other", &olaunch, "").await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, c) = s
        .get("/api/v1/projects/an/analysis/capabilities", &viewer)
        .await;
    assert_eq!(
        (
            &c["features"]["analytics"],
            &c["features"]["portfolio"],
            &c["features"]["crossProjectSimilar"]
        ),
        (&json!(true), &json!(false), &json!(false)),
        "{c}"
    );

    let (st, f) = s
        .get("/api/v1/projects/an/analytics/failures", &viewer)
        .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["window"]["backend"], "postgres");
    assert_eq!(f["window"]["partial"], false);
    assert_eq!(f["groupBy"], "fingerprint");
    assert_eq!(f["total"], 6, "{f}");
    let items = f["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{f}");
    assert_eq!(items[0]["failures"], 4, "{f}");
    assert_eq!(items[0]["tests"], 2);
    assert_eq!(items[0]["launches"], 2);
    assert_eq!(items[0]["share"], 0.667);
    assert!(
        items[0]["label"]
            .as_str()
            .unwrap()
            .contains("connection refused")
    );
    assert_eq!(items[0]["trend"].as_array().unwrap().len(), 11);
    assert_eq!(
        items[0]["trend"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .sum::<i64>(),
        4
    );
    let (_, f) = s
        .get(
            "/api/v1/projects/an/analytics/failures?groupBy=defectGroup",
            &viewer,
        )
        .await;
    let groups: Vec<(String, i64)> = f["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["key"].as_str().unwrap().into(),
                g["failures"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("toInvestigate".to_string(), 5),
            ("productBug".to_string(), 1)
        ]
    );
    assert_eq!(f["items"][1]["label"], "Product bug");
    let (_, f) = s
        .get(
            "/api/v1/projects/an/analytics/failures?groupBy=env&branch=release",
            &viewer,
        )
        .await;
    assert_eq!(f["total"], 3, "{f}");
    let (_, f) = s
        .get(
            "/api/v1/projects/an/analytics/failures?groupBy=suite",
            &viewer,
        )
        .await;
    assert_eq!(f["total"], 6);

    let (_, r) = s
        .get("/api/v1/projects/an/analytics/retries", &viewer)
        .await;
    assert_eq!(r["totalWastedMs"], 800, "{r}");
    assert_eq!(r["items"][0]["retries"], 2, "{r}");
    assert_eq!(r["items"][0]["tests"], 1);
    let (_, t) = s
        .get("/api/v1/projects/an/analytics/flaky-trend", &viewer)
        .await;
    let pts = t["items"].as_array().unwrap();
    assert_eq!(pts.len(), 31, "{t}");
    assert_eq!(
        pts.iter()
            .map(|p| p["flaky"].as_i64().unwrap())
            .sum::<i64>(),
        2,
        "{t}"
    );
    let (_, st_) = s
        .get("/api/v1/projects/an/analytics/slow-steps", &viewer)
        .await;
    let names: Vec<&str> = st_["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["pay", "open cart"], "{st_}");
    assert_eq!(st_["items"][0]["p95Ms"], 250);
    let (_, d) = s
        .get("/api/v1/projects/an/analytics/durations", &viewer)
        .await;
    assert_eq!(d["window"]["backend"], "postgres");
    assert_eq!(d["window"]["partial"], true, "{d}");
    let di = d["items"].as_array().unwrap();
    assert_eq!(di.len(), 1, "{d}");
    assert_eq!(
        (
            &di[0]["p50RecentMs"],
            &di[0]["p50BeforeMs"],
            &di[0]["ratio"]
        ),
        (&json!(300), &json!(100), &json!(3.0))
    );
    let (st, m) = s.get("/api/v1/projects/an/analytics/mttr", &viewer).await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["resolved"], 0);
    assert_eq!(m["buckets"].as_array().unwrap().len(), 7);

    // The window: 60 days asked, 30 answered.
    let from = (Utc::now() - chrono::Duration::days(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (st, f) = s
        .get(
            &format!("/api/v1/projects/an/analytics/failures?from={from}"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["window"]["partial"], true);
    assert!(
        f["window"]["reason"]
            .as_str()
            .unwrap()
            .contains("ClickHouse")
    );

    // Validation.
    for bad in [
        "groupBy=testCase",
        "limit=0",
        "limit=101",
        "from=2026-11-02T00:00:00Z&to=2026-11-01T00:00:00Z",
        "from=yesterday",
        "unknown=1",
    ] {
        let (st, b) = s
            .get(
                &format!("/api/v1/projects/an/analytics/failures?{bad}"),
                &viewer,
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }
    let long = "b".repeat(201);
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/an/analytics/failures?branch={long}"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Permissions: the same 403 for another project's token and a missing project; IDOR.
    for ep in ENDPOINTS {
        let (st, _) = s
            .call(
                Method::GET,
                &format!("/api/v1/projects/an/analytics/{ep}"),
                None,
                None,
            )
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{ep}");
        let (st, _) = s
            .get(&format!("/api/v1/projects/an/analytics/{ep}"), &outsider)
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{ep}");
        let (st, _) = s
            .get(&format!("/api/v1/projects/nope/analytics/{ep}"), &outsider)
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{ep}");
        let (st, b) = s
            .get(&format!("/api/v1/projects/an/analytics/{ep}"), &viewer)
            .await;
        assert_eq!(st, StatusCode::OK, "{ep}: {b}");
    }

    // ClickHouse-only views.
    let (st, b) = s.get("/api/v1/analytics/portfolio", &viewer).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "clickhouse_required");
    let (st, _) = s
        .call(Method::GET, "/api/v1/analytics/portfolio", None, None)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let mine = s.results("an", &launches[0], "").await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = s
        .get(
            &format!("/api/v1/analytics/similar?project=an&resultId={mine}"),
            &viewer,
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("clickhouse_required"))
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/analytics/similar?project=an&resultId={other_result}"),
            &viewer,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a result of another project is not found"
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/analytics/similar?project=other&resultId={other_result}"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(
            &format!("/api/v1/analytics/similar?project=an&resultId={mine}&limit=51"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
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

async fn ch_count(state: &AppState, sql: &str) -> i64 {
    let ch = state.clickhouse.as_ref().unwrap();
    ch.exec(sql, &[], None)
        .await
        .unwrap()
        .trim()
        .parse()
        .unwrap_or(-1)
}

async fn wait_ch(state: &AppState, what: &str, sql: &str, want: i64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let n = ch_count(state, sql).await;
        if n == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: {n}, want {want}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Pushes rows written straight to PostgreSQL to ClickHouse as the sink would.
async fn push(state: &AppState, ids: &[Uuid]) {
    let ch = state.clickhouse.as_ref().unwrap();
    let b = fastproof_stream::sink::SinkBatch {
        results: state.store.sink_results(ids).await.unwrap(),
        ..Default::default()
    };
    fastproof_stream::sink::Sink::write(ch.as_ref(), &b, &b.token())
        .await
        .unwrap();
}

/// The parts of two answers that must agree (the backend and the window's `from` may not).
fn same(a: &Value, b: &Value, what: &str) {
    let strip = |v: &Value| {
        let mut v = v.clone();
        v.as_object_mut().unwrap().remove("window");
        v
    };
    assert_eq!(strip(a), strip(b), "{what}");
}

#[tokio::test]
async fn clickhouse_answers_equal_postgres_and_keep_projects_apart() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ch_db = format!("fptest_an_{:08x}", rand::random::<u32>());
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
    s.project("an").await;
    s.project("other").await;
    s.project("third").await;
    let reader = s.token("r", "viewer", &["an"]).await;
    let both = s.token("b", "viewer", &["an", "other"]).await;
    let (launches, old) = seed(&s, &db, "an", 1000).await;
    let (_, old2) = seed(&s, &db, "other", 2000).await;
    launch(
        &s,
        "third",
        "main",
        vec![failed("1", "Cart", "total is wrong", 0)],
    )
    .await;
    push(&state, &old).await;
    push(&state, &old2).await;

    // The sink mirrors results (with the defect of the triage) and the person's decision.
    let db_ = &ch_db;
    wait_ch(
        &state,
        "results in ClickHouse",
        &format!("SELECT count() FROM {db_}.results FINAL WHERE project = 'an'"),
        10 + 24,
    )
    .await;
    wait_ch(
        &state,
        "the triage row",
        &format!("SELECT count() FROM {db_}.triage FINAL WHERE project = 'an' AND via = 'api'"),
        1,
    )
    .await;
    wait_ch(
        &state,
        "the result written again with its defect",
        &format!(
            "SELECT count() FROM {db_}.results FINAL WHERE project = 'an' AND defect_group = 'productBug' \
             AND defect_source = 'user'"
        ),
        1,
    )
    .await;
    wait_ch(
        &state,
        "attempts",
        &format!(
            "SELECT count() FROM {db_}.attempts FINAL WHERE project = 'an' AND fingerprint != ''"
        ),
        8,
    )
    .await;

    // The backfill sends decisions again (here: after the history was lost) and finishes.
    state
        .clickhouse
        .as_ref()
        .unwrap()
        .exec(&format!("TRUNCATE TABLE {db_}.triage"), &[], None)
        .await
        .unwrap();
    for _ in 0..20 {
        fastproof_server::analysis::clickhouse::backfill(&state)
            .await
            .unwrap();
        if state
            .store
            .ch_backfill()
            .await
            .unwrap()
            .is_some_and(|b| b.done())
        {
            break;
        }
    }
    let bf = state.store.ch_backfill().await.unwrap().unwrap();
    assert!(bf.done(), "{bf:?}");
    assert!(bf.decisions >= 2 && bf.results >= 4, "{bf:?}");
    assert_eq!(
        ch_count(
            &state,
            &format!("SELECT count() FROM {db_}.triage FINAL WHERE via = 'api'")
        )
        .await,
        2
    );
    fastproof_server::analysis::clickhouse::rollup(&state)
        .await
        .unwrap();
    assert!(
        state
            .store
            .ch_backfill()
            .await
            .unwrap()
            .unwrap()
            .rollup_full
    );

    // Every project endpoint: ClickHouse answers like PostgreSQL within the PostgreSQL window.
    let pid: Uuid = sqlx::query_scalar("select id from projects where key = 'an'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let to = Utc::now() + chrono::Duration::minutes(1);
    let from = to - chrono::Duration::days(30);
    let q = |by: FailureGroupBy| FailureQuery {
        group_by: by,
        from: Some(from),
        to: Some(to),
        branch: None,
        env: None,
        limit: 20,
    };
    let qs = format!(
        "from={}&to={}",
        from.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        to.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    )
    .replace('+', "%2B");
    for (by, name) in [
        (FailureGroupBy::Fingerprint, "fingerprint"),
        (FailureGroupBy::DefectGroup, "defectGroup"),
        (FailureGroupBy::Suite, "suite"),
        (FailureGroupBy::Env, "env"),
    ] {
        let (st, c) = s
            .get(
                &format!("/api/v1/projects/an/analytics/failures?groupBy={name}&{qs}"),
                &reader,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{c}");
        assert_eq!(c["window"]["backend"], "clickhouse");
        let p = serde_json::to_value(
            state
                .store
                .analytics_failures_pg(pid, &q(by))
                .await
                .unwrap(),
        )
        .unwrap();
        same(&c, &p, name);
    }
    let (_, c) = s
        .get(
            &format!("/api/v1/projects/an/analytics/failures?branch=release&{qs}"),
            &reader,
        )
        .await;
    let p = serde_json::to_value(
        state
            .store
            .analytics_failures_pg(
                pid,
                &FailureQuery {
                    branch: Some("release".into()),
                    ..q(FailureGroupBy::Fingerprint)
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    same(&c, &p, "failures on a branch");
    for (ep, pg) in [
        (
            "retries",
            serde_json::to_value(
                state
                    .store
                    .analytics_retries_pg(pid, &q(FailureGroupBy::Fingerprint))
                    .await
                    .unwrap(),
            ),
        ),
        (
            "flaky-trend",
            serde_json::to_value(
                state
                    .store
                    .analytics_flaky_trend_pg(pid, &q(FailureGroupBy::Fingerprint))
                    .await
                    .unwrap(),
            ),
        ),
        (
            "slow-steps",
            serde_json::to_value(
                state
                    .store
                    .analytics_slow_steps_pg(pid, &q(FailureGroupBy::Fingerprint))
                    .await
                    .unwrap(),
            ),
        ),
        (
            "durations",
            serde_json::to_value(
                state
                    .store
                    .analytics_durations_pg(pid, &q(FailureGroupBy::Fingerprint))
                    .await
                    .unwrap(),
            ),
        ),
    ] {
        let (st, c) = s
            .get(&format!("/api/v1/projects/an/analytics/{ep}?{qs}"), &reader)
            .await;
        assert_eq!(st, StatusCode::OK, "{ep}: {c}");
        assert_eq!(c["window"]["backend"], "clickhouse", "{ep}");
        same(&c, &pg.unwrap(), ep);
    }
    let (_, d) = s
        .get(
            &format!("/api/v1/projects/an/analytics/durations?{qs}"),
            &reader,
        )
        .await;
    assert_eq!(d["items"].as_array().unwrap().len(), 1, "{d}");

    // The error timeline (whole days) equals PostgreSQL's from midnight.
    let (_, errs) = s.get("/api/v1/projects/an/errors", &reader).await;
    let fp = errs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["message"]
                .as_str()
                .unwrap_or_default()
                .contains("connection refused")
        })
        .map(|e| e["fingerprint"].as_str().unwrap().to_string());
    let fp = match fp {
        Some(f) => f,
        None => sqlx::query_scalar(
            "select fingerprint from failure_fingerprints where project_id = $1 and message like '%connection refused%'",
        )
        .bind(pid)
        .fetch_one(&db.pool)
        .await
        .unwrap(),
    };
    let midnight = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        - chrono::Duration::days(10);
    let tl_to = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        + chrono::Duration::days(1)
        - chrono::Duration::milliseconds(1);
    let (st, tl) = s
        .get(
            &format!(
                "/api/v1/projects/an/errors/{fp}/timeline?from={}&to={}",
                midnight.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                tl_to.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            ),
            &reader,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{tl}");
    assert_eq!(tl["window"]["backend"], "clickhouse", "{tl}");
    let (_, pg_tl) = state
        .store
        .error_timeline_pg(pid, &fp, midnight, tl_to)
        .await
        .unwrap();
    assert_eq!(tl["items"], serde_json::to_value(&pg_tl).unwrap());
    assert_eq!(
        pg_tl.iter().map(|p| p.occurrences).sum::<i64>(),
        4,
        "{pg_tl:?}"
    );

    // The reads other areas call.
    let project = state.store.project("an").await.unwrap().unwrap();
    let facts = vec![fastproof_store::analysis::types::ClusterFacts {
        fingerprint: sqlx::query_scalar(
            "select fingerprint from failure_fingerprints where project_id = $1 and message like '%total is wrong%'",
        )
        .bind(pid)
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        ..Default::default()
    }];
    let h =
        fastproof_server::analysis::clickhouse::history_candidates(&state, &project, &facts, 20)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(h.len(), 1, "{h:?}");
    assert_eq!(h[0].1.group, "productBug");
    assert_eq!(h[0].1.confirmed, 1);
    let series: Uuid = sqlx::query_scalar(
        "select series_id from results where project_id = $1 and name = 'Slow' limit 1",
    )
    .bind(pid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let runs = fastproof_server::analysis::clickhouse::series_history(&state, &project, series, 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runs.len(), 24);
    assert!(runs[0].created_at < runs[23].created_at);

    // Portfolio: only readable projects, narrowed by `projects`.
    let (st, p) = s.get("/api/v1/analytics/portfolio", &reader).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let keys: Vec<&str> = p["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["an"]);
    let row = &p["items"][0];
    assert_eq!(
        (&row["launches"], &row["failures"]),
        (&json!(26), &json!(6)),
        "{p}"
    );
    assert_eq!(row["flakyTests"], 1);
    let (_, p) = s
        .get(
            "/api/v1/analytics/portfolio?projects=an,other,third",
            &reader,
        )
        .await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1, "{p}");
    let (_, p) = s
        .get("/api/v1/analytics/portfolio?projects=other,third", &reader)
        .await;
    assert!(p["items"].as_array().unwrap().is_empty(), "{p}");
    let (_, p) = s.get("/api/v1/analytics/portfolio", ADMIN_TOKEN).await;
    assert_eq!(p["items"].as_array().unwrap().len(), 3, "{p}");
    let (st, _) = s
        .get(
            &format!(
                "/api/v1/analytics/portfolio?projects={}",
                (0..201)
                    .map(|i| format!("p{i}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            &reader,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Cross-project similar: only other projects the caller can read.
    let vault = s
        .results("an", &launches[0], "")
        .await
        .into_iter()
        .find(|r| r["name"] == "Vault")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, sim) = s
        .get(
            &format!("/api/v1/analytics/similar?project=an&resultId={vault}"),
            &reader,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{sim}");
    assert!(sim["items"].as_array().unwrap().is_empty(), "{sim}");
    let (_, sim) = s
        .get(
            &format!("/api/v1/analytics/similar?project=an&resultId={vault}"),
            &both,
        )
        .await;
    let items = sim["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{sim}");
    assert!(items.iter().all(|i| i["project"] == "other"), "{sim}");
    assert_eq!(items[0]["score"], 1.0);

    // Deleting a launch removes its rows from ClickHouse.
    let gone = &launches[1];
    let (st, b) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/an/launches/{gone}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{b}");
    wait_ch(
        &state,
        "the deleted launch's results",
        &format!("SELECT count() FROM {db_}.results WHERE launch_id = '{gone}'"),
        0,
    )
    .await;
    // The sink deletes table after table: wait for the last ones too.
    for t in ["attempts", "triage"] {
        wait_ch(
            &state,
            "the deleted launch's rows",
            &format!("SELECT count() FROM {db_}.{t} WHERE launch_id = '{gone}'"),
            0,
        )
        .await;
    }
    let (_, f) = s
        .get(
            &format!("/api/v1/projects/an/analytics/failures?{qs}"),
            &reader,
        )
        .await;
    assert_eq!(f["total"], 3, "{f}");
    state
        .clickhouse
        .as_ref()
        .unwrap()
        .exec(&format!("DROP DATABASE {db_}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_backfill_does_not_bring_back_a_deleted_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ch_db = format!("fptest_anb_{:08x}", rand::random::<u32>());
    let Some(vars) = ch_vars(&ch_db) else {
        eprintln!("FASTPROOF_TEST_CLICKHOUSE_URL is not set: skipped");
        return;
    };
    let mut extra: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    extra.extend([
        ("FASTPROOF_ANALYSIS", "on"),
        ("FASTPROOF_ANALYSIS_WORKERS", "0"),
    ]);
    let s = Server::start_cfg(&db, &extra, paused(ROLE_JOBS)).await;
    let state = s.running.as_ref().unwrap().state.clone();
    let ch = state.clickhouse.clone().unwrap();
    s.project("an").await;
    let gone = launch(
        &s,
        "an",
        "main",
        vec![failed("1", "Cart", "total is wrong", 0)],
    )
    .await;
    let kept = launch(
        &s,
        "an",
        "main",
        vec![failed("2", "Pay", "card declined", 0)],
    )
    .await;
    let db_ = &ch_db;
    for l in [&gone, &kept] {
        wait_ch(
            &state,
            "results in ClickHouse",
            &format!("SELECT count() FROM {db_}.results FINAL WHERE launch_id = '{l}'"),
            1,
        )
        .await;
    }
    // The backfill read the launch's rows, then the launch was deleted and the sink removed
    // them from ClickHouse, then the backfill's write landed.
    let ids: Vec<Uuid> = s
        .results("an", &gone, "")
        .await
        .iter()
        .map(|r| r["id"].as_str().unwrap().parse().unwrap())
        .collect();
    let stale = fastproof_stream::sink::SinkBatch {
        results: state.store.sink_results(&ids).await.unwrap(),
        ..Default::default()
    };
    let (st, b) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/an/launches/{gone}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{b}");
    let count = format!("SELECT count() FROM {db_}.results WHERE launch_id = '{gone}'");
    wait_ch(&state, "the sink's delete", &count, 0).await;
    fastproof_stream::sink::Sink::write(ch.as_ref(), &stale, &stale.token())
        .await
        .unwrap();
    assert_eq!(ch_count(&state, &count).await, 1);
    let removed = fastproof_server::analysis::clickhouse::forget_deleted(
        &state,
        &ch,
        vec![
            ("an".into(), gone.parse().unwrap()),
            ("an".into(), kept.parse().unwrap()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(removed, 1);
    assert_eq!(ch_count(&state, &count).await, 0);
    assert_eq!(
        ch_count(
            &state,
            &format!("SELECT count() FROM {db_}.results FINAL WHERE launch_id = '{kept}'")
        )
        .await,
        1
    );
    ch.exec(&format!("DROP DATABASE {db_}"), &[], None)
        .await
        .unwrap();
}
