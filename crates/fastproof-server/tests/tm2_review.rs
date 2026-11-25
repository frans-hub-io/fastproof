//! Workflow and review over HTTP (TM next §7.2, §7.3): workflow settings with the remap job,
//! transitions with their permission and guards (single, PUT/PATCH and bulk), votes with self
//! approval, moved versions, vote replacement and AI drafts, bulk review "as one PR", the queue,
//! shared step votes, imports with `review` and `bypass`, the `caseReviewRequested`
//! notification, permissions and IDOR on every route.

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

struct Reply {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
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
    let etag = r
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = r.text().await.unwrap();
    Reply {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        etag,
    }
}

/// Custom roles: `writer` changes cases without `cases:review`; `proposer` only proposes.
const ACCESS: &str = "roles:\n  - name: writer\n    permissions: [\"projects:read\", \"cases:read\", \"cases:write\"]\n  - name: proposer\n    permissions: [\"projects:read\", \"cases:read\", \"ai:propose\"]\n";

async fn start(db: &TestDb, extra: &[(&str, &str)]) -> (Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("access.yaml");
    std::fs::write(&file, ACCESS).unwrap();
    let mut vars = vec![
        ("FASTPROOF_ACCESS_FILE", file.to_str().unwrap()),
        ("FASTPROOF_TM_GATE", "open"),
    ];
    vars.extend_from_slice(extra);
    (Server::start_with(db, &vars).await, dir)
}

fn workflow() -> Value {
    json!({"statuses": [
        {"key": "draft", "name": "Draft", "category": "draft"},
        {"key": "ready", "name": "Ready", "category": "draft", "blocksRuns": true},
        {"key": "active", "name": "Active", "category": "active"},
        {"key": "signed", "name": "Signed", "category": "active", "color": "#2f9e44"},
        {"key": "outdated", "name": "Outdated", "category": "outdated"}],
      "manual": {"initial": "draft", "transitions": [
        {"from": ["draft"], "to": "ready"},
        {"from": ["ready"], "to": "active", "requires": ["approved"]},
        {"from": ["active"], "to": "signed", "permission": "cases:review", "requires": ["notAuthor"]},
        {"from": ["*"], "to": "outdated", "permission": "cases:review"},
        {"from": ["outdated"], "to": "draft"}]},
      "automated": {"initial": "active", "transitions": [{"from": ["*"], "to": "*"}]}})
}

fn review(mode: &str, approvals: i64, self_ok: bool, imports: &str) -> Value {
    json!({"caseReview": {"mode": mode, "approvals": approvals, "resetOn": "content",
        "allowSelfApproval": self_ok, "imports": imports}})
}

async fn put_settings(s: &Server, key: &str, body: Value) -> Reply {
    send(
        s,
        Method::PUT,
        &format!("/api/v1/projects/{key}/settings"),
        ADMIN_TOKEN,
        None,
        Some(body),
    )
    .await
}

async fn put_workflow(s: &Server, key: &str, token: &str, body: Value) -> Reply {
    send(
        s,
        Method::PUT,
        &format!("/api/v1/projects/{key}/case-workflow"),
        token,
        None,
        Some(body),
    )
    .await
}

async fn create(s: &Server, key: &str, token: &str, body: Value) -> Value {
    let r = send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/cases"),
        token,
        None,
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r.body
}

