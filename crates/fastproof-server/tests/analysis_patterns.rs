//! Log patterns as rules (contract §7.6, §13 "P") over HTTP: rules that read logs never run at
//! ingest and are evaluated by the analysis role after the launch (levels, streams,
//! `lastBytes`, live chunks and captured output), people's decisions stay, effects are undoable
//! retro batches, pattern marks feed the result filter, the result detail, the patterns list and
//! the popular-patterns widget; budgets make the step `partial`; the dry run and the retro task
//! on PostgreSQL; the after-launch rule limit; signals; every gated write with the gate closed;
//! late ReportPortal logs inside the settle window. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeSet;
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

fn env<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut v = vec![
        ("FASTPROOF_ANALYSIS", "on"),
        ("FASTPROOF_ANALYSIS_WORKERS", "0"),
        ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
        ("FASTPROOF_ANALYSIS_GATE", "open"),
    ];
    v.extend_from_slice(extra);
    v
}

async fn start(db: &TestDb, extra: &[(&str, &str)]) -> Server {
    Server::start_with_paused(db, &env(extra), ROLE_JOBS).await
}

fn failed(id: &str, name: &str, msg: &str) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.Shop#{name}"), "status": "failed",
        "error": {"message": msg, "trace": format!("java.lang.IllegalStateException: {msg}\n\tat com.example.Shop.{name}(Shop.java:1)")}})
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

async fn post_results(s: &Server, project: &str, lid: &str, items: Vec<Value>) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["rejected"], json!([]), "{out}");
}

