//! Night-1 MCP tools of TM next: FPQL in `search_cases`, `validate_query`, shared steps
//! (`list_shared_steps`, `get_shared_step`, `create_shared_step_draft`, shared steps in
//! `propose_case_update`), `review_queue` and `plan_dispatches`; the AI rules (AI drafts only,
//! reviewed before use, audited with the agent, rate limited) and the writes an agent never gets
//! (vote, transition, CI start, workflow).

mod common;

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

async fn server(db: &TestDb) -> Server {
    Server::start_with(db, &[("FASTPROOF_TM_GATE", "open")]).await
}

async fn tool_names(s: &Server, token: &str) -> Vec<String> {
    let (_, list) = mcp(
        s,
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn refused(err: bool, r: &Value, prefix: &str) {
    assert!(
        err && r.as_str().unwrap_or("").starts_with(prefix),
        "{prefix}: {r}"
    );
}

#[tokio::test]
async fn the_night_one_tools_exist_and_an_agent_gets_no_vote_transition_or_ci() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = server(&db).await;
    s.project("ai").await;
    let viewer = s.token("v", "viewer", &["ai"]).await;
    let names = tool_names(&s, &viewer).await;
    for t in [
        "validate_query",
        "list_shared_steps",
        "get_shared_step",
        "review_queue",
        "plan_dispatches",
        "create_shared_step_draft",
    ] {
        assert!(names.contains(&t.to_string()), "{t} missing: {names:?}");
    }
    for n in &names {
        for banned in [
            "vote",
            "approve",
            "transition",
            "dispatch_ci",
            "run_plan",
            "workflow",
            "start_launch",
            "inline",
            "purge",
        ] {
            assert!(!n.contains(banned), "{n} must not exist");
        }
    }
    for t in [
        "vote_case_review",
        "transition_case",
        "start_plan_launch",
        "put_case_workflow",
    ] {
        let (_, b) = mcp(
            &s,
            ADMIN_TOKEN,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": t, "arguments": {"project": "ai"}}}),
        )
        .await;
        assert!(
            b.get("error").is_some() || b["result"]["isError"] == true,
            "{t}: {b}"
        );
    }

    let (st, c) = s
        .post(
            "/api/v1/projects/ai/cases",
            ADMIN_TOKEN,
            json!({"name": "Case", "status": "active",
            "body": {"steps": [{"action": "Open", "expected": "Shown"}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    let agent = s.token("ag", "agent", &["ai"]).await;
    let (st, _) = s
        .post(
            "/api/v1/projects/ai/cases/1/transition",
            &agent,
            json!({"to": "outdated"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "an agent does not transition");
    let (st, _) = s
        .post(
            "/api/v1/projects/ai/cases/1/reviews",
            &agent,
            json!({"version": 1, "decision": "approve"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "an agent does not vote");
    let (st, _) = s
        .post(
            "/api/v1/projects/ai/test-plans/00000000-0000-7000-8000-000000000000/launches",
            &agent,
            json!({"name": "x", "ci": {"dispatch": true}}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "an agent does not start CI");
    let (st, _) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ai/case-workflow",
            Some(&agent),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "an agent does not change the workflow"
    );
}

#[tokio::test]
async fn fpql_in_search_cases_and_validate_query() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = server(&db).await;
    s.project("ai").await;
    s.project("other").await;
    for (name, status, tag) in [
        ("Pay by card", "active", "smoke"),
        ("Pay by cash", "draft", "smoke"),
        ("Refund", "active", "money"),
    ] {
        let (st, c) = s
            .post(
                "/api/v1/projects/ai/cases",
                ADMIN_TOKEN,
                json!({"name": name, "status": status, "tags": [tag],
                "body": {"steps": [{"action": "Do", "expected": "Done"}]}}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{c}");
    }
    let viewer = s.token("v", "viewer", &["ai"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;
    let (err, r) = tool(
        &s,
        &viewer,
        "search_cases",
        json!({"project": "ai", "query": "status = active and tag = smoke"}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["items"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["items"][0]["name"], "Pay by card");
    let (err, r) = tool(
        &s,
        &viewer,
        "search_cases",
        json!({"project": "ai", "query": "tag = smoke", "q": "cash"}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(
        r["items"][0]["name"], "Pay by cash",
        "the query is ANDed with the filters"
    );
    for bad in [
        "status = ",
        "name ~ 'a'",
        "x'; drop table cases; --",
        &"a ".repeat(3000),
    ] {
        let (err, r) = tool(
            &s,
            &viewer,
            "search_cases",
            json!({"project": "ai", "query": bad}),
        )
        .await;
        assert!(err, "{bad:.30}: {r}");
    }
    let n: i64 = sqlx::query_scalar("select count(*) from cases")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 3);

    let (err, r) = tool(
        &s,
        &viewer,
        "validate_query",
        json!({"project": "ai", "query": "status = active"}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(
        (r["ok"].as_bool(), r["count"].as_i64()),
        (Some(true), Some(2)),
        "{r}"
    );
    let (err, r) = tool(
        &s,
        &viewer,
        "validate_query",
        json!({"project": "ai", "query": "status = "}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["ok"], false);
    assert!(
        r["error"]["position"].is_number()
            || r["error"]["pos"].is_number()
            || r["error"].is_object(),
        "{r}"
    );
    let (err, r) = tool(
        &s,
        &viewer,
        "validate_query",
        json!({"project": "ai", "query": "status = active", "entity": "requirements"}),
    )
    .await;
    assert!(err, "requirement queries are not available yet: {r}");
    let (err, r) = tool(
        &s,
        &outsider,
        "validate_query",
        json!({"project": "ai", "query": "status = active"}),
    )
    .await;
    refused(err, &r, "forbidden");
    let (err, r) = tool(
        &s,
        &outsider,
        "search_cases",
        json!({"project": "ai", "query": "status = active"}),
    )
    .await;
    refused(err, &r, "forbidden");
}

#[tokio::test]
async fn shared_steps_are_read_drafted_by_agents_and_reviewed_before_use() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = server(&db).await;
    s.project("ai").await;
    s.project("other").await;
    let viewer = s.token("v", "viewer", &["ai"]).await;
    let agent = s.token("ag", "agent", &["ai"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;

    let (st, ss) = s
        .post("/api/v1/projects/ai/shared-steps", ADMIN_TOKEN, json!({"name": "Sign in", "description": "d",
            "steps": [{"id": "", "action": "Open /login", "expected": "Form", "data": "", "attachments": [], "steps": []}]}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{ss}");
    let (st, c) = s
        .post("/api/v1/projects/ai/cases", ADMIN_TOKEN, json!({"name": "Uses it", "status": "active",
            "body": {"steps": [{"id": "", "action": "", "expected": "", "data": "", "attachments": [], "steps": [],
                "shared": {"id": ss["id"], "number": ss["number"]}}]}}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");

    let (err, r) = tool(
        &s,
        &viewer,
        "list_shared_steps",
        json!({"project": "ai", "q": "Sign"}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["items"][0]["number"], 1);
    let (err, r) = tool(
        &s,
        &viewer,
        "get_shared_step",
        json!({"project": "ai", "number": 1}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["key"], "SS-1");
    assert!(r.get("html").is_none());
    assert_eq!(r["usages"]["items"][0]["caseNumber"], 1, "{r}");
    let (err, r) = tool(
        &s,
        &viewer,
        "get_shared_step",
        json!({"project": "ai", "number": 99}),
    )
    .await;
    refused(err, &r, "not_found");
    let (err, r) = tool(
        &s,
        &outsider,
        "get_shared_step",
        json!({"project": "ai", "number": 1}),
    )
    .await;
    refused(err, &r, "forbidden");
    let (err, r) = tool(&s, &outsider, "list_shared_steps", json!({"project": "ai"})).await;
    refused(err, &r, "forbidden");

    let (err, r) = tool(
        &s,
        &viewer,
        "create_shared_step_draft",
        json!({"project": "ai", "name": "x", "steps": []}),
    )
    .await;
    refused(err, &r, "forbidden");
    let (err, r) = tool(
        &s,
        &agent,
        "create_shared_step_draft",
        json!({"project": "ai", "name": "Pay", "steps": [
        {"action": "shared", "sharedStep": 1}]}),
    )
    .await;
    assert!(err, "no shared steps inside a shared step: {r}");
    let (err, r) = tool(&s, &agent, "create_shared_step_draft", json!({"project": "ai", "name": "Pay with a card",
        "description": "Card payment", "steps": [{"action": "Enter the card", "expected": "Accepted", "steps": [{"action": "Type the number"}]}],
        "agent": "copilot-steps"})).await;
    assert!(!err, "{r}");
    assert_eq!(
        (r["number"].as_i64(), r["aiUnreviewed"].as_bool()),
        (Some(2), Some(true))
    );
    let (st, v) = s
        .get("/api/v1/projects/ai/shared-steps/2", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (v["origin"].as_str(), v["aiUnreviewed"].as_bool()),
        (Some("ai"), Some(true)),
        "{v}"
    );

    let (_, audit) = s.get("/api/v1/audit?limit=100", ADMIN_TOKEN).await;
    let e = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "sharedStep.create" && i["detail"]["source"] == "ai")
        .expect("audited");
    assert_eq!(
        (e["detail"]["agent"].as_str(), e["detail"]["via"].as_str()),
        (Some("copilot-steps"), Some("mcp"))
    );

    let (_, case) = s.get("/api/v1/projects/ai/cases/1", ADMIN_TOKEN).await;
    let rev = case["revision"].as_i64().unwrap();
    let propose = |step: Value| {
        json!({"project": "ai", "number": 1, "baseRevision": rev, "rationale": "reuse",
        "steps": [step, {"action": "Check the receipt", "expected": "Shown"}]})
    };
    let (err, r) = tool(
        &s,
        &agent,
        "propose_case_update",
        propose(json!({"action": "", "sharedStep": 2})),
    )
    .await;
    assert!(err, "an unreviewed AI draft cannot be referenced: {r}");
    let (err, r) = tool(
        &s,
        &agent,
        "propose_case_update",
        propose(json!({"action": "", "sharedStep": 99})),
    )
    .await;
    assert!(err, "{r}");
    let (err, r) = tool(
        &s,
        &agent,
        "propose_case_update",
        propose(json!({"action": "text", "sharedStep": 1})),
    )
    .await;
    assert!(err, "a reference carries no text: {r}");
    let (err, r) = tool(
        &s,
        &agent,
        "propose_case_update",
        propose(json!({"action": "", "sharedStep": 1})),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["status"], "pending");
    let (_, sug) = s
        .get("/api/v1/projects/ai/cases/1/suggestions", ADMIN_TOKEN)
        .await;
    assert_eq!(
        sug["items"][0]["proposal"]["body"]["steps"][0]["shared"]["number"], 1,
        "{sug}"
    );
    let put = s
        .req(Method::PUT, "/api/v1/projects/ai/shared-steps/2", Some(ADMIN_TOKEN))
        .header("if-match", "\"r1\"")
        .json(&json!({"name": "Pay with a card", "description": "Card payment", "steps": [
            {"id": "", "action": "Enter the card", "expected": "Accepted", "data": "", "attachments": [], "steps": []}]}))
        .send()
        .await
        .unwrap();
    assert!(
        put.status().is_success(),
        "a person saves the draft: {}",
        put.status()
    );
    let (err, r) = tool(
        &s,
        &agent,
        "propose_case_update",
        propose(json!({"action": "", "sharedStep": 2})),
    )
    .await;
    assert!(!err, "after a person reviewed it: {r}");
    let (_, case) = s.get("/api/v1/projects/ai/cases/1", ADMIN_TOKEN).await;
    assert_eq!(case["revision"], rev, "nothing was applied");
}

#[tokio::test]
async fn review_queue_and_plan_dispatches_are_read_only_views() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = server(&db).await;
    s.project("ai").await;
    s.project("other").await;
    let agent = s.token("ag", "agent", &["ai"]).await;
    let viewer = s.token("v", "viewer", &["ai"]).await;
    let outsider = s.token("o", "editor", &["other"]).await;

    let (st, b) = s
        .call(Method::PUT, "/api/v1/projects/ai/settings", Some(ADMIN_TOKEN), Some(json!({"caseReview":
            {"mode": "manual", "approvals": 1, "resetOn": "content", "allowSelfApproval": false, "imports": "review"}})))
        .await;
    assert!(st.is_success(), "{st} {b}");
    let (st, c) = s
        .post(
            "/api/v1/projects/ai/cases",
            ADMIN_TOKEN,
            json!({"name": "Needs review", "status": "active",
            "body": {"steps": [{"action": "Do", "expected": "Done"}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    let (err, r) = tool(&s, &agent, "review_queue", json!({"project": "ai"})).await;
    assert!(!err, "{r}");
    let items = r["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["number"] == 1), "{r}");
    let (err, r) = tool(
        &s,
        &viewer,
        "review_queue",
        json!({"project": "ai", "kind": "case", "waitingForMe": true}),
    )
    .await;
    assert!(!err, "{r}");
    for bad in [
        json!({"kind": "x"}),
        json!({"state": "approved"}),
        json!({"state": "nope"}),
    ] {
        let mut a = bad.clone();
        a["project"] = json!("ai");
        let (err, r) = tool(&s, &viewer, "review_queue", a).await;
        assert!(err, "{bad}: {r}");
    }
    let (err, r) = tool(&s, &outsider, "review_queue", json!({"project": "ai"})).await;
    refused(err, &r, "forbidden");
    let votes: i64 = sqlx::query_scalar("select count(*) from review_votes")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(votes, 0);

    let (st, plan) = s
        .post(
            "/api/v1/projects/ai/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Plan", "caseNumbers": [1]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    let (err, r) = tool(
        &s,
        &viewer,
        "plan_dispatches",
        json!({"project": "ai", "planId": pid}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["items"], json!([]));
    let (project_id,): (uuid::Uuid,) = sqlx::query_as("select id from projects where key = 'ai'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let launch = s.launch("ai", None).await;
    let lid: uuid::Uuid = launch["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("insert into plan_dispatches (id, project_id, plan_id, launch_id, provider, status, error, ci_ref, git_ref, requested_by) \
         values (gen_random_uuid(), $1, $2, $3, 'github', 'failed', 'egress denied: ci.internal.example is not allowed', 'https://ci.internal.example/runs/9', 'main', 'someone')")
        .bind(project_id)
        .bind(pid.parse::<uuid::Uuid>().unwrap())
        .bind(lid)
        .execute(&db.pool)
        .await
        .unwrap();
    let (err, r) = tool(
        &s,
        &viewer,
        "plan_dispatches",
        json!({"project": "ai", "planId": pid}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["items"][0]["status"], "failed");
    let text = r.to_string();
    assert!(
        !text.contains("internal") && !text.contains("egress"),
        "generic errors for readers: {text}"
    );
    let (err, r) = tool(
        &s,
        ADMIN_TOKEN,
        "plan_dispatches",
        json!({"project": "ai", "planId": pid}),
    )
    .await;
    assert!(
        !err && r.to_string().contains("ci.internal.example"),
        "admins see why: {r}"
    );
    let (err, r) = tool(
        &s,
        &outsider,
        "plan_dispatches",
        json!({"project": "ai", "planId": pid}),
    )
    .await;
    refused(err, &r, "forbidden");
    let (err, r) = tool(
        &s,
        ADMIN_TOKEN,
        "plan_dispatches",
        json!({"project": "other", "planId": pid}),
    )
    .await;
    refused(err, &r, "not_found");
}

#[tokio::test]
async fn shared_step_drafts_are_rate_limited() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_GATE", "open"),
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "2"),
        ],
    )
    .await;
    s.project("ai").await;
    let agent = s.token("ag", "agent", &["ai"]).await;
    let mut limited = 0;
    for i in 0..5 {
        let (err, r) = tool(
            &s,
            &agent,
            "create_shared_step_draft",
            json!({"project": "ai", "name": format!("S{i}"),
            "steps": [{"action": "a"}]}),
        )
        .await;
        if err {
            assert!(r.as_str().unwrap().contains("rate"), "{r}");
            limited += 1;
        }
    }
    assert!(limited >= 3, "{limited}");
    let n: i64 = sqlx::query_scalar("select count(*) from shared_steps")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(n <= 2, "{n}");
}
