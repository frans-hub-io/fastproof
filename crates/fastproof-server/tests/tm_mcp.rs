//! Test-management MCP tools over HTTP: every tool with its permission, the AI rules (drafts
//! only, `origin = ai`, `aiUnreviewed`, no direct edit of an existing case, suggestions
//! against a revision), audit records with the agent, and `trace_actions` without typed
//! values.

mod common;

use std::io::Write;

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

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

/// Calls a tool: `(is_error, parsed JSON or the error text)`.
async fn tool(s: &Server, token: &str, name: &str, args: Value) -> (bool, Value) {
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
    (
        err,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn trace_zip() -> Vec<u8> {
    let lines = [
        json!({"type": "context-options", "version": 8}),
        json!({"type": "before", "callId": "call@1", "startTime": 100.0, "apiName": "page.goto", "class": "Frame", "method": "goto",
            "params": {"url": "https://shop.example.com/login"}}),
        json!({"type": "after", "callId": "call@1", "endTime": 350.0}),
        json!({"type": "before", "callId": "call@2", "startTime": 400.0, "apiName": "locator.fill", "class": "Frame", "method": "fill",
            "params": {"selector": "#password", "value": "hunter2-SECRET"}}),
        json!({"type": "after", "callId": "call@2", "endTime": 420.0}),
        json!({"type": "before", "callId": "call@3", "startTime": 430.0, "title": "Press \"Enter\"", "class": "Frame", "method": "press",
            "params": {"selector": "#password", "key": "Enter"}}),
        json!({"type": "after", "callId": "call@3", "endTime": 440.0}),
        json!({"type": "before", "callId": "call@4", "startTime": 500.0, "apiName": "expect.toBeVisible", "class": "Frame", "method": "expect",
            "params": {"selector": "text=Welcome"}}),
        json!({"type": "after", "callId": "call@4", "endTime": 5500.0, "error": {"message": "Timed out 5000ms waiting for expect(locator).toBeVisible()"}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("trace.trace", o).unwrap();
        z.write_all(text.as_bytes()).unwrap();
        z.start_file("trace.network", o).unwrap();
        z.write_all(b"{}\n").unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn results_zip() -> Vec<u8> {
    let result = json!({"uuid": "r1", "historyId": "h1", "name": "checkout works", "fullName": "shop.CheckoutTest#works",
        "status": "failed", "statusDetails": {"message": "expected the receipt"}, "start": 1795249075967i64, "stop": 1795249076967i64,
        "steps": [{"name": "open the cart", "status": "passed", "steps": [{"name": "click the cart icon", "status": "passed"}]},
                  {"name": "pay", "status": "failed"}],
        "attachments": [{"name": "trace", "source": "r1-trace.zip", "type": "application/vnd.allure.playwright-trace"}],
        "parameters": [{"name": "password", "value": "hunter2-SECRET", "mode": "masked"}]});
    let container = json!({"uuid": "c1", "children": ["r1"], "befores": [{"name": "log in as a customer", "status": "passed"}]});
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("r1-result.json", o).unwrap();
        z.write_all(result.to_string().as_bytes()).unwrap();
        z.start_file("c1-container.json", o).unwrap();
        z.write_all(container.to_string().as_bytes()).unwrap();
        z.start_file("r1-trace.zip", o).unwrap();
        z.write_all(&trace_zip()).unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn tm_tools_rules_permissions_and_trace() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ai").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["ai"]).await;
    let editor = s.token("e", "editor", &["ai"]).await;
    let tester = s.token("t", "tester", &["ai"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;

    let (_, list) = mcp(
        &s,
        &viewer,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for t in [
        "search_cases",
        "get_case",
        "case_history",
        "list_test_plans",
        "get_test_plan",
        "manual_run_progress",
        "trace_actions",
        "draft_case_from_result",
        "create_case_draft",
        "propose_case_update",
        "add_cases_to_plan",
    ] {
        assert!(names.contains(&t.to_string()), "{t} missing: {names:?}");
    }
    assert!(
        !names.iter().any(
            |n| n.contains("update_case") || (n.contains("accept") && n != "accept_suggestion")
        ),
        "no direct edit or case accept tool (accept_suggestion decides failure clusters, not cases): {names:?}"
    );

    let (st, c) = s
        .post("/api/v1/projects/ai/cases", ADMIN_TOKEN, json!({"name": "Active case", "status": "active",
            "body": {"steps": [{"action": "Open the shop", "expected": "Shown"}]}, "tags": ["shop"]}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");

    let (err, r) = tool(
        &s,
        &viewer,
        "search_cases",
        json!({"project": "ai", "q": "Active"}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["items"][0]["number"], 1);
    let (err, r) = tool(
        &s,
        &viewer,
        "get_case",
        json!({"project": "ai", "number": 1}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["key"], "TC-1");
    assert!(r.get("html").is_none());
    let (err, r) = tool(
        &s,
        &viewer,
        "case_history",
        json!({"project": "ai", "number": 1}),
    )
    .await;
    assert!(!err, "{r}");
    let (err, r) = tool(
        &s,
        &outsider,
        "get_case",
        json!({"project": "ai", "number": 1}),
    )
    .await;
    assert!(err && r.as_str().unwrap().starts_with("forbidden"), "{r}");
    let (err, r) = tool(
        &s,
        &viewer,
        "get_case",
        json!({"project": "ai", "number": 99}),
    )
    .await;
    assert!(err && r.as_str().unwrap().starts_with("not_found"), "{r}");

    for (name, args) in [
        ("create_case_draft", json!({"project": "ai", "name": "x"})),
        (
            "propose_case_update",
            json!({"project": "ai", "number": 1, "baseRevision": 1, "name": "x", "rationale": "r"}),
        ),
        (
            "draft_case_from_result",
            json!({"project": "ai", "resultId": uuid::Uuid::now_v7()}),
        ),
        (
            "add_cases_to_plan",
            json!({"project": "ai", "planId": uuid::Uuid::now_v7(), "numbers": [1]}),
        ),
    ] {
        for t in [&viewer, &tester] {
            let (err, r) = tool(&s, t, name, args.clone()).await;
            assert!(
                err && r.as_str().unwrap().starts_with("forbidden"),
                "{name}: {r}"
            );
        }
    }

    let (err, r) = tool(
        &s,
        &editor,
        "create_case_draft",
        json!({"project": "ai", "name": "From a requirement", "status": "active"}),
    )
    .await;
    assert!(
        err && r.as_str().unwrap().contains("invalid arguments"),
        "an agent cannot set the status: {r}"
    );
    let (err, r) = tool(&s, &editor, "create_case_draft", json!({"project": "ai", "name": "From a requirement",
        "description": "Users can pay by card", "steps": [{"action": "Pay", "expected": "Paid", "steps": [{"action": "Enter the card"}]}],
        "tags": ["payments"], "fields": {"priority": "high"}, "agent": "copilot-cases"})).await;
    assert!(!err, "{r}");
    assert_eq!(r["status"], "draft");
    let n = r["number"].as_i64().unwrap();
    let (_, c2) = s
        .get(&format!("/api/v1/projects/ai/cases/{n}"), ADMIN_TOKEN)
        .await;
    assert_eq!(c2["status"], "draft");
    assert_eq!(c2["origin"], "ai");
    assert_eq!(c2["aiUnreviewed"], true);
    assert_eq!(c2["fields"]["priority"], "high");
    assert_eq!(
        c2["body"]["steps"][0]["steps"][0]["action"],
        "Enter the card"
    );
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/ai/cases/{n}/versions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(v["items"][0]["source"], "ai");

    let (err, r) = tool(
        &s,
        &editor,
        "propose_case_update",
        json!({"project": "ai", "number": 1, "baseRevision": 7,
        "name": "Renamed", "rationale": "clearer"}),
    )
    .await;
    assert!(
        err && r.as_str().unwrap().starts_with("suggestion_stale"),
        "{r}"
    );
    let (err, r) = tool(
        &s,
        &editor,
        "propose_case_update",
        json!({"project": "ai", "number": 1, "baseRevision": 1,
        "rationale": "nothing"}),
    )
    .await;
    assert!(err, "{r}");
    let (err, r) = tool(&s, &editor, "propose_case_update", json!({"project": "ai", "number": 1, "baseRevision": 1,
        "name": "Active case, clearer", "steps": [{"action": "Open the shop home page", "expected": "The home page is shown"}],
        "rationale": "more precise steps", "agent": "copilot-cases"})).await;
    assert!(!err, "{r}");
    assert_eq!(r["status"], "pending");
    let (_, c1) = s.get("/api/v1/projects/ai/cases/1", ADMIN_TOKEN).await;
    assert_eq!(c1["name"], "Active case", "a proposal changes nothing");
    assert_eq!(c1["revision"], 1);
    let (_, sug) = s
        .get("/api/v1/projects/ai/cases/1/suggestions", ADMIN_TOKEN)
        .await;
    assert_eq!(sug["items"][0]["agent"], "copilot-cases");
    assert_eq!(sug["items"][0]["proposal"]["name"], "Active case, clearer");
    assert_eq!(sug["items"][0]["status"], "pending");
    let (err, r) = tool(
        &s,
        &editor,
        "propose_case_update",
        json!({"project": "ai", "number": 1, "baseRevision": 1,
        "description": "x".repeat(20_000), "rationale": "too long"}),
    )
    .await;
    assert!(
        err && r.as_str().unwrap().contains("invalid_markdown"),
        "proposals are validated: {r}"
    );

    let (st, l) = s
        .post(
            "/api/v1/projects/ai/launches",
            ADMIN_TOKEN,
            json!({"name": "web"}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let lid = l["id"].as_str().unwrap().to_string();
    let u = s.ingest("ai", &lid, results_zip(), "").await;
    assert_eq!(u["status"], "done", "{u}");
    let results = s.results("ai", &lid, "").await;
    let rid = results[0]["id"].as_str().unwrap().to_string();

    let (err, r) = tool(
        &s,
        &viewer,
        "trace_actions",
        json!({"project": "ai", "resultId": rid}),
    )
    .await;
    assert!(!err, "{r}");
    let text = r.to_string();
    assert!(
        !text.contains("hunter2"),
        "typed values never leave: {text}"
    );
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 4, "{r}");
    assert_eq!(actions[0]["apiName"], "page.goto");
    assert_eq!(actions[0]["url"], "https://shop.example.com/login");
    assert_eq!(actions[0]["durationMs"], 250);
    assert_eq!(actions[1]["apiName"], "locator.fill");
    assert_eq!(actions[1]["selector"], "#password");
    assert_eq!(actions[3]["status"], "failed");
    assert!(actions[3]["error"].as_str().unwrap().contains("Timed out"));
    let (err, r) = tool(
        &s,
        &outsider,
        "trace_actions",
        json!({"project": "ai", "resultId": rid}),
    )
    .await;
    assert!(err && r.as_str().unwrap().starts_with("forbidden"), "{r}");
    let (err, r) = tool(
        &s,
        ADMIN_TOKEN,
        "trace_actions",
        json!({"project": "other", "resultId": rid}),
    )
    .await;
    assert!(
        err && r.as_str().unwrap().starts_with("not_found"),
        "another project's result: {r}"
    );

    let (err, r) = tool(
        &s,
        &editor,
        "draft_case_from_result",
        json!({"project": "ai", "resultId": rid,
        "notes": "Seen in nightly runs.", "agent": "copilot-cases"}),
    )
    .await;
    assert!(!err, "{r}");
    let dn = r["number"].as_i64().unwrap();
    let (_, d) = s
        .get(&format!("/api/v1/projects/ai/cases/{dn}"), ADMIN_TOKEN)
        .await;
    assert_eq!(d["name"], "checkout works");
    assert_eq!(d["status"], "draft");
    assert_eq!(d["origin"], "ai");
    assert_eq!(d["aiUnreviewed"], true);
    assert_eq!(d["body"]["preconditions"], "- log in as a customer");
    assert_eq!(d["body"]["steps"][0]["action"], "open the cart");
    assert_eq!(
        d["body"]["steps"][0]["steps"][0]["action"],
        "click the cart icon"
    );
    let desc = d["body"]["description"].as_str().unwrap();
    assert!(
        desc.contains("expected the receipt") && desc.contains("Seen in nightly runs."),
        "{desc}"
    );
    assert!(!d.to_string().contains("hunter2"));

    let (st, plan) = s
        .post(
            "/api/v1/projects/ai/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Regression", "caseNumbers": [1]}),
        )
        .await;
    assert!(st.is_success(), "{plan}");
    let pid = plan["id"].as_str().unwrap().to_string();
    let (err, r) = tool(
        &s,
        &editor,
        "add_cases_to_plan",
        json!({"project": "ai", "planId": pid, "numbers": [n, 999]}),
    )
    .await;
    assert!(err, "unknown cases are refused: {r}");
    let (err, r) = tool(
        &s,
        &editor,
        "add_cases_to_plan",
        json!({"project": "ai", "planId": pid, "numbers": [n], "agent": "copilot-cases"}),
    )
    .await;
    assert!(!err, "{r}");
    let (err, r) = tool(&s, &viewer, "list_test_plans", json!({"project": "ai"})).await;
    assert!(!err, "{r}");
    assert_eq!(r["items"][0]["name"], "Regression");
    let (err, r) = tool(
        &s,
        &viewer,
        "get_test_plan",
        json!({"project": "ai", "planId": pid}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["cases"]["items"].as_array().unwrap().len(), 2, "{r}");
    let (err, r) = tool(
        &s,
        ADMIN_TOKEN,
        "get_test_plan",
        json!({"project": "other", "planId": pid}),
    )
    .await;
    assert!(err && r.as_str().unwrap().starts_with("not_found"), "{r}");

    let (st, pl) = s
        .post(
            &format!("/api/v1/projects/ai/test-plans/{pid}/launches"),
            ADMIN_TOKEN,
            json!({"name": "manual regression", "assign": "none", "includeAutomated": false}),
        )
        .await;
    assert!(st.is_success(), "{pl}");
    let mlid = pl["launch"]["id"].as_str().unwrap().to_string();
    let (_, items) = s
        .get(
            &format!("/api/v1/projects/ai/launches/{mlid}/run-items"),
            ADMIN_TOKEN,
        )
        .await;
    let item = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["caseNumber"] == 1)
        .unwrap()
        .clone();
    let iid = item["id"].as_str().unwrap();
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/ai/run-items/{iid}/steps/1"),
            Some(ADMIN_TOKEN),
            Some(json!({"status": "failed", "actual": "The shop shows error 500"})),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/ai/run-items/{iid}"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    let (st, b) = {
        let r = s
            .req(
                Method::PATCH,
                &format!("/api/v1/projects/ai/run-items/{iid}"),
                Some(ADMIN_TOKEN),
            )
            .header("if-match", etag)
            .json(&json!({"status": "failed"}))
            .send()
            .await
            .unwrap();
        (r.status(), r.text().await.unwrap())
    };
    assert!(st.is_success(), "{b}");
    let (err, r) = tool(
        &s,
        &tester,
        "manual_run_progress",
        json!({"project": "ai", "launchId": mlid}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["manual"]["failed"], 1, "{r}");
    assert_eq!(r["failedItems"][0]["caseNumber"], 1);
    assert_eq!(
        r["failedItems"][0]["failedSteps"][0]["actual"],
        "The shop shows error 500"
    );
    let (err, r) = tool(
        &s,
        &tester,
        "manual_run_progress",
        json!({"project": "ai", "launchId": lid}),
    )
    .await;
    assert!(
        err && r.as_str().unwrap().starts_with("no_manual_run"),
        "{r}"
    );

    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    let items = a["items"].as_array().unwrap();
    for action in ["case.create", "caseSuggestion.create", "testPlan.items"] {
        let e = items
            .iter()
            .find(|i| i["action"] == action && i["detail"]["source"] == "ai")
            .unwrap_or_else(|| panic!("no AI audit for {action}"));
        assert_eq!(e["detail"]["agent"], "copilot-cases", "{e}");
    }
}