/// Log lines of one attempt (client id), `(level, stream, message)` oldest first.
async fn post_logs(
    s: &Server,
    project: &str,
    lid: &str,
    client: &str,
    lines: &[(&str, &str, &str)],
) {
    let body: Vec<Value> = lines
        .iter()
        .enumerate()
        .map(|(i, (level, stream, m))| {
            json!({"resultId": client, "attempt": 0, "seq": i, "ts": 1_795_249_075_967i64 + i as i64,
                "level": level, "stream": stream, "message": m})
        })
        .collect();
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/logs"),
            ADMIN_TOKEN,
            Value::Array(body),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
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

/// Runs the analysis role until it has processed a launch.
async fn analyse(s: &Server) {
    let state = s.running.as_ref().unwrap().state.clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let once = fastproof_server::analysis::run_once(&state).await.unwrap();
        if once.processed > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "no launch was analysed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn steps(s: &Server, project: &str, lid: &str) -> Value {
    let (st, a) = s
        .get(
            &format!("/api/v1/projects/{project}/launches/{lid}/analysis"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    a["steps"]["logRules"].clone()
}

async fn by_name(
    s: &Server,
    project: &str,
    lid: &str,
    q: &str,
) -> std::collections::BTreeMap<String, Value> {
    s.results(project, lid, q)
        .await
        .into_iter()
        .map(|r| (r["name"].as_str().unwrap().to_string(), r))
        .collect()
}

async fn names(s: &Server, project: &str, lid: &str, q: &str) -> BTreeSet<String> {
    by_name(s, project, lid, q).await.into_keys().collect()
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

async fn rule(s: &Server, project: &str, token: &str, body: Value) -> (StatusCode, Value) {
    s.post(&format!("/api/v1/projects/{project}/rules"), token, body)
        .await
}

async fn created_rule(s: &Server, project: &str, body: Value) -> Value {
    let (st, r) = rule(s, project, ADMIN_TOKEN, body).await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    r
}

/// `(via, source, by)` of the journal rows of a result, oldest first.
async fn journal(db: &TestDb, result: &str) -> Vec<(String, String, String, String)> {
    sqlx::query_as(
        "select via, source, by, batch_id::text from triage_decisions where result_id = $1::uuid order by id",
    )
    .bind(result)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

async fn mcp_tool(s: &Server, token: &str, name: &str, args: Value) -> (bool, Value) {
    let r = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let text = r.text().await.unwrap();
    let body: Value = serde_json::from_str(
        text.lines()
            .find_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .unwrap_or(&text),
    )
    .unwrap();
    let out = body["result"]["content"][0]["text"].as_str().unwrap_or("");
    (
        body["result"]["isError"].as_bool().unwrap_or(false),
        serde_json::from_str(out).unwrap_or(Value::String(out.into())),
    )
}

#[tokio::test]
async fn log_rules_run_after_the_launch_and_mark_patterns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("lp").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["lp"]).await;
    let outsider = s.token("o", "viewer", &["other"]).await;
    let sys = type_id(&s, "lp", "systemIssue").await;
    let pb = type_id(&s, "lp", "productBug").await;

    let oom = created_rule(
        &s,
        "lp",
        json!({"name": "Out of memory", "match": {"log": {"contains": "outofmemoryerror", "levels": ["error"]}},
            "action": {"defectTypeId": sys, "comment": "heap", "pattern": "oom"}}),
    )
    .await;
    assert_eq!(oom["phase"], "afterLaunch");
    let net = created_rule(
        &s,
        "lp",
        json!({"name": "Network", "match": {"log": {"regex": "connection (refused|reset)", "streams": ["stderr"]}},
            "action": {"pattern": "net"}}),
    )
    .await;
    created_rule(
        &s,
        "lp",
        json!({"name": "Early marker", "match": {"log": {"contains": "EARLY-MARKER", "lastBytes": 1024}},
            "action": {"pattern": "early"}}),
    )
    .await;
    let ingest_rule = created_rule(
        &s,
        "lp",
        json!({"name": "Plain", "match": {"messageRegex": "never matches anything"}, "action": {"comment": "x"}}),
    )
    .await;
    assert_eq!(ingest_rule["phase"], "ingest");
    // A pattern name and the log condition are validated.
    for (bad, what) in [
        (
            json!({"log": {"contains": "x"}}),
            json!({"pattern": "Bad Name"}),
        ),
        (
            json!({"log": {"contains": "x", "regex": "y"}}),
            json!({"pattern": "ok"}),
        ),
        (json!({"log": {"regex": "(a"}}), json!({"pattern": "ok"})),
        (
            json!({"log": {"contains": "x", "lastBytes": 10}}),
            json!({"pattern": "ok"}),
        ),
        (
            json!({"log": {"contains": "x", "levels": ["error", "error"]}}),
            json!({"pattern": "ok"}),
        ),
        (json!({"signals": []}), json!({"pattern": "ok"})),
    ] {
        let (st, b) = rule(
            &s,
            "lp",
            ADMIN_TOKEN,
            json!({"name": "bad", "match": bad, "action": what}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    }
    let (st, _) = rule(
        &s,
        "lp",
        &viewer,
        json!({"name": "v", "match": {"log": {"contains": "x"}}, "action": {"pattern": "v"}}),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let l = new_launch(&s, "lp").await;
    let mut netr = failed("net-1", "Net", "checkout failed");
    netr["stderr"] = json!("starting\nERROR connection refused by 198.51.100.7:5432\nbye\n");
    post_results(
        &s,
        "lp",
        &l,
        vec![
            failed("oom-1", "Oom", "boom"),
            netr,
            failed("early-1", "Early", "early"),
            failed("human-1", "Human", "boom"),
            json!({"id": "pass-1", "name": "Pass", "fullName": "com.example.Shop#Pass", "status": "passed"}),
        ],
    )
    .await;
    post_logs(
        &s,
        "lp",
        &l,
        "oom-1",
        &[
            ("info", "stdout", "starting the checkout"),
            (
                "warn",
                "stdout",
                "java.lang.OutOfMemoryError (only a warning)",
            ),
            (
                "error",
                "log",
                "\u{1b}[31mjava.lang.OutOfMemoryError: Java heap space\u{1b}[0m",
            ),
        ],
    )
    .await;
    let filler = "x".repeat(600);
    post_logs(
        &s,
        "lp",
        &l,
        "early-1",
        &[
            ("error", "stdout", "EARLY-MARKER seen"),
            ("info", "stdout", filler.as_str()),
            ("info", "stdout", filler.as_str()),
        ],
    )
    .await;
    post_logs(
        &s,
        "lp",
        &l,
        "human-1",
        &[("error", "stdout", "OutOfMemoryError again")],
    )
    .await;
    post_logs(
        &s,
        "lp",
        &l,
        "pass-1",
        &[("error", "stdout", "OutOfMemoryError in a pass")],
    )
    .await;

    // Nothing of the log rules happened at ingest.
    let before = by_name(&s, "lp", &l, "").await;
    assert!(
        before["Oom"]["defect"]["ruleId"].is_null(),
        "{}",
        before["Oom"]
    );
    assert_eq!(before["Oom"]["defect"]["group"], "toInvestigate");
    let marks: i64 = sqlx::query_scalar("select count(*) from result_patterns")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(marks, 0);
    let human = before["Human"]["id"].as_str().unwrap().to_string();
    let (st, out) = s
        .post(
            "/api/v1/projects/lp/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [human], "defectTypeId": pb}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");

    finish(&s, "lp", &l).await;
    analyse(&s).await;
    let step = steps(&s, "lp", &l).await;
    assert_eq!(step["status"], "done", "{step}");
    assert_eq!(step["detail"]["rules"], 3, "{step}");
    assert_eq!(step["detail"]["marked"], 3, "{step}");
    assert_eq!(step["detail"]["updated"], 1, "{step}");

    let after = by_name(&s, "lp", &l, "").await;
    assert_eq!(
        after["Oom"]["defect"]["typeId"],
        sys.as_str(),
        "{}",
        after["Oom"]
    );
    assert_eq!(after["Oom"]["defect"]["ruleId"], oom["id"]);
    assert_eq!(
        after["Human"]["defect"]["typeId"],
        pb.as_str(),
        "people's decisions stay"
    );
    assert_eq!(after["Human"]["defect"]["source"], "user");
    assert!(after["Pass"]["defect"]["ruleId"].is_null());
    let oom_id = after["Oom"]["id"].as_str().unwrap().to_string();
    let rows = journal(&db, &oom_id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str(), rows[0].2.clone()),
        (
            "retro",
            "rule",
            format!("rule:{}", oom["id"].as_str().unwrap())
        )
    );

    // The marks.
    assert_eq!(
        names(&s, "lp", &l, "&pattern=oom").await,
        BTreeSet::from(["Oom".to_string(), "Human".to_string()])
    );
    assert_eq!(
        names(&s, "lp", &l, "&pattern=net").await,
        BTreeSet::from(["Net".to_string()])
    );
    assert!(
        names(&s, "lp", &l, "&pattern=early").await.is_empty(),
        "older than lastBytes"
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/lp/launches/{l}/results?pattern=Not%20A%20Name"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, d) = s
        .get(&format!("/api/v1/projects/lp/results/{oom_id}"), &viewer)
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    let p = &d["patterns"];
    assert_eq!(p.as_array().unwrap().len(), 1, "{p}");
    assert_eq!(p[0]["pattern"], "oom");
    assert_eq!(p[0]["ruleId"], oom["id"]);
    assert_eq!(p[0]["attempt"], 0);
    assert_eq!(
        p[0]["excerpt"], "java.lang.OutOfMemoryError: Java heap space",
        "masked and cleaned: {p}"
    );
    let net_id = after["Net"]["id"].as_str().unwrap();
    let (_, d) = s
        .get(
            &format!("/api/v1/projects/lp/results/{net_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(d["patterns"][0]["ruleId"], net["id"]);
    assert!(
        d["patterns"][0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("connection refused")
    );

    // The patterns list and the widget.
    let (st, list) = s
        .get("/api/v1/projects/lp/analysis/patterns?days=7", &viewer)
        .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{list}");
    assert_eq!(
        (
            items[0]["pattern"].as_str(),
            items[0]["results"].as_i64(),
            items[0]["launches"].as_i64()
        ),
        (Some("oom"), Some(2), Some(1))
    );
    assert_eq!(
        (items[1]["pattern"].as_str(), items[1]["results"].as_i64()),
        (Some("net"), Some(1))
    );
    for q in ["days=0", "days=366", "limit=0", "limit=101"] {
        let (st, _) = s
            .get(
                &format!("/api/v1/projects/lp/analysis/patterns?{q}"),
                &viewer,
            )
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{q}");
    }
    let (st, _) = s
        .get("/api/v1/projects/lp/analysis/patterns", &outsider)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get("/api/v1/projects/missing/analysis/patterns", &outsider)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/other/results/{oom_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a result of another project");
    let (st, b) = s
        .post(
            "/api/v1/projects/lp/dashboards",
            ADMIN_TOKEN,
            json!({"name": "Patterns", "widgets": [
                {"id": "logs", "type": "popular-patterns", "settings": {"patterns": "logPatterns"}, "x": 0, "y": 0, "w": 6, "h": 4},
                {"id": "fps", "type": "popular-patterns", "settings": {"patterns": "fingerprints"}, "x": 6, "y": 0, "w": 6, "h": 4}]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let board = b["id"].as_str().unwrap().to_string();
    let (st, data) = s
        .get(
            &format!("/api/v1/projects/lp/dashboards/{board}/data"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{data}");
    let w = &data["widgets"]["logs"];
    assert_eq!(w["data"]["kind"], "logPatterns", "{w}");
    assert_eq!(w["data"]["items"][0]["pattern"], "oom");
    assert_eq!(data["widgets"]["fps"]["data"]["kind"], "patterns");
    let (st, b) = s
        .post(
            "/api/v1/projects/lp/dashboards",
            ADMIN_TOKEN,
            json!({"name": "Bad", "widgets": [{"id": "x", "type": "launches-table", "settings": {"patterns": "logPatterns"}, "x": 0, "y": 0, "w": 6, "h": 4}]}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");

    // A recompute replaces the marks of the launch: a rule that no longer matches loses them.
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/lp/rules/{}", net["id"].as_str().unwrap()),
            Some(ADMIN_TOKEN),
            Some(json!({"match": {"log": {"regex": "connection (refused|reset)", "streams": ["stdout"]}}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let state = s.running.as_ref().unwrap().state.clone();
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'lp'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        state
            .store
            .enqueue_analysis_launch(
                pid,
                l.parse().unwrap(),
                fastproof_store::analysis::types::QueueReason::Recompute,
                Duration::ZERO
            )
            .await
            .unwrap()
    );
    analyse(&s).await;
    assert!(names(&s, "lp", &l, "&pattern=net").await.is_empty());
    assert_eq!(names(&s, "lp", &l, "&pattern=oom").await.len(), 2);
    assert_eq!(
        journal(&db, &oom_id).await.len(),
        1,
        "an unchanged defect writes no row"
    );

    // The retro batch is undone like any other.
    let batch = journal(&db, &oom_id).await[0].3.clone();
    let (st, u) = s
        .post(
            "/api/v1/projects/lp/triage/undo",
            ADMIN_TOKEN,
            json!({"batchId": batch}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    assert_eq!(u["restored"], 1, "{u}");
    let back = by_name(&s, "lp", &l, "").await;
    assert!(back["Oom"]["defect"]["ruleId"].is_null(), "{}", back["Oom"]);
}

#[tokio::test]
async fn budgets_limits_and_linear_patterns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(
        &db,
        &[
            ("FASTPROOF_ANALYSIS_LOG_ATTEMPTS", "10"),
            ("FASTPROOF_ANALYSIS_MAX_LOG_RULES", "2"),
        ],
    )
    .await;
    s.project("bl").await;
    created_rule(
        &s,
        "bl",
        json!({"name": "a", "match": {"log": {"regex": "(a+)+$"}}, "action": {"pattern": "redos"}}),
    )
    .await;
    let second = created_rule(
        &s,
        "bl",
        json!({"name": "b", "match": {"log": {"contains": "needle"}}, "action": {"pattern": "needle"}}),
    )
    .await;
    let (st, b) = rule(
        &s,
        "bl",
        ADMIN_TOKEN,
        json!({"name": "c", "match": {"log": {"contains": "c"}}, "action": {"pattern": "c"}}),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "limit_reached");
    assert_eq!(b["details"], json!({"limit": "maxLogRules", "max": 2}));
    let off = created_rule(
        &s,
        "bl",
        json!({"name": "c", "enabled": false, "match": {"log": {"contains": "c"}}, "action": {"pattern": "c"}}),
    )
    .await;
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/bl/rules/{}", off["id"].as_str().unwrap()),
            Some(ADMIN_TOKEN),
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("limit_reached")),
        "{b}"
    );
    created_rule(
        &s,
        "bl",
        json!({"name": "ingest", "match": {"messageRegex": "x"}, "action": {"comment": "ingest rules do not count"}}),
    )
    .await;
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!(
                "/api/v1/projects/bl/rules/{}",
                second["id"].as_str().unwrap()
            ),
            Some(ADMIN_TOKEN),
            Some(json!({"name": "b2"})),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "patching an enabled rule keeps its place: {b}"
    );

    // Twelve failed attempts with long lines shaped to make backtracking engines explode.
    let l = new_launch(&s, "bl").await;
    let items: Vec<Value> = (0..12)
        .map(|i| failed(&format!("t-{i}"), &format!("T{i:02}"), "boom"))
        .collect();
    post_results(&s, "bl", &l, items).await;
    let long = format!("{}!", "a".repeat(60_000));
    for i in 0..12 {
        let lines: Vec<(&str, &str, &str)> =
            (0..8).map(|_| ("error", "stdout", long.as_str())).collect();
        post_logs(&s, "bl", &l, &format!("t-{i}"), &lines).await;
    }
    finish(&s, "bl", &l).await;
    let started = Instant::now();
    analyse(&s).await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    let step = steps(&s, "bl", &l).await;
    assert_eq!(step["status"], "partial", "{step}");
    assert_eq!(step["detail"]["reason"], "attempts", "{step}");
    assert_eq!(step["detail"]["attempts"], 10, "{step}");

    // The CPU budget: nothing left makes the step partial with `log_cpu`.
    let state = s.running.as_ref().unwrap().state.clone();
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'bl'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let launch = state
        .store
        .launch(pid, l.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let settings = state.store.project_settings(pid).await.unwrap();
    let limits = state.cfg.analysis.limits.clone();
    let rules = {
        let mut conn = state.store.pool().acquire().await.unwrap();
        fastproof_store::analysis::rules::enabled_rules(
            &mut conn,
            pid,
            fastproof_store::analysis::rules::RulePhase::AfterLaunch,
        )
        .await
        .unwrap()
    };
    assert_eq!(rules.len(), 2);
    let ctx = fastproof_store::analysis::types::LaunchCtx {
        project_id: pid,
        project_key: "bl",
        launch: &launch,
        settings: &settings,
        limits: &limits,
        now: chrono::Utc::now(),
    };
    let o = state
        .store
        .evaluate_log_rules(&ctx, &rules, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        o.status,
        fastproof_store::analysis::types::AnalysisStepStatus::Partial
    );
    assert_eq!(o.detail["reason"], "log_cpu");
}

#[tokio::test]
async fn dry_run_retro_task_and_signals() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[]).await;
    s.project("rt").await;
    s.project("zz").await;
    let ab = type_id(&s, "rt", "automationBug").await;
    let l = new_launch(&s, "rt").await;
    post_results(
        &s,
        "rt",
        &l,
        vec![
            failed("w-1", "Wait", "timeout"),
            failed("g-1", "Grid", "lost"),
            failed("q-1", "Quiet", "ok"),
        ],
    )
    .await;
    post_logs(
        &s,
        "rt",
        &l,
        "w-1",
        &[(
            "error",
            "stdout",
            "Timeout waiting for #pay-button after 30s",
        )],
    )
    .await;
    post_logs(&s, "rt", &l, "q-1", &[("info", "stdout", "all quiet")]).await;
    finish(&s, "rt", &l).await;
    analyse(&s).await;
    let step = steps(&s, "rt", &l).await;
    assert_eq!(
        (step["status"].as_str(), step["detail"]["reason"].as_str()),
        (Some("skipped"), Some("no log rules")),
        "{step}"
    );

    let draft = json!({"name": "Waits", "match": {"log": {"regex": "Timeout waiting for #[a-z-]+"}},
        "action": {"defectTypeId": ab, "pattern": "ui-wait"}});
    let (st, d) = s
        .post(
            "/api/v1/projects/rt/rules/test",
            ADMIN_TOKEN,
            json!({"rule": draft, "launchId": l}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    let results = by_name(&s, "rt", &l, "").await;
    let wait = results["Wait"]["id"].as_str().unwrap().to_string();
    assert_eq!(d["matches"], json!([wait]), "{d}");
    assert_eq!(d["evaluated"], 3);
    assert_eq!(d["truncated"], false);
    let marks: i64 = sqlx::query_scalar("select count(*) from result_patterns")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(marks, 0, "a dry run writes nothing");
    let (st, _) = s
        .post(
            "/api/v1/projects/zz/rules/test",
            ADMIN_TOKEN,
            json!({"rule": draft, "launchId": l}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch of another project");
    // Result-only rules keep their dry run.
    let (st, d) = s
        .post("/api/v1/projects/rt/rules/test", ADMIN_TOKEN,
            json!({"rule": {"name": "m", "match": {"messageRegex": "lost"}, "action": {"comment": "c"}}, "launchId": l}))
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["matches"].as_array().unwrap().len(), 1);

    // The retro task on PostgreSQL, as the person who queued it.
    let editor = s.token("ed", "editor", &["rt"]).await;
    let r = created_rule(&s, "rt", draft.clone()).await;
    let rid = r["id"].as_str().unwrap().to_string();
    let (st, t) = s
        .post(
            &format!("/api/v1/projects/rt/rules/{rid}/apply?launchId={l}"),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["kind"], "log_rule.apply");
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/zz/rules/{rid}/apply?launchId={l}"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a rule of another project");
    let state = s.running.as_ref().unwrap().state.clone();
    assert_eq!(
        fastproof_server::analysis::tasks::run(&state, Duration::from_secs(60))
            .await
            .unwrap(),
        1
    );
    let (_, task) = s
        .get(
            &format!("/api/v1/projects/rt/tasks/{}", t["id"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(task["status"], "done", "{task}");
    assert_eq!(task["progress"]["backend"], "postgres", "{task}");
    assert_eq!(task["progress"]["updated"], 1, "{task}");
    assert_eq!(task["progress"]["marked"], 1, "{task}");
    let rows = journal(&db, &wait).await;
    assert_eq!(
        rows.iter()
            .map(|r| (r.0.as_str(), r.1.as_str(), r.2.as_str()))
            .collect::<Vec<_>>(),
        vec![("retro", "rule", "ed")],
        "{rows:?}"
    );
    assert_eq!(
        names(&s, "rt", &l, "&pattern=ui-wait").await,
        BTreeSet::from(["Wait".to_string()])
    );

    // `match.signals`: a stored signal of the source and kind on the result.
    let grid = results["Grid"]["id"].as_str().unwrap().to_string();
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'rt'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into attempt_signals (project_id, launch_id, result_id, source, kind, at, confidence, dedup_key) \
         values ($1, $2::uuid, $3::uuid, 'grid', 'node-lost', now(), 'exact', 'n1'), \
                ($1, $2::uuid, $4::uuid, 'k8s', 'node-lost', now(), 'exact', 'n2')",
    )
    .bind(pid)
    .bind(&l)
    .bind(&grid)
    .bind(&wait)
    .execute(&db.pool)
    .await
    .unwrap();
    let sig = created_rule(
        &s,
        "rt",
        json!({"name": "Grid node", "match": {"signals": [{"source": "grid", "kind": "node-lost"}]}, "action": {"pattern": "grid-node"}}),
    )
    .await;
    assert_eq!(sig["phase"], "afterLaunch");
    let (st, _) = s
        .post(
            &format!(
                "/api/v1/projects/rt/rules/{}/apply?launchId={l}",
                sig["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(
        fastproof_server::analysis::tasks::run(&state, Duration::from_secs(60))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        names(&s, "rt", &l, "&pattern=grid-node").await,
        BTreeSet::from(["Grid".to_string()])
    );
    let (_, d) = s
        .get(&format!("/api/v1/projects/rt/results/{grid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        d["patterns"][0]["excerpt"], "lost",
        "the message's first line without a log condition"
    );

    // Lines another backend found (the ClickHouse retro of night B) are resolved here: only
    // results of the project's launches, with the rule's result conditions checked.
    let other_launch = new_launch(&s, "zz").await;
    let quiet = created_rule(
        &s,
        "rt",
        json!({"name": "Quiet", "match": {"log": {"contains": "quiet"}, "messageRegex": "^ok$"}, "action": {"pattern": "quiet"}}),
    )
    .await;
    let rule: fastproof_store::analysis::rules::Rule = serde_json::from_value(quiet).unwrap();
    let compiled =
        std::sync::Arc::new(fastproof_store::analysis::rules::CompiledRule::compile(rule).unwrap());
    let m = |launch: &str, key: &str| fastproof_store::analysis::types::LogMatch {
        launch_id: launch.parse().unwrap(),
        result_key: key.into(),
        attempt: 0,
        seq: 0,
        excerpt: "\u{1b}[2mall quiet\u{1b}[0m".into(),
    };
    let out = state
        .store
        .apply_log_matches(
            pid,
            &compiled,
            &[
                m(&l, "q-1"),
                m(&l, "w-1"),
                m(&other_launch, "q-1"),
                m(&l, "nope"),
            ],
            "ed",
        )
        .await
        .unwrap();
    assert_eq!((out.matched, out.writes.marked), (1, 1), "{out:?}");
    let quiet_id = results["Quiet"]["id"].as_str().unwrap();
    let (_, d) = s
        .get(
            &format!("/api/v1/projects/rt/results/{quiet_id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(d["patterns"][0]["excerpt"], "all quiet", "{d}");
}

#[tokio::test]
async fn gated_writes_wait_for_the_analysis_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[("FASTPROOF_ANALYSIS_GATE", "closed")]).await;
    s.project("gt").await;
    let closed = |b: &Value, what: &str| {
        assert_eq!(b["code"], "upgrade_in_progress", "{b}");
        assert_eq!(
            b["details"],
            json!({"what": what, "requiredLevel": 1, "area": "analysis"}),
            "{b}"
        );
    };
    let (st, b) = rule(
        &s,
        "gt",
        ADMIN_TOKEN,
        json!({"name": "l", "match": {"log": {"contains": "x"}}, "action": {"comment": "c"}}),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    closed(&b, "rules.match.log");
    let (st, b) = rule(
        &s,
        "gt",
        ADMIN_TOKEN,
        json!({"name": "s", "match": {"signals": [{"source": "ci"}]}, "action": {"comment": "c"}}),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    closed(&b, "rules.match.signals");
    let plain = created_rule(
        &s,
        "gt",
        json!({"name": "p", "match": {"messageRegex": "x"}, "action": {"comment": "c"}}),
    )
    .await;
    let (st, b) = s
        .call(
            Method::PATCH,
            &format!(
                "/api/v1/projects/gt/rules/{}",
                plain["id"].as_str().unwrap()
            ),
            Some(ADMIN_TOKEN),
            Some(json!({"action": {"comment": "c", "pattern": "p"}})),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    closed(&b, "rules.action.pattern");
    let (err, out) = mcp_tool(
        &s,
        ADMIN_TOKEN,
        "create_rule_draft",
        json!({"project": "gt", "name": "d", "match": {"log": {"regex": "x+"}}, "action": {"comment": "c"}}),
    )
    .await;
    assert!(
        err && out.to_string().contains("upgrade_in_progress"),
        "{out}"
    );
    let (err, out) = mcp_tool(
        &s,
        ADMIN_TOKEN,
        "create_rule_draft",
        json!({"project": "gt", "name": "d", "match": {"messageRegex": "x+"}, "action": {"comment": "c"}}),
    )
    .await;
    assert!(!err, "{out}");
    let widget = |patterns: Option<&str>| {
        let mut w = json!({"id": "p", "type": "popular-patterns", "x": 0, "y": 0, "w": 6, "h": 4});
        if let Some(p) = patterns {
            w["settings"] = json!({"patterns": p});
        }
        json!({"name": "B", "widgets": [w]})
    };
    let (st, b) = s
        .post(
            "/api/v1/projects/gt/dashboards",
            ADMIN_TOKEN,
            widget(Some("logPatterns")),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    closed(&b, "dashboards.patterns");
    let (st, b) = s
        .post("/api/v1/projects/gt/dashboards", ADMIN_TOKEN, widget(None))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let r = s
        .req(
            Method::PUT,
            &format!(
                "/api/v1/projects/gt/dashboards/{}",
                b["id"].as_str().unwrap()
            ),
            Some(ADMIN_TOKEN),
        )
        .header("if-match", format!("\"r{}\"", b["revision"]))
        .json(&widget(Some("fingerprints")))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    closed(&r.json::<Value>().await.unwrap(), "dashboards.patterns");
    // A log rule stored while the gate was open is applied only through the gate.
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'gt'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let rid: uuid::Uuid = sqlx::query_scalar(
        "insert into rules (id, project_id, name, enabled, ord, match, action, created_by) \
         values (gen_random_uuid(), $1, 'stored', true, 9, '{\"log\": {\"contains\": \"x\"}}', '{\"pattern\": \"x\"}', 'admin') returning id",
    )
    .bind(pid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let l = new_launch(&s, "gt").await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/gt/rules/{rid}/apply?launchId={l}"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    closed(&b, "tasks.log_rule.apply");
}

#[tokio::test]
async fn applying_log_rules_needs_the_analysis_role() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(
        &db,
        &[("FASTPROOF_ANALYSIS_GATE", "open")],
        &["analysis.tasks"],
    )
    .await;
    s.project("off").await;
    let r = created_rule(
        &s,
        "off",
        json!({"name": "l", "match": {"log": {"contains": "x"}}, "action": {"pattern": "x"}}),
    )
    .await;
    let (st, b) = s
        .post(
            &format!(
                "/api/v1/projects/off/rules/{}/apply",
                r["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("analysis_disabled")),
        "{b}"
    );
}

const RP_FAST: &[(&str, &str)] = &[
    ("FASTPROOF_RP_SETTLE_MS", "200"),
    ("FASTPROOF_RP_ERROR_WAIT_MS", "300"),
    ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "1"),
];

#[tokio::test]
async fn late_reportportal_logs_inside_the_settle_window_are_read() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut extra = env(&[("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "8")]);
    extra.retain(|(k, v)| !(*k == "FASTPROOF_ANALYSIS_SETTLE_SECONDS" && *v == "0"));
    extra.extend_from_slice(RP_FAST);
    let s = Server::start_with_paused(&db, &extra, ROLE_JOBS).await;
    let state = s.running.as_ref().unwrap().state.clone();
    // The first run creates the role's cursor (and would queue recent launches).
    assert_eq!(
        fastproof_server::analysis::run_once(&state)
            .await
            .unwrap()
            .processed,
        0
    );
    s.project("rpl").await;
    created_rule(
        &s,
        "rpl",
        json!({"name": "late oom", "match": {"log": {"contains": "OutOfMemoryError"}}, "action": {"pattern": "oom"}}),
    )
    .await;
    let token = s.token("agent", "uploader", &["rpl"]).await;
    let (st, l) = s
        .call(
            Method::POST,
            "/rp/api/v2/rpl/launch",
            Some(&token),
            Some(json!({"name": "pytest"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let lu = l["id"].as_str().unwrap().to_string();
    let (st, it) = s
        .call(Method::POST, "/rp/api/v2/rpl/item", Some(&token),
            Some(json!({"name": "test_pay", "type": "STEP", "launchUuid": lu, "codeRef": "t.py:test_pay"})))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{it}");
    let item = it["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/rp/api/v2/rpl/item/{item}"),
            Some(&token),
            Some(json!({"status": "FAILED", "launchUuid": lu})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/rp/api/v2/rpl/launch/{lu}/finish"),
            Some(&token),
            Some(json!({})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let fp: String = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let id: Option<(uuid::Uuid, String)> = sqlx::query_as(
                "select l.id, l.status from launches l join projects p on p.id = l.project_id where p.key = 'rpl'",
            )
            .fetch_optional(&db.pool)
            .await
            .unwrap();
            if let Some((id, status)) = id
                && status != "running"
            {
                break id.to_string();
            }
            assert!(Instant::now() < deadline, "the RP launch did not finish");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    // Inside the settle window nothing is analysed yet, and the agent's batched lines arrive.
    let first = fastproof_server::analysis::run_once(&state).await.unwrap();
    assert_eq!(first.processed, 0, "analysed inside the settle window");
    let boundary = "fp-boundary-7d1c";
    let entries = json!([{"itemUuid": item, "launchUuid": lu, "level": "ERROR", "time": "2026-11-25T10:00:00Z",
        "message": "E   java.lang.OutOfMemoryError: Java heap space"}]);
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"json_request_part\"\r\nContent-Type: application/json\r\n\r\n{entries}\r\n--{boundary}--\r\n"
    );
    let r = s
        .req(Method::POST, "/rp/api/v2/rpl/log", Some(&token))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    analyse(&s).await;
    let step = steps(&s, "rpl", &fp).await;
    assert_eq!(step["status"], "done", "{step}");
    assert_eq!(
        names(&s, "rpl", &fp, "&pattern=oom").await,
        BTreeSet::from(["test_pay".to_string()]),
        "{step}"
    );
}

/// Contract §14: log rules over 300 failed attempts × 256 KiB within 2 s of matching time.
/// Run on a release build: `cargo test --release -p fastproof-server --test analysis_patterns
/// -- --ignored log_rules_target`.
#[tokio::test]
#[ignore]
async fn log_rules_target_300_attempts_of_256_kib() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = start(&db, &[("FASTPROOF_ANALYSIS_LOG_CPU_MS", "60000")]).await;
    s.project("perf").await;
    for (i, body) in [
        json!({"log": {"regex": r"java\.lang\.OutOfMemoryError: .*heap", "levels": ["error"]}}),
        json!({"log": {"contains": "connection refused"}}),
        json!({"log": {"regex": r"Timeout waiting for #[a-z-]+ after \d+s"}}),
        json!({"log": {"regex": r"(?i)deadlock detected"}}),
        json!({"log": {"contains": "SIGSEGV", "streams": ["stderr"]}}),
    ]
    .into_iter()
    .enumerate()
    {
        created_rule(
            &s,
            "perf",
            json!({"name": format!("r{i}"), "match": body, "action": {"pattern": format!("p{i}")}}),
        )
        .await;
    }
    let l = new_launch(&s, "perf").await;
    let items: Vec<Value> = (0..300)
        .map(|i| failed(&format!("a-{i}"), &format!("T{i:03}"), "boom"))
        .collect();
    post_results(&s, "perf", &l, items).await;
    let line = "INFO 2026-11-25T10:00:00Z worker-3 processed order 12345 for customer 67890 in 42 ms; cart=[sku-1,sku-2]";
    for i in 0..300 {
        let mut lines: Vec<(&str, &str, String)> = (0..2400)
            .map(|_| ("info", "stdout", line.to_string()))
            .collect();
        if i % 10 == 0 {
            lines.push((
                "error",
                "stdout",
                "java.lang.OutOfMemoryError: Java heap space".into(),
            ));
        }
        let refs: Vec<(&str, &str, &str)> =
            lines.iter().map(|(a, b, c)| (*a, *b, c.as_str())).collect();
        post_logs(&s, "perf", &l, &format!("a-{i}"), &refs).await;
    }
    finish(&s, "perf", &l).await;
    let started = Instant::now();
    analyse(&s).await;
    let step = steps(&s, "perf", &l).await;
    eprintln!("log rules: wall {:?}, step {step}", started.elapsed());
    assert_eq!(step["status"], "done", "{step}");
    assert_eq!(step["detail"]["attempts"], 300);
    assert_eq!(step["detail"]["marked"], 30);
    assert!(step["detail"]["cpuMs"].as_u64().unwrap() < 2000, "{step}");
}
