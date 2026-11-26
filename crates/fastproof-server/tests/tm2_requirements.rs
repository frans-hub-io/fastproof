//! Requirements and coverage over HTTP (R, `74-tm-next-interfaces.md` §7.6): the tree with
//! ETags, cycles, depth and archive; links (manual, AI proposals, from trusted code, limits);
//! coverage in every scope with the status rules; exports; FPQL over requirements; the
//! dashboard widget and its gate; the tracker import and state sync against a local tracker
//! mock; permissions and IDOR. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{Method as AxMethod, Uri};
use axum::response::{IntoResponse, Response};
use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::{RequirementLinkOrigin, RequirementLinksChange, TmLimits};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const KEY_HEX: &str = "8f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

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

fn base(key: &str) -> String {
    format!("/api/v1/projects/{key}")
}

async fn req(s: &Server, key: &str, body: Value) -> Value {
    let r = send(
        s,
        Method::POST,
        &format!("{}/requirements", base(key)),
        ADMIN_TOKEN,
        None,
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(
        r.etag.as_deref(),
        Some(format!("\"r{}\"", r.body["revision"]).as_str())
    );
    r.body
}

async fn case(s: &Server, key: &str, name: &str) -> i64 {
    let (st, b) = s
        .post(
            &format!("{}/cases", base(key)),
            ADMIN_TOKEN,
            json!({"name": name}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["number"].as_i64().unwrap()
}

async fn links(s: &Server, key: &str, n: i64, body: Value) -> (StatusCode, Value) {
    s.post(
        &format!("{}/requirements/{n}/links", base(key)),
        ADMIN_TOKEN,
        body,
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

fn numbers(v: &Value) -> Vec<i64> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["number"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn tree_etags_cycles_depth_archive_and_safety() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_REQUIREMENT_DEPTH", "3")]).await;
    s.project("rq").await;
    let b = base("rq");
    let xss = "<script>alert(1)</script>\n\n[x](javascript:alert(1)) <img src=x onerror=alert(1)>";
    let epic = req(
        &s,
        "rq",
        json!({"title": " Checkout ", "type": "epic", "description": xss, "issueKey": "SHOP-1",
            "trackerId": "jira", "url": "https://tracker.example.com/browse/SHOP-1"}),
    )
    .await;
    assert_eq!(epic["number"], 1);
    assert_eq!(epic["title"], "Checkout");
    assert_eq!(epic["depth"], 1);
    assert_eq!(epic["status"], "active");
    assert_eq!(epic["origin"], "ui");
    assert_eq!(epic["tracker"]["issueKey"], "SHOP-1");
    let html = epic["descriptionHtml"].as_str().unwrap();
    assert!(
        !html.contains("<script") && !html.contains("<img") && !html.contains("href=\"javascript:"),
        "{html}"
    );
    let story = req(
        &s,
        "rq",
        json!({"title": "Pay by card", "type": "story", "parentNumber": 1}),
    )
    .await;
    assert_eq!(story["depth"], 2);
    assert_eq!(
        story["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["number"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let leaf = req(&s, "rq", json!({"title": "3-D Secure", "parentNumber": 2})).await;
    assert_eq!(leaf["depth"], 3);
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/requirements"),
        ADMIN_TOKEN,
        None,
        Some(json!({"title": "too deep", "parentNumber": 3})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "limit_reached");
    assert_eq!(r.body["details"]["limit"], "maxRequirementDepth");
    for (body, what) in [
        (json!({"title": ""}), "empty title"),
        (json!({"title": "x", "type": "Epic"}), "type"),
        (json!({"title": "x", "url": "javascript:alert(1)"}), "url"),
        (
            json!({"title": "x", "trackerId": "jira"}),
            "tracker without key",
        ),
        (json!({"title": "x", "issueKey": "has space"}), "issue key"),
        (json!({"title": "x", "unknown": 1}), "unknown key"),
    ] {
        let r = send(
            &s,
            Method::POST,
            &format!("{b}/requirements"),
            ADMIN_TOKEN,
            None,
            Some(body),
        )
        .await;
        assert!(
            r.status.is_client_error(),
            "{what}: {} {}",
            r.status,
            r.body
        );
    }
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/requirements"),
        ADMIN_TOKEN,
        None,
        Some(json!({"title": "dup", "issueKey": "SHOP-1", "trackerId": "jira"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["code"], "issue_key_taken");

    let r = send(
        &s,
        Method::GET,
        &format!("{b}/requirements/1"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["children"], 1);
    let tag = r.etag.clone().unwrap();
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/1"),
        ADMIN_TOKEN,
        None,
        Some(json!({"status": "done"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/1"),
        ADMIN_TOKEN,
        Some(&tag),
        Some(json!({"parentNumber": 3})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "requirement_cycle");
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/1"),
        ADMIN_TOKEN,
        Some(&tag),
        Some(json!({"status": "done", "title": "Checkout flow"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["status"], "done");
    assert!(
        r.body["contentChangedAt"].as_str().unwrap() > epic["contentChangedAt"].as_str().unwrap()
    );
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/1"),
        ADMIN_TOKEN,
        Some(&tag),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);

    let other = req(&s, "rq", json!({"title": "Search"})).await;
    let r = send(
        &s,
        Method::GET,
        &format!("{b}/requirements/2"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/2"),
        ADMIN_TOKEN,
        r.etag.as_deref(),
        Some(json!({"parentNumber": other["number"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["depth"], 2);
    let (_, leaf) = s.get(&format!("{b}/requirements/3"), ADMIN_TOKEN).await;
    assert_eq!(leaf["depth"], 3);
    assert_eq!(leaf["path"][0]["number"], other["number"]);
    let r = send(
        &s,
        Method::GET,
        &format!("{b}/requirements/2"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::PUT,
        &format!("{b}/requirements/2"),
        ADMIN_TOKEN,
        r.etag.as_deref(),
        Some(json!({"title": "Pay by card", "type": "story"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body["depth"], 1,
        "PUT without parentNumber moves it to the top"
    );
    let (_, leaf) = s.get(&format!("{b}/requirements/3"), ADMIN_TOKEN).await;
    assert_eq!(leaf["depth"], 2);

    let (_, top) = s.get(&format!("{b}/requirements/tree"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&top), vec![1, 2, 4]);
    let (_, kids) = s
        .get(&format!("{b}/requirements/tree?parent=REQ-2"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&kids), vec![3]);
    assert_eq!(top["items"][1]["hasChildren"], true);
    let (_, page) = s
        .get(&format!("{b}/requirements?limit=2"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&page), vec![1, 2]);
    let (_, page2) = s
        .get(
            &format!(
                "{b}/requirements?limit=2&before={}",
                page["next"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&page2), vec![3, 4]);
    let (_, found) = s
        .get(&format!("{b}/requirements?q=secure"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&found), vec![3]);
    let (_, found) = s
        .get(&format!("{b}/requirements?q=REQ-4"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&found), vec![4]);
    let (_, found) = s
        .get(&format!("{b}/requirements?q=SHOP-1"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&found), vec![1]);
    let (_, under) = s
        .get(&format!("{b}/requirements?parent=2"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&under), vec![3]);
    let (_, stories) = s
        .get(&format!("{b}/requirements?type=story"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&stories), vec![2]);
    let (st, _) = s
        .get(
            &format!("{b}/requirements?q={}", "x".repeat(600)),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s.get(&format!("{b}/requirements/99"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s.get(&format!("{b}/requirements/abc"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let r = send(
        &s,
        Method::DELETE,
        &format!("{b}/requirements/2"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let (_, trash) = s
        .get(&format!("{b}/requirements?archived=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&trash), vec![2, 3], "the subtree goes to the trash");
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/requirements/3/restore"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    let r = send(
        &s,
        Method::GET,
        &format!("{b}/requirements/3"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/requirements/3"),
        ADMIN_TOKEN,
        r.etag.as_deref(),
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "archived ones are read only"
    );
    let r = send(
        &s,
        Method::POST,
        &format!("{b}/requirements/2/restore"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let (_, trash) = s
        .get(&format!("{b}/requirements?archived=true"), ADMIN_TOKEN)
        .await;
    assert!(numbers(&trash).is_empty());
    audit_has(
        &s,
        &[
            "requirement.create",
            "requirement.update",
            "requirement.archive",
            "requirement.restore",
        ],
    )
    .await;
}

#[tokio::test]
async fn limits_on_count_and_links() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MAX_REQUIREMENTS", "2"),
            ("FASTPROOF_TM_MAX_REQUIREMENT_CASES", "2"),
        ],
    )
    .await;
    s.project("lim").await;
    req(&s, "lim", json!({"title": "a"})).await;
    req(&s, "lim", json!({"title": "b"})).await;
    let (st, b) = s
        .post(
            &format!("{}/requirements", base("lim")),
            ADMIN_TOKEN,
            json!({"title": "c"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["details"]["limit"], "maxRequirements");
    for i in 0..3 {
        case(&s, "lim", &format!("case {i}")).await;
    }
    let (st, b) = links(&s, "lim", 1, json!({"add": [1, 2, 3]})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, _) = links(&s, "lim", 1, json!({"add": [1, 2]})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = links(&s, "lim", 1, json!({"add": [3]})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["details"]["limit"], "maxRequirementCases");
    let (_, list) = s
        .get(
            &format!("{}/requirements/1/cases", base("lim")),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        2,
        "the refused add left nothing"
    );
}

#[tokio::test]
async fn links_manual_ai_proposals_and_the_case_tab() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ln").await;
    let b = base("ln");
    req(&s, "ln", json!({"title": "Login"})).await;
    req(&s, "ln", json!({"title": "Logout"})).await;
    let c1 = case(&s, "ln", "Login works").await;
    let c2 = case(&s, "ln", "Wrong password").await;
    let c3 = case(&s, "ln", "Remember me").await;
    let (st, r) = links(&s, "ln", 1, json!({"add": [c1, c2, c1]})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["cases"], 2);
    assert_eq!(r["revision"], 2);
    let (st, _) = links(&s, "ln", 1, json!({"add": [999]})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = links(&s, "ln", 1, json!({})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = links(&s, "ln", 1, json!({"add": [1], "extra": true})).await;
    assert!(st.is_client_error());

    let store = Store::new(db.pool.clone());
    let p = store.project("ln").await.unwrap().unwrap();
    let limits = TmLimits::default();
    let ai = CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: "copilot",
        limits: &limits,
        can_review: false,
    };
    let (_, ch) = store
        .change_requirement_links(
            &ai,
            1,
            &RequirementLinksChange {
                add: Some(vec![c3, c1]),
                ..Default::default()
            },
            RequirementLinkOrigin::Ai,
        )
        .await
        .unwrap();
    assert_eq!(
        ch.added,
        vec![c3],
        "an existing active link is not proposed again"
    );
    let err = store
        .change_requirement_links(
            &ai,
            1,
            &RequirementLinksChange {
                confirm: Some(vec![c3]),
                ..Default::default()
            },
            RequirementLinkOrigin::Ai,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("proposal"), "{err}");
    let (_, proposed) = s
        .get(
            &format!("{b}/requirements/1/cases?linkStatus=proposed"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(proposed["items"][0]["caseNumber"], c3);
    assert_eq!(proposed["items"][0]["origin"], "ai");
    let (_, req1) = s.get(&format!("{b}/requirements/1"), ADMIN_TOKEN).await;
    assert_eq!(req1["cases"], 2, "proposals do not count as coverage");
    let (_, tab) = s
        .get(&format!("{b}/cases/{c3}/requirements"), ADMIN_TOKEN)
        .await;
    assert_eq!(tab[0]["linkStatus"], "proposed");
    let (st, r) = links(&s, "ln", 1, json!({"confirm": [c3]})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["cases"], 3);
    let (st, r) = links(&s, "ln", 1, json!({"remove": [c2]})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, list) = s
        .get(&format!("{b}/requirements/1/cases"), ADMIN_TOKEN)
        .await;
    let got: Vec<(i64, &str)> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["caseNumber"].as_i64().unwrap(),
                i["linkStatus"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, vec![(c1, "active"), (c3, "active")]);
    links(&s, "ln", 2, json!({"add": [c1]})).await;
    let (_, tab) = s
        .get(&format!("{b}/cases/{c1}/requirements"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        tab.as_array()
            .unwrap()
            .iter()
            .map(|r| r["number"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let (st, _) = s
        .get(&format!("{b}/cases/999/requirements"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    audit_has(&s, &["requirement.links"]).await;
    let events: Vec<serde_json::Value> = sqlx::query_scalar(
        "select payload from events where type = 'requirement.updated' and project = 'ln' order by id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        events.contains(&json!({"number": 1, "revision": 2})),
        "a link change is a live event: {events:?}"
    );
}

#[tokio::test]
async fn permissions_and_idor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pa").await;
    s.project("pb").await;
    req(&s, "pa", json!({"title": "A one"})).await;
    let ca = case(&s, "pa", "case of a").await;
    let viewer = s.token("v", "viewer", &["pa"]).await;
    let editor_b = s.token("eb", "editor", &["pb"]).await;
    let a = base("pa");
    let (st, _) = s.get(&format!("{a}/requirements/1"), &viewer).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s.get(&format!("{a}/coverage"), &viewer).await;
    assert_eq!(st, StatusCode::OK);
    let writes: Vec<(Method, String, Value)> = vec![
        (
            Method::POST,
            format!("{a}/requirements"),
            json!({"title": "x"}),
        ),
        (
            Method::POST,
            format!("{a}/requirements/1/links"),
            json!({"add": [ca]}),
        ),
        (Method::DELETE, format!("{a}/requirements/1"), Value::Null),
        (
            Method::POST,
            format!("{a}/requirements/1/restore"),
            Value::Null,
        ),
        (
            Method::POST,
            format!("{a}/requirements/import"),
            json!({"trackerId": "jc", "query": "project = SHOP"}),
        ),
    ];
    for (m, path, body) in &writes {
        let r = send(
            &s,
            m.clone(),
            path,
            &viewer,
            Some("\"r1\""),
            (!body.is_null()).then(|| body.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "viewer {m} {path}");
        let r = send(
            &s,
            m.clone(),
            path,
            &editor_b,
            Some("\"r1\""),
            (!body.is_null()).then(|| body.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "other project {m} {path}");
    }
    let r = send(
        &s,
        Method::PATCH,
        &format!("{a}/requirements/1"),
        &viewer,
        Some("\"r1\""),
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let (st, _) = s.get(&format!("{a}/requirements"), &editor_b).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .get("/api/v1/projects/missing/requirements", &editor_b)
        .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "the same answer whether the project exists or not"
    );
    let (st, _) = s.get(&format!("{a}/requirements"), "nope").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let bp = base("pb");
    let (st, _) = s.get(&format!("{bp}/requirements/1"), &editor_b).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a number of another project");
    req(&s, "pb", json!({"title": "B one"})).await;
    let (st, b) = s
        .post(
            &format!("{bp}/requirements/1/links"),
            &editor_b,
            json!({"add": [ca]}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a case of another project: {b}");
    let (st, _) = s
        .post(
            &format!("{bp}/requirements"),
            &editor_b,
            json!({"title": "child", "parentNumber": 2}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .get(
            &format!("{bp}/coverage?launchIds={}", uuid::Uuid::now_v7()),
            &editor_b,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .get(
            &format!("{bp}/coverage?planId={}", uuid::Uuid::now_v7()),
            &editor_b,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .post(
            &format!("{bp}/requirements/import/{}/apply", uuid::Uuid::now_v7()),
            &editor_b,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn result(id: &str, name: &str, full: &str, status: &str, links: Value, labels: Value) -> Value {
    let stop = now_ms();
    json!({"id": id, "name": name, "fullName": full, "status": status, "start": stop - 1000,
        "stop": stop, "labels": labels, "links": links})
}

fn param_result(id: &str, full: &str, status: &str, browser: &str, stop: i64) -> Value {
    json!({"id": id, "name": "pays", "fullName": full, "status": status, "start": stop - 1000,
        "stop": stop, "labels": [], "parameters": [{"name": "browser", "value": browser}]})
}

async fn run_launch(s: &Server, key: &str, token: &str, start: Value, results: Value) -> String {
    let (st, l) = s
        .post(&format!("/api/v1/projects/{key}/launches"), token, start)
        .await;
    assert!(st.is_success(), "{st} {l}");
    let lid = l["id"].as_str().unwrap().to_string();
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{lid}/results"),
            token,
            results,
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let (st, f) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/{lid}/finish"),
            token,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{st} {f}");
    lid
}

async fn sync(db: &TestDb) {
    Store::new(db.pool.clone())
        .run_case_sync("test", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap();
}

async fn link_rows(db: &TestDb) -> Vec<(String, String, String, String)> {
    let mut rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "select c.name, r.title, l.origin, l.status from requirement_links l join cases c on c.id = l.case_id \
         join requirements r on r.id = l.requirement_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    rows.sort();
    rows
}

fn row(c: &str, r: &str, o: &str, s: &str) -> (String, String, String, String) {
    (c.into(), r.into(), o.into(), s.into())
}

#[tokio::test]
async fn code_links_follow_the_trusted_sync() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["cases.sync", "cases.backfill"]).await;
    s.project("cl").await;
    let up = s.token("ci", "uploader", &["cl"]).await;
    req(&s, "cl", json!({"title": "Cart"})).await;
    req(
        &s,
        "cl",
        json!({"title": "Promo", "issueKey": "SHOP-7", "trackerId": "jira"}),
    )
    .await;
    req(&s, "cl", json!({"title": "Old"})).await;
    let r = send(
        &s,
        Method::DELETE,
        &format!("{}/requirements/3", base("cl")),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let main = json!({"name": "ci", "vcs": {"branch": "main"}});
    run_launch(
        &s,
        "cl",
        &up,
        main.clone(),
        json!([
            result("a", "adds", "shop.Cart#adds", "passed",
                json!([{"name": "REQ-1", "type": "requirement", "url": ""},
                       {"name": "", "type": "Requirement", "url": "https://tracker.example.com/browse/SHOP-7"},
                       {"name": "REQ-3", "type": "requirement", "url": ""},
                       {"name": "REQ-99", "type": "requirement", "url": ""}]),
                json!([{"name": "suite", "value": "Cart"}])),
            result("b", "removes", "shop.Cart#removes", "failed", json!([]),
                json!([{"name": "requirement", "value": "SHOP-7"}])),
        ]),
    )
    .await;
    sync(&db).await;
    assert_eq!(
        link_rows(&db).await,
        vec![
            row("adds", "Cart", "code", "active"),
            row("adds", "Promo", "code", "active"),
            row("removes", "Promo", "code", "active"),
        ],
        "archived and unknown keys link nothing"
    );
    let adds: i64 = sqlx::query_scalar("select number from cases where name = 'adds'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let (st, r) = links(&s, "cl", 2, json!({"add": [adds]})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(
        link_rows(&db).await[1],
        row("adds", "Promo", "ui", "active"),
        "a person takes the link over"
    );
    let (_, rq) = s
        .get(&format!("{}/requirements/2", base("cl")), ADMIN_TOKEN)
        .await;
    let rev = rq["revision"].as_i64().unwrap();

    run_launch(
        &s,
        "cl",
        &up,
        json!({"name": "feature", "vcs": {"branch": "feature/x"}}),
        json!([
            result(
                "a",
                "adds",
                "shop.Cart#adds",
                "passed",
                json!([]),
                json!([{"name": "suite", "value": "Cart"}])
            ),
            result(
                "b",
                "removes",
                "shop.Cart#removes",
                "passed",
                json!([]),
                json!([])
            ),
        ]),
    )
    .await;
    sync(&db).await;
    assert_eq!(
        link_rows(&db).await.len(),
        3,
        "untrusted launches change no links"
    );

    run_launch(
        &s,
        "cl",
        &up,
        main,
        json!([
            result(
                "a",
                "adds",
                "shop.Cart#adds",
                "passed",
                json!([]),
                json!([{"name": "suite", "value": "Cart"}])
            ),
            result(
                "b",
                "removes",
                "shop.Cart#removes",
                "passed",
                json!([]),
                json!([])
            ),
        ]),
    )
    .await;
    sync(&db).await;
    assert_eq!(
        link_rows(&db).await,
        vec![row("adds", "Promo", "ui", "active")],
        "code links go with the code; the manual one stays"
    );
    let (_, rq) = s
        .get(&format!("{}/requirements/2", base("cl")), ADMIN_TOKEN)
        .await;
    assert!(rq["revision"].as_i64().unwrap() > rev);

    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/cl/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"caseSync": {"requirementLinkTypes": ["story"]}})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    run_launch(
        &s,
        "cl",
        &up,
        json!({"name": "ci", "vcs": {"branch": "main"}}),
        json!([
            result("a", "adds", "shop.Cart#adds", "passed",
                json!([{"name": "REQ-1", "type": "story", "url": ""}, {"name": "REQ-2", "type": "requirement", "url": ""}]),
                json!([{"name": "suite", "value": "Cart"}])),
        ]),
    )
    .await;
    sync(&db).await;
    assert_eq!(
        link_rows(&db).await,
        vec![
            row("adds", "Cart", "code", "active"),
            row("adds", "Promo", "ui", "active")
        ],
        "only the configured link types count"
    );
}

async fn coverage(s: &Server, key: &str, q: &str) -> Value {
    let (st, b) = s
        .get(&format!("{}/coverage{q}", base(key)), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{q}: {b}");
    b
}

fn by_number(page: &Value) -> Vec<(i64, String, [i64; 5], bool)> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let c = &i["cases"];
            (
                i["number"].as_i64().unwrap(),
                i["coverage"].as_str().unwrap().to_string(),
                [
                    c["total"].as_i64().unwrap(),
                    c["passed"].as_i64().unwrap(),
                    c["failed"].as_i64().unwrap(),
                    c["notRun"].as_i64().unwrap(),
                    c["skipped"].as_i64().unwrap(),
                ],
                i["changedAfterRun"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn coverage_in_every_scope() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["cases.sync", "cases.backfill"]).await;
    s.project("cv").await;
    let up = s.token("ci", "uploader", &["cv"]).await;
    req(&s, "cv", json!({"title": "Root"})).await;
    req(
        &s,
        "cv",
        json!({"title": "Passed child", "parentNumber": 1}),
    )
    .await;
    req(
        &s,
        "cv",
        json!({"title": "Failed child", "parentNumber": 1}),
    )
    .await;
    req(&s, "cv", json!({"title": "Uncovered"})).await;
    req(&s, "cv", json!({"title": "Not run"})).await;
    req(&s, "cv", json!({"title": "Partial"})).await;
    req(&s, "cv", json!({"title": "Parameters"})).await;
    let main = json!({"name": "ci", "vcs": {"branch": "main"}});
    let t = now_ms();
    let l1 = run_launch(
        &s,
        "cv",
        &up,
        main.clone(),
        json!([
            result("a", "a", "t.A", "passed", json!([]), json!([])),
            result("b", "b", "t.B", "failed", json!([]), json!([])),
            result("c", "c", "t.C", "skipped", json!([]), json!([])),
            param_result("p1", "t.P", "passed", "chrome", t),
            param_result("p2", "t.P", "failed", "firefox", t - 500),
        ]),
    )
    .await;
    sync(&db).await;
    let num = |name: &str| {
        let pool = db.pool.clone();
        let name = name.to_string();
        async move {
            sqlx::query_scalar::<_, i64>("select number from cases where name = $1")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let (a, b, c, p) = (
        num("a").await,
        num("b").await,
        num("c").await,
        num("pays").await,
    );
    let manual = case(&s, "cv", "manual, never run").await;
    links(&s, "cv", 2, json!({"add": [a]})).await;
    links(&s, "cv", 3, json!({"add": [b, a]})).await;
    links(&s, "cv", 5, json!({"add": [c, manual]})).await;
    links(&s, "cv", 6, json!({"add": [a, manual]})).await;
    links(&s, "cv", 7, json!({"add": [p]})).await;

    let d = coverage(&s, "cv", "").await;
    assert_eq!(d["scope"]["kind"], "default");
    assert_eq!(d["scope"]["launches"], 0);
    let got = by_number(&d);
    assert_eq!(
        got[0],
        (1, "failed".into(), [2, 1, 1, 0, 0], false),
        "children count once per case"
    );
    assert_eq!(got[1], (4, "uncovered".into(), [0, 0, 0, 0, 0], false));
    assert_eq!(got[2], (5, "notRun".into(), [2, 0, 0, 1, 1], false));
    assert_eq!(got[3], (6, "partial".into(), [2, 1, 0, 1, 0], false));
    assert_eq!(
        got[4],
        (7, "passed".into(), [1, 1, 0, 0, 0], false),
        "the last result"
    );
    let kids = coverage(&s, "cv", "?parent=REQ-1").await;
    assert_eq!(
        by_number(&kids)
            .iter()
            .map(|k| (k.0, k.1.clone()))
            .collect::<Vec<_>>(),
        vec![(2, "passed".to_string()), (3, "failed".to_string())]
    );

    let (_, q) = s
        .get(
            &format!(
                "{}/requirements?query={}",
                base("cv"),
                urlencode("coverage = failed")
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&q), vec![1, 3]);
    let (_, q) = s
        .get(
            &format!(
                "{}/requirements?query={}",
                base("cv"),
                urlencode("parent = REQ-1 and title ~ \"child\" and coverage in [passed, partial]")
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&q), vec![2]);
    let (_, q) = s
        .get(
            &format!(
                "{}/requirements?query={}",
                base("cv"),
                urlencode("coverage = uncovered or parent is empty and number > 5")
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&q), vec![4, 6, 7]);
    let (_, q) = s
        .get(
            &format!(
                "{}/requirements?query={}",
                base("cv"),
                urlencode("not coverage = passed and issue is empty")
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&q), vec![1, 3, 4, 5, 6]);
    let (st, e) = s
        .get(
            &format!(
                "{}/requirements?query={}",
                base("cv"),
                urlencode("coverage = maybe")
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(e["code"], "invalid_query");
    let (st, v) = s
        .post(
            &format!("{}/query/validate", base("cv")),
            ADMIN_TOKEN,
            json!({"entity": "requirements", "query": "status = active and issue is empty"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["count"], 7);
    let (st, sc) = s
        .get(
            &format!("{}/query/schema?entity=requirements", base("cv")),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let names: Vec<&str> = sc["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "number",
            "title",
            "type",
            "status",
            "parent",
            "issue",
            "externalState",
            "coverage"
        ]
    );

    let l = coverage(&s, "cv", &format!("?launchIds={l1}")).await;
    assert_eq!(l["scope"]["kind"], "launches");
    assert_eq!(l["scope"]["launches"], 1);
    assert_eq!(
        by_number(&l)[4],
        (7, "failed".into(), [1, 0, 1, 0, 0], false),
        "the worst parameter set"
    );

    let l2 = run_launch(
        &s,
        "cv",
        &up,
        json!({"name": "ci", "vcs": {"branch": "release"}}),
        json!([result("b", "b", "t.B", "passed", json!([]), json!([]))]),
    )
    .await;
    sync(&db).await;
    let w = coverage(&s, "cv", "?branch=release&days=7").await;
    assert_eq!(w["scope"]["kind"], "window");
    assert_eq!(w["scope"]["launches"], 1);
    assert_eq!(
        by_number(&w)[0],
        (1, "partial".into(), [2, 1, 0, 1, 0], false)
    );
    let w = coverage(&s, "cv", "?environment=nowhere").await;
    assert_eq!(w["scope"]["days"], 30);
    assert_eq!(by_number(&w)[0].1, "notRun");
    let both = coverage(&s, "cv", &format!("?launchIds={l1},{l2}")).await;
    assert_eq!(
        by_number(&both)[0].1,
        "passed",
        "the newest result of b is the passed one"
    );

    let (st, plan) = s
        .post(
            &format!("{}/test-plans", base("cv")),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "regression", "caseNumbers": [a, b]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let plan_id = plan["id"].as_str().unwrap();
    let pl = coverage(&s, "cv", &format!("?planId={plan_id}")).await;
    assert_eq!(pl["scope"]["kind"], "plan");
    assert_eq!(pl["scope"]["launches"], 0);
    assert_eq!(by_number(&pl)[0].1, "notRun");
    sqlx::query("update launches set test_plan_id = $1 where id = $2")
        .bind(uuid::Uuid::parse_str(plan_id).unwrap())
        .bind(uuid::Uuid::parse_str(&l1).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let pl = coverage(&s, "cv", &format!("?planId={plan_id}")).await;
    assert_eq!(pl["scope"]["launchIds"], json!([l1]));
    assert_eq!(by_number(&pl)[0].1, "failed");
    let pl = coverage(&s, "cv", &format!("?planId={plan_id}&launchId={l1}")).await;
    assert_eq!(by_number(&pl)[0].1, "failed");
    let (st, _) = s
        .get(
            &format!("{}/coverage?planId={plan_id}&launchId={l2}", base("cv")),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a launch of another plan");

    for bad in [
        format!("?planId={plan_id}&branch=main"),
        format!("?launchIds={l1}&days=3"),
        "?days=0".to_string(),
        "?days=400".to_string(),
        "?launchIds=x".to_string(),
        "?parent=abc".to_string(),
        format!("?launchIds={}", vec![l1.clone(); 51].join(",")),
    ] {
        let (st, b) = s
            .get(&format!("{}/coverage{bad}", base("cv")), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {b}");
    }

    let r = send(
        &s,
        Method::GET,
        &format!("{}/requirements/6", base("cv")),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::PATCH,
        &format!("{}/requirements/6", base("cv")),
        ADMIN_TOKEN,
        r.etag.as_deref(),
        Some(json!({"description": "changed after the run"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let d = coverage(&s, "cv", "?limit=1&before=5").await;
    assert_eq!(
        by_number(&d)[0],
        (6, "partial".into(), [2, 1, 0, 1, 0], true)
    );
    assert_eq!(d["next"], "6");
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn wait_job(s: &Server, key: &str, id: &str, token: &str, want: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (st, j) = s.get(&format!("{}/tm-jobs/{id}", base(key)), token).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        if want.contains(&j["status"].as_str().unwrap_or_default()) {
            return j;
        }
        assert!(
            !matches!(j["status"].as_str(), Some("failed" | "cancelled"))
                || want.contains(&"failed"),
            "{j}"
        );
        assert!(Instant::now() < deadline, "job stays {j}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn coverage_exports_download_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ex").await;
    req(
        &s,
        "ex",
        json!({"title": "=HYPERLINK(\"https://example.com\")"}),
    )
    .await;
    req(
        &s,
        "ex",
        json!({"title": "Child, \"quoted\"", "parentNumber": 1}),
    )
    .await;
    req(&s, "ex", json!({"title": "Second"})).await;
    let c = case(&s, "ex", "c").await;
    links(&s, "ex", 2, json!({"add": [c]})).await;
    let other = s.token("other", "editor", &["ex"]).await;
    for format in ["csv", "xlsx"] {
        let (st, job) = s
            .post(
                &format!("{}/coverage/exports", base("ex")),
                ADMIN_TOKEN,
                json!({"format": format}),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{job}");
        assert_eq!(job["kind"], "coverage_export");
        let url = job["downloadUrl"].as_str().unwrap().to_string();
        let id = job["id"].as_str().unwrap();
        let done = wait_job(&s, "ex", id, ADMIN_TOKEN, &["done"]).await;
        assert_eq!(done["progress"]["total"], 3);
        let (st, _) = s.get(&format!("{}/tm-jobs/{id}", base("ex")), &other).await;
        assert_eq!(
            st,
            StatusCode::NOT_FOUND,
            "a coverage export job is its creator's"
        );
        let r = s.req(Method::GET, &url, Some(&other)).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "only its creator");
        let r = s
            .req(
                Method::GET,
                &url.replace("token=", "token=0"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = s
            .req(Method::GET, &url, Some(ADMIN_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let ct = r.headers()["content-type"].to_str().unwrap().to_string();
        let bytes = r.bytes().await.unwrap();
        if format == "csv" {
            assert!(ct.starts_with("text/csv"), "{ct}");
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            let lines: Vec<&str> = text.trim_start_matches('\u{feff}').lines().collect();
            assert_eq!(lines.len(), 4, "{text}");
            assert!(lines[0].starts_with("\"Key\",\"Title\""));
            assert!(
                lines[1].starts_with("\"REQ-1\",\"'=HYPERLINK"),
                "{}",
                lines[1]
            );
            assert!(lines[1].contains("\"notRun\",1,0,0,1,0"), "{}", lines[1]);
            assert!(
                lines[2].starts_with("\"REQ-2\",\"Child, \"\"quoted\"\"\""),
                "tree order: {}",
                lines[2]
            );
            assert!(lines[3].starts_with("\"REQ-3\""));
        } else {
            assert!(ct.contains("spreadsheetml"), "{ct}");
            assert_eq!(&bytes[..2], b"PK");
        }
        let r = s
            .req(Method::GET, &url, Some(ADMIN_TOKEN))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "a second download");
    }
    let (st, b) = s
        .post(
            &format!("{}/coverage/exports", base("ex")),
            ADMIN_TOKEN,
            json!({"format": "pdf"}),
        )
        .await;
    assert!(st.is_client_error(), "{b}");
    let (st, _) = s
        .post(
            &format!("{}/coverage/exports", base("ex")),
            ADMIN_TOKEN,
            json!({"format": "csv", "planId": uuid::Uuid::now_v7()}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    audit_has(&s, &["export.create", "export.download"]).await;
}

fn widget(id: &str, kind: &str) -> Value {
    json!({"id": id, "type": kind, "title": "", "settings": {"limit": 2}, "x": 0, "y": 0, "w": 6, "h": 4})
}

#[tokio::test]
async fn dashboard_widget_and_its_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    sqlx::query(
        "update fastproof_schema_migrations set installed_on = installed_on - interval '2 days' where version < 911",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into replica_levels (instance, tm_level, version, seen_at) values ('old', 1, '0.2.0-dev2', now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let s = Server::start(&db).await;
    s.project("wd").await;
    let path = format!("{}/dashboards", base("wd"));
    let (st, b) = s
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"name": "rq", "widgets": [widget("cov", "requirement-coverage")]}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "upgrade_in_progress");
    assert_eq!(b["details"]["what"], "dashboards.requirement-coverage");
    let (st, b) = s
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"name": "plain", "widgets": [widget("ls", "launch-statistics")]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    sqlx::query("delete from replica_levels")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut open = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    let (st, b) = open
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"name": "rq", "widgets": [widget("cov", "requirement-coverage"),
                widget("cov2", "requirement-coverage"), widget("cov3", "requirement-coverage")]}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "at most two: {b}");
    let (st, board) = open
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"name": "rq", "widgets": [widget("cov", "requirement-coverage")]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{board}");
    req(&open, "wd", json!({"title": "A"})).await;
    req(&open, "wd", json!({"title": "B"})).await;
    req(&open, "wd", json!({"title": "C"})).await;
    let c = case(&open, "wd", "c").await;
    links(&open, "wd", 1, json!({"add": [c]})).await;
    let (st, data) = open
        .get(
            &format!("{path}/{}/data", board["id"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{data}");
    let w = &data["widgets"]["cov"];
    assert_eq!(w["status"], "ok", "{w}");
    assert_eq!(w["data"]["kind"], "requirementCoverage");
    assert_eq!(w["data"]["total"], 3);
    assert_eq!(
        w["data"]["counts"],
        json!({"uncovered": 2, "failed": 0, "notRun": 1, "partial": 0, "passed": 0})
    );
    assert_eq!(w["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(w["data"]["scope"]["kind"], "default");
    open.stop().await;
}

#[derive(Default)]
struct Tracker {
    issues: Vec<(String, String, String, String)>,
    searches: Vec<String>,
}

type Shared = Arc<Mutex<Tracker>>;

async fn tracker_handle(
    State(st): State<Shared>,
    method: AxMethod,
    uri: Uri,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path().to_string();
    let reply = |v: Value| axum::Json(v).into_response();
    let mut t = st.lock().unwrap();
    let issue = |base: &str, key: &str, title: &str, state: &str, project: &str| {
        json!({"id": format!("1{}", key.len()), "key": key, "fields": {"summary": title,
            "status": {"name": state, "statusCategory": {"key": if state == "Done" { "done" } else { "indeterminate" }}},
            "project": {"key": project},
            "description": {"type": "doc", "version": 1, "content": [{"type": "paragraph", "content": [
                {"type": "text", "text": format!("Spec of {key}")}]}]}},
            "self": format!("{base}/rest/api/3/issue/{key}")})
    };
    if method == AxMethod::POST && path == "/jc/rest/api/3/search/jql" {
        let b: Value = serde_json::from_slice(&body).unwrap_or_default();
        let jql = b["jql"].as_str().unwrap_or_default().to_string();
        t.searches.push(jql.clone());
        let all: Vec<Value> = t
            .issues
            .iter()
            .map(|(k, title, state, project)| issue("/jc", k, title, state, project))
            .collect();
        if let Some(list) = jql
            .strip_prefix("key in (")
            .and_then(|r| r.strip_suffix(')'))
        {
            let keys: Vec<&str> = list.split(',').collect();
            let found: Vec<Value> = all
                .into_iter()
                .filter(|i| keys.contains(&i["key"].as_str().unwrap()))
                .collect();
            return reply(json!({"issues": found, "isLast": true}));
        }
        return match b["nextPageToken"].as_str() {
            None => reply(
                json!({"issues": all[..2.min(all.len())], "nextPageToken": "p2", "isLast": false}),
            ),
            Some(_) => reply(json!({"issues": all[2.min(all.len())..], "isLast": true})),
        };
    }
    reply(json!({"errorMessages": ["no route"]}))
}

#[tokio::test]
async fn tracker_import_dry_run_apply_and_state_sync() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state: Shared = Arc::default();
    {
        let mut t = state.lock().unwrap();
        t.issues = vec![
            (
                "SHOP-1".into(),
                "Login".into(),
                "To Do".into(),
                "SHOP".into(),
            ),
            (
                "SHOP-2".into(),
                "Logout".into(),
                "In Progress".into(),
                "SHOP".into(),
            ),
            (
                "OTHER-1".into(),
                "Elsewhere".into(),
                "To Do".into(),
                "OTHER".into(),
            ),
            (
                "SHOP-3".into(),
                "Search".into(),
                "Done".into(),
                "SHOP".into(),
            ),
        ];
    }
    let app = axum::Router::new()
        .fallback(tracker_handle)
        .with_state(state.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    let origin = format!("http://{addr}");
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let s = Server::start_cfg(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", "127.0.0.1"),
            ("FASTPROOF_SECRETS_KEY_FILE", key.to_str().unwrap()),
        ],
        |c| {
            c.integrations.tracker_tick = Duration::from_millis(100);
            c.integrations.tracker_poll = Duration::from_secs(1);
        },
    )
    .await;
    s.project("ti").await;
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ti/secrets/jc",
            Some(ADMIN_TOKEN),
            Some(json!({"value": "bot@example.com:jira-api-token", "purpose": "tracker", "origin": origin})),
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    let (st, b) = s
        .call(
            Method::PUT,
            "/api/v1/projects/ti/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"issueTrackers": [{"id": "jc", "type": "jira",
                "urlTemplate": format!("{origin}/jc/browse/{{key}}"), "keyRegex": "SHOP-[0-9]+",
                "api": {"url": format!("{origin}/jc"), "tokenSecretRef": "jc", "project": "SHOP"}}]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    req(&s, "ti", json!({"title": "Imported", "type": "epic"})).await;
    let b = base("ti");
    for (body, code) in [
        (
            json!({"trackerId": "nope", "query": "project = SHOP"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"trackerId": "jc", "query": ""}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"trackerId": "jc", "query": "x".repeat(1001)}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"trackerId": "jc", "query": "a\u{0}b"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"trackerId": "jc", "query": "project = SHOP", "parentNumber": 9}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"trackerId": "jc", "query": "labels = req) OR (project = OTHER"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"trackerId": "jc", "query": "summary ~ \"open"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (st, e) = s
            .post(&format!("{b}/requirements/import"), ADMIN_TOKEN, body)
            .await;
        assert_eq!(st, code, "{e}");
    }
    let editor = s.token("ed", "editor", &["ti"]).await;
    let (st, job) = s
        .post(
            &format!("{b}/requirements/import"),
            &editor,
            json!({"trackerId": "jc", "query": "project = SHOP AND labels = req", "parentNumber": 1}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["kind"], "requirements");
    let id = job["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            &format!("{b}/requirements/import/{id}/apply"),
            &editor,
            json!({}),
        )
        .await;
    assert!(
        st == StatusCode::CONFLICT || st == StatusCode::ACCEPTED,
        "apply before the dry run is ready: {st}"
    );
    let ready = wait_job(&s, "ti", id, &editor, &["ready", "done"]).await;
    if ready["status"] == "ready" {
        assert_eq!(ready["report"]["create"], 3, "{ready}");
        assert_eq!(ready["report"]["skip"], 1, "the issue of another project");
        let (_, none) = s
            .get(&format!("{b}/requirements?parent=1"), ADMIN_TOKEN)
            .await;
        assert!(numbers(&none).is_empty(), "a dry run writes nothing");
        let (st, j) = s
            .post(
                &format!("{b}/requirements/import/{id}/apply"),
                &editor,
                json!({}),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    }
    let done = wait_job(&s, "ti", id, &editor, &["done"]).await;
    assert_eq!(done["report"]["create"], 3, "{done}");
    assert_eq!(
        state.lock().unwrap().searches[0],
        "project = \"SHOP\" AND (project = SHOP AND labels = req)",
        "the query goes to the tracker limited to its project"
    );
    let (_, under) = s
        .get(&format!("{b}/requirements?parent=1"), ADMIN_TOKEN)
        .await;
    assert_eq!(under["items"].as_array().unwrap().len(), 3);
    let (_, r) = s.get(&format!("{b}/requirements/2"), ADMIN_TOKEN).await;
    assert_eq!(r["origin"], "tracker");
    assert_eq!(r["title"], "Login");
    assert_eq!(r["description"], "Spec of SHOP-1");
    assert_eq!(r["tracker"]["issueKey"], "SHOP-1");
    assert_eq!(r["tracker"]["trackerId"], "jc");
    assert_eq!(r["tracker"]["externalState"], "To Do");
    assert!(
        r["tracker"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/jc/browse/SHOP-1")
    );

    let (st, job2) = s
        .post(
            &format!("{b}/requirements/import"),
            &editor,
            json!({"trackerId": "jc", "query": "project = SHOP"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job2}");
    let id2 = job2["id"].as_str().unwrap();
    let r2 = wait_job(&s, "ti", id2, &editor, &["ready"]).await;
    assert_eq!(
        r2["report"]["create"], 0,
        "a repeated import matches by issue key: {r2}"
    );
    assert_eq!(r2["report"]["update"], 0);

    {
        let mut t = state.lock().unwrap();
        t.issues[0].1 = "Login with SSO".into();
        t.issues[0].2 = "Done".into();
    }
    sqlx::query("update requirements set synced_at = now() - interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, r) = s.get(&format!("{b}/requirements/2"), ADMIN_TOKEN).await;
        if r["tracker"]["externalState"] == "Done" {
            assert_eq!(r["title"], "Login with SSO");
            break;
        }
        assert!(Instant::now() < deadline, "the state never synced: {r}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let viewer = s.token("v", "viewer", &["ti"]).await;
    let (st, _) = s
        .post(
            &format!("{b}/requirements/import/{id2}/apply"),
            &viewer,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.get(&format!("{b}/tm-jobs/{id2}"), &viewer).await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a requirement import job needs cases:import"
    );
    audit_has(&s, &["requirement.import"]).await;
}

async fn wait_for_lock(db: &TestDb, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity where datname = current_database() \
             and pid <> pg_backend_pid() and wait_event_type = 'Lock' and query ilike $1",
        )
        .bind(format!("%{what}%"))
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "nothing waited on {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn searches_and_scoped_coverage_take_the_heavy_guard() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "1")]).await;
    s.project("hv").await;
    req(&s, "hv", json!({"title": "Checkout"})).await;
    let viewer = s.token("hvv", "viewer", &["hv"]).await;
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("lock table requirements in access exclusive mode")
        .execute(&mut *lock)
        .await
        .unwrap();
    let slow = {
        let c = Server::client_only(&s.base);
        let v = viewer.clone();
        tokio::spawn(async move {
            c.get(
                &format!(
                    "/api/v1/projects/hv/requirements?query={}",
                    urlencode("coverage = uncovered")
                ),
                &v,
            )
            .await
        })
    };
    wait_for_lock(&db, "as has_children").await;
    for path in [
        "/api/v1/projects/hv/requirements?q=check",
        "/api/v1/projects/hv/requirements?query=status%20%3D%20active",
        "/api/v1/projects/hv/coverage?days=365",
        "/api/v1/projects/hv/coverage?branch=main",
    ] {
        let (st, b) = s.get(path, &viewer).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{path}: {b}");
    }
    lock.rollback().await.unwrap();
    let (st, l) = slow.await.unwrap();
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![1]));
    let (st, _) = s.get("/api/v1/projects/hv/coverage?days=7", &viewer).await;
    assert_eq!(st, StatusCode::OK, "the guard is released");
}

#[tokio::test]
async fn a_project_keeps_few_dry_runs_and_syncs_clean_titles() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["cases.jobs"]).await;
    s.project("dr").await;
    req(
        &s,
        "dr",
        json!({"title": "Tracked", "trackerId": "jc", "issueKey": "SHOP-1"}),
    )
    .await;
    let store = Store::new(db.pool.clone());
    let p = store.project("dr").await.unwrap().unwrap();
    let limits = TmLimits::default();
    let ctx = CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: "editor",
        limits: &limits,
        can_review: false,
    };
    let params = fastproof_store::tm::requirements::RequirementImportParams {
        tracker_id: "jc".into(),
        query: "labels = req".into(),
        phase: "dryRun".into(),
        ..Default::default()
    };
    let mut ids = Vec::new();
    for _ in 0..5 {
        let job = store
            .start_requirement_import(&ctx, "editor", &params)
            .await
            .unwrap();
        sqlx::query(
            "update tm_jobs set status = 'ready', \
             params = params || jsonb_build_object('items', jsonb_build_array(jsonb_build_object( \
               'issueKey', 'SHOP-2', 'title', repeat('x', 1000)))) where id = $1",
        )
        .bind(job.id)
        .execute(&db.pool)
        .await
        .unwrap();
        ids.push(job.id);
    }
    let rows: Vec<(uuid::Uuid, String, i64)> = sqlx::query_as(
        "select id, status, jsonb_array_length(params -> 'items')::bigint from tm_jobs \
         where project_id = $1 and kind = 'requirements'",
    )
    .bind(p.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    for (i, id) in ids.iter().enumerate() {
        let (_, status, items) = rows.iter().find(|r| r.0 == *id).unwrap();
        let kept = i >= ids.len() - fastproof_store::tm::requirements::MAX_READY_IMPORTS as usize;
        assert_eq!(
            (status.as_str(), *items),
            if kept { ("ready", 1) } else { ("cancelled", 0) },
            "dry run {i}"
        );
    }

    let rid: uuid::Uuid = sqlx::query_scalar("select id from requirements where project_id = $1")
        .bind(p.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let tracked = fastproof_store::tm::requirements::TrackedRequirement {
        id: rid,
        project_id: p.id,
        project_key: p.key.clone(),
        tracker_id: "jc".into(),
        issue_key: "SHOP-1".into(),
    };
    assert!(
        store
            .record_requirement_state(
                &tracked,
                "Login\n\u{7}with SSO",
                "Done\r\n",
                None,
                limits.max_name_bytes
            )
            .await
            .unwrap()
    );
    let (_, r) = s
        .get(&format!("{}/requirements/1", base("dr")), ADMIN_TOKEN)
        .await;
    assert_eq!(r["title"], "Login  with SSO", "{r}");
    assert_eq!(r["tracker"]["externalState"], "Done");
}

#[tokio::test]
async fn code_link_sync_waits_for_a_person_without_a_deadlock() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &[]).await;
    s.project("dl").await;
    req(&s, "dl", json!({"title": "Checkout"})).await;
    let c = case(&s, "dl", "c").await;
    let store = Store::new(db.pool.clone());
    let p = store.project("dl").await.unwrap().unwrap();
    let (rid, cid): (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "select r.id, c.id from requirements r join cases c on c.project_id = r.project_id \
         where r.project_id = $1 and r.number = 1 and c.number = $2",
    )
    .bind(p.id)
    .bind(c)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let mut person = db.pool.begin().await.unwrap();
    sqlx::query("select 1 from requirements where id = $1 for update")
        .bind(rid)
        .execute(&mut *person)
        .await
        .unwrap();
    let sync = {
        let pool = db.pool.clone();
        let (pid, pkey) = (p.id, p.key.clone());
        tokio::spawn(async move {
            let limits = TmLimits::default();
            let ctx = CaseCtx {
                project_id: pid,
                project_key: &pkey,
                by: "code",
                limits: &limits,
                can_review: false,
            };
            let data = fastproof_store::tm::sync::CodeData {
                labels: vec![("requirement".into(), "REQ-1".into())],
                ..Default::default()
            };
            let mut tx = pool.begin().await?;
            let n = fastproof_store::tm::requirements::sync_code_links(
                &mut tx,
                &ctx,
                &fastproof_store::analysis::settings::CaseSyncSettings::default(),
                &[(cid, &data)],
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            tx.commit().await?;
            anyhow::Ok(n)
        })
    };
    wait_for_lock(&db, "requirement").await;
    sqlx::query(
        "insert into requirement_links (requirement_id, case_id, project_id, origin, status, created_by) \
         values ($1, $2, $3, 'ui', 'active', 'person')",
    )
    .bind(rid)
    .bind(cid)
    .bind(p.id)
    .execute(&mut *person)
    .await
    .expect("the person's link is written while the sync waits");
    person.commit().await.unwrap();
    assert_eq!(sync.await.unwrap().unwrap(), 0, "the person's link stays");
    let origin: String = sqlx::query_scalar("select origin from requirement_links")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(origin, "ui");
}

fn p95(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
    (at(0.5), at(0.95))
}

async fn timed(s: &Server, path: &str, n: usize) -> (f64, f64) {
    let mut ms = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        let (st, b) = s.get(path, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{b}");
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    p95(ms)
}

/// §14: a coverage page of 50 requirements < 300 ms in the default scope and < 1 s in a plan
/// scope, with 3000 requirements, 25 000 links and 20 000 automated cases. Run in release:
/// `cargo test --release -p fastproof-server --test tm2_requirements coverage_perf -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn coverage_perf() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(&db, &["cases.sync", "cases.backfill"]).await;
    s.project("perf").await;
    let up = s.token("ci", "uploader", &["perf"]).await;
    let cases = 20_000usize;
    let (_, l) = s
        .post(
            "/api/v1/projects/perf/launches",
            &up,
            json!({"name": "ci", "vcs": {"branch": "main"}}),
        )
        .await;
    let lid = l["id"].as_str().unwrap().to_string();
    let t = Instant::now();
    for chunk in 0..cases / 1000 {
        let batch: Vec<Value> = (0..1000)
            .map(|i| {
                let n = chunk * 1000 + i;
                let status = match n % 10 {
                    0 => "failed",
                    1 => "skipped",
                    _ => "passed",
                };
                result(
                    &format!("r{n}"),
                    &format!("test {n}"),
                    &format!("perf.T{n}"),
                    status,
                    json!([]),
                    json!([]),
                )
            })
            .collect();
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/perf/launches/{lid}/results"),
                &up,
                Value::Array(batch),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    }
    let (st, f) = s
        .post(
            &format!("/api/v1/projects/perf/launches/{lid}/finish"),
            &up,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{f}");
    let store = Store::new(db.pool.clone());
    let deadline = Instant::now() + Duration::from_secs(1800);
    loop {
        store
            .run_case_sync("perf", &TmLimits::default(), Duration::from_secs(600))
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("select count(*) from cases")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if n as usize >= cases {
            break;
        }
        assert!(Instant::now() < deadline, "only {n} cases");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    println!(
        "seeded {cases} automated cases in {:.1} s",
        t.elapsed().as_secs_f64()
    );
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'perf'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "with top as (select gen_random_uuid() as id, n from generate_series(1, 60) n) \
         insert into requirements (id, project_id, number, title, path_ids, depth, created_by) \
         select id, $1, n, 'Epic ' || n, array[id], 1, 'perf' from top",
    )
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "with s as (select gen_random_uuid() as id, p.id as parent, p.number as pn, k \
           from requirements p cross join generate_series(1, 49) k where p.project_id = $1) \
         insert into requirements (id, project_id, number, title, parent_id, path_ids, depth, created_by) \
         select s.id, $1, 60 + row_number() over (order by s.pn, s.k), 'Story ' || s.pn || '.' || s.k, s.parent, \
           array[s.parent, s.id], 2, 'perf' from s",
    )
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update projects set requirement_seq = 3000 where id = $1")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into requirement_links (requirement_id, case_id, project_id, origin, created_by) \
         select r.id, c.id, $1, 'ui', 'perf' from \
           (select id, row_number() over (order by number) - 1 as k from requirements where depth = 2) r \
           join (select id, row_number() over (order by number) - 1 as k from cases) c \
             on c.k in ((r.k * 7) % 20000, (r.k * 7 + 1) % 20000, (r.k * 7 + 2) % 20000, (r.k * 13 + 5) % 20000, \
               (r.k * 13 + 6) % 20000, (r.k * 17 + 9) % 20000, (r.k * 19 + 3) % 20000, (r.k * 23 + 11) % 20000, \
               (r.k * 29 + 4) % 20000) \
         on conflict do nothing",
    )
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    let (reqs, links): (i64, i64) = sqlx::query_as(
        "select (select count(*) from requirements), (select count(*) from requirement_links)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let first: i64 = sqlx::query_scalar("select number from cases order by number limit 1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let (st, plan) = s
        .post(
            "/api/v1/projects/perf/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "all", "caseNumbers": [first]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let plan_id = plan["id"].as_str().unwrap().to_string();
    sqlx::query("update launches set test_plan_id = $1 where id = $2")
        .bind(uuid::Uuid::parse_str(&plan_id).unwrap())
        .bind(uuid::Uuid::parse_str(&lid).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("analyze").execute(&db.pool).await.unwrap();
    let b = base("perf");
    timed(&s, &format!("{b}/coverage?limit=50"), 3).await;
    let d = timed(&s, &format!("{b}/coverage?limit=50"), 30).await;
    let p = timed(&s, &format!("{b}/coverage?limit=50&planId={plan_id}"), 30).await;
    let c = timed(
        &s,
        &format!("{b}/coverage?limit=50&parent=REQ-1&planId={plan_id}"),
        30,
    )
    .await;
    let w = timed(
        &s,
        &format!("{b}/coverage?limit=50&branch=main&days=30"),
        30,
    )
    .await;
    let q = timed(
        &s,
        &format!(
            "{b}/requirements?limit=50&query={}",
            urlencode("coverage = failed")
        ),
        10,
    )
    .await;
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    println!(
        "requirements {reqs}, links {links}, cases {cases}\n\
         coverage default page of 50: p50 {:.0} ms, p95 {:.0} ms (target < 300)\n\
         coverage plan page of 50:    p50 {:.0} ms, p95 {:.0} ms (target < 1000)\n\
         coverage plan, 49 children:  p50 {:.0} ms, p95 {:.0} ms\n\
         coverage window 30 days:     p50 {:.0} ms, p95 {:.0} ms\n\
         FPQL coverage = failed:      p50 {:.0} ms, p95 {:.0} ms\n\
         load average: {}",
        d.0,
        d.1,
        p.0,
        p.1,
        c.0,
        c.1,
        w.0,
        w.1,
        q.0,
        q.1,
        load.trim()
    );
}
