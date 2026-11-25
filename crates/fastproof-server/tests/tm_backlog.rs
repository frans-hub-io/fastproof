//! Test-management backlog: agent tokens held to the AI rules on REST, optional request
//! fields, the per-caller rate of multiplying writes and the cap on open plan launches,
//! pruning of case versions, people from bound groups with display names, defect types by
//! group name over MCP, case numbers taken outside the create transaction, and custom columns
//! of CSV imports created as fields.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

struct Reply {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
    retry_after: Option<String>,
}

async fn send(
    s: &Server,
    method: Method,
    path: &str,
    token: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut rb = s.req(method, path, Some(token));
    if let Some(m) = if_match {
        rb = rb.header("if-match", m);
    }
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.unwrap();
    let status = r.status();
    let header = |k: &str| {
        r.headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let etag = header("etag");
    let retry_after = header("retry-after");
    let text = r.text().await.unwrap();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        etag,
        retry_after,
    }
}

async fn mcp_tool(s: &Server, token: &str, name: &str, args: Value) -> (bool, Value) {
    let r = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": args}}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let text = r.text().await.unwrap();
    let body = text
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .map(|d| d.trim().to_string())
        .unwrap_or(text);
    let b: Value = serde_json::from_str(&body).unwrap();
    let res = &b["result"];
    let t = res["content"][0]["text"].as_str().unwrap_or("").to_string();
    (
        res["isError"].as_bool().unwrap_or(false),
        serde_json::from_str(&t).unwrap_or(Value::String(t)),
    )
}

