//! FastProof Analysis foundation over HTTP: the decision journal from every path (REST, MCP,
//! rules at ingest, the retro task, RP issues) with history and undo and their permissions,
//! capabilities, launch status, the replicas view, the analysis role with its queue and
//! pipeline, and the gated settings. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::analysis::types::{AnalysisLaunchRow, AnalysisStepStatus, StepOutcome};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

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

/// A role that triages without `results:mute` and is no agent.
const ACCESS: &str = "roles:\n  - name: triager\n    permissions: [\"projects:read\", \"launches:read\", \"results:triage\"]\n";

fn res(id: &str, name: &str, msg: &str) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"), "status": "failed",
        "error": {"message": msg, "trace": format!("java.lang.IllegalStateException: {msg}\n\tat com.example.{name}.run({name}.java:1)")}})
}

async fn launch(s: &Server, project: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": "run", "vcs": {"branch": "main"}}),
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
}

async fn by_name(
    s: &Server,
    project: &str,
    lid: &str,
) -> std::collections::BTreeMap<String, Value> {
    s.results(project, lid, "")
        .await
        .into_iter()
        .map(|r| (r["name"].as_str().unwrap().to_string(), r))
        .collect()
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

async fn history(s: &Server, project: &str, q: &str, token: &str) -> (StatusCode, Value) {
    s.get(
        &format!("/api/v1/projects/{project}/triage/history?{q}"),
        token,
    )
    .await
}

async fn undo(s: &Server, project: &str, batch: &str, token: &str) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/triage/undo"),
        token,
        json!({"batchId": batch}),
    )
    .await
}

