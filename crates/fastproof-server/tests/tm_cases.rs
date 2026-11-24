//! Test cases over HTTP: create/read/update with ETags and versions, Markdown safety, lists,
//! filters, cursors, the tree, bulk actions, limits, the permission matrix and IDOR checks,
//! attachments, suggestions, code sync (trusted vs feature branch vs debug vs a launch
//! without `cases:sync`, `ALLURE_ID`, rename through identities) and the backfill route.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use fastproof_store::tm::TmLimits;
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

fn steps(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| {
                json!({"action": format!("Step {i}: open the **cart** page and add item {i}"),
                    "expected": format!("Item {i} is in the cart"), "data": "qty=1"})
            })
            .collect(),
    )
}

async fn create(s: &Server, key: &str, body: Value) -> Reply {
    let r = send(
        s,
        Method::POST,
        &format!("/api/v1/projects/{key}/cases"),
        ADMIN_TOKEN,
        None,
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    r
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
async fn case_lifecycle_etags_versions_and_markdown_safety() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lc").await;
    let base = "/api/v1/projects/lc/cases";
    let xss = "<script>alert(1)</script><img src=x onerror=alert(1)>\n\n[x](javascript:alert(1)) \
               ![p](https://example.com/p.png) <a href=\"javascript:alert(2)\">a</a> <img src=y onerror=z>";
    let r = create(
        &s,
        "lc",
        json!({"name": "  Pay by card ", "body": {"description": xss, "preconditions": "Logged in",
            "steps": [{"action": "Open *cart*", "expected": "Cart shown",
                       "steps": [{"action": "Nested", "expected": "ok"}]}],
            "links": [{"name": "Spec", "url": "https://example.com/spec", "type": "link"}]},
            "fields": {"priority": "high"}, "tags": ["smoke", "smoke", "pay"]}),
    )
    .await;
    let c = r.body;
    assert_eq!(c["number"], 1);
    assert_eq!(c["name"], "Pay by card");
    assert_eq!(c["revision"], 1);
    assert_eq!(c["version"], 1);
    assert_eq!(c["origin"], "ui");
    assert_eq!(c["tags"], json!(["smoke", "pay"]));
    assert_eq!(r.etag.as_deref(), Some("\"r1\""));
    assert_eq!(c["keys"], json!([{"kind": "allure_id", "value": "1"}]));
    assert_eq!(c["identities"][0]["kind"], "manual");
    let step_id = c["body"]["steps"][0]["id"].as_str().unwrap().to_string();
    let nested_id = c["body"]["steps"][0]["steps"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(step_id.starts_with("s_") && step_id != nested_id);
    assert_eq!(
        c["body"]["description"], xss,
        "Markdown is stored as written"
    );
    let html = c["html"]["description"].as_str().unwrap();
    for bad in [
        "<script",
        "<img src=\"x",
        "<img src=\"y",
        "onerror=\"",
        "<a href=\"javascript",
        "src=\"https://example.com/p.png",
    ] {
        assert!(!html.contains(bad), "{bad} in {html}");
    }
    assert!(html.contains("&lt;script&gt;"), "{html}");
    assert!(
        c["html"]["steps"][0]["action"]
            .as_str()
            .unwrap()
            .contains("<em>cart</em>")
    );

    let path = format!("{base}/1");
    let g = send(&s, Method::GET, &path, ADMIN_TOKEN, None, None).await;
    assert_eq!(g.status, StatusCode::OK);
    assert_eq!(g.etag.as_deref(), Some("\"r1\""));
    for p in [
        format!("{base}/x"),
        format!("{base}/-1"),
        format!("{base}/0"),
        format!("{base}/99"),
    ] {
        let r = send(&s, Method::GET, &p, ADMIN_TOKEN, None, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}");
    }

    let mut body = c["body"].clone();
    body["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"action": "Pay", "expected": "Paid"}));
    let write = json!({"name": "Pay by card", "body": body, "fields": {"priority": "high"}, "tags": ["smoke", "pay"]});
    let r = send(
        &s,
        Method::PUT,
        &path,
        ADMIN_TOKEN,
        None,
        Some(write.clone()),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            Some("precondition_required")
        )
    );
    let r = send(
        &s,
        Method::PUT,
        &path,
        ADMIN_TOKEN,
        Some("\"r7\""),
        Some(write.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(r.body["code"], "precondition_failed");
    assert_eq!(r.body["details"]["currentRevision"], 1);
    assert_eq!(r.body["details"]["changedBy"], "admin");
    assert_eq!(r.body["details"]["source"], "ui");

    let (a, b) = tokio::join!(
        send(
            &s,
            Method::PUT,
            &path,
            ADMIN_TOKEN,
            Some("\"r1\""),
            Some(write.clone())
        ),
        send(
            &s,
            Method::PUT,
            &path,
            ADMIN_TOKEN,
            Some("\"r1\""),
            Some(write.clone())
        ),
    );
    let mut statuses = [a.status, b.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::PRECONDITION_FAILED],
        "{} {}",
        a.body,
        b.body
    );
    let won = if a.status == StatusCode::OK { a } else { b };
    assert_eq!(won.body["revision"], 2);
    assert_eq!(won.body["version"], 2);
    assert_eq!(won.etag.as_deref(), Some("\"r2\""));
    assert_eq!(
        won.body["body"]["steps"][0]["id"],
        step_id.as_str(),
        "ids stay"
    );
    assert_eq!(
        won.body["body"]["steps"][0]["steps"][0]["id"],
        nested_id.as_str()
    );
    assert!(
        won.body["body"]["steps"][1]["id"]
            .as_str()
            .unwrap()
            .starts_with("s_")
    );

    let same = send(&s, Method::PUT, &path, ADMIN_TOKEN, Some("\"r2\""), Some(won.body["body"].clone().pipe(|b| {
        json!({"name": "Pay by card", "body": b, "fields": {"priority": "high"}, "tags": ["smoke", "pay"]})
    }))).await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.body);
    assert_eq!(
        same.body["revision"], 2,
        "an unchanged write changes nothing"
    );

    let p = send(&s, Method::PATCH, &path, ADMIN_TOKEN, Some("\"r2\""),
        Some(json!({"status": "outdated", "fields": {"priority": null, "severity": "minor"}, "tags": ["regression"]}))).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(p.body["status"], "outdated");
    assert_eq!(p.body["fields"], json!({"severity": "minor"}));
    assert_eq!(p.body["tags"], json!(["regression"]));
    assert_eq!(p.body["version"], 3);
    let r = send(
        &s,
        Method::PATCH,
        &path,
        ADMIN_TOKEN,
        Some("\"r3\""),
        Some(json!({"fields": {"severity": "huge"}})),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_case"))
    );
    assert_eq!(r.body["details"][0]["path"], "fields.severity");
    let r = send(
        &s,
        Method::PATCH,
        &path,
        ADMIN_TOKEN,
        Some("\"r3\""),
        Some(json!({"fields": {"nope": "x"}})),
    )
    .await;
    assert_eq!(r.body["details"][0]["path"], "fields.nope");
    let r = send(
        &s,
        Method::PATCH,
        &path,
        ADMIN_TOKEN,
        Some("\"r3\""),
        Some(json!({"bogus": 1})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let v = send(
        &s,
        Method::GET,
        &format!("{path}/versions"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(v.status, StatusCode::OK);
    let items = v.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["version"], 3);
    assert_eq!(items[0]["changed"], json!(["status", "fields", "tags"]));
    assert_eq!(items[1]["changed"], json!(["steps"]));
    assert_eq!(items[2]["source"], "ui");
    let v = send(
        &s,
        Method::GET,
        &format!("{path}/versions?limit=1"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    let next = v.body["next"].as_str().unwrap();
    let v2 = send(
        &s,
        Method::GET,
        &format!("{path}/versions?limit=1&before={next}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(v2.body["items"][0]["version"], 2);
    let one = send(
        &s,
        Method::GET,
        &format!("{path}/versions/1"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(one.body["snapshot"]["status"], "active");
    assert_eq!(
        one.body["snapshot"]["body"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let r = send(
        &s,
        Method::GET,
        &format!("{path}/versions/9"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &s,
        Method::POST,
        &format!("{path}/versions/1/restore"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::POST,
        &format!("{path}/versions/1/restore"),
        ADMIN_TOKEN,
        Some("\"r3\""),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["version"], 4);
    assert_eq!(r.body["status"], "active");
    assert_eq!(r.body["fields"], json!({"priority": "high"}));
    let v = send(
        &s,
        Method::GET,
        &format!("{path}/versions?limit=1"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(v.body["items"][0]["source"], "restore");

    let r = send(&s, Method::DELETE, &path, ADMIN_TOKEN, None, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let (_, l) = s.get(base, ADMIN_TOKEN).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 0);
    let (_, l) = s.get(&format!("{base}?archived=true"), ADMIN_TOKEN).await;
    assert_eq!(l["items"][0]["archived"], true);
    let r = send(
        &s,
        Method::POST,
        &format!("{path}/restore"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["archivedAt"], Value::Null);

    let sid = uuid::Uuid::now_v7();
    let rev = r.body["revision"].as_i64().unwrap();
    sqlx::query(
        "insert into case_suggestions (id, project_id, case_id, base_revision, proposal, rationale, agent) \
         select $1, project_id, id, $2, $3, 'clearer', 'mcp:test' from cases where number = 1",
    )
    .bind(sid)
    .bind(rev)
    .bind(json!({"name": "Pay with a card", "tags": ["ai"]}))
    .execute(&db.pool)
    .await
    .unwrap();
    let l = send(
        &s,
        Method::GET,
        &format!("{path}/suggestions"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(l.body["items"][0]["status"], "pending");
    let acc = format!("/api/v1/projects/lc/case-suggestions/{sid}/accept");
    let r = send(&s, Method::POST, &acc, ADMIN_TOKEN, None, None).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::POST,
        &acc,
        ADMIN_TOKEN,
        Some(&format!("\"r{rev}\"")),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["case"]["name"], "Pay with a card");
    assert_eq!(r.body["suggestion"]["status"], "accepted");
    let v = send(
        &s,
        Method::GET,
        &format!("{path}/versions?limit=1"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(v.body["items"][0]["source"], "ai");
    let stale = uuid::Uuid::now_v7();
    sqlx::query(
        "insert into case_suggestions (id, project_id, case_id, base_revision, proposal, agent) \
         select $1, project_id, id, 1, '{\"name\": \"old\"}', 'mcp:test' from cases where number = 1",
    )
    .bind(stale)
    .execute(&db.pool)
    .await
    .unwrap();
    let cur = r.body["case"]["revision"].as_i64().unwrap();
    let r = send(
        &s,
        Method::POST,
        &format!("/api/v1/projects/lc/case-suggestions/{stale}/accept"),
        ADMIN_TOKEN,
        Some(&format!("\"r{cur}\"")),
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("suggestion_stale"))
    );
    let r = send(
        &s,
        Method::POST,
        &format!("/api/v1/projects/lc/case-suggestions/{stale}/reject"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("suggestion_decided"))
    );

    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/lc/markdown/preview",
        ADMIN_TOKEN,
        None,
        Some(json!({"markdown": "**b** <script>x</script>"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let h = r.body["html"].as_str().unwrap();
    assert!(
        h.contains("<strong>b</strong>") && !h.contains("<script"),
        "{h}"
    );
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/lc/markdown/preview",
        ADMIN_TOKEN,
        None,
        Some(json!({"markdown": "a".repeat(64 * 1024 + 1)})),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_markdown"))
    );

    let r = send(
        &s,
        Method::DELETE,
        &format!("{path}?purge=true"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(&s, Method::GET, &path, ADMIN_TOKEN, None, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let n: i64 = sqlx::query_scalar("select count(*) from case_versions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    audit_has(
        &s,
        &[
            "case.create",
            "case.update",
            "case.archive",
            "case.restore",
            "case.purge",
            "caseSuggestion.accept",
        ],
    )
    .await;
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for Value {}

#[tokio::test]
async fn case_lists_filters_tree_and_bulk() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ls").await;
    let (_, top) = s
        .post(
            "/api/v1/projects/ls/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Checkout"}),
        )
        .await;
    let top = top["id"].as_str().unwrap().to_string();
    let (_, sub) = s
        .post(
            "/api/v1/projects/ls/case-folders",
            ADMIN_TOKEN,
            json!({"name": "Cards", "parentId": top}),
        )
        .await;
    let sub = sub["id"].as_str().unwrap().to_string();
    for i in 1..=7 {
        let folder = match i % 3 {
            0 => Value::Null,
            1 => json!(top),
            _ => json!(sub),
        };
        create(
            &s,
            "ls",
            json!({"name": format!("Case {i} {}", if i == 5 { "refund" } else { "pay" }), "folderId": folder,
                "status": if i == 7 { "draft" } else { "active" },
                "body": {"steps": [{"action": format!("press button {i}")}]},
                "fields": {"priority": if i % 2 == 0 { "high" } else { "low" }, "epic": "Shop", "feature": format!("F{}", i % 2)},
                "tags": if i <= 2 { json!(["smoke"]) } else { json!([]) }}),
        )
        .await;
    }
    let base = "/api/v1/projects/ls/cases";
    let numbers = |v: &Value| -> Vec<i64> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["number"].as_i64().unwrap())
            .collect()
    };
    let (_, l) = s.get(&format!("{base}?folder={top}"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![1, 4, 7]);
    let (_, l) = s
        .get(&format!("{base}?folder={top}&recursive=true"), ADMIN_TOKEN)
        .await;
    assert_eq!(numbers(&l), vec![1, 2, 4, 5, 7]);
    let (_, l) = s.get(&format!("{base}?status=draft"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![7]);
    let (_, l) = s
        .get(
            &format!("{base}?status=active&status=draft&field=priority:high"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&l), vec![2, 4, 6]);
    let (_, l) = s
        .get(
            &format!("{base}?field=priority:high&field=priority:low&field=feature:F1"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(numbers(&l), vec![1, 3, 5, 7]);
    let (_, l) = s.get(&format!("{base}?tag=smoke"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![1, 2]);
    let (_, l) = s.get(&format!("{base}?q=refund"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![5]);
    let (_, l) = s.get(&format!("{base}?q=button"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l).len(), 7, "step text is searchable");
    let (_, l) = s.get(&format!("{base}?q=TC-3"), ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![3]);
    let (_, l) = s
        .get(&format!("{base}?automation=automated"), ADMIN_TOKEN)
        .await;
    assert!(numbers(&l).is_empty());
    let (st, _) = s.get(&format!("{base}?status=bogus"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .get(
            &format!("{base}?folder={}", uuid::Uuid::now_v7()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let mut seen = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let q = match &before {
            Some(b) => format!("{base}?limit=3&before={b}"),
            None => format!("{base}?limit=3"),
        };
        let (st, l) = s.get(&q, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{l}");
        seen.extend(numbers(&l));
        match l["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, (1..=7).collect::<Vec<_>>());
    let mut seen = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let q = match &before {
            Some(b) => format!("{base}?sort=updated&limit=2&before={b}"),
            None => format!("{base}?sort=updated&limit=2"),
        };
        let (_, l) = s.get(&q, ADMIN_TOKEN).await;
        seen.extend(numbers(&l));
        match l["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, (1..=7).rev().collect::<Vec<_>>());
    let (st, _) = s.get(&format!("{base}?before=garbage"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (_, t) = s.get(&format!("{base}/tree"), ADMIN_TOKEN).await;
    assert_eq!(t["nodes"][0]["name"], "Checkout");
    assert_eq!(
        t["nodes"][0]["counts"],
        json!({"total": 5, "manual": 5, "automated": 0})
    );
    assert_eq!(t["nodes"][0]["hasChildren"], true);
    let (_, t) = s
        .get(&format!("{base}/tree?groupBy=behavior"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        t["nodes"],
        json!([{"id": "Shop", "name": "Shop", "counts": {"total": 7, "manual": 7, "automated": 0}, "hasChildren": true}])
    );
    let (_, t) = s
        .get(
            &format!("{base}/tree?groupBy=behavior&parent=Shop"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(t["nodes"][0]["id"], "Shop\u{1f}F0");
    assert_eq!(t["nodes"][1]["counts"]["total"], 4);

    let bulk = format!("{base}/bulk");
    let (st, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"numbers": [1, 2, 99], "action": "addTags", "value": ["bulk"]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["updated"], 2);
    assert_eq!(r["skipped"], json!([{"number": 99, "code": "not_found"}]));
    let (_, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"filter": {"tags": ["bulk"]}, "action": "setStatus", "value": "outdated"}),
        )
        .await;
    assert_eq!(r["updated"], 2, "{r}");
    let (_, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"numbers": [1, 2], "action": "setStatus", "value": "outdated"}),
        )
        .await;
    assert_eq!(r["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(r["skipped"][0]["code"], "unchanged");
    let (_, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"numbers": [3], "action": "move", "value": sub}),
        )
        .await;
    assert_eq!(r["updated"], 1);
    let (_, r) = s.post(&bulk, ADMIN_TOKEN, json!({"numbers": [3, 4], "action": "setField", "value": {"key": "priority", "value": "critical"}})).await;
    assert_eq!(r["updated"], 2);
    let (st, r) = s.post(&bulk, ADMIN_TOKEN, json!({"numbers": [3], "action": "setField", "value": {"key": "priority", "value": "nope"}})).await;
    assert_eq!(
        (st, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_case"))
    );
    let (st, _) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"numbers": [3], "action": "move", "value": uuid::Uuid::now_v7()}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .post(&bulk, ADMIN_TOKEN, json!({"action": "archive"}))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"numbers": [6, 7], "action": "archive"}),
        )
        .await;
    assert_eq!(r["updated"], 2);
    let (_, l) = s.get(base, ADMIN_TOKEN).await;
    assert_eq!(numbers(&l), vec![1, 2, 3, 4, 5]);
    let (_, r) = s
        .post(
            &bulk,
            ADMIN_TOKEN,
            json!({"filter": {}, "action": "restore"}),
        )
        .await;
    assert_eq!(r["updated"], 2, "{r}");
    let (_, v) = s.get(&format!("{base}/3/versions"), ADMIN_TOKEN).await;
    assert_eq!(v["items"][0]["source"], "bulk");
    assert_eq!(v["items"][0]["changed"], json!(["fields"]));
    assert_eq!(v["items"][1]["changed"], json!(["folderId"]));
    let (_, c) = s.get(&format!("{base}/3"), ADMIN_TOKEN).await;
    assert_eq!(c["folder"]["path"][0]["name"], "Checkout");
    assert_eq!(c["folder"]["path"][1]["name"], "Cards");
    audit_has(&s, &["case.bulk"]).await;
}

#[tokio::test]
async fn case_limits_attachments_and_validation() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MAX_STEPS", "5"),
            ("FASTPROOF_TM_MAX_STEP_DEPTH", "2"),
            ("FASTPROOF_TM_MAX_MARKDOWN_BYTES", "100"),
            ("FASTPROOF_TM_MAX_TAGS", "3"),
            ("FASTPROOF_TM_MAX_ATTACHMENT_BYTES", "1000"),
            ("FASTPROOF_TM_MAX_CASE_ATTACHMENTS", "2"),
            ("FASTPROOF_TM_MAX_BULK", "3"),
            ("FASTPROOF_TM_MAX_CASES", "4"),
        ],
    )
    .await;
    s.project("lim").await;
    let base = "/api/v1/projects/lim/cases";
    let post = |b: Value| send(&s, Method::POST, base, ADMIN_TOKEN, None, Some(b));
    let r = post(json!({"name": "x", "body": {"steps": steps(6)}})).await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("limit_reached"))
    );
    assert_eq!(r.body["details"], json!({"limit": "maxSteps", "max": 5}));
    let r = post(json!({"name": "x", "body": {"steps": [{"action": "a", "steps": [{"action": "b", "steps": [{"action": "c"}]}]}]}})).await;
    assert_eq!(r.body["details"]["limit"], "maxStepDepth");
    let r = post(json!({"name": "x", "body": {"steps": [{"action": "a"}, {"action": "b", "expected": "e".repeat(101)}]}})).await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_markdown"))
    );
    assert_eq!(
        r.body["details"],
        json!({"path": "body.steps[1].expected", "max": 100})
    );
    let r = post(json!({"name": "x", "body": {}, "tags": ["a", "b", "c", "d"]})).await;
    assert_eq!(r.body["details"]["limit"], "maxTags");
    let r = post(json!({"name": "", "body": {}})).await;
    assert_eq!(
        (r.status, r.body["details"][0]["path"].as_str()),
        (StatusCode::BAD_REQUEST, Some("name"))
    );
    let r = post(json!({"name": "x", "body": {"links": [{"url": "javascript:alert(1)"}]}})).await;
    assert_eq!(r.body["details"][0]["path"], "body.links[0].url");
    let r = post(json!({"name": "x", "body": {"steps": [{"id": "s_same", "action": "a"}, {"id": "s_same", "action": "b"}]}})).await;
    assert_eq!(r.body["details"][0]["path"], "body.steps[1].id");
    let r = post(json!({"name": "x", "body": {"steps": [{"id": "bad id", "action": "a"}]}})).await;
    assert_eq!(r.body["details"][0]["path"], "body.steps[0].id");
    let r = post(json!({"name": "x", "body": {"steps": [{"action": "a", "attachments": [uuid::Uuid::now_v7()]}]}})).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let (_, f) = s
        .post(
            "/api/v1/projects/lim/case-fields",
            ADMIN_TOKEN,
            json!({"key": "team", "name": "Team", "type": "text", "required": true}),
        )
        .await;
    assert_eq!(f["required"], true);
    let r = post(json!({"name": "x", "body": {}})).await;
    assert_eq!(r.body["details"][0]["path"], "fields.team");
    let r = post(json!({"name": "draft", "status": "draft", "body": {}})).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "drafts skip required fields: {}",
        r.body
    );

    let c = create(
        &s,
        "lim",
        json!({"name": "with files", "body": {}, "fields": {"team": "qa"}}),
    )
    .await;
    let n = c.body["number"].as_i64().unwrap();
    let up = |bytes: Vec<u8>, name: &'static str| {
        s.req(
            Method::POST,
            &format!("{base}/{n}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "image/png")
        .header("x-fastproof-attachment-name", name)
        .body(bytes)
        .send()
    };
    let r = up(vec![1u8; 10], "a.png").await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let a1: Value = r.json().await.unwrap();
    assert_eq!(
        (
            a1["size"].as_i64(),
            a1["name"].as_str(),
            a1["contentType"].as_str()
        ),
        (Some(10), Some("a.png"), Some("image/png"))
    );
    let r = up(vec![2u8; 1001], "big.png").await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let r = up(vec![3u8; 10], "b.png").await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = up(vec![4u8; 10], "c.png").await.unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let r = s
        .req(
            Method::POST,
            &format!("{base}/999/attachments"),
            Some(ADMIN_TOKEN),
        )
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let aid = a1["id"].as_str().unwrap();
    let r = send(&s, Method::PUT, &format!("{base}/{n}"), ADMIN_TOKEN, Some("\"r1\""),
        Some(json!({"name": "with files", "body": {"description": format!("![shot](attachment:{aid})"),
            "steps": [{"action": "look", "attachments": [aid]}]}}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(
        r.body["html"]["description"]
            .as_str()
            .unwrap()
            .contains(&format!("/api/v1/projects/lim/attachments/{aid}/content"))
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/lim/attachments/{aid}/content"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let r = post(json!({"name": "third", "body": {}, "fields": {"team": "qa"}})).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = post(json!({"name": "fourth", "body": {}, "fields": {"team": "qa"}})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let r = post(json!({"name": "fifth", "body": {}, "fields": {"team": "qa"}})).await;
    assert_eq!(r.body["details"]["limit"], "maxCases", "{}", r.body);
    let (_, r) = s
        .post(
            &format!("{base}/bulk"),
            ADMIN_TOKEN,
            json!({"numbers": [1, 2, 3, 4], "action": "archive"}),
        )
        .await;
    assert_eq!(r["details"]["limit"], "maxBulk");
}

#[tokio::test]
async fn case_permission_matrix_and_idor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pa").await;
    s.project("pb").await;
    let (_, fa) = s
        .post(
            "/api/v1/projects/pa/case-folders",
            ADMIN_TOKEN,
            json!({"name": "A"}),
        )
        .await;
    let fa = fa["id"].as_str().unwrap().to_string();
    create(&s, "pa", json!({"name": "a1", "folderId": fa, "body": {}})).await;
    create(&s, "pb", json!({"name": "b1", "body": {}})).await;
    let r = s
        .req(
            Method::POST,
            "/api/v1/projects/pa/cases/1/attachments",
            Some(ADMIN_TOKEN),
        )
        .body("png")
        .send()
        .await
        .unwrap();
    let att: Value = r.json().await.unwrap();
    let att = att["id"].as_str().unwrap().to_string();
    let launch = s.launch("pa", None).await;
    let lid = launch["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/pa/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "t", "fullName": "pa.T#t", "status": "passed"}]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    let deadline = Instant::now() + Duration::from_secs(30);
    let ident = loop {
        let (_, l) = s
            .get(
                "/api/v1/projects/pa/test-identities?unlinked=true",
                ADMIN_TOKEN,
            )
            .await;
        if let Some(id) = l["items"][0]["testCaseId"].as_str() {
            break id.to_string();
        }
        assert!(Instant::now() < deadline, "identity not ingested: {l}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let sug = uuid::Uuid::now_v7();
    sqlx::query(
        "insert into case_suggestions (id, project_id, case_id, base_revision, proposal, agent) \
         select $1, c.project_id, c.id, 1, '{}', 'mcp' from cases c join projects p on p.id = c.project_id \
         where p.key = 'pa' and c.number = 1",
    )
    .bind(sug)
    .execute(&db.pool)
    .await
    .unwrap();

    let idor = [
        (
            Method::POST,
            "/api/v1/projects/pb/cases".to_string(),
            Some(json!({"name": "x", "folderId": fa, "body": {}})),
            None,
        ),
        (
            Method::POST,
            "/api/v1/projects/pb/cases".to_string(),
            Some(json!({"name": "x", "body": {"steps": [{"action": "a", "attachments": [att]}]}})),
            None,
        ),
        (
            Method::PATCH,
            "/api/v1/projects/pb/cases/1".to_string(),
            Some(json!({"folderId": fa})),
            Some("\"r1\""),
        ),
        (
            Method::GET,
            format!("/api/v1/projects/pb/cases?folder={fa}"),
            None,
            None,
        ),
        (
            Method::GET,
            format!("/api/v1/projects/pb/cases/tree?parent={fa}"),
            None,
            None,
        ),
        (
            Method::POST,
            "/api/v1/projects/pb/cases/bulk".to_string(),
            Some(json!({"numbers": [1], "action": "move", "value": fa})),
            None,
        ),
        (
            Method::POST,
            "/api/v1/projects/pb/cases/1/identities".to_string(),
            Some(json!({"testCaseId": ident})),
            None,
        ),
        (
            Method::DELETE,
            format!("/api/v1/projects/pb/cases/1/identities/{ident}"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/projects/pb/case-suggestions/{sug}/accept"),
            None,
            Some("\"r1\""),
        ),
        (
            Method::POST,
            format!("/api/v1/projects/pb/case-suggestions/{sug}/reject"),
            None,
            None,
        ),
        (
            Method::GET,
            "/api/v1/projects/pb/cases/2".to_string(),
            None,
            None,
        ),
        (
            Method::GET,
            "/api/v1/projects/pb/cases/2/versions".to_string(),
            None,
            None,
        ),
        (
            Method::GET,
            "/api/v1/projects/pb/cases/2/history".to_string(),
            None,
            None,
        ),
        (
            Method::GET,
            "/api/v1/projects/pb/cases/2/suggestions".to_string(),
            None,
            None,
        ),
        (
            Method::POST,
            "/api/v1/projects/pb/cases/2/attachments".to_string(),
            None,
            None,
        ),
        (
            Method::GET,
            format!("/api/v1/projects/pb/attachments/{att}/content"),
            None,
            None,
        ),
    ];
    for (m, path, body, im) in idor {
        let r = send(&s, m.clone(), &path, ADMIN_TOKEN, im, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {path}: {}", r.body);
    }
    let (_, l) = s
        .get("/api/v1/projects/pb/test-identities", ADMIN_TOKEN)
        .await;
    assert!(
        l["items"].as_array().unwrap().is_empty(),
        "identities of pa are not listed in pb"
    );

    let viewer = s.token("v", "viewer", &["pa"]).await;
    let tester = s.token("t", "tester", &["pa"]).await;
    let uploader = s.token("u", "uploader", &["pa"]).await;
    let editor = s.token("e", "editor", &["pa"]).await;
    let outsider = s.token("o", "editor", &["pb"]).await;
    let p = "/api/v1/projects/pa";
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Need {
        Read,
        Write,
        Settings,
    }
    let routes: Vec<(Method, String, Option<Value>, Need)> = vec![
        (Method::GET, format!("{p}/cases"), None, Need::Read),
        (Method::GET, format!("{p}/cases/tree"), None, Need::Read),
        (Method::GET, format!("{p}/cases/1"), None, Need::Read),
        (
            Method::GET,
            format!("{p}/cases/1/versions"),
            None,
            Need::Read,
        ),
        (
            Method::GET,
            format!("{p}/cases/1/versions/1"),
            None,
            Need::Read,
        ),
        (
            Method::GET,
            format!("{p}/cases/1/history"),
            None,
            Need::Read,
        ),
        (
            Method::GET,
            format!("{p}/cases/1/suggestions"),
            None,
            Need::Read,
        ),
        (
            Method::GET,
            format!("{p}/test-identities?unlinked=true"),
            None,
            Need::Read,
        ),
        (
            Method::POST,
            format!("{p}/markdown/preview"),
            Some(json!({"markdown": "x"})),
            Need::Read,
        ),
        (
            Method::POST,
            format!("{p}/cases"),
            Some(json!({"name": "m", "body": {}})),
            Need::Write,
        ),
        (
            Method::PUT,
            format!("{p}/cases/1"),
            Some(json!({"name": "a1", "body": {}})),
            Need::Write,
        ),
        (
            Method::PATCH,
            format!("{p}/cases/1"),
            Some(json!({})),
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/cases/bulk"),
            Some(json!({"numbers": [1], "action": "addTags", "value": ["x"]})),
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/cases/1/versions/1/restore"),
            None,
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/cases/1/identities"),
            Some(json!({"testCaseId": ident})),
            Need::Write,
        ),
        (
            Method::DELETE,
            format!("{p}/cases/1/identities/{ident}"),
            None,
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/cases/1/attachments"),
            None,
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/case-suggestions/{sug}/reject"),
            None,
            Need::Write,
        ),
        (
            Method::POST,
            format!("{p}/case-suggestions/{sug}/accept"),
            None,
            Need::Write,
        ),
        (Method::DELETE, format!("{p}/cases/1"), None, Need::Write),
        (
            Method::POST,
            format!("{p}/cases/1/restore"),
            None,
            Need::Write,
        ),
        (
            Method::DELETE,
            format!("{p}/cases/99?purge=true"),
            None,
            Need::Settings,
        ),
        (
            Method::POST,
            format!("{p}/cases/backfill"),
            None,
            Need::Settings,
        ),
    ];
    let tokens = [
        ("viewer", viewer.as_str()),
        ("tester", tester.as_str()),
        ("uploader", uploader.as_str()),
        ("editor", editor.as_str()),
        ("admin", ADMIN_TOKEN),
        ("outsider", outsider.as_str()),
    ];
    for (m, path, body, need) in &routes {
        for (role, t) in tokens {
            let allowed = match (role, need) {
                ("outsider", _) => false,
                ("admin", _) => true,
                ("editor", Need::Read | Need::Write) => true,
                (_, Need::Read) => true,
                _ => false,
            };
            let r = send(&s, m.clone(), path, t, Some("\"r999\""), body.clone()).await;
            if allowed {
                assert!(
                    r.status != StatusCode::FORBIDDEN && r.status != StatusCode::UNAUTHORIZED,
                    "{role} {m} {path}: {} {}",
                    r.status,
                    r.body
                );
            } else {
                assert_eq!(
                    r.status,
                    StatusCode::FORBIDDEN,
                    "{role} {m} {path}: {}",
                    r.body
                );
            }
        }
        let r = send(&s, m.clone(), path, "not-a-token", None, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {path}");
    }
}

fn result(id: &str, name: &str, full: &str, labels: Value) -> Value {
    json!({"id": id, "name": name, "fullName": full, "status": "passed", "start": 1795249075967i64,
        "stop": 1795249076967i64, "labels": labels, "description": format!("Code says: {name}"),
        "links": [{"name": "spec", "url": "https://example.com/spec", "type": "link"}]})
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

async fn sync(db: &TestDb) -> fastproof_store::tm::sync::SyncStats {
    Store::new(db.pool.clone())
        .run_case_sync("test", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap()
}

#[tokio::test]
async fn code_sync_trust_allure_id_and_rename() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("sy").await;
    let up = s.token("ci", "uploader", &["sy"]).await;
    let base = "/api/v1/projects/sy/cases";
    let manual = create(&s, "sy", json!({"name": "Removes an item (manual)", "body": {
        "preconditions": "Cart has items", "steps": [{"action": "Open cart"}, {"action": "Press remove", "expected": "Gone"}]},
        "fields": {"priority": "high"}})).await;
    assert_eq!(manual.body["number"], 1);
    let manual_steps = manual.body["body"]["steps"].clone();

    let main = json!({"name": "ci", "vcs": {"branch": "main"}});
    let l1 = run_launch(&s, "sy", &up, main.clone(), json!([
        result("r1", "adds an item", "shop.Cart#adds", json!([{"name": "suite", "value": "Cart"},
            {"name": "tag", "value": "smoke"}, {"name": "severity", "value": "critical"}, {"name": "epic", "value": "Shop"}])),
        result("r2", "removes an item", "shop.Cart#removes", json!([{"name": "ALLURE_ID", "value": "1"},
            {"name": "suite", "value": "Cart"}])),
    ])).await;
    let queued: i64 = sqlx::query_scalar("select count(*) from case_sync_queue")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 1, "finishing queues the launch");
    let st = sync(&db).await;
    assert_eq!((st.created, st.linked), (1, 1), "{st:?}");
    let queued: i64 = sqlx::query_scalar("select count(*) from case_sync_queue")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);

    let (_, c1) = s.get(&format!("{base}/1"), ADMIN_TOKEN).await;
    assert_eq!(c1["automated"], true);
    assert_eq!(
        c1["name"], "Removes an item (manual)",
        "the manual name wins when code automates the case"
    );
    assert_eq!(
        c1["body"]["description"], "Code says: removes an item",
        "an empty description is filled from code"
    );
    assert_eq!(c1["body"]["steps"], manual_steps, "manual steps stay");
    assert_eq!(c1["body"]["preconditions"], "Cart has items");
    assert_eq!(
        c1["fields"]["priority"], "high",
        "fields without a label stay"
    );
    assert_eq!(c1["origin"], "ui");
    assert!(c1["codeOwned"].as_array().unwrap().contains(&json!("name")));
    let kinds: Vec<&str> = c1["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["manual", "code"]);
    assert_eq!(c1["identities"][1]["branches"], json!(["main"]));
    let (_, v) = s.get(&format!("{base}/1/versions"), ADMIN_TOKEN).await;
    assert_eq!(v["items"][0]["source"], "code");
    assert_eq!(v["items"][0]["launchId"], l1.as_str());

    let (_, c2) = s.get(&format!("{base}/2"), ADMIN_TOKEN).await;
    assert_eq!(c2["origin"], "code");
    assert_eq!(c2["name"], "adds an item");
    assert_eq!(c2["body"]["description"], "Code says: adds an item");
    assert_eq!(c2["body"]["links"][0]["url"], "https://example.com/spec");
    assert_eq!(c2["tags"], json!(["smoke"]));
    assert_eq!(
        c2["fields"],
        json!({"severity": "critical", "epic": "Shop", "suite": "Cart"})
    );
    assert_eq!(
        c2["folder"]["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["Automated", "Cart"]
    );
    assert_eq!(c2["keys"], json!([{"kind": "allure_id", "value": "2"}]));
    let (_, h) = s.get(&format!("{base}/2/history"), ADMIN_TOKEN).await;
    assert_eq!(h["items"][0]["source"], "automated");
    assert_eq!(h["items"][0]["branch"], "main");
    let (_, l) = s
        .get(&format!("{base}?automation=automated"), ADMIN_TOKEN)
        .await;
    assert_eq!(l["items"].as_array().unwrap().len(), 2);
    assert_eq!(l["items"][0]["lastResult"]["status"], "passed");
    assert_eq!(l["items"][0]["lastResult"]["source"], "automated");

    let etag = format!("\"r{}\"", c2["revision"]);
    let mut w = json!({"name": "renamed by hand", "folderId": c2["folder"]["id"], "body": c2["body"], "fields": c2["fields"], "tags": c2["tags"]});
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/2"),
        ADMIN_TOKEN,
        Some(&etag),
        Some(w.clone()),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("field_from_code"))
    );
    assert_eq!(r.body["details"]["fields"], json!(["name"]));
    w["name"] = json!("adds an item");
    w["body"]["steps"] = json!([{"action": "Check the badge"}]);
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/2"),
        ADMIN_TOKEN,
        Some(&etag),
        Some(w),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "steps belong to the case: {}",
        r.body
    );
    let rev_before = r.body["revision"].as_i64().unwrap();

    run_launch(&s, "sy", &up, main.clone(), json!([
        result("r1", "adds an item", "shop.Cart#adds", json!([{"name": "suite", "value": "Cart"},
            {"name": "tag", "value": "smoke"}, {"name": "severity", "value": "critical"}, {"name": "epic", "value": "Shop"}])),
    ])).await;
    let st = sync(&db).await;
    assert_eq!((st.created, st.updated), (0, 0), "same code values: {st:?}");
    run_launch(
        &s,
        "sy",
        &up,
        main.clone(),
        json!([result(
            "r1",
            "adds an item",
            "shop.Cart#adds",
            json!([{"name": "suite", "value": "Cart"},
            {"name": "severity", "value": "normal"}, {"name": "epic", "value": "Shop"}])
        ),]),
    )
    .await;
    let st = sync(&db).await;
    assert_eq!(st.updated, 1, "{st:?}");
    let (_, c2) = s.get(&format!("{base}/2"), ADMIN_TOKEN).await;
    assert_eq!(c2["revision"].as_i64().unwrap(), rev_before + 1);
    assert_eq!(c2["fields"]["severity"], "normal");
    assert_eq!(
        c2["tags"],
        json!(["smoke"]),
        "no tag from code keeps the tags"
    );
    assert_eq!(c2["body"]["steps"][0]["action"], "Check the badge");

    run_launch(
        &s,
        "sy",
        &up,
        json!({"name": "mr", "vcs": {"branch": "feature/x"}}),
        json!([result(
            "r3",
            "new on a branch",
            "shop.Cart#branchOnly",
            json!([])
        )]),
    )
    .await;
    run_launch(
        &s,
        "sy",
        &up,
        json!({"name": "dbg", "debug": true, "vcs": {"branch": "main"}}),
        json!([result("r4", "debug only", "shop.Cart#debugOnly", json!([]))]),
    )
    .await;
    let (st, l) = s
        .post(
            "/api/v1/projects/sy/launches",
            &up,
            json!({"name": "nosync", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success());
    let nosync = l["id"].as_str().unwrap().to_string();
    sqlx::query("update launches set created_by_can_sync = false where id = $1::uuid")
        .bind(&nosync)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/sy/launches/{nosync}/results"),
            &up,
            json!([result("r5", "no permission", "shop.Cart#noSync", json!([]))]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    s.post(
        &format!("/api/v1/projects/sy/launches/{nosync}/finish"),
        &up,
        json!({}),
    )
    .await;
    run_launch(
        &s,
        "sy",
        &up,
        json!({"name": "trunk", "vcs": {"branch": "trunk", "defaultBranch": "trunk"}}),
        json!([result(
            "r6",
            "on the default branch",
            "shop.Cart#trunk",
            json!([])
        )]),
    )
    .await;
    let st = sync(&db).await;
    assert_eq!(
        st.created, 1,
        "only the CI default branch launch creates a case: {st:?}"
    );
    let (_, l) = s.get(&format!("{base}?q=default"), ADMIN_TOKEN).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    let (_, u) = s
        .get(
            "/api/v1/projects/sy/test-identities?unlinked=true",
            ADMIN_TOKEN,
        )
        .await;
    let names: Vec<&str> = u["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["fullName"].as_str().unwrap())
        .collect();
    for n in [
        "shop.Cart#branchOnly",
        "shop.Cart#debugOnly",
        "shop.Cart#noSync",
    ] {
        assert!(names.contains(&n), "{n} stays unlinked: {names:?}");
    }
    let (_, b) = s
        .get(
            "/api/v1/projects/sy/test-identities?unlinked=true&branch=feature/x",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    assert_eq!(b["items"][0]["branches"], json!(["feature/x"]));
    assert_eq!(b["items"][0]["caseNumber"], Value::Null);

    run_launch(
        &s,
        "sy",
        &up,
        main.clone(),
        json!([result(
            "r7",
            "adds an item",
            "shop.Cart#addsItem",
            json!([{"name": "suite", "value": "Cart"}])
        ),]),
    )
    .await;
    let st = sync(&db).await;
    assert_eq!(st.created, 1, "a renamed test is a new identity: {st:?}");
    let (_, l) = s
        .get(&format!("{base}?sort=-number&limit=1"), ADMIN_TOKEN)
        .await;
    let dup = l["items"][0]["number"].as_i64().unwrap();
    let (_, d) = s.get(&format!("{base}/{dup}"), ADMIN_TOKEN).await;
    let renamed = d["identities"][0]["testCaseId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/2/identities"),
        ADMIN_TOKEN,
        None,
        Some(json!({"testCaseId": renamed})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["identities"].as_array().unwrap().len(), 2);
    let (_, d) = s.get(&format!("{base}/{dup}"), ADMIN_TOKEN).await;
    assert!(d["archivedAt"].is_string(), "{d}");
    assert_eq!(d["automated"], false);
    assert_eq!(
        d["body"]["links"].as_array().unwrap().last().unwrap()["url"],
        "/p/sy/cases/2"
    );
    let r = send(
        &s,
        Method::DELETE,
        &format!("{base}/2/identities/{renamed}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["identities"].as_array().unwrap().len(), 1);
    assert_eq!(r.body["automated"], true);
    let manual_ident = c1["identities"][0]["testCaseId"].as_str().unwrap();
    let r = send(
        &s,
        Method::DELETE,
        &format!("{base}/1/identities/{manual_ident}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/2/identities"),
        ADMIN_TOKEN,
        None,
        Some(json!({"testCaseId": manual_ident})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    sqlx::query("update projects set settings = jsonb_set(settings, '{caseSync}', '{\"enabled\": false}') where key = 'sy'")
        .execute(&db.pool)
        .await
        .unwrap();
    run_launch(
        &s,
        "sy",
        &up,
        main,
        json!([result("r8", "off", "shop.Cart#off", json!([]))]),
    )
    .await;
    // Every finished launch is queued for the last result of cases; with sync off the job
    // leaves the cases' content alone.
    let queued: i64 = sqlx::query_scalar("select count(*) from case_sync_queue")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(queued, 1, "sync off: queued for the last result only");
    let st = sync(&db).await;
    assert_eq!(
        (st.created, st.linked, st.updated),
        (0, 0, 0),
        "sync off: {st:?}"
    );
    audit_has(&s, &["case.linkIdentity", "case.unlinkIdentity"]).await;
}

#[tokio::test]
async fn backfill_route_creates_cases_from_trusted_history() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("bf").await;
    let (st, l) = s
        .post(
            "/api/v1/projects/bf/launches",
            ADMIN_TOKEN,
            json!({"name": "old", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success());
    let lid = l["id"].as_str().unwrap().to_string();
    s.post(
        &format!("/api/v1/projects/bf/launches/{lid}/results"),
        ADMIN_TOKEN,
        json!([
            result(
                "a",
                "first",
                "bf.T#first",
                json!([{"name": "ALLURE_ID", "value": "500"}])
            ),
            result("b", "second", "bf.T#second", json!([])),
        ]),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let n: i64 = sqlx::query_scalar("select count(*) from test_cases where kind = 'code'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        if n == 2 {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let editor = s.token("e", "editor", &["bf"]).await;
    let (st, _) = s
        .post("/api/v1/projects/bf/cases/backfill", &editor, json!({}))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, job) = s
        .post("/api/v1/projects/bf/cases/backfill", ADMIN_TOKEN, json!({}))
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    assert_eq!(job["kind"], "backfill");
    assert_eq!(job["status"], "queued");
    let (_, again) = s
        .post("/api/v1/projects/bf/cases/backfill", ADMIN_TOKEN, json!({}))
        .await;
    assert_eq!(again["id"], job["id"], "one backfill at a time");
    let store = Store::new(db.pool.clone());
    let run = store
        .run_case_backfill("test", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(run.done);
    assert_eq!(
        run.stats.created, 2,
        "admin launches carry cases:sync: {run:?}"
    );
    let (_, c) = s.get("/api/v1/projects/bf/cases/1", ADMIN_TOKEN).await;
    assert_eq!(c["name"], "first");
    let keys: Vec<&str> = c["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["value"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["1", "500"]);
    let status: String = sqlx::query_scalar("select status from tm_jobs where id = $1::uuid")
        .bind(job["id"].as_str().unwrap())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(status, "done");
    audit_has(&s, &["case.backfill"]).await;
}

#[tokio::test]
async fn save_of_a_100_step_case_is_fast() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("perf").await;
    let base = "/api/v1/projects/perf/cases";
    let t0 = Instant::now();
    let c = create(
        &s,
        "perf",
        json!({"name": "Long case", "body": {"preconditions": "Logged in", "steps": steps(100)}}),
    )
    .await;
    let create_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut body = c.body["body"].clone();
    let mut rev = 1;
    let mut times = Vec::new();
    for i in 0..20 {
        body["steps"][i]["expected"] = json!(format!("changed {i}"));
        let t = Instant::now();
        let r = send(
            &s,
            Method::PUT,
            &format!("{base}/1"),
            ADMIN_TOKEN,
            Some(&format!("\"r{rev}\"")),
            Some(json!({"name": "Long case", "body": body})),
        )
        .await;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        rev = r.body["revision"].as_i64().unwrap();
        assert_eq!(r.body["body"]["steps"].as_array().unwrap().len(), 100);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = times[times.len() / 2];
    let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
    eprintln!(
        "100-step case: create {create_ms:.1} ms, save p50 {p50:.1} ms, p95 {p95:.1} ms (20 saves)"
    );
    assert_eq!(rev, 21);
    assert!(p50 < 2000.0, "save p50 {p50} ms");
}

#[tokio::test]
async fn review_clears_the_ai_draft_mark_with_if_match() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rv").await;
    s.project("other").await;
    let c = create(&s, "rv", json!({"name": "Drafted by an agent", "body": {}})).await;
    sqlx::query("update cases set ai_unreviewed = true where id = $1::uuid")
        .bind(c.body["id"].as_str().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let path = "/api/v1/projects/rv/cases/1/review";
    let r = send(
        &s,
        Method::GET,
        "/api/v1/projects/rv/cases/1",
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.body["aiUnreviewed"], true);
    let etag = r.etag.unwrap();
    let r = send(&s, Method::POST, path, ADMIN_TOKEN, None, None).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.body);
    let r = send(&s, Method::POST, path, ADMIN_TOKEN, Some("\"r0\""), None).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.body);
    assert_eq!(r.body["details"]["currentRevision"], 1);
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/other/cases/1/review",
        ADMIN_TOKEN,
        Some(&etag),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    let r = send(&s, Method::POST, path, ADMIN_TOKEN, Some(&etag), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["aiUnreviewed"], false);
    assert_eq!(r.body["revision"], 2);
    assert_eq!(r.body["version"], 1, "no new version");
    assert_eq!(r.etag.as_deref(), Some("\"r2\""));
    let again = send(&s, Method::POST, path, ADMIN_TOKEN, Some("\"r2\""), None).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["revision"], 2, "already reviewed: unchanged");
    audit_has(&s, &["case.review"]).await;
    let anon = s
        .req(Method::POST, path, None)
        .header("if-match", "\"r2\"")
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
}
