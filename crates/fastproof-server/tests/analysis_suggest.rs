//! FastProof Analysis suggestions over HTTP: the `suggestions` step after a finished launch,
//! the launch view, recompute, accept, reject, the rule draft, the memory, the backtest task
//! and its report, the decision-event export, with their permissions, cross-project ids, the
//! analysis gate and Analysis off; then the ReportPortal fixture backtest (synthetic history
//! imported through `fastproof rp import`) and the offline CLI on its export. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;
#[path = "common/rp_fake.rs"]
mod rp_fake;

use std::collections::HashMap;
use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_analysis::synth::{SynthProfile, generate};
use fastproof_server::cli::analysis::{backtest_events, read_events_file};
use fastproof_server::state::AppState;
use fastproof_store::analysis::types::{BacktestReport, BacktestSource};
use reqwest::StatusCode;
use rp_fake::{Fake, RP_KEY, start_fake};
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
];

const ON: &[(&str, &str)] = &[
    ("FASTPROOF_ANALYSIS", "on"),
    ("FASTPROOF_ANALYSIS_WORKERS", "0"),
    ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
    ("FASTPROOF_ANALYSIS_GATE", "open"),
];

fn res(id: &str, name: &str, msg: &str) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"), "status": "failed",
        "error": {"message": msg, "trace": format!("java.lang.IllegalStateException: {msg}\n\tat com.example.{name}.run({name}.java:1)")}})
}

async fn finished_launch(s: &Server, project: &str, items: Vec<Value>) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "run", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let lid = b["id"].as_str().unwrap().to_string();
    let n = items.len();
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while s.results(project, &lid, "").await.len() < n {
        assert!(std::time::Instant::now() < deadline, "results not stored");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success());
    lid
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

