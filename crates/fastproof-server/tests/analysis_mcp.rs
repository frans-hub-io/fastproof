//! The FastProof Analysis tools of the MCP endpoint over HTTP: the tool list and schemas, the
//! permission of each tool, refused writes, the AI journal and audit records, undo that never
//! reaches another principal's batch, `untrustedContent`, the output clip, the write rate and
//! Analysis off. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::Duration;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::StatusCode;
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

const ON: &[(&str, &str)] = &[
    ("FASTPROOF_ANALYSIS", "on"),
    ("FASTPROOF_ANALYSIS_WORKERS", "0"),
    ("FASTPROOF_ANALYSIS_SETTLE_SECONDS", "0"),
    ("FASTPROOF_ANALYSIS_GATE", "open"),
];

const ANALYSIS_TOOLS: &[&str] = &[
    "launch_suggestions",
    "accept_suggestion",
    "reject_suggestion",
    "unique_errors",
    "error_detail",
    "search_logs",
    "triage_history",
    "undo_triage",
    "simulate_gate",
];

async fn mcp(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    let r = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
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

/// Calls a tool: `(is_error, parsed JSON or the error text, raw text length)`.
async fn call(s: &Server, token: &str, name: &str, args: Value) -> (bool, Value, usize) {
    let (st, b) = mcp(
        s,
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let r = &b["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or("").to_string();
    let err = r["isError"].as_bool().unwrap_or(false);
    let len = text.len();
    (
        err,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
        len,
    )
}

async fn tool(s: &Server, token: &str, name: &str, args: Value) -> (bool, Value) {
    let (e, v, _) = call(s, token, name, args).await;
    (e, v)
}

async fn ok(s: &Server, token: &str, name: &str, args: Value) -> Value {
    let (e, v) = tool(s, token, name, args).await;
    assert!(!e, "{name} failed: {v}");
    v
}

async fn refused(s: &Server, token: &str, name: &str, args: Value, code: &str) {
    let (e, v) = tool(s, token, name, args).await;
    assert!(
        e && v.as_str().is_some_and(|t| t.starts_with(code)),
        "{name}: wanted an error starting with {code:?}, got {e} {v}"
    );
}

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

async fn analysed(s: &Server, project: &str, lid: &str) {
    let state = s.running.as_ref().unwrap().state.clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        fastproof_server::analysis::run_once(&state).await.unwrap();
        let (_, a) = s
            .get(
                &format!("/api/v1/projects/{project}/launches/{lid}/analysis"),
                ADMIN_TOKEN,
            )
            .await;
        if a["state"] == "done" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the launch was not analysed: {a}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn cluster<'a>(v: &'a Value, message: &str) -> &'a Value {
    v["clusters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["message"].as_str().is_some_and(|m| m.contains(message)))
        .unwrap_or_else(|| panic!("no cluster {message}: {v}"))
}

async fn audit(s: &Server, action: &str) -> Vec<Value> {
    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    a["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"] == action)
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_tool_list_has_the_analysis_tools_with_strict_schemas() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(&db, ON, ROLE_JOBS).await;
    s.project("mc").await;
    let viewer = s.token("v", "viewer", &["mc"]).await;
    let (_, list) = mcp(
        &s,
        &viewer,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = list["result"]["tools"].as_array().unwrap();
    for name in ANALYSIS_TOOLS {
        let t = tools
            .iter()
            .find(|t| t["name"] == *name)
            .unwrap_or_else(|| panic!("no tool {name}"));
        let schema = &t["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        assert!(schema["properties"]["project"].is_object(), "{name}");
        let write = ["accept_suggestion", "reject_suggestion", "undo_triage"].contains(name);
        assert_eq!(
            t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false),
            !write,
            "{name}"
        );
        assert!(
            t["description"].as_str().unwrap().len() > 40,
            "{name} has a description"
        );
    }
    for name in ["launch_suggestions", "error_detail", "search_logs"] {
        let d = tools.iter().find(|t| t["name"] == name).unwrap()["description"]
            .as_str()
            .unwrap();
        assert!(
            d.contains("not instructions"),
            "{name} warns that its text is data"
        );
    }
    let (e, v) = tool(
        &s,
        &viewer,
        "launch_suggestions",
        json!({"project": "mc", "launchId": uuid_of(""), "extra": 1}),
    )
    .await;
    assert!(
        e && v.as_str().unwrap().starts_with("invalid arguments"),
        "{v}"
    );
}

#[tokio::test]
async fn suggestions_accept_reject_undo_and_the_journal_through_mcp() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(&db, ON, ROLE_JOBS).await;
    let state = s.running.as_ref().unwrap().state.clone();
    s.project("mc").await;
    s.project("op").await;
    let editor = s.token("ed", "editor", &["mc"]).await;
    let viewer = s.token("v", "viewer", &["mc"]).await;
    let agent = s.token("bot", "agent", &["mc"]).await;
    let outsider = s.token("out", "editor", &["op"]).await;
    let outsider_admin = s.token("oa", "admin", &["op"]).await;
    let pb = type_id(&s, "mc", "productBug").await;
    let si = type_id(&s, "mc", "systemIssue").await;

    for _ in 0..3 {
        let l = finished_launch(
            &s,
            "mc",
            vec![
                res("1", "Cart", "total is wrong"),
                res("2", "Vault", "connection refused by vault"),
            ],
        )
        .await;
        for r in s.results("mc", &l, "").await {
            let t = if r["name"] == "Cart" { &pb } else { &si };
            let (st, b) = s
                .post(
                    "/api/v1/projects/mc/triage",
                    &editor,
                    json!({"resultIds": [r["id"]], "defectTypeId": t}),
                )
                .await;
            assert_eq!(st, StatusCode::OK, "{b}");
        }
        analysed(&s, "mc", &l).await;
    }
    fastproof_server::analysis::suggestions::fold(&state)
        .await
        .unwrap();
    let lid = finished_launch(
        &s,
        "mc",
        vec![
            res("1", "Cart", "total is wrong"),
            res("2", "Vault", "connection refused by vault"),
            res("3", "Fresh", "a brand new failure"),
        ],
    )
    .await;
    analysed(&s, "mc", &lid).await;

    let v = ok(
        &s,
        &viewer,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid}),
    )
    .await;
    assert_eq!(v["untrustedContent"], true);
    assert_eq!(v["state"], "ready");
    let cart = cluster(&v, "total is wrong");
    assert_eq!(cart["suggestions"][0]["band"], "same", "{cart}");
    let sid = cart["suggestions"][0]["id"].as_str().unwrap().to_string();
    let vault_sid = cluster(&v, "vault")["suggestions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(cluster(&v, "brand new")["noSuggestion"], "newError");

    refused(
        &s,
        &outsider,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid}),
        "forbidden",
    )
    .await;
    refused(
        &s,
        &outsider,
        "launch_suggestions",
        json!({"project": "op", "launchId": lid}),
        "not_found",
    )
    .await;

    refused(
        &s,
        &viewer,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid, "compute": true}),
        "forbidden",
    )
    .await;
    refused(
        &s,
        &editor,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid, "compute": true}),
        "rate_limited",
    )
    .await;

    for (name, a) in [
        (
            "accept_suggestion",
            json!({"project": "mc", "suggestionId": sid}),
        ),
        (
            "reject_suggestion",
            json!({"project": "mc", "suggestionId": sid}),
        ),
        (
            "undo_triage",
            json!({"project": "mc", "batchId": uuid_of(&sid)}),
        ),
    ] {
        refused(&s, &viewer, name, a, "forbidden").await;
    }
    refused(
        &s,
        &outsider_admin,
        "accept_suggestion",
        json!({"project": "op", "suggestionId": sid}),
        "not_found",
    )
    .await;
    refused(
        &s,
        &outsider_admin,
        "reject_suggestion",
        json!({"project": "op", "suggestionId": sid}),
        "not_found",
    )
    .await;
    let many: Vec<String> = (0..501).map(|_| uuid_of("x")).collect();
    refused(
        &s,
        &agent,
        "accept_suggestion",
        json!({"project": "mc", "suggestionId": sid, "resultIds": many}),
        "resultIds",
    )
    .await;
    refused(
        &s,
        &agent,
        "accept_suggestion",
        json!({"project": "mc", "suggestionId": sid, "agent": "$bad"}),
        "agent must",
    )
    .await;
    refused(
        &s,
        &agent,
        "reject_suggestion",
        json!({"project": "mc", "suggestionId": vault_sid, "reason": "x".repeat(501)}),
        "reason",
    )
    .await;
    assert_eq!(
        ok(
            &s,
            &viewer,
            "launch_suggestions",
            json!({"project": "mc", "launchId": lid})
        )
        .await["clusters"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "refused writes changed nothing"
    );

    let acc = ok(
        &s,
        &agent,
        "accept_suggestion",
        json!({"project": "mc", "suggestionId": sid, "comment": "known", "agent": "copilot-triage"}),
    )
    .await;
    assert_eq!(acc["updated"], 1, "{acc}");
    let batch = acc["batchId"].as_str().unwrap().to_string();
    refused(
        &s,
        &agent,
        "accept_suggestion",
        json!({"project": "mc", "suggestionId": sid}),
        "suggestion_closed",
    )
    .await;
    let a = audit(&s, "suggestion.accept").await;
    let e = a.iter().find(|e| e["detail"]["batchId"] == batch.as_str());
    let e = e.unwrap_or_else(|| panic!("no accept audit: {a:?}"));
    assert_eq!(e["detail"]["via"], "mcp");
    assert_eq!(e["detail"]["source"], "ai");
    assert_eq!(e["detail"]["agent"], "copilot-triage");

    let h = ok(
        &s,
        &viewer,
        "triage_history",
        json!({"project": "mc", "batchId": batch}),
    )
    .await;
    assert_eq!(h["untrustedContent"], true);
    assert_eq!(h["items"][0]["via"], "suggestion", "{h}");
    assert_eq!(h["items"][0]["source"], "ai", "{h}");
    assert_eq!(h["items"][0]["undoable"], true, "{h}");
    refused(
        &s,
        &viewer,
        "triage_history",
        json!({"project": "mc"}),
        "give one of",
    )
    .await;
    refused(
        &s,
        &viewer,
        "triage_history",
        json!({"project": "mc", "batchId": batch, "resultId": batch}),
        "give one of",
    )
    .await;
    let theirs = ok(
        &s,
        &outsider,
        "triage_history",
        json!({"project": "op", "batchId": batch}),
    )
    .await;
    assert_eq!(
        theirs["items"].as_array().map(Vec::len),
        Some(0),
        "no leak: {theirs}"
    );

    let results = s.results("mc", &lid, "").await;
    let fresh = results.iter().find(|r| r["name"] == "Fresh").unwrap();
    let (st, ed) = s
        .post(
            "/api/v1/projects/mc/triage",
            &editor,
            json!({"resultIds": [fresh["id"]], "defectTypeId": pb}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{ed}");
    let editor_batch = ed["batchId"].as_str().unwrap().to_string();
    refused(
        &s,
        &agent,
        "undo_triage",
        json!({"project": "mc", "batchId": editor_batch}),
        "not_your_batch",
    )
    .await;
    refused(
        &s,
        &editor,
        "undo_triage",
        json!({"project": "mc", "batchId": batch}),
        "not_your_batch",
    )
    .await;
    let (_, h) = s
        .get(
            &format!(
                "/api/v1/projects/mc/triage/history?resultId={}",
                fresh["id"].as_str().unwrap()
            ),
            &viewer,
        )
        .await;
    assert_eq!(
        h["items"][0]["after"]["defect"]["typeId"],
        pb.as_str(),
        "{h}"
    );

    let undo = ok(
        &s,
        &agent,
        "undo_triage",
        json!({"project": "mc", "batchId": batch, "agent": "copilot-triage"}),
    )
    .await;
    assert_eq!(undo["restored"], 1, "{undo}");
    let u = audit(&s, "triage.undo").await;
    assert!(
        u.iter().any(|e| e["detail"]["via"] == "mcp"
            && e["detail"]["source"] == "ai"
            && e["detail"]["agent"] == "copilot-triage"),
        "{u:?}"
    );
    let own = ok(
        &s,
        &editor,
        "undo_triage",
        json!({"project": "mc", "batchId": editor_batch}),
    )
    .await;
    assert_eq!(own["restored"], 1, "{own}");

    let rej = ok(
        &s,
        &agent,
        "reject_suggestion",
        json!({"project": "mc", "suggestionId": vault_sid, "reason": "not now"}),
    )
    .await;
    assert_eq!(rej["state"], "rejected", "{rej}");
    assert_eq!(
        rej["untrustedContent"], true,
        "a suggestion carries people's text"
    );
    refused(
        &s,
        &agent,
        "reject_suggestion",
        json!({"project": "mc", "suggestionId": vault_sid}),
        "suggestion_closed",
    )
    .await;
    let r = audit(&s, "suggestion.reject").await;
    assert!(
        r.iter().any(|e| e["detail"]["via"] == "mcp"
            && e["detail"]["source"] == "ai"
            && e["detail"]["reason"] == "not now"),
        "{r:?}"
    );
}

fn uuid_of(_: &str) -> String {
    "7b0c6c2e-8f6a-4f56-9d67-5f1a1c1d0001".to_string()
}

#[tokio::test]
async fn errors_logs_and_the_gate_simulation_through_mcp() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(&db, ON, ROLE_JOBS).await;
    s.project("mc").await;
    s.project("op").await;
    let viewer = s.token("v", "viewer", &["mc"]).await;
    let outsider = s.token("out", "editor", &["op"]).await;
    let outsider_admin = s.token("oa", "admin", &["op"]).await;
    let lid = finished_launch(
        &s,
        "mc",
        vec![
            res("1", "Cart", "total is wrong"),
            res("2", "Vault", "connection refused by vault"),
        ],
    )
    .await;
    analysed(&s, "mc", &lid).await;

    let e = ok(&s, &viewer, "unique_errors", json!({"project": "mc"})).await;
    assert_eq!(e["untrustedContent"], true);
    let items = e["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{e}");
    let fp = items[0]["fingerprint"].as_str().unwrap().to_string();
    let state = items[0]["state"].as_str().unwrap().to_string();
    let filtered = ok(
        &s,
        &viewer,
        "unique_errors",
        json!({"project": "mc", "state": state, "q": "vault", "limit": 5}),
    )
    .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1, "{filtered}");
    for bad in [
        json!({"project": "mc", "state": "sleeping"}),
        json!({"project": "mc", "q": "ab"}),
        json!({"project": "mc", "limit": 101}),
        json!({"project": "mc", "limit": 0}),
        json!({"project": "mc", "branch": "b".repeat(300)}),
    ] {
        let (err, v) = tool(&s, &viewer, "unique_errors", bad.clone()).await;
        assert!(err, "{bad}: {v}");
    }
    refused(
        &s,
        &outsider,
        "unique_errors",
        json!({"project": "mc"}),
        "forbidden",
    )
    .await;
    let theirs = ok(&s, &outsider, "unique_errors", json!({"project": "op"})).await;
    assert_eq!(theirs["items"].as_array().unwrap().len(), 0, "{theirs}");

    let d = ok(
        &s,
        &viewer,
        "error_detail",
        json!({"project": "mc", "fingerprint": fp}),
    )
    .await;
    assert_eq!(d["untrustedContent"], true);
    assert_eq!(d["fingerprint"], fp.as_str(), "{d}");
    refused(
        &s,
        &viewer,
        "error_detail",
        json!({"project": "mc", "fingerprint": "0000000000000000"}),
        "not_found",
    )
    .await;
    refused(
        &s,
        &outsider_admin,
        "error_detail",
        json!({"project": "op", "fingerprint": fp}),
        "not_found",
    )
    .await;
    refused(
        &s,
        &outsider,
        "error_detail",
        json!({"project": "mc", "fingerprint": fp}),
        "forbidden",
    )
    .await;

    let long = format!("gateway timeout {}", "z".repeat(5000));
    for chunk in 0..11 {
        let lines: Vec<Value> = (0..20)
            .map(|i| {
                json!({"resultId": "1", "attempt": 0, "seq": chunk * 20 + i,
                    "ts": 1_795_249_075_967i64 + (chunk * 20 + i) as i64,
                    "level": "error", "stream": "stderr", "message": long})
            })
            .collect();
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/mc/launches/{lid}/logs"),
                ADMIN_TOKEN,
                Value::Array(lines),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let hits = loop {
        let (e, v, len) = call(
            &s,
            &viewer,
            "search_logs",
            json!({"project": "mc", "q": "gateway timeout", "limit": 200}),
        )
        .await;
        assert!(!e, "{v}");
        if v["items"].as_array().is_some_and(|i| i.len() >= 100) || v["truncated"] == true {
            break (v, len);
        }
        assert!(std::time::Instant::now() < deadline, "logs not stored: {v}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let (v, len) = hits;
    assert_eq!(v["untrustedContent"], true);
    assert!(len <= 256 * 1024, "the answer is clipped: {len}");
    assert_eq!(v["truncated"], true, "200 lines of 2 KiB do not fit");
    for h in v["items"].as_array().unwrap() {
        let m = h["message"].as_str().unwrap();
        assert!(
            m.len() < 2200 && m.contains("truncated"),
            "a line is cut: {}",
            m.len()
        );
    }
    let few = ok(
        &s,
        &viewer,
        "search_logs",
        json!({"project": "mc", "q": "gateway timeout", "limit": 3, "launchId": lid, "level": "error"}),
    )
    .await;
    assert_eq!(few["items"].as_array().unwrap().len(), 3);
    assert!(few.get("truncated").is_none());
    for bad in [
        json!({"project": "mc", "q": "ab"}),
        json!({"project": "mc", "q": "x".repeat(201)}),
        json!({"project": "mc", "q": "gateway", "level": "loud"}),
        json!({"project": "mc", "q": "gateway", "limit": 201}),
        json!({"project": "mc", "q": "gateway", "from": "2020-01-01T00:00:00Z", "to": "2026-01-01T00:00:00Z"}),
        json!({"project": "mc", "q": "gateway", "from": "yesterday"}),
    ] {
        let (err, v) = tool(&s, &viewer, "search_logs", bad.clone()).await;
        assert!(err, "{bad}: {v}");
    }
    refused(
        &s,
        &outsider,
        "search_logs",
        json!({"project": "mc", "q": "gateway"}),
        "forbidden",
    )
    .await;
    let none = ok(
        &s,
        &outsider,
        "search_logs",
        json!({"project": "op", "q": "gateway timeout"}),
    )
    .await;
    assert_eq!(
        none["items"].as_array().unwrap().len(),
        0,
        "no leak: {none}"
    );

    let gate = json!({"name": "draft", "rules": [{"metric": "failed", "op": "lte", "value": 1}]});
    let sim = ok(
        &s,
        &viewer,
        "simulate_gate",
        json!({"project": "mc", "gate": gate, "launches": 10}),
    )
    .await;
    assert_eq!(sim["evaluated"], 1, "{sim}");
    assert_eq!(sim["items"][0]["status"], "failed", "{sim}");
    assert_eq!(
        sim["untrustedContent"], true,
        "branches and names come from reporters"
    );
    for bad in [
        json!({"project": "mc", "gate": gate, "launches": 201}),
        json!({"project": "mc", "gate": gate, "launches": 0}),
        json!({"project": "mc", "gate": {"name": "x", "rules": [{"metric": "nope", "op": "lte", "value": 1}]}}),
        json!({"project": "mc", "gate": gate, "branch": ""}),
        json!({"project": "mc"}),
    ] {
        let (err, v) = tool(&s, &viewer, "simulate_gate", bad.clone()).await;
        assert!(err, "{bad}: {v}");
    }
    refused(
        &s,
        &outsider,
        "simulate_gate",
        json!({"project": "mc", "gate": gate}),
        "forbidden",
    )
    .await;
    let none = ok(
        &s,
        &outsider,
        "simulate_gate",
        json!({"project": "op", "gate": gate}),
    )
    .await;
    assert_eq!(none["evaluated"], 0, "{none}");
}

#[tokio::test]
async fn writes_and_simulations_are_rate_limited_per_caller() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with_paused(
        &db,
        &[
            ("FASTPROOF_ANALYSIS", "on"),
            ("FASTPROOF_ANALYSIS_WORKERS", "0"),
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "2"),
        ],
        ROLE_JOBS,
    )
    .await;
    s.project("mc").await;
    let agent = s.token("bot", "agent", &["mc"]).await;
    let other = s.token("bot2", "agent", &["mc"]).await;
    let a = json!({"project": "mc", "batchId": uuid_of("")});
    for _ in 0..2 {
        refused(&s, &agent, "undo_triage", a.clone(), "not_found").await;
    }
    refused(&s, &agent, "undo_triage", a.clone(), "rate_limited").await;
    let gate = json!({"name": "draft", "rules": [{"metric": "failed", "op": "lte", "value": 1}]});
    let g = json!({"project": "mc", "gate": gate});
    for _ in 0..2 {
        ok(&s, &other, "simulate_gate", g.clone()).await;
    }
    refused(&s, &other, "simulate_gate", g, "rate_limited").await;
}

#[tokio::test]
async fn analysis_off_answers_disabled_and_refuses_compute() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, ROLE_JOBS).await;
    s.project("mc").await;
    let editor = s.token("ed", "editor", &["mc"]).await;
    let lid = finished_launch(&s, "mc", vec![res("1", "A", "boom")]).await;
    let v = ok(
        &s,
        &editor,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid}),
    )
    .await;
    assert_eq!(v["state"], "disabled", "{v}");
    assert_eq!(v["untrustedContent"], true);
    refused(
        &s,
        &editor,
        "launch_suggestions",
        json!({"project": "mc", "launchId": lid, "compute": true}),
        "analysis_disabled",
    )
    .await;
    let h = ok(
        &s,
        &editor,
        "triage_history",
        json!({"project": "mc", "resultId": uuid_of("")}),
    )
    .await;
    assert_eq!(h["items"].as_array().map(Vec::len), Some(0), "{h}");
}