/// `(via, source, by)` of the journal rows of a result, oldest first.
async fn journal(db: &TestDb, result: &str) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "select via, source, by from triage_decisions where result_id = $1::uuid order by id",
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
async fn every_path_is_journaled_and_undo_checks_permissions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let access = dir.path().join("access.yaml");
    std::fs::write(&access, ACCESS).unwrap();
    let s = Server::start_with_paused(
        &db,
        &[("FASTPROOF_ACCESS_FILE", access.to_str().unwrap())],
        &["analysis.tasks", "mutes.expire"],
    )
    .await;
    s.project("jp").await;
    s.project("op").await;
    let editor = s.token("ed", "editor", &["jp"]).await;
    let viewer = s.token("v", "viewer", &["jp"]).await;
    let agent = s.token("bot", "agent", &["jp"]).await;
    let triager = s.token("tr", "triager", &["jp"]).await;
    let outsider = s.token("out", "editor", &["op"]).await;
    let si = type_id(&s, "jp", "systemIssue").await;
    let pb = type_id(&s, "jp", "productBug").await;

    // A rule at ingest is journaled; the project default is not.
    let (st, rule) = s
        .post(
            "/api/v1/projects/jp/rules",
            &editor,
            json!({"name": "infra", "match": {"messageRegex": "refused"}, "action": {"defectTypeId": si}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{rule}");
    let rule_id = rule["id"].as_str().unwrap().to_string();
    let l = launch(&s, "jp").await;
    post_results(
        &s,
        "jp",
        &l,
        vec![
            res("1", "Vault", "connection refused"),
            res("2", "Cart", "total is wrong"),
            res("3", "Pay", "card declined"),
        ],
    )
    .await;
    let r = by_name(&s, "jp", &l).await;
    let (vault, cart, pay) = (
        r["Vault"]["id"].as_str().unwrap().to_string(),
        r["Cart"]["id"].as_str().unwrap().to_string(),
        r["Pay"]["id"].as_str().unwrap().to_string(),
    );
    assert_eq!(r["Vault"]["defect"]["group"], "systemIssue");
    assert_eq!(
        journal(&db, &vault).await,
        vec![("rule".into(), "rule".into(), format!("rule:{rule_id}"))]
    );
    assert!(
        journal(&db, &cart).await.is_empty(),
        "the default defect is not journaled"
    );
    let (st, h) = history(&s, "jp", &format!("resultId={vault}"), &viewer).await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(h["items"][0]["undoable"], false);
    let rule_batch = h["items"][0]["batchId"].as_str().unwrap().to_string();
    let (st, b) = undo(&s, "jp", &rule_batch, &editor).await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("batch_not_undoable"))
    );

    // REST triage answers its batch.
    let (st, out) = s
        .post(
            "/api/v1/projects/jp/triage",
            &editor,
            json!({"resultIds": [cart], "defectTypeId": pb}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["updated"], 1);
    let rest_batch = out["batchId"].as_str().unwrap().to_string();
    let rows = journal(&db, &cart).await;
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("api", "user"));
    let audit: Value = sqlx::query_scalar(
        "select detail from platform_audit where action = 'results.triage' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (audit["batchId"].as_str(), audit["via"].as_str()),
        (Some(rest_batch.as_str()), Some("api"))
    );
    let (st, h) = history(&s, "jp", &format!("batchId={rest_batch}"), &viewer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["items"].as_array().unwrap().len(), 1);
    assert_eq!(h["items"][0]["after"]["defect"]["group"], "productBug");
    assert_eq!(h["items"][0]["before"]["defect"]["group"], "toInvestigate");
    assert_eq!(h["items"][0]["undoable"], true);

    // MCP triage is `mcp`/`ai` and answers its batch too.
    let (err, out) = mcp_tool(
        &s,
        &agent,
        "triage",
        json!({"project": "jp", "resultIds": [pay], "defectTypeId": "automationBug", "comment": "flaky mock"}),
    )
    .await;
    assert!(!err, "{out}");
    assert_eq!(out["updated"], 1);
    let ai_batch = out["batchId"].as_str().unwrap().to_string();
    assert_eq!(
        journal(&db, &pay).await,
        vec![(
            "mcp".into(),
            "ai".into(),
            journal(&db, &pay).await[0].2.clone()
        )]
    );

    // The retro task is `retro`/`rule`, by the person who queued it.
    let (st, rule2) = s
        .post(
            "/api/v1/projects/jp/rules",
            &editor,
            json!({"name": "cards", "match": {"messageRegex": "declined|wrong"}, "action": {"comment": "known"}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{rule2}");
    // Cart and Pay were decided by a person or an agent: the retro leaves them; a new result
    // of a second launch gets the rule.
    let l2 = launch(&s, "jp").await;
    post_results(&s, "jp", &l2, vec![res("4", "Card", "card declined again")]).await;
    let card = by_name(&s, "jp", &l2).await["Card"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let rows = journal(&db, &card).await;
    assert_eq!(rows.len(), 1, "the new rule matched at ingest: {rows:?}");
    sqlx::query("delete from triage_decisions where result_id = $1::uuid")
        .bind(&card)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update results set defect = null where id = $1::uuid")
        .bind(&card)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, task) = s
        .post(
            &format!(
                "/api/v1/projects/jp/rules/{}/apply?launchId={l2}",
                rule2["id"].as_str().unwrap()
            ),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{task}");
    db.run_scheduled("analysis.tasks").await;
    let rows = journal(&db, &card).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("retro", "rule"));
    assert_eq!(
        rows[0].2,
        journal(&db, &cart).await[0].2,
        "by the editor who queued it"
    );
    let retro_events: i64 = sqlx::query_scalar(
        "select count(*) from events where type = 'triage.decided' and payload->>'via' = 'retro'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(retro_events, 1);

    // Undo and its permissions.
    let (st, _) = undo(&s, "jp", &rest_batch, &viewer).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = undo(&s, "jp", &rest_batch, &outsider).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, b) = undo(&s, "op", &rest_batch, &outsider).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    let (st, _) = history(&s, "jp", &format!("resultId={cart}"), &outsider).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, h) = history(&s, "op", &format!("resultId={cart}"), &outsider).await;
    assert_eq!(st, StatusCode::OK);
    assert!(h["items"].as_array().unwrap().is_empty(), "{h}");
    let (st, b) = undo(&s, "jp", &rest_batch, &agent).await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_your_batch"))
    );
    let (st, b) = s
        .post(
            "/api/v1/projects/jp/triage/undo",
            &editor,
            json!({"batchId": rest_batch, "force": true}),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (st, b) = undo(&s, "jp", &rest_batch, &editor).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (b["restored"].as_u64(), b["reverts"].as_str()),
        (Some(1), Some(rest_batch.as_str()))
    );
    assert_eq!(
        by_name(&s, "jp", &l).await["Cart"]["defect"]["group"],
        "toInvestigate"
    );
    let undo_batch = b["batchId"].as_str().unwrap().to_string();
    let (st, h) = history(&s, "jp", &format!("batchId={undo_batch}"), &viewer).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["items"][0]["via"], "undo");
    assert_eq!(h["items"][0]["revertsBatch"], rest_batch.as_str());
    let (st, b) = undo(&s, "jp", &undo_batch, &editor).await;
    assert_eq!(
        (st, b["restored"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{b}"
    );
    let (st, b) = undo(&s, "jp", &ai_batch, &agent).await;
    assert_eq!(
        (st, b["restored"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{b}"
    );
    let audit: Value = sqlx::query_scalar(
        "select detail from platform_audit where action = 'triage.undo' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(audit["reverts"], ai_batch.as_str());

    // A batch that changed mutes needs results:mute to be undone.
    let (st, out) = s
        .post(
            "/api/v1/projects/jp/triage",
            &editor,
            json!({"resultIds": [vault], "mute": {"reason": "infra outage"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let mute_batch = out["batchId"].as_str().unwrap().to_string();
    let (st, b) = undo(&s, "jp", &mute_batch, &triager).await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden")),
        "{b}"
    );
    let (st, b) = undo(&s, "jp", &mute_batch, &editor).await;
    assert_eq!(
        (st, b["restored"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{b}"
    );
    assert_eq!(s.launch_get("jp", &l).await["counts"]["muted"], 0);
}

#[tokio::test]
async fn rp_issues_are_journaled_as_rp() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_RP_SETTLE_MS", "200"),
            ("FASTPROOF_RP_ERROR_WAIT_MS", "300"),
            ("FASTPROOF_RP_FINISH_GRACE_SECONDS", "3"),
        ],
    )
    .await;
    s.project("shop").await;
    // Issues of callers with results:triage become triage.
    let token = s.token("ci", "editor", &["shop"]).await;
    let rp = |method: Method, path: String, body: Value| {
        let s = &s;
        let token = token.clone();
        async move {
            s.call(method, &format!("/rp/api/{path}"), Some(&token), Some(body))
                .await
        }
    };
    let (st, l) = rp(
        Method::POST,
        "v2/shop/launch".into(),
        json!({"name": "nightly", "startTime": 1_795_000_000_000i64}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    let launch = l["id"].as_str().unwrap().to_string();
    let (st, i) = rp(
        Method::POST,
        "v2/shop/item".into(),
        json!({"name": "test_refund", "type": "STEP", "launchUuid": launch, "startTime": 1_795_000_000_100i64}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{i}");
    let item = i["id"].as_str().unwrap().to_string();
    let (st, b) = rp(
        Method::PUT,
        format!("v2/shop/item/{item}"),
        json!({"endTime": 1_795_000_000_400i64, "status": "FAILED", "launchUuid": launch,
            "issue": {"issueType": "pb001", "comment": "known"}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = rp(
        Method::PUT,
        format!("v1/shop/launch/{launch}/finish"),
        json!({"endTime": 1_795_000_001_000i64}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let rows: Vec<(String, String)> = loop {
        let rows: Vec<(String, String)> =
            sqlx::query_as("select via, source from triage_decisions order by id")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        if !rows.is_empty() {
            break rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no RP decision was journaled"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(rows, vec![("rp".to_string(), "user".to_string())]);
    let audit: Value = sqlx::query_scalar(
        "select detail from platform_audit where action = 'results.triage' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        audit["batchId"].is_string() && audit["via"] == "rp",
        "{audit}"
    );
}

#[tokio::test]
async fn capabilities_launch_status_and_replicas_with_analysis_off() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cp").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["cp"]).await;
    let (st, c) = s
        .get("/api/v1/projects/cp/analysis/capabilities", &viewer)
        .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["enabled"], false);
    assert_eq!(c["instanceEnabled"], false);
    assert_eq!(c["projectEnabled"], true);
    assert_eq!(c["clickhouse"], false);
    assert_eq!(c["features"]["suggestions"], false);
    assert_eq!(c["windows"]["analyticsDays"], 30);
    let (st, _) = s
        .get("/api/v1/projects/other/analysis/capabilities", &viewer)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get("/api/v1/projects/cp/analysis/capabilities", "nope")
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let l = launch(&s, "cp").await;
    let (st, a) = s
        .get(
            &format!("/api/v1/projects/cp/launches/{l}/analysis"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(
        a,
        json!({"state": "none", "queuedAt": null, "finishedAt": null, "steps": {}})
    );
    let lo = launch(&s, "other").await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/cp/launches/{lo}/analysis"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch of another project");
    let (st, r) = s.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["analysis"]["mode"], "auto");
    assert_eq!(r["analysis"]["gateOpen"], true, "{r}");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (_, r) = s.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
        if let Some(rep) = r["replicas"].as_array().and_then(|a| a.first()) {
            assert_eq!(rep["analysisLevel"], 1, "{r}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "no heartbeat: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (st, _) = s.get("/api/v1/admin/replicas", &viewer).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // Analysis off: the role does not run, so finished launches are not queued.
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/cp/launches/{l}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success());
    let queued: i64 = sqlx::query_scalar("select count(*) from analysis_launches")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

#[tokio::test]
async fn the_role_analyses_finished_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
        ],
        ROLE_JOBS,
    )
    .await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("an").await;
    let viewer = s.token("v", "viewer", &["an"]).await;
    let (_, c) = s
        .get("/api/v1/projects/an/analysis/capabilities", &viewer)
        .await;
    assert_eq!(
        (c["enabled"].as_bool(), c["instanceEnabled"].as_bool()),
        (Some(true), Some(true))
    );
    let first = fastproof_server::analysis::run_once(&state).await.unwrap();
    assert_eq!(first.processed, 0);
    let l = launch(&s, "an").await;
    post_results(&s, "an", &l, vec![res("1", "A", "boom")]).await;
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/an/launches/{l}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success());
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let once = fastproof_server::analysis::run_once(&state).await.unwrap();
        if once.processed > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the finished launch was not analysed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (st, a) = s
        .get(
            &format!("/api/v1/projects/an/launches/{l}/analysis"),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(a["state"], "done", "{a}");
    for step in ["errors", "signals", "logRules", "suggestions", "flakyHints"] {
        assert_eq!(a["steps"][step]["status"], "skipped", "{a}");
        assert_eq!(a["steps"][step]["detail"]["reason"], "not available");
    }

    // A failing step is recorded and the next ones still run.
    let row = AnalysisLaunchRow {
        launch_id: l.parse().unwrap(),
        project_id: sqlx::query_scalar("select id from projects where key = 'an'")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        ..state
            .store
            .analysis_launch_row(l.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
    };
    let job = fastproof_server::analysis::pipeline::job(&state, &row)
        .await
        .unwrap()
        .unwrap();
    let mut ran = Vec::new();
    let steps = fastproof_server::analysis::pipeline::run_steps_with(&state, &job, |name| {
        ran.push(name);
        Box::pin(async move {
            if name == "signals" {
                anyhow::bail!("broken step")
            }
            Ok(StepOutcome::done(json!({"step": name})))
        })
    })
    .await;
    assert_eq!(
        ran,
        vec!["errors", "signals", "logRules", "suggestions", "flakyHints"]
    );
    assert_eq!(
        steps.signals.as_ref().unwrap().status,
        AnalysisStepStatus::Failed
    );
    assert_eq!(
        steps.signals.as_ref().unwrap().detail["error"],
        "broken step"
    );
    assert_eq!(
        steps.flaky_hints.as_ref().unwrap().status,
        AnalysisStepStatus::Done
    );

    // A project that opted out has its steps skipped.
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/an/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"analysis": {"enabled": false}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["analysis"]["enabled"], false);
    let audit: Value = sqlx::query_scalar(
        "select detail from platform_audit where action = 'analysis.settings' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (
            audit["old"]["enabled"].as_bool(),
            audit["new"]["enabled"].as_bool()
        ),
        (Some(true), Some(false))
    );
    let pid: Uuid = sqlx::query_scalar("select id from projects where key = 'an'")
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
    let once = fastproof_server::analysis::run_once(&state).await.unwrap();
    assert_eq!(once.processed, 1);
    let (_, a) = s
        .get(
            &format!("/api/v1/projects/an/launches/{l}/analysis"),
            &viewer,
        )
        .await;
    assert_eq!(
        a["steps"]["errors"]["detail"]["reason"], "analysis is off for the project",
        "{a}"
    );
    let (_, c) = s
        .get("/api/v1/projects/an/analysis/capabilities", &viewer)
        .await;
    assert_eq!(
        (c["enabled"].as_bool(), c["projectEnabled"].as_bool()),
        (Some(false), Some(false))
    );
}

async fn put(s: &Server, body: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        "/api/v1/projects/gs/settings",
        Some(ADMIN_TOKEN),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn analysis_formats_in_settings_wait_for_the_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut closed = Server::start_with(&db, &[("FASTPROOF_ANALYSIS_GATE", "closed")]).await;
    closed.project("gs").await;
    for (body, what) in [
        (
            json!({"analysis": {"autoApply": {"enabled": true}}}),
            "analysis.autoApply",
        ),
        (
            json!({"gates": [{"name": "g", "rules": [{"metric": "newErrors", "op": "lte", "value": 0}]}]}),
            "gates.newErrors",
        ),
        (
            json!({"gates": [{"name": "g", "rules": [{"metric": "total", "op": "gt", "value": 0}]}]}),
            "gates.total",
        ),
    ] {
        let (st, b) = put(&closed, body).await;
        assert_eq!(st, StatusCode::CONFLICT, "{b}");
        assert_eq!(b["code"], "upgrade_in_progress");
        assert_eq!(
            b["details"],
            json!({"what": what, "requiredLevel": 1, "area": "analysis"})
        );
    }
    let (st, b) = put(&closed, json!({"analysis": {"memoryDays": 10}})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = put(
        &closed,
        json!({"analysis": {"autoApply": {"bands": ["high"]}}}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = put(
        &closed,
        json!({"analysis": {"enabled": false, "memoryDays": 90}}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "settings without new formats pass a closed gate: {b}"
    );
    closed.stop().await;

    let open = Server::start_with(&db, &[("FASTPROOF_ANALYSIS_GATE", "open")]).await;
    let (st, b) = put(&open, json!({"analysis": {"autoApply": {"enabled": true}}})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["analysis"]["autoApply"],
        json!({"enabled": true, "bands": ["same"]})
    );
    let (st, b) = put(
        &open,
        json!({"gates": [{"name": "g", "rules": [{"metric": "toInvestigate", "op": "lte", "value": 0}]}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = put(&open, json!({"analysis": null})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(
        b.get("analysis").is_none(),
        "the default area is left out: {b}"
    );
}

#[tokio::test]
async fn a_stored_rule_this_version_cannot_read_is_not_applied_at_ingest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rr").await;
    let si = type_id(&s, "rr", "systemIssue").await;
    let pid: Uuid = sqlx::query_scalar("select id from projects where key = 'rr'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into rules (id, project_id, name, match, action) values ($1, $2, 'future', \
           '{\"log\": {\"contains\": \"x\"}, \"fromTheFuture\": {\"a\": 1}}', jsonb_build_object('defectTypeId', $3::text))",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .bind(&si)
    .execute(&db.pool)
    .await
    .unwrap();
    let l = launch(&s, "rr").await;
    post_results(&s, "rr", &l, vec![res("1", "A", "boom")]).await;
    let r = by_name(&s, "rr", &l).await;
    assert_eq!(r["A"]["defect"]["group"], "toInvestigate", "{}", r["A"]);
    let n: i64 = sqlx::query_scalar("select count(*) from triage_decisions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let (_, rules) = s.get("/api/v1/projects/rr/rules", ADMIN_TOKEN).await;
    assert_eq!(rules["items"][0]["phase"], "afterLaunch");
    assert!(
        rules["items"][0]["note"]
            .as_str()
            .unwrap()
            .contains("not readable")
    );
}