/// Runs the analysis role until the launch is analysed.
async fn analysed(state: &AppState, s: &Server, project: &str, lid: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        fastproof_server::analysis::run_once(state).await.unwrap();
        let (_, a) = s
            .get(
                &format!("/api/v1/projects/{project}/launches/{lid}/analysis"),
                ADMIN_TOKEN,
            )
            .await;
        if a["state"] == "done" {
            return a;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the launch was not analysed: {a}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn suggestions(s: &Server, project: &str, lid: &str, token: &str) -> (StatusCode, Value) {
    s.get(
        &format!("/api/v1/projects/{project}/launches/{lid}/suggestions"),
        token,
    )
    .await
}

fn cluster<'a>(v: &'a Value, message: &str) -> &'a Value {
    v["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["message"].as_str().is_some_and(|m| m.contains(message)))
        .unwrap_or_else(|| panic!("no cluster {message}: {v}"))
}

async fn get_text(s: &Server, path: &str, token: &str) -> (StatusCode, String, Option<String>) {
    let r = s
        .http
        .get(format!("{}{path}", s.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let next = r
        .headers()
        .get("x-fastproof-next-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (st, r.text().await.unwrap(), next)
}

#[tokio::test]
async fn suggestions_over_http_with_permissions_and_cross_project_ids() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(&db, ON, ROLE_JOBS).await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("sg").await;
    s.project("op").await;
    let editor = s.token("ed", "editor", &["sg"]).await;
    let viewer = s.token("v", "viewer", &["sg"]).await;
    let agent = s.token("bot", "agent", &["sg"]).await;
    let owner = s.token("own", "admin", &["sg"]).await;
    let outsider = s.token("out", "editor", &["op"]).await;
    let outsider_admin = s.token("oa", "admin", &["op"]).await;
    let pb = type_id(&s, "sg", "productBug").await;
    let si = type_id(&s, "sg", "systemIssue").await;

    // History: three launches where a person decided both errors.
    for _ in 0..3 {
        let l = finished_launch(
            &s,
            "sg",
            vec![
                res("1", "Cart", "total is wrong"),
                res("2", "Vault", "connection refused by vault"),
            ],
        )
        .await;
        for r in s.results("sg", &l, "").await {
            let t = if r["name"] == "Cart" { &pb } else { &si };
            let (st, b) = s
                .post(
                    "/api/v1/projects/sg/triage",
                    &editor,
                    json!({"resultIds": [r["id"]], "defectTypeId": t}),
                )
                .await;
            assert_eq!(st, StatusCode::OK, "{b}");
        }
        analysed(&state, &s, "sg", &l).await;
    }
    fastproof_server::analysis::suggestions::fold(&state)
        .await
        .unwrap();
    let l = finished_launch(
        &s,
        "sg",
        vec![
            res("1", "Cart", "total is wrong"),
            res("2", "Vault", "connection refused by vault"),
            res("3", "Fresh", "a brand new failure"),
        ],
    )
    .await;
    let a = analysed(&state, &s, "sg", &l).await;
    assert_eq!(a["steps"]["suggestions"]["status"], "done", "{a}");

    // The launch view: viewers read it; others are refused or do not find it.
    let (st, v) = suggestions(&s, "sg", &l, &viewer).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "ready");
    let cart = cluster(&v, "total is wrong");
    let top = &cart["suggestions"][0];
    assert_eq!(top["band"], "same", "{cart}");
    assert_eq!(top["defectTypeId"], pb.as_str());
    assert_eq!(top["defectName"], "Product bug");
    assert!(
        top["why"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["signal"] == "sameFingerprint")
    );
    assert_eq!(top["ruleDraftOffered"], true);
    assert!(top["precision"].is_null());
    assert_eq!(cluster(&v, "brand new")["noSuggestion"], "newError", "{v}");
    let (st, _) = suggestions(&s, "sg", &l, &outsider).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = suggestions(&s, "op", &l, &outsider).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch of another project");
    let sid = top["id"].as_str().unwrap().to_string();
    let vault_sid = cluster(&v, "vault")["suggestions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Recompute: not for viewers; then too soon after the last run.
    let rc = format!("/api/v1/projects/sg/launches/{l}/suggestions/recompute");
    let (st, _) = s.post(&rc, &viewer, json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = s.post(&rc, &editor, json!({})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    assert_eq!(b["code"], "rate_limited");
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/op/launches/{l}/suggestions/recompute"),
            &outsider,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Memory of a fingerprint.
    let fp = cart["fingerprint"].as_str().unwrap();
    let (st, m) = s
        .get(
            &format!("/api/v1/projects/sg/analysis/memory?fingerprint={fp}"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(m["items"][0]["confirmed"], 3, "{m}");
    let (st, _) = s
        .get(
            "/api/v1/projects/sg/analysis/memory?fingerprint=nope",
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, m) = s
        .get(
            &format!("/api/v1/projects/op/analysis/memory?fingerprint={fp}"),
            &outsider,
        )
        .await;
    assert_eq!(m["items"].as_array().map(Vec::len), Some(0), "no leak: {m}");

    // Accept: not for viewers, not from another project; the agent accepts as AI.
    let acc = |id: &str, k: &str| format!("/api/v1/projects/{k}/suggestions/{id}/accept");
    let (st, _) = s.post(&acc(&sid, "sg"), &viewer, json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.post(&acc(&sid, "op"), &outsider, json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, b) = s
        .post(&acc(&sid, "sg"), &editor, json!({"applyTo": "everything"}))
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (st, out) = s
        .post(&acc(&sid, "sg"), &editor, json!({"comment": "known"}))
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["updated"], 1);
    let batch = out["batchId"].as_str().unwrap().to_string();
    let (_, h) = s
        .get(
            &format!("/api/v1/projects/sg/triage/history?batchId={batch}"),
            &viewer,
        )
        .await;
    assert_eq!(h["items"][0]["via"], "suggestion", "{h}");
    assert_eq!(h["items"][0]["suggestionId"], sid.as_str());
    let (st, b) = s.post(&acc(&sid, "sg"), &editor, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(b["code"], "suggestion_closed");
    assert_eq!(b["details"]["state"], "accepted");
    let (st, out) = s.post(&acc(&vault_sid, "sg"), &agent, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let (_, h) = s
        .get(
            &format!(
                "/api/v1/projects/sg/triage/history?batchId={}",
                out["batchId"].as_str().unwrap()
            ),
            &viewer,
        )
        .await;
    assert_eq!(h["items"][0]["source"], "ai", "{h}");
    // The person's acceptance is undoable.
    let (st, u) = s
        .post(
            "/api/v1/projects/sg/triage/undo",
            &editor,
            json!({"batchId": batch}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    assert_eq!(u["restored"], 1);

    // Reject and the rule draft on a fresh computation.
    fastproof_server::analysis::suggestions::fold(&state)
        .await
        .unwrap();
    recompute_now(&state, &s, &l).await;
    let (_, v) = suggestions(&s, "sg", &l, &viewer).await;
    let sid = cluster(&v, "total is wrong")["suggestions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let rej = |k: &str| format!("/api/v1/projects/{k}/suggestions/{sid}/reject");
    let (st, _) = s.post(&rej("sg"), &viewer, json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.post(&rej("op"), &outsider, json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, b) = s
        .post(&rej("sg"), &editor, json!({"reason": "x".repeat(501)}))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = s
        .post(&rej("sg"), &editor, json!({"reason": "flaky"}))
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["state"], "rejected");
    let rule = |k: &str| format!("/api/v1/projects/{k}/suggestions/{sid}/rule");
    let (st, _) = s.post(&rule("sg"), &viewer, json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.post(&rule("op"), &outsider, json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, r) = s.post(&rule("sg"), &agent, json!({})).await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    assert_eq!(r["enabled"], false);
    assert_eq!(r["match"]["fingerprint"], fp);
    assert_eq!(r["action"]["defectTypeId"], pb.as_str());

    // Backtest: settings:write, a task, then the report; the export.
    let (st, _) = s
        .post("/api/v1/projects/sg/analysis/backtest", &editor, json!({}))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = s
        .post(
            "/api/v1/projects/sg/analysis/backtest",
            &owner,
            json!({"days": 0}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, t) = s
        .post(
            "/api/v1/projects/sg/analysis/backtest",
            &owner,
            json!({"days": 30}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{t}");
    assert_eq!(t["kind"], "backtest");
    let (st, _) = s
        .get("/api/v1/projects/sg/analysis/backtest/latest", &viewer)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "no report yet");
    fastproof_server::analysis::tasks::run(&state, Duration::from_secs(60))
        .await
        .unwrap();
    let (st, task) = s
        .get(
            &format!("/api/v1/projects/sg/tasks/{}", t["id"].as_str().unwrap()),
            &owner,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(task["status"], "done", "{task}");
    let (st, rep) = s
        .get("/api/v1/projects/sg/analysis/backtest/latest", &viewer)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rep["id"], task["progress"]["backtestId"]);
    assert!(rep["decisions"].as_u64().unwrap() >= 6, "{rep}");
    let (st, _) = s
        .get("/api/v1/projects/op/analysis/backtest/latest", &outsider)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = get_text(&s, "/api/v1/projects/sg/analysis/decision-events", &editor).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, text, next) = get_text(
        &s,
        "/api/v1/projects/sg/analysis/decision-events?limit=2",
        &owner,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(next.is_some());
    for e in &lines {
        let by = e["by"].as_str().unwrap();
        assert_eq!(by.len(), 16, "hashed: {e}");
        assert_ne!(by, "ed");
    }
    let (st, text, _) = get_text(
        &s,
        "/api/v1/projects/op/analysis/decision-events",
        &outsider_admin,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(text.is_empty(), "no events of another project");
    let (st, _, _) = get_text(
        &s,
        "/api/v1/projects/sg/analysis/decision-events?limit=10001",
        &owner,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, caps) = s
        .get("/api/v1/projects/sg/analysis/capabilities", &viewer)
        .await;
    assert_eq!(caps["features"]["suggestions"], true);
}

/// Queues the launch again without the HTTP rate limit and analyses it.
async fn recompute_now(state: &AppState, s: &Server, lid: &str) {
    let id: uuid::Uuid = lid.parse().unwrap();
    let project: uuid::Uuid = sqlx::query_scalar("select project_id from launches where id = $1")
        .bind(id)
        .fetch_one(state.store.pool())
        .await
        .unwrap();
    assert!(
        state
            .store
            .enqueue_analysis_launch(
                project,
                id,
                fastproof_store::analysis::types::QueueReason::Recompute,
                Duration::ZERO,
            )
            .await
            .unwrap()
    );
    sqlx::query("update analysis_launches set due_at = now() where launch_id = $1")
        .bind(id)
        .execute(state.store.pool())
        .await
        .unwrap();
    analysed(state, s, "sg", lid).await;
}

#[tokio::test]
async fn gated_and_disabled_writes_are_refused() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut closed = Server::start_with_paused(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_ANALYSIS_GATE", "closed"),
        ],
        ROLE_JOBS,
    )
    .await;
    closed.project("gp").await;
    let (st, b) = closed
        .post(
            "/api/v1/projects/gp/analysis/backtest",
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "upgrade_in_progress");
    assert_eq!(b["details"]["area"], "analysis");
    closed.stop().await;
    let off = Server::start_paused(&db, ROLE_JOBS).await;
    let l = finished_launch(&off, "gp", vec![res("1", "A", "boom")]).await;
    let (st, v) = suggestions(&off, "gp", &l, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["state"], "disabled");
    for path in [
        format!("/api/v1/projects/gp/launches/{l}/suggestions/recompute"),
        "/api/v1/projects/gp/analysis/backtest".to_string(),
    ] {
        let (st, b) = off.post(&path, ADMIN_TOKEN, json!({})).await;
        assert_eq!(st, StatusCode::CONFLICT, "{path}: {b}");
        assert_eq!(b["code"], "analysis_disabled");
    }
    let (st, _, _) = get_text(
        &off,
        "/api/v1/projects/gp/analysis/decision-events",
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

const DAY: i64 = 86_400_000;

fn locator(group: &str) -> &'static str {
    match group {
        "productBug" => "pb001",
        "automationBug" => "ab001",
        "systemIssue" => "si001",
        "noDefect" => "nd001",
        _ => "ti001",
    }
}

/// An RP read-API dataset of the first `launches` synthetic launches: one suite per launch,
/// a failed step per failed result whose ERROR log is the error's trace, and the issue type
/// of the person's decision on its cluster (none when it was not triaged).
#[allow(clippy::field_reassign_with_default)]
fn rp_dataset(now: i64, launches: usize) -> (Fake, usize) {
    let data = generate(&SynthProfile::named("s").unwrap());
    let mut decided: HashMap<(u32, usize), String> = HashMap::new();
    for (ev, t) in data.decisions.iter().zip(&data.truth) {
        decided.insert((t.launch, t.error), ev.group.clone());
    }
    let t0 = now - 20 * DAY;
    let mut f = Fake::default();
    let mut issued = 0;
    for l in data.launches.iter().take(launches) {
        let lid = l.index as i64 + 1;
        let start = t0 + l.index as i64 * 3 * 3_600_000;
        f.launches.push(
            json!({"id": lid, "uuid": format!("sl-{lid}"), "name": "nightly", "number": lid,
            "startTime": start, "endTime": start + 600_000, "status": "FAILED", "mode": "DEFAULT",
            "attributes": [{"key": "branch", "value": l.branch}]}),
        );
        let suite = lid * 100_000;
        let mut items = vec![
            json!({"id": suite, "uuid": format!("su-{lid}"), "name": "tests/test_shop.py",
            "type": "SUITE", "startTime": start + 1, "endTime": start + 500_000, "status": "FAILED",
            "hasStats": true, "launchId": lid}),
        ];
        for r in &l.failed {
            let id = suite + r.test as i64 + 1;
            let mut it = json!({"id": id, "uuid": format!("it-{lid}-{}", r.test), "name": r.name,
                "type": "STEP", "parent": suite, "startTime": start + 10 + r.test as i64,
                "endTime": start + 20 + r.test as i64, "status": "FAILED", "hasStats": true, "launchId": lid,
                "codeRef": format!("tests/test_shop.py:{}", r.name)});
            if let Some(g) = decided.get(&(l.index, r.error)) {
                it["issue"] = json!({"issueType": locator(g)});
                issued += 1;
            }
            items.push(it);
            f.logs
                .push(json!({"itemId": id, "logTime": start + 15 + r.test as i64,
                "message": data.errors[r.error].trace, "level": "ERROR"}));
        }
        f.items.insert(lid, items);
    }
    (f, issued)
}

async fn run_cli(args: Vec<String>) -> anyhow::Result<i32> {
    let mut a = vec!["fastproof".to_string()];
    a.extend(args);
    let cli = <fastproof_server::cli::Cli as clap::Parser>::try_parse_from(a).unwrap();
    fastproof_server::cli::run(cli).await
}

fn same_numbers(a: &BacktestReport, b: &BacktestReport) {
    assert_eq!(
        (a.decisions, a.coverage, a.no_suggestion, a.truncated),
        (b.decisions, b.coverage, b.no_suggestion, b.truncated)
    );
    assert_eq!(a.bands, b.bands);
    assert_eq!(a.confusions, b.confusions);
    assert_eq!((a.from, a.to), (b.from, b.to));
}

#[tokio::test]
async fn the_rp_fixture_backtest_meets_the_thresholds_and_its_export_gives_the_same_report() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut env: Vec<(&str, &str)> = ON.to_vec();
    env.extend([
        ("FASTPROOF_RP_SETTLE_MS", "200"),
        ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "2"),
    ]);
    let s = Server::start_with_paused(&db, &env, ROLE_JOBS).await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("shop").await;
    // RP issues are applied as triage only for a caller that may triage.
    let token = s.token("importer", "editor", &["shop"]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fp-token"), &token).unwrap();
    std::fs::write(dir.path().join("rp-key"), format!("{RP_KEY}\n")).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    let launches = 40;
    let (fake, issued) = rp_dataset(now, launches);
    let (rp, _fake) = start_fake(fake).await;
    let args: Vec<String> = [
        "rp",
        "import",
        "--url",
        &s.base,
        "--token-file",
        dir.path().join("fp-token").to_str().unwrap(),
        "--project",
        "shop",
        "--rp-url",
        &rp,
        "--rp-project",
        "team",
        "--rp-key-file",
        dir.path().join("rp-key").to_str().unwrap(),
        "--state",
        dir.path().join("state.json").to_str().unwrap(),
        "--retry-delay-ms",
        "1",
        "--since",
        "60d",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(run_cli(args).await.unwrap(), 0);
    // The RP issues become people's decisions (`via: rp`) once their results settle.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        let n: i64 = sqlx::query_scalar("select count(*) from triage_decisions where via = 'rp'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if n as usize >= issued {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let facts: (i64, i64, i64, i64) = sqlx::query_as(
                "select (select count(*) from launches), (select count(*) from launches where status = 'finished'), \
                   (select count(*) from results), (select count(*) from results where defect is not null)",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
            panic!(
                "{n} of {issued} RP issues were applied; launches, finished, results, with defect: {facts:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    while fastproof_server::analysis::suggestions::fold(&state)
        .await
        .is_ok()
    {
        let left: i64 = sqlx::query_scalar(
            "select count(*) from triage_decisions where folded_at is null and source = 'user'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if left == 0 {
            break;
        }
    }
    let (st, _) = s
        .post(
            "/api/v1/projects/shop/analysis/backtest",
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    fastproof_server::analysis::tasks::run(&state, Duration::from_secs(120))
        .await
        .unwrap();
    let (st, rep) = s
        .get(
            "/api/v1/projects/shop/analysis/backtest/latest",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{rep}");
    let server: BacktestReport = serde_json::from_value(rep).unwrap();
    eprintln!(
        "RP fixture backtest: {}",
        serde_json::to_string(&server).unwrap()
    );
    assert!(server.decisions as usize >= issued / 2, "{server:?}");
    let same = &server.bands[0];
    let high = &server.bands[1];
    assert!(same.suggested >= 50, "{server:?}");
    assert!(same.precision.unwrap() >= 0.97, "{server:?}");
    if high.suggested >= 20 {
        assert!(high.precision.unwrap() >= 0.85, "{server:?}");
    }
    // Export every event and replay it offline: the same report.
    let mut file = String::new();
    let mut after: Option<String> = None;
    loop {
        let q = match &after {
            Some(a) => format!("?limit=500&after={a}"),
            None => "?limit=500".into(),
        };
        let (st, text, next) = get_text(
            &s,
            &format!("/api/v1/projects/shop/analysis/decision-events{q}"),
            ADMIN_TOKEN,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        file.push_str(&text);
        match next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    let path = dir.path().join("events.ndjson");
    std::fs::write(&path, &file).unwrap();
    let events = read_events_file(&path, 200_000).unwrap();
    assert_eq!(events.len() as u64, server.decisions);
    let offline = backtest_events(
        events,
        &fastproof_analysis::backtest::BacktestOptions::default(),
        BacktestSource::Events,
    );
    same_numbers(&offline, &server);
    // The CLI on the export: the thresholds hold (exit 0) and an impossible one fails (1).
    let cli = |min: &str| -> Vec<String> {
        vec![
            "analysis".into(),
            "backtest".into(),
            "--events".into(),
            path.to_str().unwrap().into(),
            "--min-precision".into(),
            min.into(),
        ]
    };
    assert_eq!(run_cli(cli("same=0.97")).await.unwrap(), 0);
    assert_eq!(run_cli(cli("same=1.0,low=1.0")).await.unwrap(), 1);
    // Synthetic CI thresholds through the CLI.
    let synth: Vec<String> = [
        "analysis",
        "backtest",
        "--synthetic",
        "s",
        "--json",
        "--min-precision",
        "same=0.97,high=0.85",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(run_cli(synth).await.unwrap(), 0);
}