async fn new_case(s: &Server, key: &str, name: &str) -> i64 {
    let r = send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/cases"),
        ADMIN_TOKEN,
        None,
        Some(json!({"name": name, "body": {"steps": [{"action": "Open"}]}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r.body["number"].as_i64().unwrap()
}

/// M1 / F13: a token with the `agent` role drafts and proposes over MCP, but over REST it can
/// neither create, change, review, activate nor bulk-change cases, nor accept or reject
/// suggestions, whoever made them; a person (editor) is not affected.
#[tokio::test]
async fn agent_tokens_are_held_to_the_ai_rules_on_rest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ag").await;
    let agent = s.token("copilot", "agent", &["ag"]).await;
    let other = s.token("copilot-2", "agent", &["ag"]).await;
    let person = s.token("ann", "editor", &["ag"]).await;
    let base = "/api/v1/projects/ag";

    let (err, d) = mcp_tool(
        &s,
        &agent,
        "create_case_draft",
        json!({"project": "ag", "name": "Logs in", "steps": [{"action": "Open"}], "agent": "copilot"}),
    )
    .await;
    assert!(!err, "{d}");
    let n = d["number"].as_i64().unwrap();
    let get = send(
        &s,
        Method::GET,
        &format!("{base}/cases/{n}"),
        &agent,
        None,
        None,
    )
    .await;
    assert_eq!(
        get.status,
        StatusCode::OK,
        "agents read cases: {}",
        get.body
    );
    let tag = get.etag.clone().unwrap();
    let rev = get.body["revision"].as_i64().unwrap();
    let (err, p) = mcp_tool(
        &s,
        &other,
        "propose_case_update",
        json!({"project": "ag", "number": n, "baseRevision": rev, "name": "Logs in with a password",
            "rationale": "clearer"}),
    )
    .await;
    assert!(!err, "{p}");
    let sid = p["suggestionId"].as_str().unwrap().to_string();

    let forbidden = [
        (Method::POST, format!("{base}/cases/{n}/review"), None),
        (
            Method::PATCH,
            format!("{base}/cases/{n}"),
            Some(json!({"status": "active"})),
        ),
        (
            Method::PUT,
            format!("{base}/cases/{n}"),
            Some(json!({"name": "Logs in", "status": "active"})),
        ),
        (
            Method::POST,
            format!("{base}/cases"),
            Some(json!({"name": "Direct", "status": "active"})),
        ),
        (
            Method::POST,
            format!("{base}/cases/bulk"),
            Some(json!({"numbers": [n], "action": "setStatus", "value": "active"})),
        ),
        (Method::DELETE, format!("{base}/cases/{n}"), None),
        (
            Method::POST,
            format!("{base}/case-suggestions/{sid}/accept"),
            None,
        ),
        (
            Method::POST,
            format!("{base}/case-suggestions/{sid}/reject"),
            None,
        ),
    ];
    for (m, path, body) in &forbidden {
        for t in [&agent, &other] {
            let r = send(&s, m.clone(), path, t, Some(&tag), body.clone()).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {path}: {}", r.body);
        }
    }
    let r = send(
        &s,
        Method::GET,
        &format!("{base}/cases/{n}"),
        &agent,
        None,
        None,
    )
    .await;
    assert_eq!(r.body["status"], "draft");
    assert_eq!(r.body["aiUnreviewed"], true);
    assert_eq!(r.body["name"], "Logs in");

    let (err, list) = mcp_tool(&s, &agent, "list_defect_types", json!({"project": "ag"})).await;
    assert!(!err, "{list}");
    let (err, e) = mcp_tool(
        &s,
        &agent,
        "add_cases_to_plan",
        json!({"project": "ag", "planId": uuid::Uuid::now_v7(), "numbers": [n]}),
    )
    .await;
    assert!(err && e.to_string().contains("forbidden"), "{e}");

    let r = send(
        &s,
        Method::POST,
        &format!("{base}/case-suggestions/{sid}/accept"),
        &person,
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "a person accepts: {}", r.body);
    let tag = r.etag.unwrap();
    let r = send(
        &s,
        Method::PATCH,
        &format!("{base}/cases/{n}"),
        &person,
        Some(&tag),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["aiUnreviewed"], false);
    assert_eq!(r.body["name"], "Logs in with a password");
}

/// F15: `list_defect_types` lists the built-in groups and `triage` / `create_rule_draft` take
/// a group name for the group's built-in type; an unknown name is an error.
#[tokio::test]
async fn defect_types_by_group_name_over_mcp() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dt").await;
    let agent = s.token("triager", "agent", &["dt"]).await;
    let viewer = s.token("v", "viewer", &["dt"]).await;
    let (err, e) = mcp_tool(&s, &viewer, "list_defect_types", json!({"project": "dt"})).await;
    assert!(err && e.to_string().contains("forbidden"), "{e}");
    let (err, list) = mcp_tool(&s, &agent, "list_defect_types", json!({"project": "dt"})).await;
    assert!(!err, "{list}");
    let items = list["items"].as_array().unwrap();
    let groups: Vec<&str> = items.iter().map(|t| t["group"].as_str().unwrap()).collect();
    assert_eq!(
        groups,
        [
            "productBug",
            "automationBug",
            "systemIssue",
            "noDefect",
            "toInvestigate"
        ]
    );
    let pb = items[0]["id"].as_str().unwrap().to_string();

    let l = s.launch("dt", None).await;
    let launch = l["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/dt/launches/{launch}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "adds", "fullName": "shop.Cart#adds", "status": "failed",
                "start": 1795249075967i64, "stop": 1795249076967i64,
                "error": {"message": "boom"}}]),
        )
        .await;
    assert!(st.is_success() && b["accepted"] == 1, "{b}");
    let deadline = Instant::now() + Duration::from_secs(60);
    let result = loop {
        let (_, r) = s
            .get(
                &format!("/api/v1/projects/dt/launches/{launch}/results?limit=10"),
                ADMIN_TOKEN,
            )
            .await;
        if let Some(id) = r["items"][0]["id"].as_str() {
            break id.to_string();
        }
        assert!(Instant::now() < deadline, "no result: {r}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (err, out) = mcp_tool(
        &s,
        &agent,
        "triage",
        json!({"project": "dt", "resultIds": [result], "defectTypeId": "product_bug", "comment": "cart"}),
    )
    .await;
    assert!(!err, "{out}");
    let (_, r) = s
        .get(
            &format!("/api/v1/projects/dt/results/{result}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(r["defect"]["typeId"], pb.as_str(), "{r}");

    let (err, e) = mcp_tool(
        &s,
        &agent,
        "triage",
        json!({"project": "dt", "resultIds": [result], "defectTypeId": "flaky"}),
    )
    .await;
    assert!(err && e.to_string().contains("unknown defect type"), "{e}");

    let (err, rule) = mcp_tool(
        &s,
        &agent,
        "create_rule_draft",
        json!({"project": "dt", "name": "Cart", "match": {"messageRegex": "boom"},
            "action": {"defectTypeId": "systemIssue"}}),
    )
    .await;
    assert!(!err, "agents draft rules: {rule}");
    let si = items[2]["id"].as_str().unwrap();
    assert!(rule.to_string().contains(si), "{rule}");
}

/// F5: requests with only the documented required fields succeed.
#[tokio::test]
async fn optional_request_fields_have_defaults() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("opt").await;
    let base = "/api/v1/projects/opt";
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/cases"),
        ADMIN_TOKEN,
        None,
        Some(json!({"name": "No body"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["body"]["steps"], json!([]));
    let n = new_case(&s, "opt", "Has steps").await;
    let g = send(
        &s,
        Method::GET,
        &format!("{base}/cases/{n}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/cases/{n}"),
        ADMIN_TOKEN,
        g.etag.as_deref(),
        Some(json!({"name": "Renamed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["name"], "Renamed");
    assert_eq!(
        r.body["body"]["steps"][0]["action"], "Open",
        "PUT without body keeps it"
    );

    let (st, plan) = s
        .post(
            &format!("{base}/test-plans"),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P", "caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    let (st, b) = s
        .post(
            &format!("{base}/test-plans/{pid}/launches"),
            ADMIN_TOKEN,
            json!({"name": "x", "externalId": "nightly-1"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["ciHint"], "", "includeAutomated defaults to false");
    let launch = b["launch"]["id"].as_str().unwrap().to_string();
    let (_, items) = s
        .get(&format!("{base}/launches/{launch}/run-items"), ADMIN_TOKEN)
        .await;
    assert!(
        items["items"][0]["assignee"].is_null(),
        "assign defaults to none: {items}"
    );
    let (st, b) = s
        .post(
            &format!("{base}/launches/{launch}/manual/close"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "untestedTo defaults to skipped: {b}");
    let (_, items) = s
        .get(&format!("{base}/launches/{launch}/run-items"), ADMIN_TOKEN)
        .await;
    assert_eq!(items["items"][0]["status"], "skipped", "{items}");

    let csv = b"Title,Steps\nLogin,1. Open\n";
    let (ct, body) = multipart(&[
        ("options", None, br#"{"format":"generic-csv"}"#),
        ("file", Some("cases.csv"), csv),
    ]);
    let r = s
        .req(Method::POST, &format!("{base}/imports"), Some(ADMIN_TOKEN))
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    let j: Value = r.json().await.unwrap();
    assert_eq!(
        st,
        StatusCode::ACCEPTED,
        "mode and unknownFieldValues default: {j}"
    );
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(&s, "opt", &id, "ready").await;
    assert_eq!(ready["report"]["create"], 1, "{ready}");
}

/// L4: plan launches and other multiplying writes are rate-limited per caller (429
/// `rate_limited` with `Retry-After`), and one caller keeps at most
/// `FASTPROOF_TM_MAX_OPEN_PLAN_LAUNCHES` launches with an open manual run per project.
#[tokio::test]
async fn multiplying_writes_are_capped_per_caller() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "3"),
            ("FASTPROOF_TM_MAX_OPEN_PLAN_LAUNCHES", "2"),
        ],
    )
    .await;
    s.project("cap").await;
    let base = "/api/v1/projects/cap";
    let tester = s.token("tess", "tester", &["cap"]).await;
    let other = s.token("tom", "tester", &["cap"]).await;
    let n = new_case(&s, "cap", "Manual").await;
    let (st, plan) = s
        .post(
            &format!("{base}/test-plans"),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P", "caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    let start = |t: String, ext: &str| {
        let path = format!("{base}/test-plans/{pid}/launches");
        let body = json!({"name": "run", "externalId": ext});
        let s = &s;
        async move { send(s, Method::POST, &path, &t, None, Some(body)).await }
    };
    assert_eq!(start(tester.clone(), "a").await.status, StatusCode::CREATED);
    assert_eq!(start(tester.clone(), "b").await.status, StatusCode::CREATED);
    let r = start(tester.clone(), "c").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "limit_reached", "{}", r.body);
    assert_eq!(
        r.body["details"]["limit"], "maxOpenPlanLaunches",
        "{}",
        r.body
    );
    let r = start(tester.clone(), "d").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.body);
    assert_eq!(r.body["code"], "rate_limited", "{}", r.body);
    let wait: u64 = r.retry_after.unwrap().parse().unwrap();
    assert!((1..=20).contains(&wait), "{wait}");
    assert_eq!(
        start(other.clone(), "e").await.status,
        StatusCode::CREATED,
        "another caller has its own budget and cap"
    );
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/cases/bulk"),
        &other,
        None,
        Some(json!({"numbers": [n], "action": "addTags", "value": ["x"]})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "permission first: {}",
        r.body
    );
}

/// L5: with `retention.caseVersionsKeep` the prune job keeps the newest versions of each
/// case, the versions run items were run against, and nothing in projects without the
/// setting.
#[tokio::test]
async fn case_versions_are_pruned_by_retention() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["cases.versions.prune"]).await;
    s.project("ver").await;
    s.project("all").await;
    let base = "/api/v1/projects/ver";
    let n = new_case(&s, "ver", "V").await;
    let keep_all = new_case(&s, "all", "V").await;
    let save = |key: &'static str, num: i64, name: String| {
        let s = &s;
        async move {
            let path = format!("/api/v1/projects/{key}/cases/{num}");
            let g = send(s, Method::GET, &path, ADMIN_TOKEN, None, None).await;
            let r = send(
                s,
                Method::PATCH,
                &path,
                ADMIN_TOKEN,
                g.etag.as_deref(),
                Some(json!({"tags": [name]})),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        }
    };
    save("ver", n, "v2".into()).await;
    let (st, plan) = s
        .post(
            &format!("{base}/test-plans"),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P", "caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    let (st, b) = s
        .post(
            &format!("{base}/test-plans/{pid}/launches"),
            ADMIN_TOKEN,
            json!({"name": "run"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    for i in 3..=7 {
        save("ver", n, format!("v{i}")).await;
        save("all", keep_all, format!("v{i}")).await;
    }
    let (st, settings) = s.get(&format!("{base}/settings"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let mut retention = settings["retention"].clone();
    retention["caseVersionsKeep"] = json!(2);
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/settings"),
        ADMIN_TOKEN,
        None,
        Some(json!({"retention": retention})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let mut bad = retention.clone();
    bad["caseVersionsKeep"] = json!(100_001);
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/settings"),
        ADMIN_TOKEN,
        None,
        Some(json!({"retention": bad})),
    )
    .await;
    assert_eq!(r.status.as_u16() / 100, 4, "{}", r.body);

    let fresh = new_case(&s, "ver", "Fresh").await;
    for i in 2..=3 {
        save("ver", fresh, format!("f{i}")).await;
    }
    sqlx::query(
        "update case_versions set created_at = created_at - interval '2 hours' \
         where case_id = (select c.id from cases c join projects p on p.id = c.project_id \
           where p.key = 'ver' and c.number = $1)",
    )
    .bind(n)
    .execute(&db.pool)
    .await
    .unwrap();
    let store = fastproof_store::Store::new(db.pool.clone());
    let deleted = store
        .prune_case_versions(Duration::from_secs(60), 1, 7)
        .await
        .unwrap();
    assert_eq!(deleted, 4, "versions 1, 3, 4, 5 of 7");
    let versions = |key: &'static str, num: i64| {
        let s = &s;
        async move {
            let (_, v) = s
                .get(
                    &format!("/api/v1/projects/{key}/cases/{num}/versions?limit=100"),
                    ADMIN_TOKEN,
                )
                .await;
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["version"].as_i64().unwrap())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        versions("ver", n).await,
        [7, 6, 2],
        "the newest two and the run item's version 2"
    );
    assert_eq!(versions("all", keep_all).await.len(), 6, "no retention set");
    assert_eq!(
        versions("ver", fresh).await,
        [3, 2, 1],
        "versions replaced less than an hour ago are kept"
    );
    assert_eq!(
        store
            .prune_case_versions(Duration::from_secs(60), 1000, 0)
            .await
            .unwrap(),
        0
    );
}

/// People who signed in through a bound group are listed with their display names; tokens
/// and names already used come without one.
#[tokio::test]
async fn people_lists_members_of_bound_groups() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("access.yaml");
    std::fs::write(
        &file,
        "roles:\n  - name: reader\n    permissions: [\"projects:read\"]\nbindings:\n  - subject: group:qa\n    role: tester\n    scopes: [pp]\n  - subject: group:ops\n    role: viewer\n    scopes: [pp]\n",
    )
    .unwrap();
    let s = Server::start_with(&db, &[("FASTPROOF_ACCESS_FILE", file.to_str().unwrap())]).await;
    s.project("pp").await;
    s.token("ci-runner", "tester", &["pp"]).await;
    let store = fastproof_store::Store::new(db.pool.clone());
    store
        .note_person("ann", Some("Ann Example"), &["qa".into()])
        .await
        .unwrap();
    store
        .note_person("vic", Some("Vic Viewer"), &["ops".into()])
        .await
        .unwrap();
    store
        .note_person("bob", None, &["qa".into()])
        .await
        .unwrap();
    store
        .note_person("ann", None, &["qa".into(), "dev".into()])
        .await
        .unwrap();
    let (st, b) = s.get("/api/v1/projects/pp/people", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["items"],
        json!([
            {"principal": "ann", "display": "Ann Example"},
            {"principal": "bob"},
            {"principal": "ci-runner"}
        ]),
        "vic's group gives no runs:execute"
    );
    let (_, b) = s
        .get("/api/v1/projects/pp/people?q=example", ADMIN_TOKEN)
        .await;
    assert_eq!(
        b["items"],
        json!([{"principal": "ann", "display": "Ann Example"}])
    );

    // P1: display names only for eligible members, never for token names, and only to
    // callers who read cases or execute runs.
    store
        .note_person("ci-runner", Some("Not A Token"), &["qa".into()])
        .await
        .unwrap();
    s.project("other").await;
    store
        .note_person("carl", Some("Carl Elsewhere"), &["ops".into()])
        .await
        .unwrap();
    let (st, plan) = s
        .post(
            "/api/v1/projects/pp/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let n = new_case(&s, "pp", "Manual").await;
    let pid = plan["id"].as_str().unwrap();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/pp/test-plans/{pid}/items"),
            ADMIN_TOKEN,
            json!({"add": [n], "assign": [{"number": n, "assignee": "carl"}]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, b) = s.get("/api/v1/projects/pp/people", ADMIN_TOKEN).await;
    assert_eq!(
        b["items"],
        json!([
            {"principal": "ann", "display": "Ann Example"},
            {"principal": "bob"},
            {"principal": "carl"},
            {"principal": "ci-runner"}
        ]),
        "carl is only a name in a plan, ci-runner is a token: {b}"
    );
    let viewer = s.token("val", "viewer", &["pp"]).await;
    let (_, b) = s.get("/api/v1/projects/pp/people", &viewer).await;
    assert!(
        b.to_string().contains("Ann Example"),
        "cases:read shows display names: {b}"
    );
    let reader = s.token("rea", "reader", &["pp"]).await;
    let (st, b) = s.get("/api/v1/projects/pp/people", &reader).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(
        !b.to_string().contains("Ann Example"),
        "projects:read alone gets no display names: {b}"
    );
    let (_, b) = s.get("/api/v1/projects/pp/people?q=example", &reader).await;
    assert_eq!(
        b["items"],
        json!([]),
        "q does not match display names for them"
    );
}

/// Case numbers are taken outside the create transaction: parallel creates get unique
/// numbers, and a create that fails validation takes none.
#[tokio::test]
async fn parallel_case_creates_get_unique_numbers() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("num").await;
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/num/cases",
        ADMIN_TOKEN,
        None,
        Some(json!({"name": ""})),
    )
    .await;
    assert_eq!(r.status.as_u16() / 100, 4, "{}", r.body);
    let mut tasks = Vec::new();
    for i in 0..40 {
        let http = s.http.clone();
        let url = format!("{}/api/v1/projects/num/cases", s.base);
        tasks.push(tokio::spawn(async move {
            let r = http
                .post(url)
                .bearer_auth(ADMIN_TOKEN)
                .json(&json!({"name": format!("Case {i}")}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::CREATED);
            r.json::<Value>().await.unwrap()["number"].as_i64().unwrap()
        }));
    }
    let mut numbers = Vec::new();
    for t in tasks {
        numbers.push(t.await.unwrap());
    }
    numbers.sort();
    assert_eq!(numbers, (1..=40).collect::<Vec<i64>>());
}

/// F12: custom columns of Qase, TestOps and generic CSV files become fields with
/// `unknownFieldValues: create`, as with TestRail and NDJSON.
#[tokio::test]
async fn csv_custom_columns_become_fields() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let cases: &[(&str, &str, &[u8], &str, &str)] = &[
        (
            "gen",
            "generic-csv",
            b"Title,Steps,Browser,Ticket\nLogin,1. Open,Firefox,T-1\nLogout,1. Open,Chrome,T-2\n",
            "Browser",
            "Firefox",
        ),
        (
            "tom",
            "testops-csv",
            b"Allure ID,Name,Scenario,Layer,Browser\n11,Login,Open,UI,Firefox\n12,Logout,Open,API,Chrome\n",
            "Browser",
            "Firefox",
        ),
    ];
    for (key, format, file, column, value) in cases {
        s.project(key).await;
        let (ct, body) = multipart(&[
            (
                "options",
                None,
                json!({"format": format, "unknownFieldValues": "create"})
                    .to_string()
                    .as_bytes(),
            ),
            ("file", Some("cases.csv"), file),
        ]);
        let r = s
            .req(
                Method::POST,
                &format!("/api/v1/projects/{key}/imports"),
                Some(ADMIN_TOKEN),
            )
            .header("content-type", ct)
            .body(body)
            .send()
            .await
            .unwrap();
        let j: Value = r.json().await.unwrap();
        let id = j["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{j}"))
            .to_string();
        let ready = wait_job(&s, key, &id, "ready").await;
        let created = ready["report"]["createdFields"].to_string();
        assert!(created.contains(column), "{format}: {ready}");
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/{key}/imports/{id}/apply"),
                ADMIN_TOKEN,
                json!({}),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
        wait_job(&s, key, &id, "done").await;
        let (_, defs) = s
            .get(&format!("/api/v1/projects/{key}/case-fields"), ADMIN_TOKEN)
            .await;
        let def = defs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == *column)
            .unwrap_or_else(|| panic!("{format}: no field {column}: {defs}"))
            .clone();
        let k = def["key"].as_str().unwrap();
        let (_, list) = s
            .get(
                &format!("/api/v1/projects/{key}/cases?limit=10"),
                ADMIN_TOKEN,
            )
            .await;
        let login = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Login")
            .unwrap_or_else(|| panic!("{list}"))
            .clone();
        assert_eq!(login["fields"][k], *value, "{format}: {login}");
    }
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    use std::io::Write;
    let boundary = "fastproofboundaryBacklogC0";
    let mut body = Vec::new();
    for (name, file, bytes) in parts {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""
        )
        .unwrap();
        if let Some(f) = file {
            write!(body, "; filename=\"{f}\"").unwrap();
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn wait_job(s: &Server, key: &str, id: &str, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (st, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        let status = b["status"].as_str().unwrap_or("");
        if status == want {
            return b;
        }
        assert!(
            !["failed", "cancelled", "done"].contains(&status),
            "job ended {status}: {b}"
        );
        assert!(Instant::now() < deadline, "job {id} stuck: {b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn triage_rest(s: &Server, token: &str, key: &str, body: Value) -> Reply {
    send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/triage"),
        token,
        None,
        Some(body),
    )
    .await
}

async fn failed_result(s: &Server, key: &str) -> String {
    let l = s.launch(key, None).await;
    let launch = l["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{launch}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "adds", "fullName": "shop.Cart#adds", "status": "failed",
                "start": 1795249075967i64, "stop": 1795249076967i64, "error": {"message": "boom"}}]),
        )
        .await;
    assert!(st.is_success() && b["accepted"] == 1, "{b}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let r = s.results(key, &launch, "").await;
        if let Some(id) = r.first().and_then(|x| x["id"].as_str()) {
            return id.to_string();
        }
        assert!(Instant::now() < deadline, "no result");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// M1: an agent's triage is recorded as AI triage whatever it sends, and muting needs
/// `results:mute`, which agents lack (they propose a mute as a disabled rule); editors keep
/// muting.
#[tokio::test]
async fn agents_cannot_mute_and_their_triage_is_ai() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("mu").await;
    let agent = s.token("copilot", "agent", &["mu"]).await;
    let editor = s.token("ed", "editor", &["mu"]).await;
    let rid = failed_result(&s, "mu").await;
    let until = (chrono::Utc::now() + chrono::Duration::days(3000)).to_rfc3339();
    let r = triage_rest(
        &s,
        &agent,
        "mu",
        json!({"resultIds": [rid], "mute": {"until": until, "reason": "x"}, "source": "user"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.body["details"]["required"], "results:mute", "{}", r.body);
    let r = triage_rest(
        &s,
        &agent,
        "mu",
        json!({"resultIds": [rid], "unmute": true}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    let r = triage_rest(
        &s,
        &agent,
        "mu",
        json!({"resultIds": [rid], "comment": "flaky network", "source": "user"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let (_, d) = s
        .get(&format!("/api/v1/projects/mu/results/{rid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(d["defect"]["source"], "ai", "{d}");
    let (err, e) = mcp_tool(
        &s,
        &agent,
        "triage",
        json!({"project": "mu", "resultIds": [rid], "mute": {"until": until, "reason": "x"}}),
    )
    .await;
    assert!(err && e.to_string().contains("results:mute"), "{e}");
    let (err, rule) = mcp_tool(
        &s,
        &agent,
        "create_rule_draft",
        json!({"project": "mu", "name": "Mute boom", "match": {"messageRegex": "boom"},
            "action": {"mute": {"days": 7}}}),
    )
    .await;
    assert!(!err, "a mute is proposed as a disabled rule: {rule}");
    let r = triage_rest(
        &s,
        &editor,
        "mu",
        json!({"resultIds": [rid], "mute": {"until": until, "reason": "known"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "editors mute: {}", r.body);
    let (_, d) = s
        .get(&format!("/api/v1/projects/mu/results/{rid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(d["muted"], true, "{d}");
}

/// R1: agents' unreviewed drafts are capped per principal (cases and rule drafts), and MCP
/// write tools share the rate of multiplying writes.
#[tokio::test]
async fn agent_drafts_are_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "1000")],
    )
    .await;
    s.project("dr").await;
    let agent = s.token("copilot", "agent", &["dr"]).await;
    for i in 0..20 {
        let (err, r) = mcp_tool(
            &s,
            &agent,
            "create_rule_draft",
            json!({"project": "dr", "name": format!("Rule {i}"), "match": {"messageRegex": "boom"},
                "action": {"comment": "x"}}),
        )
        .await;
        assert!(!err, "{i}: {r}");
    }
    let (err, e) = mcp_tool(
        &s,
        &agent,
        "create_rule_draft",
        json!({"project": "dr", "name": "One more", "match": {"messageRegex": "boom"},
            "action": {"comment": "x"}}),
    )
    .await;
    assert!(err && e.to_string().contains("maxRuleDrafts"), "{e}");

    for i in 0..200 {
        new_case(&s, "dr", &format!("C{i}")).await;
    }
    sqlx::query(
        "update cases set origin = 'ai', ai_unreviewed = true, created_by = 'copilot' \
         where project_id = (select id from projects where key = 'dr')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let (err, e) = mcp_tool(
        &s,
        &agent,
        "create_case_draft",
        json!({"project": "dr", "name": "Draft 201"}),
    )
    .await;
    assert!(
        err && e.to_string().contains("maxUnreviewedAiDrafts"),
        "{e}"
    );
    let other = s.token("copilot-2", "agent", &["dr"]).await;
    let (err, d) = mcp_tool(
        &s,
        &other,
        "create_case_draft",
        json!({"project": "dr", "name": "Another agent"}),
    )
    .await;
    assert!(!err, "the cap is per principal: {d}");
}

/// L1 / R1: MCP writes count against the caller's rate, `add_cases_to_plan` too.
#[tokio::test]
async fn mcp_writes_are_rate_limited() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "2")]).await;
    s.project("rl").await;
    let editor = s.token("ed", "editor", &["rl"]).await;
    let n = new_case(&s, "rl", "Manual").await;
    let (st, plan) = s
        .post(
            "/api/v1/projects/rl/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let pid = plan["id"].as_str().unwrap();
    for _ in 0..2 {
        let (err, r) = mcp_tool(
            &s,
            &editor,
            "add_cases_to_plan",
            json!({"project": "rl", "planId": pid, "numbers": [n]}),
        )
        .await;
        assert!(!err, "{r}");
    }
    let (err, e) = mcp_tool(
        &s,
        &editor,
        "create_case_draft",
        json!({"project": "rl", "name": "Third write"}),
    )
    .await;
    assert!(err && e.to_string().contains("rate_limited"), "{e}");
}

/// L1: the open plan launch cap holds for concurrent starts and for reopening a closed run.
#[tokio::test]
async fn open_plan_launch_cap_holds_under_concurrency_and_reopen() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MAX_OPEN_PLAN_LAUNCHES", "2"),
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "1000"),
            ("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "20"),
        ],
    )
    .await;
    s.project("ol").await;
    let base = "/api/v1/projects/ol";
    let n = new_case(&s, "ol", "Manual").await;
    let (_, plan) = s
        .post(
            &format!("{base}/test-plans"),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "P", "caseNumbers": [n]}),
        )
        .await;
    let pid = plan["id"].as_str().unwrap().to_string();
    let mut tasks = Vec::new();
    for i in 0..6 {
        let http = s.http.clone();
        let url = format!("{}{base}/test-plans/{pid}/launches", s.base);
        tasks.push(tokio::spawn(async move {
            http.post(url)
                .bearer_auth(ADMIN_TOKEN)
                .json(&json!({"name": "run", "externalId": format!("p{i}")}))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let mut created = 0;
    for t in tasks {
        let st = t.await.unwrap();
        if st == StatusCode::CREATED {
            created += 1;
        } else {
            assert_eq!(st, StatusCode::CONFLICT);
        }
    }
    assert_eq!(created, 2, "concurrent starts never exceed the cap");
    let (_, launches) = s
        .get(&format!("{base}/test-plans/{pid}/launches"), ADMIN_TOKEN)
        .await;
    let first = launches["items"][0]["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("{base}/launches/{first}/manual/close"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s
        .post(
            &format!("{base}/test-plans/{pid}/launches"),
            ADMIN_TOKEN,
            json!({"name": "run", "externalId": "again"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let (st, b) = s
        .post(
            &format!("{base}/launches/{first}/manual/reopen"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["details"]["limit"], "maxOpenPlanLaunches", "{b}");
}

/// N1: parallel creates near `maxCases` never exceed it.
#[tokio::test]
async fn parallel_creates_never_exceed_max_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_CASES", "5")]).await;
    s.project("mx").await;
    for i in 0..3 {
        new_case(&s, "mx", &format!("C{i}")).await;
    }
    let mut tasks = Vec::new();
    for i in 0..10 {
        let http = s.http.clone();
        let url = format!("{}/api/v1/projects/mx/cases", s.base);
        tasks.push(tokio::spawn(async move {
            http.post(url)
                .bearer_auth(ADMIN_TOKEN)
                .json(&json!({"name": format!("P{i}")}))
                .send()
                .await
                .unwrap()
                .status()
        }));
    }
    let mut ok = 0;
    for t in tasks {
        let st = t.await.unwrap();
        if st == StatusCode::CREATED {
            ok += 1;
        } else {
            assert_eq!(st, StatusCode::CONFLICT);
        }
    }
    assert_eq!(ok, 2);
    let n: i64 = sqlx::query_scalar(
        "select count(*) from cases where project_id = (select id from projects where key = 'mx')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 5);
}