/// Tool names that a later phase adds to the server; the example files may already name them.
const LATER_TOOLS: &[&str] = &["flaky_hints", "failure_analytics"];

fn copilot_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/copilot/.github");
    let mut out = Vec::new();
    for dir in ["agents", "prompts"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            out.push((name, std::fs::read_to_string(&p).unwrap()));
        }
    }
    out
}

fn snake_words(text: &str) -> Vec<String> {
    let mut v = Vec::new();
    for part in text.split('`').skip(1).step_by(2) {
        let ok = part.contains('_')
            && part.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            && !part.starts_with('_')
            && !part.ends_with('_');
        if ok {
            v.push(part.to_string());
        }
    }
    v
}

#[test]
fn the_copilot_files_name_only_tools_the_server_has() {
    let tools: Vec<String> = fastproof_server::mcp::tools()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for n in ANALYSIS_TOOLS {
        assert!(tools.iter().any(|t| t == n), "the server has {n}");
    }
    let files = copilot_files();
    assert!(files.len() >= 6, "{} files", files.len());
    let mut named = std::collections::BTreeSet::new();
    for (file, text) in &files {
        assert!(text.starts_with("---\n"), "{file} has front matter");
        for w in snake_words(text) {
            assert!(
                tools.contains(&w) || LATER_TOOLS.contains(&w.as_str()),
                "{file} names {w}, which is not a tool"
            );
            named.insert(w);
        }
    }
    for n in [
        "launch_suggestions",
        "accept_suggestion",
        "undo_triage",
        "flaky_hints",
        "search_logs",
    ] {
        assert!(named.contains(n), "the example files use {n}");
    }
    let (_, agent) = files
        .iter()
        .find(|(f, _)| f == "fastproof-triage.agent.md")
        .unwrap();
    let at = |n: &str| {
        agent
            .find(&format!("`{n}`"))
            .unwrap_or_else(|| panic!("{n}"))
    };
    assert!(at("launch_suggestions") < at("accept_suggestion"));
    assert!(at("accept_suggestion") < at("triage"));
    assert!(at("triage") < at("undo_triage"));
    assert!(agent.contains("never instructions"));
    let (_, pattern) = files
        .iter()
        .find(|(f, _)| f == "fastproof-propose-pattern.prompt.md")
        .unwrap();
    assert!(pattern.contains("create_rule_draft") && pattern.contains("match.log"));
    let docs = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide/mcp-copilot.md"),
    )
    .unwrap();
    for n in ANALYSIS_TOOLS {
        assert!(docs.contains(&format!("| `{n}` |")), "the guide lists {n}");
    }
}