async fn case(s: &Server, key: &str, n: i64) -> Reply {
    let r = send(
        s,
        Method::GET,
        &format!("/api/v1/projects/{key}/cases/{n}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r
}

async fn transition(s: &Server, key: &str, n: i64, token: &str, to: &str) -> Reply {
    let etag = case(s, key, n).await.etag.unwrap();
    send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/cases/{n}/transition"),
        token,
        Some(&etag),
        Some(json!({"to": to})),
    )
    .await
}

async fn patch(s: &Server, key: &str, n: i64, token: &str, body: Value) -> Reply {
    let etag = case(s, key, n).await.etag.unwrap();
    send(
        s,
        Method::PATCH,
        &format!("/api/v1/projects/{key}/cases/{n}"),
        token,
        Some(&etag),
        Some(body),
    )
    .await
}

async fn edit_steps(s: &Server, key: &str, n: i64, token: &str, text: &str) -> Value {
    let cur = case(s, key, n).await;
    let r = send(
        s,
        Method::PUT,
        &format!("/api/v1/projects/{key}/cases/{n}"),
        token,
        cur.etag.as_deref(),
        Some(json!({"name": cur.body["name"], "body": {"steps": [{"action": text, "expected": "ok"}]}})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    r.body
}

/// `kind` is `cases` or `shared-steps`.
async fn thread(s: &Server, key: &str, kind: &str, n: i64, token: &str) -> Reply {
    send(
        s,
        Method::GET,
        &format!("/api/v1/projects/{key}/{kind}/{n}/reviews"),
        token,
        None,
        None,
    )
    .await
}

async fn vote(
    s: &Server,
    key: &str,
    kind: &str,
    n: i64,
    token: &str,
    version: i64,
    decision: &str,
) -> Reply {
    let etag = thread(s, key, kind, n, ADMIN_TOKEN).await.etag.unwrap();
    send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/{kind}/{n}/reviews"),
        token,
        Some(&etag),
        Some(json!({"version": version, "decision": decision, "comment": format!("{decision} by vote")})),
    )
    .await
}

async fn audit_has(s: &Server, want: &[&str]) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
        let items = a["items"].as_array().cloned().unwrap_or_default();
        if want.iter().all(|w| items.iter().any(|i| i["action"] == *w)) {
            return items;
        }
        assert!(Instant::now() < deadline, "audit lacks {want:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_job(s: &Server, key: &str, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (st, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        match b["status"].as_str() {
            Some("done") => return b,
            Some("failed") | Some("cancelled") => panic!("job ended: {b}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "job {id} stuck: {b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn workflow_transitions_guards_bulk_and_idor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    s.project("wf").await;
    s.project("wf2").await;
    let vic = s.token("vic", "viewer", &["wf"]).await;
    let wanda = s.token("wanda", "writer", &["wf"]).await;
    let erin = s.token("erin", "editor", &["wf"]).await;
    let rita = s.token("rita", "editor", &["wf"]).await;

    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/wf/case-workflow",
        &vic,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["configured"], false);
    assert_eq!(r.body["workflow"]["statuses"][1]["key"], "ready_for_review");
    assert_eq!(r.body["review"]["mode"], "off");
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/wf2/case-workflow",
        &vic,
        None,
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "another project's workflow"
    );
    let r = put_workflow(&s, "wf", &erin, json!({"workflow": workflow()})).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "the workflow needs settings:write"
    );
    let mut bad = workflow();
    bad["statuses"].as_array_mut().unwrap().pop();
    let r = put_workflow(&s, "wf", ADMIN_TOKEN, json!({"workflow": bad})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert!(
        r.body["message"].as_str().unwrap().contains("outdated"),
        "{}",
        r.body
    );
    let r = put_workflow(&s, "wf", ADMIN_TOKEN, json!({"workflow": workflow()})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["view"]["configured"], true);
    assert!(r.body.get("job").is_none(), "{}", r.body);

    let a = create(&s, "wf", &wanda, json!({"name": "A", "status": "draft"})).await;
    assert_eq!(a["state"], "draft", "creates take the initial state");
    let b = create(&s, "wf", &erin, json!({"name": "B", "status": "active"})).await;
    assert_eq!(
        b["state"], "active",
        "the state of the category when initial has another"
    );
    let (na, nb) = (a["number"].as_i64().unwrap(), b["number"].as_i64().unwrap());

    let r = transition(&s, "wf", na, &wanda, "active").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "invalid_transition");
    assert_eq!(
        r.body["details"],
        json!({"from": "draft", "to": "active", "missing": ["transition"]})
    );
    let r = transition(&s, "wf", na, &wanda, "nowhere").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = transition(&s, "wf", na, &wanda, "ready").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["state"], "ready");
    assert_eq!(r.body["status"], "draft");
    assert_eq!(r.body["version"], 1, "the category stays: no version");
    assert_eq!(r.body["revision"], 2);
    assert_eq!(r.etag.as_deref(), Some("\"r2\""));

    assert_eq!(
        put_settings(&s, "wf", review("manual", 1, false, "review"))
            .await
            .status,
        StatusCode::OK
    );
    let edited = edit_steps(&s, "wf", na, &wanda, "Open the cart").await;
    assert_eq!(edited["reviewState"], "pending", "{edited}");
    assert_eq!(
        edited["approvedVersion"], 1,
        "never reviewed: the previous version stays runnable"
    );
    let r = patch(&s, "wf", na, &wanda, json!({"status": "active"})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["details"]["missing"], json!(["approved"]));
    let r = vote(&s, "wf", "cases", na, &erin, 2, "approve").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["reviewState"], "approved");
    let r = patch(&s, "wf", na, &wanda, json!({"status": "active"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body["state"], "active",
        "PATCH status moves to the state of that key"
    );
    assert_eq!(r.body["version"], 3);
    assert_eq!(r.body["reviewState"], "approved");
    assert_eq!(
        r.body["approvedVersion"], 3,
        "a status change needs no review"
    );

    let r = transition(&s, "wf", na, &wanda, "signed").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(
        r.body["details"]["missing"],
        json!(["permission", "notAuthor"])
    );
    let r = transition(&s, "wf", na, &erin, "signed").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (r.body["state"].clone(), r.body["version"].clone()),
        (json!("signed"), json!(3))
    );
    let r = transition(&s, "wf", nb, &erin, "signed").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(
        r.body["details"]["missing"],
        json!(["notAuthor"]),
        "the author does not sign"
    );
    let t = thread(&s, "wf", "cases", nb, &erin).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    let signed = t.body["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["to"] == "signed")
        .unwrap()
        .clone();
    assert_eq!(
        signed,
        json!({"to": "signed", "name": "Signed", "allowed": false, "missing": ["notAuthor"]})
    );
    assert_eq!(t.body["canVote"], false);
    assert_eq!(
        t.body["whyNot"], "not_pending",
        "B was never edited under review: its head counts as approved"
    );

    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf/cases/bulk",
        &wanda,
        None,
        Some(json!({"numbers": [na, nb], "action": "setStatus", "value": "outdated"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["updated"], 0);
    assert!(
        r.body["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["code"] == "invalid_transition"),
        "{}",
        r.body
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf/cases/bulk",
        &erin,
        None,
        Some(json!({"numbers": [nb], "action": "setState", "value": "signed"})),
    )
    .await;
    assert_eq!(
        r.body["skipped"][0]["code"], "invalid_transition",
        "bulk setState checks the exact key: {}",
        r.body
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf/cases/bulk",
        &rita,
        None,
        Some(json!({"numbers": [nb], "action": "setState", "value": "signed"})),
    )
    .await;
    assert_eq!(r.body["updated"], 1, "{}", r.body);
    assert_eq!(case(&s, "wf", nb).await.body["state"], "signed");
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf/cases/bulk",
        &erin,
        None,
        Some(json!({"numbers": [na, nb], "action": "setState", "value": "outdated"})),
    )
    .await;
    assert_eq!(r.body["updated"], 2, "{}", r.body);
    let c = case(&s, "wf", na).await.body;
    assert_eq!(
        (c["state"].clone(), c["status"].clone()),
        (json!("outdated"), json!("outdated"))
    );

    let r = send(
        &s,
        Method::POST,
        &format!("/api/v1/projects/wf/cases/{na}/transition"),
        &wanda,
        Some("\"r1\""),
        Some(json!({"to": "draft"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.body);
    let r = send(
        &s,
        Method::POST,
        &format!("/api/v1/projects/wf/cases/{na}/transition"),
        &wanda,
        None,
        Some(json!({"to": "draft"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = transition(&s, "wf", na, &vic, "draft").await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a viewer does not transition"
    );
    create(&s, "wf2", ADMIN_TOKEN, json!({"name": "other"})).await;
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf2/cases/1/transition",
        &wanda,
        Some("\"r1\""),
        Some(json!({"to": "draft"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a case of a project outside the token's scope"
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf/cases/999/transition",
        ADMIN_TOKEN,
        Some("\"r1\""),
        Some(json!({"to": "draft"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/wf2/cases/1/transition",
        ADMIN_TOKEN,
        Some("\"r1\""),
        Some(json!({"to": "draft"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "wf2 has no workflow: {}",
        r.body
    );

    let items = audit_has(
        &s,
        &[
            "case.transition",
            "caseWorkflow.settings",
            "caseReview.vote",
        ],
    )
    .await;
    let t = items
        .iter()
        .find(|i| i["action"] == "case.transition")
        .unwrap();
    assert!(
        t["detail"]["from"].is_string() && t["detail"]["to"].is_string(),
        "{t}"
    );
}

#[tokio::test]
async fn removing_used_states_needs_a_remap_job() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    s.project("rm").await;
    let erin = s.token("erin", "editor", &["rm"]).await;
    assert_eq!(
        put_workflow(&s, "rm", ADMIN_TOKEN, json!({"workflow": workflow()}))
            .await
            .status,
        StatusCode::OK
    );
    let mut ns = Vec::new();
    for name in ["one", "two"] {
        let c = create(&s, "rm", &erin, json!({"name": name, "status": "draft"})).await;
        let n = c["number"].as_i64().unwrap();
        assert_eq!(
            transition(&s, "rm", n, &erin, "ready").await.status,
            StatusCode::OK
        );
        ns.push(n);
    }
    let c = create(
        &s,
        "rm",
        &erin,
        json!({"name": "three", "status": "active"}),
    )
    .await;
    let n3 = c["number"].as_i64().unwrap();
    assert_eq!(
        transition(&s, "rm", n3, ADMIN_TOKEN, "signed").await.status,
        StatusCode::OK
    );

    let mut without_ready = workflow();
    without_ready["statuses"].as_array_mut().unwrap().remove(1);
    without_ready["manual"]["transitions"] = json!([{"from": ["*"], "to": "*"}]);
    let r = put_workflow(&s, "rm", ADMIN_TOKEN, json!({"workflow": without_ready})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "workflow_status_in_use");
    assert_eq!(
        r.body["details"]["statuses"],
        json!([{"key": "ready", "cases": 2}])
    );
    let r = put_settings(&s, "rm", json!({"caseWorkflow": without_ready})).await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "PUT /settings refuses it too: {}",
        r.body
    );
    assert_eq!(r.body["code"], "workflow_status_in_use");

    let mut moved = without_ready.clone();
    moved["statuses"][2]["category"] = json!("outdated");
    let r = put_workflow(
        &s,
        "rm",
        ADMIN_TOKEN,
        json!({"workflow": moved, "remap": {"ready": "draft"}}),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "signed changes category: {}",
        r.body
    );
    assert_eq!(
        r.body["details"]["statuses"],
        json!([{"key": "signed", "cases": 1}])
    );
    let r = put_workflow(
        &s,
        "rm",
        ADMIN_TOKEN,
        json!({"workflow": moved, "remap": {"ready": "nowhere"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = put_workflow(
        &s,
        "rm",
        ADMIN_TOKEN,
        json!({"workflow": moved, "remap": {"ready": "draft", "signed": "signed"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["job"]["kind"], "workflow_remap", "{}", r.body);
    let job = wait_job(&s, "rm", r.body["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["progress"]["done"], 3, "{job}");
    for n in &ns {
        let c = case(&s, "rm", *n).await.body;
        assert_eq!(
            (
                c["state"].clone(),
                c["status"].clone(),
                c["version"].clone()
            ),
            (json!("draft"), json!("draft"), json!(1)),
            "{c}"
        );
    }
    let c = case(&s, "rm", n3).await.body;
    assert_eq!(
        (
            c["state"].clone(),
            c["status"].clone(),
            c["version"].clone()
        ),
        (json!("signed"), json!("outdated"), json!(2))
    );
    let (_, v) = s
        .get(
            &format!("/api/v1/projects/rm/cases/{n3}/versions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(v["items"][0]["source"], "bulk");

    let r = put_workflow(&s, "rm", ADMIN_TOKEN, json!({"workflow": null})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["view"]["configured"], false);
    wait_job(&s, "rm", r.body["job"]["id"].as_str().unwrap()).await;
    for n in ns.iter().chain([&n3]) {
        assert!(case(&s, "rm", *n).await.body["state"].is_null());
    }
    let r = put_workflow(
        &s,
        "rm",
        ADMIN_TOKEN,
        json!({"workflow": null, "remap": {"ready": "draft"}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "remap needs a workflow");
    audit_has(&s, &["caseWorkflow.settings"]).await;
}

#[tokio::test]
async fn votes_follow_the_review_rules() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    s.project("rv").await;
    s.project("rv2").await;
    let erin = s.token("erin", "editor", &["rv"]).await;
    let rita = s.token("rita", "editor", &["rv"]).await;
    let ruth = s.token("ruth", "editor", &["rv"]).await;
    let wanda = s.token("wanda", "writer", &["rv"]).await;
    let pat = s.token("pat", "proposer", &["rv"]).await;
    let agent = s.token("copilot", "agent", &["rv"]).await;
    let vic = s.token("vic", "viewer", &["rv"]).await;

    let c = create(
        &s,
        "rv",
        &erin,
        json!({"name": "Checkout", "body": {"steps": [{"action": "Pay"}]}}),
    )
    .await;
    let n = c["number"].as_i64().unwrap();
    assert!(
        c["reviewState"].is_null(),
        "review is off: nothing is written"
    );
    let r = vote(&s, "rv", "cases", n, &rita, 1, "approve").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "review_not_enabled");

    assert_eq!(
        put_settings(&s, "rv", review("manual", 2, false, "review"))
            .await
            .status,
        StatusCode::OK
    );
    let c = create(
        &s,
        "rv",
        &erin,
        json!({"name": "Refund", "body": {"steps": [{"action": "Refund"}]}}),
    )
    .await;
    let n = c["number"].as_i64().unwrap();
    assert_eq!(c["reviewState"], "pending", "a new case waits for review");
    assert!(c["approvedVersion"].is_null());

    let r = vote(&s, "rv", "cases", n, &erin, 1, "approve").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.body["code"], "self_approval");
    for (who, t) in [
        ("writer", &wanda),
        ("proposer", &pat),
        ("agent", &agent),
        ("viewer", &vic),
    ] {
        let r = vote(&s, "rv", "cases", n, t, 1, "approve").await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{who} has no cases:review: {}",
            r.body
        );
    }
    let t = thread(&s, "rv", "cases", n, &vic).await;
    assert_eq!(
        (t.body["canVote"].clone(), t.body["whyNot"].clone()),
        (json!(false), json!("permission"))
    );
    let t = thread(&s, "rv", "cases", n, &erin).await;
    assert_eq!(t.body["whyNot"], "self_approval");
    assert_eq!(t.body["author"], "erin");

    let r = vote(&s, "rv", "cases", n, &rita, 1, "approve").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (
            r.body["reviewState"].clone(),
            r.body["approvals"].clone(),
            r.body["required"].clone()
        ),
        (json!("pending"), json!(1), json!(2))
    );
    assert_eq!(r.body["canVote"], true, "a reviewer may change the vote");
    let r = vote(&s, "rv", "cases", n, &ruth, 2, "approve").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(
        (r.body["code"].clone(), r.body["details"].clone()),
        (json!("version_moved"), json!({"currentVersion": 1}))
    );
    let r = vote(&s, "rv", "cases", n, &rita, 1, "request_changes").await;
    assert_eq!(r.body["reviewState"], "changes_requested", "{}", r.body);
    assert_eq!(
        r.body["votes"].as_array().unwrap().len(),
        1,
        "the later vote replaced the earlier one"
    );
    let r = vote(&s, "rv", "cases", n, &ruth, 1, "approve").await;
    assert_eq!(
        r.body["reviewState"], "changes_requested",
        "a request for changes stays: {}",
        r.body
    );
    let r = vote(&s, "rv", "cases", n, &rita, 1, "approve").await;
    assert_eq!(
        (
            r.body["reviewState"].clone(),
            r.body["approvedVersion"].clone()
        ),
        (json!("approved"), json!(1)),
        "{}",
        r.body
    );
    let r = vote(&s, "rv", "cases", n, &ruth, 1, "approve").await;
    assert_eq!(r.body["code"], "not_pending", "{}", r.body);

    let c = edit_steps(&s, "rv", n, &erin, "Refund twice").await;
    assert_eq!(
        (
            c["version"].clone(),
            c["reviewState"].clone(),
            c["approvedVersion"].clone()
        ),
        (json!(2), json!("pending"), json!(1)),
        "runs keep v1 while v2 waits"
    );
    let c = patch(&s, "rv", n, &erin, json!({"tags": ["refunds"]}))
        .await
        .body;
    assert_eq!(
        (
            c["version"].clone(),
            c["reviewState"].clone(),
            c["approvedVersion"].clone()
        ),
        (json!(3), json!("pending"), json!(1)),
        "{c}"
    );
    let r = vote(&s, "rv", "cases", n, &rita, 2, "approve").await;
    assert_eq!(
        r.body["code"], "version_moved",
        "approving a version that moved: {}",
        r.body
    );
    vote(&s, "rv", "cases", n, &rita, 3, "approve").await;
    let r = vote(&s, "rv", "cases", n, &ruth, 3, "approve").await;
    assert_eq!(
        (
            r.body["reviewState"].clone(),
            r.body["approvedVersion"].clone()
        ),
        (json!("approved"), json!(3))
    );
    let c = patch(&s, "rv", n, &erin, json!({"tags": ["refunds", "smoke"]}))
        .await
        .body;
    assert_eq!(
        (c["reviewState"].clone(), c["approvedVersion"].clone()),
        (json!("approved"), json!(4)),
        "the approval follows a change that needs none"
    );

    let etag = thread(&s, "rv", "cases", n, ADMIN_TOKEN)
        .await
        .etag
        .unwrap();
    assert_eq!(
        etag,
        case(&s, "rv", n).await.etag.unwrap(),
        "the review ETag is the case revision"
    );
    let path = format!("/api/v1/projects/rv/cases/{n}/reviews");
    let r = send(
        &s,
        Method::POST,
        &path,
        &rita,
        Some("\"r1\""),
        Some(json!({"version": 4, "decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    let r = send(
        &s,
        Method::POST,
        &path,
        &rita,
        None,
        Some(json!({"version": 4, "decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::POST,
        &path,
        &rita,
        Some(&etag),
        Some(json!({"version": 4, "decision": "approve", "comment": "x".repeat(4001)})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &s,
        Method::POST,
        &path,
        &rita,
        Some(&etag),
        Some(json!({"version": 4, "decision": "maybe"})),
    )
    .await;
    assert!(
        r.status.is_client_error() && r.status != StatusCode::NOT_FOUND,
        "{}",
        r.status
    );

    let auto = create(&s, "rv", &erin, json!({"name": "Automated"})).await;
    sqlx::query(
        "update cases set automated = true, review_state = 'pending' where project_id = \
        (select id from projects where key = 'rv') and number = $1",
    )
    .bind(auto["number"].as_i64().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let r = vote(
        &s,
        "rv",
        "cases",
        auto["number"].as_i64().unwrap(),
        &rita,
        1,
        "approve",
    )
    .await;
    assert_eq!(
        r.body["code"], "review_not_enabled",
        "manual mode leaves automated cases out: {}",
        r.body
    );

    create(&s, "rv2", ADMIN_TOKEN, json!({"name": "x"})).await;
    for _ in 0..12 {
        create(&s, "rv2", ADMIN_TOKEN, json!({"name": "y"})).await;
    }
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/rv2/cases/1/reviews",
        &rita,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "another project");
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/rv/cases/13/reviews",
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "TC-13 exists only in rv2");
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rv/cases/13/reviews",
        ADMIN_TOKEN,
        Some("\"r1\""),
        Some(json!({"version": 1, "decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/rv/cases/TC-1/reviews",
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let items = audit_has(&s, &["caseReview.vote"]).await;
    assert!(
        items
            .iter()
            .any(|i| i["action"] == "caseReview.vote"
                && i["detail"]["decision"] == "request_changes")
    );
}

#[tokio::test]
async fn ai_drafts_are_never_approved_by_their_creator() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    s.project("ai").await;
    let editor = s.token("ed", "editor", &["ai"]).await;
    assert_eq!(
        put_settings(&s, "ai", review("manual", 1, true, "review"))
            .await
            .status,
        StatusCode::OK
    );
    let r = s
        .http
        .post(format!("{}/mcp", s.base))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-06-18")
        .bearer_auth(&editor)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "create_case_draft",
            "arguments": {"project": "ai", "name": "Drafted", "steps": [{"action": "Do"}], "agent": "copilot"}}}))
        .send()
        .await
        .unwrap();
    let text = r.text().await.unwrap();
    let data = text
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .unwrap_or(&text)
        .trim()
        .to_string();
    let v: Value = serde_json::from_str(&data).unwrap();
    let inner: Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let n = inner["number"].as_i64().unwrap();
    let c = case(&s, "ai", n).await.body;
    assert_eq!(
        (c["origin"].clone(), c["reviewState"].clone()),
        (json!("ai"), json!("pending"))
    );
    let r = vote(&s, "ai", "cases", n, &editor, 1, "approve").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(
        r.body["code"], "ai_self_review",
        "even with allowSelfApproval"
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/ai/reviews/bulk",
        &editor,
        None,
        Some(
            json!({"items": [{"kind": "case", "number": n, "version": 1}], "decision": "approve"}),
        ),
    )
    .await;
    assert_eq!(
        r.body["skipped"],
        json!([{"kind": "case", "number": n, "code": "ai_self_review"}]),
        "{}",
        r.body
    );
    let r = vote(&s, "ai", "cases", n, ADMIN_TOKEN, 1, "approve").await;
    assert_eq!(
        r.body["reviewState"], "approved",
        "another person approves: {}",
        r.body
    );
}

async fn shared_step(db: &TestDb, key: &str, number: i64, by: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into shared_steps (id, project_id, number, name, steps, version, review_state, created_by, updated_by) \
         select $1, id, $2, $3, '[]', 1, 'pending', $4, $4 from projects where key = $5",
    )
    .bind(id)
    .bind(number)
    .bind(format!("Shared {number}"))
    .bind(by)
    .bind(key)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into shared_step_versions (shared_step_id, version, project_id, source, changed, snapshot_z, created_by) \
         select $1, 1, project_id, 'ui', '{name,steps}', '\\x00', $2 from shared_steps where id = $1",
    )
    .bind(id)
    .bind(by)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn bulk_review_queue_and_shared_steps() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[("FASTPROOF_TM_MAX_BULK_REVIEW", "8")]).await;
    s.project("rq").await;
    s.project("rq2").await;
    let erin = s.token("erin", "editor", &["rq"]).await;
    let rita = s.token("rita", "editor", &["rq"]).await;
    let wanda = s.token("wanda", "writer", &["rq"]).await;
    assert_eq!(
        put_settings(&s, "rq", review("all", 1, false, "review"))
            .await
            .status,
        StatusCode::OK
    );
    let (_, f) = s
        .post(
            "/api/v1/projects/rq/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Payments"}),
        )
        .await;
    let folder = f["id"].as_str().unwrap().to_string();
    let mut mine = Vec::new();
    for i in 0..3 {
        let mut body = json!({"name": format!("Erin {i}")});
        if i == 0 {
            body["folderId"] = json!(folder);
        }
        mine.push(
            create(&s, "rq", &erin, body).await["number"]
                .as_i64()
                .unwrap(),
        );
    }
    let theirs = create(&s, "rq", &rita, json!({"name": "Rita"})).await["number"]
        .as_i64()
        .unwrap();
    shared_step(&db, "rq", 1, "erin").await;
    shared_step(&db, "rq2", 2, "erin").await;

    let q = |t: &str, query: &str| {
        let s = &s;
        let t = t.to_string();
        let query = query.to_string();
        async move {
            let (st, b) = s
                .get(&format!("/api/v1/projects/rq/reviews{query}"), &t)
                .await;
            assert_eq!(st, StatusCode::OK, "{b}");
            b
        }
    };
    let all = q(ADMIN_TOKEN, "").await;
    assert_eq!(all["items"].as_array().unwrap().len(), 5, "{all}");
    let ss = &all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "sharedStep")
        .unwrap()
        .clone();
    assert_eq!(
        (
            ss["number"].clone(),
            ss["author"].clone(),
            ss["changed"].clone()
        ),
        (json!(1), json!("erin"), json!(["name", "steps"]))
    );
    let waiting = q(&erin, "?reviewer=me").await;
    let keys: Vec<(String, i64)> = waiting["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap().to_string(),
                i["number"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![("case".to_string(), theirs)],
        "erin may approve only rita's case"
    );
    assert_eq!(
        q(&wanda, "?reviewer=me").await["items"],
        json!([]),
        "no cases:review, nothing to approve"
    );
    assert_eq!(
        q(&erin, "?author=me").await["items"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        q(&erin, "?kind=sharedStep").await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let in_folder = q(&erin, &format!("?folder={folder}")).await;
    assert_eq!(in_folder["items"].as_array().unwrap().len(), 1);
    assert_eq!(in_folder["items"][0]["folderId"], json!(folder));
    let page = q(&erin, "?limit=2").await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let next = page["next"].as_str().unwrap().to_string();
    let rest = q(&erin, &format!("?limit=10&before={next}")).await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 3, "{rest}");
    assert!(rest["next"].is_null());
    for bad in [
        "?reviewer=bob",
        "?state=approved",
        "?before=garbage",
        "?kind=plan",
    ] {
        let (st, _) = s
            .get(&format!("/api/v1/projects/rq/reviews{bad}"), &erin)
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (_, other_folder) = s
        .post(
            "/api/v1/projects/rq2/case-folders",
            ADMIN_TOKEN,
            json!({"name": "X"}),
        )
        .await;
    let (st, _) = s
        .get(
            &format!(
                "/api/v1/projects/rq/reviews?folder={}",
                other_folder["id"].as_str().unwrap()
            ),
            &erin,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a folder of another project");
    let (st, _) = s.get("/api/v1/projects/rq2/reviews", &erin).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let mut items: Vec<Value> = mine
        .iter()
        .map(|n| json!({"kind": "case", "number": n, "version": 1}))
        .collect();
    items.push(json!({"kind": "case", "number": theirs, "version": 1}));
    items.push(json!({"kind": "sharedStep", "number": 1, "version": 1}));
    items.push(json!({"kind": "sharedStep", "number": 2, "version": 1}));
    items.push(json!({"kind": "case", "number": 404, "version": 1}));
    items.push(json!({"kind": "case", "number": theirs, "version": 1}));
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rq/reviews/bulk",
        &erin,
        None,
        Some(json!({"items": items, "decision": "approve", "comment": "one PR"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["applied"], 1, "{}", r.body);
    let codes: Vec<(i64, String)> = r.body["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["number"].as_i64().unwrap(),
                x["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        codes,
        vec![
            (mine[0], "self_approval".into()),
            (mine[1], "self_approval".into()),
            (mine[2], "self_approval".into()),
            (1, "self_approval".into()),
            (2, "not_found".into()),
            (404, "not_found".into())
        ],
        "self approval through bulk, another project's shared step, an unknown case"
    );
    assert_eq!(case(&s, "rq", theirs).await.body["reviewState"], "approved");

    let edited = edit_steps(&s, "rq", mine[0], &erin, "changed").await;
    assert_eq!(edited["version"], 2);
    let mut items: Vec<Value> = mine
        .iter()
        .map(|n| json!({"kind": "case", "number": n, "version": 1}))
        .collect();
    items.push(json!({"kind": "case", "number": theirs, "version": 1}));
    items.push(json!({"kind": "sharedStep", "number": 1, "version": 1}));
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rq/reviews/bulk",
        &rita,
        None,
        Some(json!({"items": items, "decision": "approve"})),
    )
    .await;
    assert_eq!(r.body["applied"], 3, "{}", r.body);
    let codes: Vec<String> = r.body["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes, vec!["version_moved", "not_pending"], "{}", r.body);
    assert_eq!(
        q(ADMIN_TOKEN, "").await["items"].as_array().unwrap().len(),
        1,
        "only the moved case waits"
    );

    let too_many: Vec<Value> = (1..=9)
        .map(|n| json!({"kind": "case", "number": n, "version": 1}))
        .collect();
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rq/reviews/bulk",
        &rita,
        None,
        Some(json!({"items": too_many, "decision": "approve"})),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].clone()),
        (StatusCode::CONFLICT, json!("limit_reached")),
        "{}",
        r.body
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rq/reviews/bulk",
        &rita,
        None,
        Some(json!({"items": [], "decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(&s, Method::POST, "/api/v1/projects/rq/reviews/bulk", &wanda, None,
        Some(json!({"items": [{"kind": "case", "number": mine[0], "version": 2}], "decision": "approve"}))).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "bulk review needs cases:review"
    );
    let r = send(&s, Method::POST, "/api/v1/projects/rq2/reviews/bulk", &rita, None,
        Some(json!({"items": [{"kind": "sharedStep", "number": 2, "version": 1}], "decision": "approve"}))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let id = shared_step(&db, "rq", 3, "erin").await;
    let r = vote(&s, "rq", "shared-steps", 3, &erin, 1, "approve").await;
    assert_eq!(
        r.body["code"], "self_approval",
        "self approval through the shared-step route: {}",
        r.body
    );
    let r = vote(&s, "rq", "shared-steps", 3, &rita, 2, "approve").await;
    assert_eq!(r.body["code"], "version_moved");
    let r = vote(&s, "rq", "shared-steps", 3, &rita, 1, "approve").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (
            r.body["kind"].clone(),
            r.body["reviewState"].clone(),
            r.body["approvedVersion"].clone()
        ),
        (json!("sharedStep"), json!("approved"), json!(1))
    );
    assert_eq!(r.body["transitions"], json!([]));
    let rev: i64 = sqlx::query_scalar("select revision from shared_steps where id = $1")
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rev, 2, "the vote moved the state: revision + 1");
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/rq/shared-steps/2/reviews",
        &erin,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "SS-2 belongs to rq2");
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/rq/shared-steps/2/reviews",
        ADMIN_TOKEN,
        Some("\"r1\""),
        Some(json!({"version": 1, "decision": "approve"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    audit_has(&s, &["caseReview.bulk", "caseReview.vote"]).await;
}

fn import_fixture(name: &str) -> Vec<u8> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fastproof-tm-import/tests/fixtures")
        .join(name);
    std::fs::read(p).unwrap()
}

async fn import(s: &Server, key: &str, mode: &str) {
    let boundary = "fastproofboundary7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    let options =
        json!({"format": "testrail-csv", "mode": mode, "unknownFieldValues": "create"}).to_string();
    let file = import_fixture("testrail-rows.csv");
    for (name, fname, bytes) in [
        ("options", None, options.as_bytes()),
        ("file", Some("cases.csv"), file.as_slice()),
    ] {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""
        )
        .unwrap();
        if let Some(f) = fname {
            write!(body, "; filename=\"{f}\"").unwrap();
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    write!(body, "--{boundary}--\r\n").unwrap();
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{key}/imports"),
            Some(ADMIN_TOKEN),
        )
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let job: Value = r.json().await.unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        if b["status"] == "ready" {
            break;
        }
        assert!(Instant::now() < deadline, "{b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    wait_job(s, key, &id).await;
}

#[tokio::test]
async fn imports_need_review_unless_the_policy_bypasses_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    for (key, imports) in [("imr", "review"), ("imb", "bypass")] {
        s.project(key).await;
        assert_eq!(
            put_settings(&s, key, review("manual", 1, false, imports))
                .await
                .status,
            StatusCode::OK
        );
        import(&s, key, "createOnly").await;
        let (_, list) = s
            .get(
                &format!("/api/v1/projects/{key}/cases?limit=50"),
                ADMIN_TOKEN,
            )
            .await;
        let items = list["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "{list}");
        for c in items {
            let full = case(&s, key, c["number"].as_i64().unwrap()).await.body;
            match imports {
                "review" => assert_eq!(
                    (full["reviewState"].clone(), full["approvedVersion"].clone()),
                    (json!("pending"), Value::Null),
                    "imported cases wait for review: {full}"
                ),
                _ => assert_eq!(
                    (full["reviewState"].clone(), full["approvedVersion"].clone()),
                    (json!("approved"), json!(1)),
                    "{full}"
                ),
            }
        }
    }
    let items = audit_has(&s, &["import.apply"]).await;
    let applies: Vec<&Value> = items
        .iter()
        .filter(|i| i["action"] == "import.apply")
        .collect();
    assert_eq!(applies.len(), 2);
    assert!(
        applies
            .iter()
            .any(|i| i["scope"] == "imb" && i["detail"]["reviewBypassed"] == true),
        "{applies:?}"
    );
    assert!(
        applies
            .iter()
            .any(|i| i["scope"] == "imr" && i["detail"].get("reviewBypassed").is_none()),
        "{applies:?}"
    );

    let n = 1;
    edit_steps(&s, "imb", n, ADMIN_TOKEN, "edited by a person").await;
    assert_eq!(
        case(&s, "imb", n).await.body["reviewState"],
        "pending",
        "a person's edit still needs review"
    );
    let edited = case(&s, "imb", n).await.body["version"].as_i64().unwrap();
    import(&s, "imb", "upsert").await;
    let c = case(&s, "imb", n).await.body;
    assert!(c["version"].as_i64().unwrap() > edited, "{c}");
    assert_eq!(
        (c["reviewState"].clone(), c["approvedVersion"].clone()),
        (json!("pending"), json!(1)),
        "an upsert with bypass does not approve the person's change waiting for review: {c}"
    );
}

#[tokio::test]
async fn later_versions_and_unapproved_starts_leave_a_trace() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (s, _dir) = start(&db, &[]).await;
    s.project("fe").await;
    let erin = s.token("erin", "editor", &["fe"]).await;
    let rita = s.token("rita", "editor", &["fe"]).await;
    let ruth = s.token("ruth", "editor", &["fe"]).await;
    assert_eq!(
        put_settings(&s, "fe", review("manual", 1, false, "review"))
            .await
            .status,
        StatusCode::OK
    );
    let c = create(
        &s,
        "fe",
        &erin,
        json!({"name": "Pay", "body": {"steps": [{"action": "Pay"}]}}),
    )
    .await;
    let n = c["number"].as_i64().unwrap();
    let r = patch(&s, "fe", n, &rita, json!({"tags": ["smoke"]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["version"], 2);
    let r = vote(&s, "fe", "cases", n, &erin, 2, "approve").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(
        r.body["code"], "self_approval",
        "rita's tag on top does not let erin approve her own steps"
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/fe/reviews/bulk",
        &erin,
        None,
        Some(
            json!({"items": [{"kind": "case", "number": n, "version": 2}], "decision": "approve"}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["skipped"][0]["code"], "self_approval", "{}", r.body);

    let (st, plan) = s
        .post(
            "/api/v1/projects/fe/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "p", "caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let path = format!(
        "/api/v1/projects/fe/test-plans/{}/launches",
        plan["id"].as_str().unwrap()
    );
    let (st, b) = s
        .post(
            &path,
            &ruth,
            json!({"name": "run", "externalId": "u-1", "includeUnapproved": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let launch = b["launch"]["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/fe/launches/{launch}/run-items"),
            &ruth,
            json!({"caseNumbers": [n], "includeUnapproved": true}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let items = audit_has(&s, &["run.start"]).await;
    let starts: Vec<&Value> = items
        .iter()
        .filter(|i| i["action"] == "run.start" && i["scope"] == "fe")
        .collect();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert!(
        starts
            .iter()
            .all(|i| i["detail"]["includeUnapproved"] == true),
        "starts with unapproved versions are audited as such: {starts:?}"
    );
}

#[tokio::test]
async fn review_requests_reach_channels_once_per_window() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let hook = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = hook.local_addr().unwrap();
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    {
        let got = got.clone();
        let app = axum::Router::new().fallback(move |h: axum::http::HeaderMap, body: String| {
            let got = got.clone();
            async move {
                let ev = h
                    .get("x-fastproof-event")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                got.lock().unwrap().push((ev, body));
                "ok"
            }
        });
        tokio::spawn(async move { axum::serve(hook, app).await.unwrap() });
    }
    let (s, _dir) = {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("access.yaml");
        std::fs::write(&file, ACCESS).unwrap();
        let s = Server::start_cfg(
            &db,
            &[
                ("FASTPROOF_ACCESS_FILE", file.to_str().unwrap()),
                ("FASTPROOF_TM_GATE", "open"),
                ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
                ("FASTPROOF_PUBLIC_URL", "https://fastproof.example.com"),
            ],
            |c| {
                c.integrations.poll = Duration::from_millis(50);
                c.integrations.delivery_backoff = Duration::from_millis(50);
            },
        )
        .await;
        (s, dir)
    };
    s.project("rn").await;
    let erin = s.token("erin@example.com", "editor", &["rn"]).await;
    let r = put_settings(&s, "rn", json!({
        "caseReview": {"mode": "manual", "approvals": 1, "resetOn": "content", "allowSelfApproval": false, "imports": "review"},
        "notifications": [
            {"id": "hook", "kind": "webhook", "target": format!("http://{addr}/review"), "on": ["caseReviewRequested"]},
            {"id": "mail", "kind": "email", "target": "erin@example.com", "on": ["caseReviewRequested"]},
            {"id": "launches", "kind": "webhook", "target": format!("http://{addr}/launch"), "on": ["launchFinished"]}]})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let n = create(&s, "rn", &erin, json!({"name": "Pay <b>now</b>"})).await["number"]
        .as_i64()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while got.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "no review request arrived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (ev, body) = got.lock().unwrap()[0].clone();
    assert_eq!(ev, "caseReviewRequested");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (
            v["event"].clone(),
            v["project"].clone(),
            v["new"].clone(),
            v["waiting"].clone()
        ),
        (
            json!("caseReviewRequested"),
            json!("rn"),
            json!(1),
            json!(1)
        ),
        "{v}"
    );
    assert_eq!(v["items"][0]["key"], format!("TC-{n}"));
    assert_eq!(
        v["items"][0]["name"], "Pay <b>now</b>",
        "JSON keeps the text; text channels escape it"
    );
    assert_eq!(v["items"][0]["author"], "erin@example.com");
    assert_eq!(v["url"], "https://fastproof.example.com/p/rn/reviews");

    for i in 0..3 {
        create(&s, "rn", &erin, json!({"name": format!("More {i}")})).await;
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        got.lock().unwrap().len(),
        1,
        "at most one delivery per channel every 5 minutes"
    );
    let (_, d) = s
        .get(
            "/api/v1/projects/rn/notifications/deliveries?limit=50",
            ADMIN_TOKEN,
        )
        .await;
    let targets: Vec<&str> = d["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["notificationId"].as_str())
        .collect();
    assert_eq!(
        targets,
        vec!["hook"],
        "the email list is only the author: never to the actor alone: {d}"
    );
}
