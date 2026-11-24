//! Shared steps over HTTP: the routes with their permissions, `If-Match`, IDOR across projects,
//! Markdown safety, versions, archive and purge, references from cases (with the gate), usages,
//! extract, inline, pins in runs and `POST /run-items/{itemId}/refresh`, and the audit trail.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

struct Reply {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

async fn send(
    s: &Server,
    method: Method,
    path: &str,
    token: Option<&str>,
    if_match: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut rb = s.req(method, path, token);
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

async fn hit(
    s: &Server,
    method: &Method,
    path: &str,
    token: Option<&str>,
    needs_match: bool,
    body: &Option<Value>,
) -> Reply {
    send(
        s,
        method.clone(),
        path,
        token,
        needs_match.then_some("\"r1\""),
        body.clone(),
    )
    .await
}

fn base(key: &str) -> String {
    format!("/api/v1/projects/{key}")
}

fn login_steps() -> Value {
    json!([
        {"action": "Open the **login** page", "expected": "The form is shown"},
        {"action": "Sign in", "steps": [{"action": "Type the user"}, {"action": "Type the password"}]}
    ])
}

async fn create_shared(s: &Server, key: &str, token: &str, name: &str) -> Reply {
    let r = send(
        s,
        Method::POST,
        &format!("{}/shared-steps", base(key)),
        Some(token),
        None,
        Some(json!({"name": name, "description": "Shared *login*", "steps": login_steps()})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r
}

fn reference(ss: &Value) -> Value {
    json!({"id": "", "shared": {"id": ss["id"], "number": ss["number"]}})
}

async fn create_case(s: &Server, key: &str, name: &str, steps: Value) -> Reply {
    send(
        s,
        Method::POST,
        &format!("{}/cases", base(key)),
        Some(ADMIN_TOKEN),
        None,
        Some(json!({"name": name, "body": {"steps": steps}})),
    )
    .await
}

async fn audit_has(s: &Server, want: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
        let items = a["items"].as_array().cloned().unwrap_or_default();
        if want.iter().all(|w| items.iter().any(|i| i["action"] == *w)) {
            return;
        }
        assert!(Instant::now() < deadline, "audit lacks {want:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn every_route_checks_authentication_permission_and_project() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("sa").await;
    s.project("sb").await;
    let viewer = s.token("vic", "viewer", &["sa"]).await;
    let tester = s.token("tess", "tester", &["sa"]).await;
    let editor_b = s.token("edb", "editor", &["sb"]).await;
    let ss_a = create_shared(&s, "sa", ADMIN_TOKEN, "Log in").await.body;
    let ss_b = create_shared(&s, "sb", ADMIN_TOKEN, "Theirs").await.body;
    let case_a = create_case(&s, "sa", "A", json!([reference(&ss_a)])).await;
    assert_eq!(case_a.status, StatusCode::CREATED, "{}", case_a.body);
    let case_n = case_a.body["number"].as_i64().unwrap();
    let step = json!({"name": "x", "steps": [{"action": "a"}]});
    let launch = s.launch("sa", None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("{}/launches/{lid}/run-items", base("sa")),
            ADMIN_TOKEN,
            json!({"caseNumbers": [case_n]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, items) = s
        .get(
            &format!("{}/launches/{lid}/run-items", base("sa")),
            ADMIN_TOKEN,
        )
        .await;
    let item = items["items"][0]["id"].as_str().unwrap().to_string();

    // (method, path suffix, body, needs If-Match, minimum role)
    let routes: Vec<(Method, String, Option<Value>, bool, &str)> = vec![
        (Method::GET, "/shared-steps".into(), None, false, "read"),
        (
            Method::GET,
            "/shared-steps?q=SS-1".into(),
            None,
            false,
            "read",
        ),
        (
            Method::POST,
            "/shared-steps".into(),
            Some(step.clone()),
            false,
            "write",
        ),
        (Method::GET, "/shared-steps/1".into(), None, false, "read"),
        (
            Method::PUT,
            "/shared-steps/1".into(),
            Some(step.clone()),
            true,
            "write",
        ),
        (
            Method::DELETE,
            "/shared-steps/1".into(),
            None,
            false,
            "write",
        ),
        (
            Method::DELETE,
            "/shared-steps/1?purge=true".into(),
            None,
            false,
            "settings",
        ),
        (
            Method::POST,
            "/shared-steps/1/restore".into(),
            None,
            false,
            "write",
        ),
        (
            Method::GET,
            "/shared-steps/1/versions".into(),
            None,
            false,
            "read",
        ),
        (
            Method::GET,
            "/shared-steps/1/versions/1".into(),
            None,
            false,
            "read",
        ),
        (
            Method::POST,
            "/shared-steps/1/versions/1/restore".into(),
            None,
            true,
            "write",
        ),
        (
            Method::GET,
            "/shared-steps/1/usages".into(),
            None,
            false,
            "read",
        ),
        (
            Method::POST,
            "/shared-steps/1/inline".into(),
            None,
            false,
            "write",
        ),
        (
            Method::POST,
            format!("/cases/{case_n}/extract-shared"),
            Some(json!({"stepIds": ["s_none"], "name": "x"})),
            true,
            "write",
        ),
        (
            Method::GET,
            format!("/cases/{case_n}/shared-step-changes"),
            None,
            false,
            "read",
        ),
        (
            Method::POST,
            format!("/run-items/{item}/refresh"),
            None,
            false,
            "execute",
        ),
    ];
    for (method, suffix, body, needs_match, role) in &routes {
        let path = format!("{}{suffix}", base("sa"));
        let anon = hit(&s, method, &path, None, *needs_match, body).await;
        assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{method} {suffix}");
        let foreign = hit(&s, method, &path, Some(&editor_b), *needs_match, body).await;
        assert_eq!(
            foreign.status,
            StatusCode::FORBIDDEN,
            "a token of another project: {method} {suffix}"
        );
        let nowhere = send(
            &s,
            method.clone(),
            &format!("{}{suffix}", base("zz")),
            Some(&editor_b),
            needs_match.then_some("\"r1\""),
            body.clone(),
        )
        .await;
        assert_eq!(
            nowhere.status,
            StatusCode::FORBIDDEN,
            "the same 403 for a project that does not exist: {method} {suffix}"
        );
        if *role != "read" {
            let r = hit(&s, method, &path, Some(&viewer), *needs_match, body).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "viewer {method} {suffix}");
        }
        if *role == "write" || *role == "settings" {
            let r = hit(&s, method, &path, Some(&tester), *needs_match, body).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "tester {method} {suffix}");
        }
        if *role == "read" {
            let r = hit(&s, method, &path, Some(&viewer), *needs_match, body).await;
            assert!(
                r.status.is_success(),
                "viewer {method} {suffix}: {}",
                r.body
            );
        }
    }
    let editor_a = s.token("eda", "editor", &["sa"]).await;
    let purge = send(
        &s,
        Method::DELETE,
        &format!("{}/shared-steps/1?purge=true", base("sa")),
        Some(&editor_a),
        None,
        None,
    )
    .await;
    assert_eq!(
        purge.status,
        StatusCode::FORBIDDEN,
        "an editor has no settings:write"
    );

    // Project ownership of every id: SS-1 of "sb" is not reachable through "sa" and the other way.
    let theirs = ss_b["id"].as_str().unwrap();
    let sneaky = create_case(
        &s,
        "sa",
        "sneaky",
        json!([{"id": "", "shared": {"id": theirs, "number": 1}}]),
    )
    .await;
    assert_eq!(sneaky.status, StatusCode::NOT_FOUND, "{}", sneaky.body);
    let r = send(
        &s,
        Method::GET,
        &format!("{}/shared-steps/2", base("sa")),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &s,
        Method::POST,
        &format!("{}/run-items/{item}/refresh", base("sb")),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "a run item of another project"
    );
    for bad in ["x", "0", "-1", "99999999999999999999", "1.5"] {
        let r = send(
            &s,
            Method::GET,
            &format!("{}/shared-steps/{bad}", base("sa")),
            Some(ADMIN_TOKEN),
            None,
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
    }
}

#[tokio::test]
async fn lifecycle_versions_markdown_and_purge() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lf").await;
    let b = base("lf");
    let xss =
        "**bold** <script>alert(1)</script>[x](javascript:alert(1)) <img src=x onerror=alert(1)>";
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/shared-steps"),
        Some(ADMIN_TOKEN),
        None,
        Some(json!({"name": "Log in", "description": xss, "steps": [{"action": xss}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.etag.as_deref(), Some("\"r1\""));
    let html = serde_json::to_string(&r.body["html"]).unwrap();
    assert!(html.contains("<strong>bold</strong>"), "{html}");
    assert!(
        !html.contains("<script") && !html.contains("<img") && !html.contains("href"),
        "{html}"
    );
    assert_eq!(r.body["number"], 1);
    assert_eq!(r.body["usage"], json!({"cases": 0, "openRuns": 0}));

    let ss = create_shared(&s, "lf", ADMIN_TOKEN, "Checkout").await.body;
    assert_eq!(ss["number"], 2);
    let (st, list) = s
        .get(&format!("{b}/shared-steps?q=checkout"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["stepCount"], 4);
    let (_, page) = s
        .get(&format!("{b}/shared-steps?limit=1"), ADMIN_TOKEN)
        .await;
    assert_eq!(page["next"], "1");
    let (st, _) = s
        .get(&format!("{b}/shared-steps?before=zz"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let put = |token: &'static str, m: Option<&'static str>, body: Value| {
        let s = &s;
        let path = format!("{b}/shared-steps/2");
        async move { send(s, Method::PUT, &path, Some(token), m, Some(body)).await }
    };
    let new_body = json!({"name": "Checkout", "steps": [{"action": "Pay"}]});
    assert_eq!(
        put(ADMIN_TOKEN, None, new_body.clone()).await.status,
        StatusCode::PRECONDITION_REQUIRED
    );
    let stale = put(ADMIN_TOKEN, Some("\"r7\""), new_body.clone()).await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    assert_eq!(stale.body["details"]["currentRevision"], 1);
    let ok = put(ADMIN_TOKEN, Some("\"r1\""), new_body.clone()).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(
        (ok.body["version"].as_i64(), ok.etag.as_deref()),
        (Some(2), Some("\"r2\""))
    );
    for (bad, why) in [
        (
            json!({"name": " ", "steps": [{"action": "a"}]}),
            "empty name",
        ),
        (json!({"name": "x", "steps": []}), "no steps"),
        (
            json!({"name": "x", "steps": [{"action": "a", "shared": {"id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b", "number": 1}}]}),
            "nested shared step",
        ),
        (
            json!({"name": "x", "steps": [{"action": "a", "steps": [{"action": "b", "steps": [{"action": "c"}]}]}]}),
            "three levels",
        ),
        (
            json!({"name": "x", "steps": [{"action": "see attachment:0193a1b2-0000-7000-8000-000000000001"}]}),
            "attachment image",
        ),
    ] {
        let r = send(
            &s,
            Method::POST,
            &format!("{b}/shared-steps"),
            Some(ADMIN_TOKEN),
            None,
            Some(bad),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{why}: {}", r.body);
    }
    let big = "x".repeat(70_000);
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/shared-steps"),
        Some(ADMIN_TOKEN),
        None,
        Some(json!({"name": "big", "steps": (0..8).map(|_| json!({"action": big[..10_000].to_string()})).collect::<Vec<_>>()})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "limit_reached");
    assert_eq!(r.body["details"]["limit"], "maxSharedStepBytes");

    let (_, versions) = s
        .get(&format!("{b}/shared-steps/2/versions"), ADMIN_TOKEN)
        .await;
    assert_eq!(versions["items"].as_array().unwrap().len(), 2);
    let (st, v1) = s
        .get(&format!("{b}/shared-steps/2/versions/1"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{v1}");
    assert_eq!(v1["snapshot"]["steps"].as_array().unwrap().len(), 2);
    let (st, _) = s
        .get(&format!("{b}/shared-steps/2/versions/9"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let rest = send(
        &s,
        Method::POST,
        &format!("{b}/shared-steps/2/versions/1/restore"),
        Some(ADMIN_TOKEN),
        Some("\"r2\""),
        None,
    )
    .await;
    assert_eq!(rest.status, StatusCode::OK, "{}", rest.body);
    assert_eq!(rest.body["version"], 3);

    let arch = send(
        &s,
        Method::DELETE,
        &format!("{b}/shared-steps/2"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(arch.status, StatusCode::NO_CONTENT);
    let r = put(ADMIN_TOKEN, Some("\"r4\""), new_body.clone()).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["code"], "shared_step_archived");
    let (_, trash) = s
        .get(&format!("{b}/shared-steps?archived=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(trash["items"].as_array().unwrap().len(), 1);
    let r = create_case(&s, "lf", "uses archived", json!([reference(&ss)])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "shared_step_archived");
    let back = send(
        &s,
        Method::POST,
        &format!("{b}/shared-steps/2/restore"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.body);
    assert!(back.body["archivedAt"].is_null());

    let used = create_case(&s, "lf", "uses it", json!([reference(&ss)])).await;
    assert_eq!(used.status, StatusCode::CREATED, "{}", used.body);
    let purge = send(
        &s,
        Method::DELETE,
        &format!("{b}/shared-steps/2?purge=true"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(purge.status, StatusCode::CONFLICT, "{}", purge.body);
    assert_eq!(purge.body["code"], "shared_step_in_use");
    assert_eq!(purge.body["details"], json!({"cases": 1, "openRuns": 0}));
    let (_, usages) = s
        .get(&format!("{b}/shared-steps/2/usages"), ADMIN_TOKEN)
        .await;
    assert_eq!(usages["items"][0]["caseNumber"], used.body["number"]);
    assert_eq!(usages["openRuns"], 0);
    let free = send(
        &s,
        Method::DELETE,
        &format!("{b}/shared-steps/1?purge=true"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(free.status, StatusCode::NO_CONTENT);
    let (st, _) = s.get(&format!("{b}/shared-steps/1"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    audit_has(
        &s,
        &[
            "sharedStep.create",
            "sharedStep.update",
            "sharedStep.archive",
            "sharedStep.restore",
            "sharedStep.restoreVersion",
            "sharedStep.purge",
        ],
    )
    .await;
}

#[tokio::test]
async fn cases_extract_inline_and_runs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cr").await;
    let b = base("cr");
    let tester = s.token("tess", "tester", &["cr"]).await;
    let ss = create_shared(&s, "cr", ADMIN_TOKEN, "Log in").await.body;

    let case = create_case(
        &s,
        "cr",
        "Checkout",
        json!([{"action": "first"}, reference(&ss), {"action": "last"}]),
    )
    .await;
    assert_eq!(case.status, StatusCode::CREATED, "{}", case.body);
    let n = case.body["number"].as_i64().unwrap();
    let shared_view = &case.body["sharedSteps"][0];
    assert_eq!(shared_view["number"], 1);
    assert_eq!(shared_view["version"], 1);
    let html = serde_json::to_string(&shared_view["stepsHtml"]).unwrap();
    assert!(html.contains("<strong>login</strong>"), "{html}");
    let (_, got) = s.get(&format!("{b}/cases/{n}"), ADMIN_TOKEN).await;
    assert_eq!(got["sharedSteps"][0]["steps"].as_array().unwrap().len(), 2);
    assert!(
        got["sharedSteps"][0]["stepsHtml"][0]["action"]
            .as_str()
            .unwrap()
            .contains("<strong>login</strong>")
    );
    let (_, versions) = s.get(&format!("{b}/cases/{n}/versions"), ADMIN_TOKEN).await;
    assert_eq!(
        versions["items"].as_array().unwrap().len(),
        1,
        "a shared step edit never writes case versions"
    );

    let (_, ss_now) = s.get(&format!("{b}/shared-steps/1"), ADMIN_TOKEN).await;
    assert_eq!(ss_now["usage"]["cases"], 1);
    let launch = s.launch("cr", None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let (st, added) = s
        .post(
            &format!("{b}/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{added}");
    let (_, items) = s
        .get(&format!("{b}/launches/{lid}/run-items"), ADMIN_TOKEN)
        .await;
    let item = items["items"][0]["id"].as_str().unwrap().to_string();
    let (st, run) = s.get(&format!("{b}/run-items/{item}"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{run}");
    assert_eq!(run["steps"][1]["shared"]["version"], 1);
    assert_eq!(run["steps"][1]["steps"].as_array().unwrap().len(), 2);
    assert_eq!(
        run["steps"][1]["steps"][1]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        run["stepsHtml"][1]["steps"][0]["action"]
            .as_str()
            .map(|a| a.contains("<strong>")),
        Some(true)
    );
    assert_eq!(run["sharedPins"][0]["version"], 1);
    assert_eq!(run["sharedPins"][0]["current"], 1);
    let (_, usages) = s
        .get(&format!("{b}/shared-steps/1/usages"), ADMIN_TOKEN)
        .await;
    assert_eq!(usages["openRuns"], 1);

    let edit = send(
        &s,
        Method::PUT,
        &format!("{b}/shared-steps/1"),
        Some(ADMIN_TOKEN),
        Some("\"r1\""),
        Some(json!({"name": "Log in", "description": "Shared *login*", "steps": [{"action": "Use single sign-on"}]})),
    )
    .await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    let (_, changes) = s
        .get(&format!("{b}/cases/{n}/shared-step-changes"), ADMIN_TOKEN)
        .await;
    assert_eq!(changes["items"][0]["sharedStepNumber"], 1);
    assert_eq!(changes["items"][0]["version"], 2);
    assert_eq!(changes["items"][0]["changed"], json!(["steps"]));
    let (_, run) = s.get(&format!("{b}/run-items/{item}"), ADMIN_TOKEN).await;
    assert_eq!(
        run["steps"][1]["steps"].as_array().unwrap().len(),
        2,
        "the run keeps the pinned steps"
    );
    assert_eq!(run["sharedPins"][0]["current"], 2);

    let r = send(
        &s,
        Method::POST,
        &format!("{b}/run-items/{item}/refresh"),
        Some(&tester),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["sharedPins"][0]["version"], 2);
    assert_eq!(r.body["steps"][1]["steps"].as_array().unwrap().len(), 1);
    assert!(r.etag.is_some());
    let (st, _) = s
        .call(
            Method::PUT,
            &format!("{b}/run-items/{item}/steps/1"),
            Some(&tester),
            Some(json!({"status": "passed"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/run-items/{item}/refresh"),
        Some(&tester),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "run_item_started");

    // Extract two adjacent steps of another case into a new shared step.
    let two = create_case(
        &s,
        "cr",
        "Two",
        json!([{"id": "s_a", "action": "a"}, {"id": "s_b", "action": "b"}, {"id": "s_c", "action": "c"}]),
    )
    .await;
    let m = two.body["number"].as_i64().unwrap();
    let rev = two.etag.clone().unwrap();
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/cases/{m}/extract-shared"),
        Some(ADMIN_TOKEN),
        Some(&rev),
        Some(json!({"stepIds": ["s_a", "s_c"], "name": "Gap"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/cases/{m}/extract-shared"),
        Some(ADMIN_TOKEN),
        None,
        Some(json!({"stepIds": ["s_a", "s_b"], "name": "Gap"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/cases/{m}/extract-shared"),
        Some(ADMIN_TOKEN),
        Some(&rev),
        Some(json!({"stepIds": ["s_a", "s_b"], "name": "First two", "description": "**both**"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["sharedStep"]["number"], 2);
    assert_eq!(r.body["sharedStep"]["usage"]["cases"], 1);
    assert!(
        r.body["sharedStep"]["html"]["description"]
            .as_str()
            .unwrap()
            .contains("<strong>both</strong>")
    );
    assert_eq!(r.body["case"]["body"]["steps"].as_array().unwrap().len(), 2);
    assert_eq!(r.body["case"]["sharedSteps"][0]["number"], 2);
    assert_eq!(r.body["case"]["version"], 2);

    let r = send(
        &s,
        Method::POST,
        &format!("{b}/shared-steps/2/inline"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body, json!({"updated": 1, "skipped": 0}));
    let (_, inlined) = s.get(&format!("{b}/cases/{m}"), ADMIN_TOKEN).await;
    assert_eq!(inlined["body"]["steps"].as_array().unwrap().len(), 3);
    assert_eq!(inlined["sharedSteps"], json!([]));
    assert_eq!(inlined["version"], 3);
    audit_has(
        &s,
        &["sharedStep.extract", "sharedStep.inline", "runItem.refresh"],
    )
    .await;
}

#[tokio::test]
async fn a_closed_gate_refuses_new_references() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let open = Server::start(&db).await;
    open.project("gt").await;
    let ss = create_shared(&open, "gt", ADMIN_TOKEN, "Log in").await.body;
    let closed = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "closed")]).await;
    let r = create_case(&closed, "gt", "gated", json!([reference(&ss)])).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "upgrade_in_progress");
    assert_eq!(r.body["details"]["what"], "shared steps");
    let r = create_case(&closed, "gt", "plain", json!([{"action": "a"}])).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let again = create_shared(&closed, "gt", ADMIN_TOKEN, "Another").await;
    assert_eq!(
        again.body["number"], 2,
        "creating shared steps is not gated"
    );
    let n = r.body["number"].as_i64().unwrap();
    let rev = r.etag.clone().unwrap();
    let r = send(
        &closed,
        Method::POST,
        &format!("/api/v1/projects/gt/cases/{n}/extract-shared"),
        Some(ADMIN_TOKEN),
        Some(&rev),
        Some(json!({"stepIds": ["s_none"], "name": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
}
