//! Managed plans and manual runs over HTTP: plan CRUD with ETags, items, filter sync and the
//! live `testplan.json`; launches started from a plan; run items, step writes, the projection
//! into results (counters, labels, steps, history, blocked → broken, back to untested, rerun as
//! a new attempt), flaky exclusion, close and reopen with the gate, the partition drop keeping
//! a closed manual launch, the permission matrix and IDOR checks, parallel step writers, the
//! manual report, new-issue links, the `source` filter and the `fastproof testplan` CLI.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::tm::{CaseBody, CaseSnapshot, CaseStatus, CaseStep};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

fn step(action: &str, expected: &str, children: Vec<CaseStep>) -> CaseStep {
    CaseStep {
        id: format!("s_{}", Uuid::now_v7().simple()),
        action: action.into(),
        expected: expected.into(),
        data: String::new(),
        attachments: Vec::new(),
        steps: children,
        shared: None,
    }
}

async fn pid(db: &TestDb, key: &str) -> Uuid {
    sqlx::query_scalar("select id from projects where key = $1")
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// A case with steps; returns its number.
async fn case(db: &TestDb, project: Uuid, name: &str, steps: Vec<CaseStep>) -> i64 {
    let mut conn = db.pool.acquire().await.unwrap();
    let body = CaseBody {
        description: format!("About {name}"),
        preconditions: "Signed in as **admin**".into(),
        steps,
        links: Vec::new(),
        parameters: None,
    };
    fastproof_store::tm::testutil::insert_case(&mut conn, project, name, &body, None)
        .await
        .unwrap()
        .number
}

/// Links a code identity (with an optional ALLURE_ID) to a case and marks it automated;
/// returns the identity's series.
async fn automate(
    db: &TestDb,
    project: Uuid,
    number: i64,
    full_name: &str,
    allure_id: Option<&str>,
) -> Uuid {
    let tc = Uuid::now_v7();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, explicit_id, \
           explicit_id_source, kind, case_id, first_seen_at, last_seen_at) \
         select $1, $2, md5($3), $3, $3, $4, case when $4 is null then null else 'allure_id' end, 'code', c.id, now(), now() \
         from cases c where c.project_id = $2 and c.number = $5",
    )
    .bind(tc)
    .bind(project)
    .bind(full_name)
    .bind(allure_id)
    .bind(number)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update cases set automated = true where project_id = $1 and number = $2")
        .bind(project)
        .bind(number)
        .execute(&db.pool)
        .await
        .unwrap();
    let series = Uuid::now_v7();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash, parameters, \
           first_seen_at, last_seen_at) values ($1, $2, $3, md5($4), md5(''), '[]', now(), now())",
    )
    .bind(series)
    .bind(project)
    .bind(tc)
    .bind(full_name)
    .execute(&db.pool)
    .await
    .unwrap();
    series
}

async fn send(
    s: &Server,
    m: Method,
    path: &str,
    token: Option<&str>,
    if_match: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut rb = s.req(m, path, token);
    if let Some(e) = if_match {
        rb = rb.header("if-match", e);
    }
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.unwrap();
    let st = r.status();
    let etag = r
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = r.text().await.unwrap();
    (
        st,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
        etag,
    )
}

async fn me(s: &Server, token: &str) -> String {
    let (st, b) = s.get("/api/v1/me", token).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["name"].as_str().unwrap().to_string()
}

async fn managed(s: &Server, key: &str, token: &str, body: Value) -> Value {
    let mut body = body;
    body["kind"] = json!("managed");
    let (st, b) = s
        .post(&format!("/api/v1/projects/{key}/test-plans"), token, body)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b
}

async fn start(s: &Server, key: &str, token: &str, plan: &str, body: Value) -> Value {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/test-plans/{plan}/launches"),
            token,
            body,
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b
}

async fn items(s: &Server, key: &str, launch: &str) -> Vec<Value> {
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/{key}/launches/{launch}/run-items?limit=500"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["items"].as_array().unwrap().clone()
}

fn item_of(items: &[Value], number: i64) -> String {
    items
        .iter()
        .find(|i| i["caseNumber"] == number)
        .unwrap_or_else(|| panic!("no item for TC-{number}: {items:?}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn item(s: &Server, key: &str, id: &str) -> (Value, String) {
    let (st, b, etag) = send(
        s,
        Method::GET,
        &format!("/api/v1/projects/{key}/run-items/{id}"),
        Some(ADMIN_TOKEN),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    (b, etag.unwrap())
}

/// Sets the status of an item (reading its ETag first).
async fn set_status(s: &Server, key: &str, token: &str, id: &str, status: &str) -> Value {
    let (_, etag) = item(s, key, id).await;
    let (st, b, _) = send(
        s,
        Method::PATCH,
        &format!("/api/v1/projects/{key}/run-items/{id}"),
        Some(token),
        Some(&etag),
        Some(json!({"status": status})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

async fn put_step(
    s: &Server,
    key: &str,
    token: &str,
    id: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        &format!("/api/v1/projects/{key}/run-items/{id}/steps/{path}"),
        Some(token),
        Some(body),
    )
    .await
}

async fn wait_for<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f().await {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn audit(s: &Server, want: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (st, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{a}");
        let items = a["items"].as_array().cloned().unwrap_or_default();
        if want.iter().all(|w| items.iter().any(|i| i["action"] == *w)) {
            return;
        }
        assert!(Instant::now() < deadline, "audit lacks {want:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn label<'a>(r: &'a Value, name: &str) -> Option<&'a str> {
    r["labels"]
        .as_array()?
        .iter()
        .find(|l| l["name"] == name)
        .and_then(|l| l["value"].as_str())
}

#[tokio::test]
async fn managed_plans_items_sync_and_testplan() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pl").await;
    s.project("other").await;
    let p = pid(&db, "pl").await;
    let login = case(
        &db,
        p,
        "Login",
        vec![step("Open", "", vec![]), step("Submit", "", vec![])],
    )
    .await;
    let logout = case(&db, p, "Logout", vec![step("Click", "", vec![])]).await;
    let search = case(&db, p, "Search", vec![]).await;
    let api = case(&db, p, "API smoke", vec![]).await;
    let api_series = automate(&db, p, api, "com.example.ApiTest.smoke", Some("4001")).await;
    sqlx::query("update cases set tags = '{smoke}' where project_id = $1 and number = $2")
        .bind(p)
        .bind(search)
        .execute(&db.pool)
        .await
        .unwrap();
    let base = "/api/v1/projects/pl/test-plans";

    let (st, b) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "x", "caseNumbers": [login, 999]}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["details"]["unknownCases"], json!([999]));
    let (st, b) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "x", "testCaseIds": []}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = s
        .post(base, ADMIN_TOKEN, json!({"name": "x", "caseNumbers": [1]}))
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");

    let plan = managed(
        &s,
        "pl",
        ADMIN_TOKEN,
        json!({"name": "Release", "description": "Before **every** release", "caseNumbers": [login, logout],
            "filter": {"tags": ["smoke"]}}),
    )
    .await;
    assert_eq!(plan["kind"], "managed");
    assert_eq!(
        plan["counts"],
        json!({"total": 3, "manual": 3, "automated": 0})
    );
    let id = plan["id"].as_str().unwrap().to_string();
    let url = format!("{base}/{id}");

    let (st, got, etag) = send(&s, Method::GET, &url, Some(ADMIN_TOKEN), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(etag.as_deref(), Some("\"r1\""));
    assert_eq!(got["filter"], json!({"tags": ["smoke"]}));
    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &url,
        Some(ADMIN_TOKEN),
        None,
        Some(json!({"name": "R2"})),
    )
    .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            Some("precondition_required")
        )
    );
    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &url,
        Some(ADMIN_TOKEN),
        Some("\"r0\""),
        Some(json!({"name": "R2"})),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{b}");
    assert_eq!(b["details"]["currentRevision"], 1);
    let (st, b, etag) = send(
        &s,
        Method::PATCH,
        &url,
        Some(ADMIN_TOKEN),
        Some("\"r1\""),
        Some(json!({"name": "Release 2"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (b["name"].as_str(), etag.as_deref()),
        (Some("Release 2"), Some("\"r2\""))
    );

    let (st, l) = s.get(&format!("{url}/items"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let list = l["items"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    let via: Vec<(i64, bool)> = list
        .iter()
        .map(|i| {
            (
                i["caseNumber"].as_i64().unwrap(),
                i["viaFilter"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(
        via.contains(&(search, true)) && via.contains(&(login, false)),
        "{via:?}"
    );
    let (st, page) = s.get(&format!("{url}/items?limit=2"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let next = page["next"].as_str().unwrap();
    let (_, rest) = s
        .get(&format!("{url}/items?limit=2&before={next}"), ADMIN_TOKEN)
        .await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);

    let (st, b) = s
        .post(
            &format!("{url}/items"),
            ADMIN_TOKEN,
            json!({"add": [api], "remove": [logout], "assign": [{"number": login, "assignee": "ann"}]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["counts"],
        json!({"total": 3, "manual": 2, "automated": 1})
    );
    let (_, ann) = s
        .get(&format!("{url}/items?assignee=ann"), ADMIN_TOKEN)
        .await;
    assert_eq!(ann["items"][0]["caseNumber"], login);

    sqlx::query("update cases set tags = case when number = $2 then '{}'::text[] else '{smoke}'::text[] end \
                 where project_id = $1 and number in ($2, $3)")
        .bind(p)
        .bind(search)
        .bind(logout)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, diff) = s
        .post(&format!("{url}/sync?dryRun=true"), ADMIN_TOKEN, json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{diff}");
    assert_eq!(diff, json!({"added": [logout], "removed": [search]}));
    let (_, same) = s.get(&url, ADMIN_TOKEN).await;
    assert_eq!(same["counts"]["total"], 3);
    let (st, diff) = s.post(&format!("{url}/sync"), ADMIN_TOKEN, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(diff, json!({"added": [logout], "removed": [search]}));
    let (_, l) = s.get(&format!("{url}/items"), ADMIN_TOKEN).await;
    let numbers: Vec<i64> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["caseNumber"].as_i64().unwrap())
        .collect();
    assert!(
        numbers.contains(&logout) && !numbers.contains(&search),
        "{numbers:?}"
    );

    let (st, tp) = s.get(&format!("{url}/testplan.json"), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{tp}");
    assert_eq!(
        tp,
        json!({"version": "1.0", "tests": [{"id": "4001", "selector": "com.example.ApiTest.smoke"}]})
    );
    sqlx::query(
        "insert into series_mutes (series_id, project_id, until, reason, created_by) values ($1, $2, null, 'x', 't')",
    )
    .bind(api_series)
    .bind(p)
    .execute(&db.pool)
    .await
    .unwrap();
    let (_, tp) = s.get(&format!("{url}/testplan.json"), ADMIN_TOKEN).await;
    assert_eq!(tp["mutedTests"], tp["tests"]);

    let manual_only = managed(
        &s,
        "pl",
        ADMIN_TOKEN,
        json!({"name": "Manual", "caseNumbers": [login]}),
    )
    .await;
    let (st, b) = s
        .get(
            &format!(
                "{base}/{}/testplan.json",
                manual_only["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("plan_has_no_automated_cases"))
    );

    let (st, l) = s
        .get(&format!("{base}?kind=managed&q=rel"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    let (_, l) = s.get(&format!("{base}?limit=1"), ADMIN_TOKEN).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    let (_, l2) = s
        .get(
            &format!("{base}?limit=1&before={}", l["next"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_ne!(l["items"][0]["id"], l2["items"][0]["id"]);

    let (st, _) = s
        .get(
            &format!("/api/v1/projects/other/test-plans/{id}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _, _) = send(&s, Method::DELETE, &url, Some(ADMIN_TOKEN), None, None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &url,
        Some(ADMIN_TOKEN),
        Some("\"r5\""),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("plan_archived"))
    );
    let (_, l) = s.get(&format!("{base}?archived=true"), ADMIN_TOKEN).await;
    assert_eq!(l["items"][0]["id"].as_str(), Some(id.as_str()));

    let (st, adhoc) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"name": "adhoc", "testCaseIds": []}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{adhoc}");
    assert!(adhoc["testCount"].is_number());
    sqlx::query(
        "update test_plans set created_at = now() - interval '100 days' where project_id = $1",
    )
    .bind(p)
    .execute(&db.pool)
    .await
    .unwrap();
    let store = fastproof_store::Store::new(db.pool.clone());
    assert_eq!(store.delete_old_test_plans(100).await.unwrap(), 1);
    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from test_plans where project_id = $1")
            .bind(p)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert!(
        kinds.iter().all(|k| k == "managed") && kinds.len() == 2,
        "{kinds:?}"
    );
}

#[tokio::test]
async fn plan_launch_run_items_and_projection() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("run").await;
    let p = pid(&db, "run").await;
    let tester = s.token("tess", "tester", &["run"]).await;
    let tess = me(&s, &tester).await;
    let m1 = case(
        &db,
        p,
        "Checkout",
        vec![
            step("Open the cart", "The cart shows 2 items", vec![]),
            step(
                "Pay",
                "",
                vec![
                    step("Enter the card", "", vec![]),
                    step("Confirm", "Paid", vec![]),
                ],
            ),
        ],
    )
    .await;
    let m2 = case(&db, p, "Refund", vec![step("Ask", "", vec![])]).await;
    let m3 = case(&db, p, "Receipt", vec![]).await;
    let a1 = case(&db, p, "Checkout API", vec![]).await;
    automate(&db, p, a1, "shop.CheckoutApiTest.pay", None).await;
    let plan = managed(
        &s,
        "run",
        ADMIN_TOKEN,
        json!({"name": "Release", "caseNumbers": [m1, m2, m3, a1]}),
    )
    .await;
    let plan_id = plan["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/run/test-plans/{plan_id}/items"),
            ADMIN_TOKEN,
            json!({"assign": [{"number": m1, "assignee": tess}, {"number": m3, "assignee": tess}]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let started = start(
        &s,
        "run",
        &tester,
        &plan_id,
        json!({"name": "Release 1.4", "assign": "plan", "includeAutomated": true,
            "attributes": [{"key": "build", "value": "77"}], "environment": {"browser": "firefox"}}),
    )
    .await;
    let launch = &started["launch"];
    let lid = launch["id"].as_str().unwrap().to_string();
    let ext = launch["externalId"].as_str().unwrap().to_string();
    assert!(ext.starts_with("plan-"), "{ext}");
    assert_eq!(launch["testPlanId"].as_str(), Some(plan_id.as_str()));
    assert_eq!(launch["manual"]["state"], "open");
    assert_eq!(launch["manual"]["total"], 3);
    assert_eq!(launch["manual"]["untested"], 3);
    assert!(
        started["testplanUrl"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/launches/{lid}/testplan.json"))
    );
    let hint = started["ciHint"].as_str().unwrap();
    assert!(
        hint.contains(&format!("fastproof testplan --launch {ext}")),
        "{hint}"
    );
    let (st, again) = s
        .post(
            &format!("/api/v1/projects/run/test-plans/{plan_id}/launches"),
            &tester,
            json!({"name": "again", "externalId": ext, "assign": "none", "includeAutomated": true}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["launch"]["id"].as_str(), Some(lid.as_str()));
    let other_launch = s.launch("run", Some("ci-123")).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/run/test-plans/{plan_id}/launches"),
            &tester,
            json!({"name": "x", "externalId": "ci-123", "assign": "none", "includeAutomated": true}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_exists"))
    );
    let _ = other_launch;

    let (st, tp) = s
        .get(
            &format!("/api/v1/projects/run/launches/{lid}/testplan.json"),
            &tester,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{tp}");
    assert_eq!(
        tp["tests"],
        json!([{"selector": "shop.CheckoutApiTest.pay"}])
    );

    let list = items(&s, "run", &lid).await;
    assert_eq!(list.len(), 3, "automated cases get no run items: {list:?}");
    let i1 = item_of(&list, m1);
    let i2 = item_of(&list, m2);
    let i3 = item_of(&list, m3);
    assert_eq!(
        list.iter().find(|i| i["id"] == i1).unwrap()["assignee"].as_str(),
        Some(tess.as_str())
    );
    let (st, mine) = s.get("/api/v1/projects/run/run-items", &tester).await;
    assert_eq!(st, StatusCode::OK, "{mine}");
    let mine: Vec<&str> = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(mine.len(), 2);
    assert!(mine.contains(&i1.as_str()) && mine.contains(&i3.as_str()));

    let (got, etag) = item(&s, "run", &i1).await;
    assert_eq!(etag, "\"r1\"");
    assert_eq!(got["caseVersion"], 1);
    assert!(
        got["preconditionsHtml"]
            .as_str()
            .unwrap()
            .contains("<strong>admin</strong>"),
        "{got}"
    );
    assert_eq!(got["steps"][1]["steps"][1]["action"], "Confirm");
    assert!(
        got["stepsHtml"][1]["steps"][1]["action"]
            .as_str()
            .unwrap()
            .contains("Confirm")
    );

    let (st, ack) = put_step(&s, "run", &tester, &i1, "1", json!({"status": "passed"})).await;
    assert_eq!(st, StatusCode::OK, "{ack}");
    assert_eq!(
        ack,
        json!({"itemStatus": "in_progress", "suggestedStatus": "in_progress", "revision": 2})
    );
    for bad in ["9", "1.3", "0", "x", "2.2.1"] {
        let (st, b) = put_step(&s, "run", &tester, &i1, bad, json!({"status": "passed"})).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{bad}: {b}");
    }
    let (st, ack) = put_step(
        &s,
        "run",
        &tester,
        &i1,
        "2.2",
        json!({"status": "failed", "actual": "HTTP 500", "comment": "flaky gateway"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ack["suggestedStatus"], "failed");
    let (st, again) = put_step(&s, "run", &tester, &i1, "2.2", json!({"status": "failed"})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(again["revision"], 4);
    let (got, _) = item(&s, "run", &i1).await;
    let steps = got["stepResults"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[1]["path"], "2.2");
    assert_eq!(
        steps[1]["actual"], "HTTP 500",
        "an absent actual keeps the value"
    );
    assert_eq!(got["status"], "in_progress");
    let l = s.launch_get("run", &lid).await;
    assert_eq!(
        (
            l["manual"]["inProgress"].as_i64(),
            l["manual"]["untested"].as_i64()
        ),
        (Some(1), Some(2))
    );
    assert_eq!(l["counts"]["total"], 0);

    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/run/run-items/{i1}"),
        Some(&tester),
        None,
        Some(json!({"status": "failed"})),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "{b}");
    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/run/run-items/{i1}"),
        Some(&tester),
        Some("\"r1\""),
        Some(json!({"status": "failed"})),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{b}");
    assert_eq!(b["details"]["currentRevision"], 4);
    let done = set_status(&s, "run", &tester, &i1, "failed").await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["executedBy"].as_str(), Some(tess.as_str()));
    let rid = done["resultId"].as_str().unwrap().to_string();

    let manual = s.results("run", &lid, "&source=manual").await;
    assert_eq!(manual.len(), 1);
    let r = &manual[0];
    assert_eq!(r["id"].as_str(), Some(rid.as_str()));
    assert_eq!(r["status"], "failed");
    assert_eq!(r["source"], "manual");
    assert_eq!(r["caseNumber"], m1);
    assert_eq!(r["runItemId"].as_str(), Some(i1.as_str()));
    assert_eq!(r["fullName"], format!("TC-{m1}"));
    assert_eq!(label(r, "manual"), Some("true"));
    assert_eq!(label(r, "case"), Some(format!("TC-{m1}").as_str()));
    assert_eq!(label(r, "executedBy"), Some(tess.as_str()));
    assert_eq!(label(r, "manualStatus"), Some("failed"));
    assert_eq!(r["message"], "Step 2.2: HTTP 500");
    assert!(s.results("run", &lid, "&source=automated").await.is_empty());
    let l = s.launch_get("run", &lid).await;
    assert_eq!(
        (
            l["counts"]["total"].as_i64(),
            l["counts"]["failed"].as_i64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(l["manual"]["failed"], 1);

    let (st, d) = s
        .get(&format!("/api/v1/projects/run/results/{rid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    let at = &d["attempts"][0];
    assert_eq!(at["status"], "failed");
    let tree = at["steps"].as_array().unwrap();
    assert_eq!(tree.len(), 2);
    assert_eq!(
        (tree[0]["name"].as_str(), tree[0]["status"].as_str()),
        (Some("Open the cart"), Some("passed"))
    );
    assert_eq!(
        tree[1]["status"], "failed",
        "a parent without its own status takes the worst child's"
    );
    let confirm = &tree[1]["steps"][1];
    assert_eq!(
        (confirm["name"].as_str(), confirm["status"].as_str()),
        (Some("Confirm"), Some("failed"))
    );
    assert_eq!(confirm["error"]["actual"], "HTTP 500");
    assert_eq!(confirm["error"]["expected"], "Paid");
    assert_eq!(confirm["error"]["message"], "flaky gateway");
    assert_eq!(d["descriptionText"], "About Checkout");

    let b2 = set_status(&s, "run", &tester, &i2, "blocked").await;
    let r2 = b2["resultId"].as_str().unwrap().to_string();
    let (_, d2) = s
        .get(&format!("/api/v1/projects/run/results/{r2}"), ADMIN_TOKEN)
        .await;
    assert_eq!(d2["status"], "broken");
    assert_eq!(label(&d2, "manualStatus"), Some("blocked"));
    let tc: Uuid = d2["testCaseId"].as_str().unwrap().parse().unwrap();
    let (kind, full): (String, String) =
        sqlx::query_as("select kind, full_name from test_cases where id = $1")
            .bind(tc)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((kind.as_str(), full), ("manual", format!("TC-{m2}")));
    let (st, h) = s
        .get(
            &format!("/api/v1/projects/run/test-cases/{tc}/history"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(h["items"][0]["status"], "broken");
    assert_eq!(h["items"][0]["resultId"].as_str(), Some(r2.as_str()));

    let back = set_status(&s, "run", &tester, &i2, "untested").await;
    assert!(back["resultId"].is_null());
    let gone: i64 = sqlx::query_scalar("select count(*) from results where id = $1::uuid")
        .bind(&r2)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(gone, 0, "back to untested removes the projected result");
    let l = s.launch_get("run", &lid).await;
    assert_eq!(l["counts"]["total"], 1);
    assert_eq!(l["manual"]["untested"], 2);

    let (_, i1_etag) = item(&s, "run", &i1).await;
    let (st, rerun, _) = send(
        &s,
        Method::POST,
        &format!("/api/v1/projects/run/run-items/{i1}/rerun"),
        Some(&tester),
        Some(&i1_etag),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rerun}");
    assert_eq!(
        (rerun["executionNo"].as_i64(), rerun["status"].as_str()),
        (Some(2), Some("untested"))
    );
    assert!(rerun["stepResults"].as_array().unwrap().is_empty());
    assert_eq!(
        rerun["resultId"].as_str(),
        Some(rid.as_str()),
        "the previous attempt keeps the result"
    );
    let passed = set_status(&s, "run", &tester, &i1, "passed").await;
    assert_eq!(passed["resultId"].as_str(), Some(rid.as_str()));
    let (_, d) = s
        .get(&format!("/api/v1/projects/run/results/{rid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(d["status"], "passed");
    assert_eq!(d["attemptCount"], 2);
    let statuses: Vec<&str> = d["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses.len(), 2);
    assert!(
        statuses.contains(&"failed") && statuses.contains(&"passed"),
        "{statuses:?}"
    );
    let keys: Vec<String> = sqlx::query_scalar(
        "select client_key from attempts where result_id = $1::uuid order by seq",
    )
    .bind(&rid)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        keys,
        vec![format!("manual:{i1}:1"), format!("manual:{i1}:2")]
    );

    let (st, mine) = s.get("/api/v1/projects/run/run-items", &tester).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine["items"][0]["launchId"].as_str(), Some(lid.as_str()));

    audit(
        &s,
        &[
            "testPlan.create",
            "testPlan.items",
            "run.start",
            "runItem.update",
        ],
    )
    .await;
    let events: Vec<String> = sqlx::query_scalar(
        "select distinct type from events where project = 'run' and type in ('runItem.updated', 'manualRun.updated')",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
}

#[tokio::test]
async fn manual_series_are_never_flaky() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fl").await;
    let p = pid(&db, "fl").await;
    let n = case(&db, p, "Flip", vec![step("Do", "", vec![])]).await;
    for (k, status) in ["passed", "failed", "passed", "failed", "passed", "failed"]
        .iter()
        .enumerate()
    {
        let l = s.launch("fl", Some(&format!("m{k}"))).await;
        let lid = l["id"].as_str().unwrap();
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/fl/launches/{lid}/run-items"),
                ADMIN_TOKEN,
                json!({"caseNumbers": [n]}),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        assert_eq!(b, json!({"added": 1, "skipped": 0, "skips": []}));
        let list = items(&s, "fl", lid).await;
        set_status(&s, "fl", ADMIN_TOKEN, &item_of(&list, n), status).await;
    }
    let (flaky_results, transitions): (bool, i64) = sqlx::query_as(
        "select bool_or(r.flaky or r.flaky_in_run), count(r.transition) \
         from results r where r.project_id = $1 and r.run_item_id is not null",
    )
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let stats_flaky: Option<bool> = sqlx::query_scalar(
        "select bool_or(st.flaky) from series_stats st join series s on s.id = st.series_id \
         join test_cases t on t.id = s.test_case_id where t.project_id = $1 and t.kind = 'manual'",
    )
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!flaky_results);
    assert_eq!(
        stats_flaky,
        Some(false),
        "series statistics are kept, never flaky"
    );
    assert!(
        transitions > 0,
        "the analysis hook ran on the manual results"
    );
    let (st, f) = s.get("/api/v1/projects/fl/flaky", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert!(f["items"].as_array().unwrap().is_empty(), "{f}");
}

#[tokio::test]
async fn close_reopen_gate_and_finish() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(&db, &[], |c| {
        c.integrations.poll = Duration::from_millis(100);
        c.integrations.settle = Duration::ZERO;
    })
    .await;
    s.project("cl").await;
    let p = pid(&db, "cl").await;
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(
            p,
            json!({"gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}]}),
        )
        .await
        .unwrap();
    let tester = s.token("t", "tester", &["cl"]).await;
    let editor = s.token("e", "editor", &["cl"]).await;
    let a = case(&db, p, "A", vec![step("x", "", vec![])]).await;
    let b = case(&db, p, "B", vec![]).await;
    let gate_state = |launch: String| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_as::<_, (String, Option<String>)>(
                "select state, status from launch_gates where launch_id = $1::uuid",
            )
            .bind(launch)
            .fetch_optional(&pool)
            .await
            .unwrap()
        }
    };

    let l = s.launch("cl", None).await;
    let lid = l["id"].as_str().unwrap().to_string();
    let base = format!("/api/v1/projects/cl/launches/{lid}");
    let (st, _) = s
        .post(
            &format!("{base}/manual/close"),
            &tester,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "no manual run yet");
    let (st, added) = s
        .post(
            &format!("{base}/run-items"),
            &tester,
            json!({"caseNumbers": [a, b]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{added}");
    let list = items(&s, "cl", &lid).await;
    let (ia, ib) = (item_of(&list, a), item_of(&list, b));
    set_status(&s, "cl", &tester, &ia, "failed").await;
    let (st, closed) = s
        .post(
            &format!("{base}/manual/close"),
            &tester,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{closed}");
    assert_eq!(
        closed["status"], "finished",
        "a purely manual launch finishes on close"
    );
    assert_eq!(closed["manual"]["state"], "closed");
    assert_eq!(closed["manual"]["skipped"], 1);
    assert_eq!(
        (
            closed["counts"]["total"].as_i64(),
            closed["counts"]["skipped"].as_i64()
        ),
        (Some(2), Some(1))
    );
    let lid2 = lid.clone();
    wait_for("gate of the manual launch", || {
        let f = gate_state(lid2.clone());
        async move { f.await.is_some_and(|g| g.0 == "done") }
    })
    .await;
    assert_eq!(
        gate_state(lid.clone()).await.unwrap().1.as_deref(),
        Some("failed")
    );

    let (st, e) = put_step(&s, "cl", &tester, &ib, "1", json!({"status": "passed"})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("manual_run_closed"))
    );
    let (_, etag) = item(&s, "cl", &ia).await;
    let (st, e, _) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/cl/run-items/{ia}"),
        Some(&tester),
        Some(&etag),
        Some(json!({"status": "passed"})),
    )
    .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("manual_run_closed"))
    );
    for (path, body) in [
        (
            format!("/api/v1/projects/cl/run-items/{ia}/rerun"),
            json!({}),
        ),
        (format!("{base}/run-items"), json!({"caseNumbers": [a]})),
        (
            format!("{base}/manual/close"),
            json!({"untestedTo": "leave"}),
        ),
    ] {
        let (st, e, _) = send(
            &s,
            Method::POST,
            &path,
            Some(&tester),
            Some("\"r1\""),
            Some(body),
        )
        .await;
        assert_eq!(
            (st, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("manual_run_closed")),
            "{path}"
        );
    }
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/cl/run-items/{ia}/attachments"),
            Some(&tester),
        )
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);

    let (st, _) = s
        .post(&format!("{base}/manual/reopen"), &tester, json!({}))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "reopen needs plans:write");
    let (st, reopened) = s
        .post(&format!("{base}/manual/reopen"), &editor, json!({}))
        .await;
    assert_eq!(st, StatusCode::OK, "{reopened}");
    assert_eq!(reopened["manual"]["state"], "open");
    let (st, _) = s
        .post(&format!("{base}/manual/reopen"), &editor, json!({}))
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    set_status(&s, "cl", &tester, &ia, "passed").await;

    let l = s.launch("cl", Some("ci-1")).await;
    let cid = l["id"].as_str().unwrap().to_string();
    let (st, b2) = s
        .post(
            &format!("/api/v1/projects/cl/launches/{cid}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "api", "fullName": "suite.api", "status": "passed",
                "start": 1_795_249_079_000i64, "stop": 1_795_249_080_000i64}]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b2}");
    let sr = &s;
    wait_for("automated result", || {
        let c = cid.clone();
        async move { !sr.results("cl", &c, "&source=automated").await.is_empty() }
    })
    .await;
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/cl/launches/{cid}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let cid2 = cid.clone();
    wait_for("gate of the combined launch", || {
        let f = gate_state(cid2.clone());
        async move { f.await.is_some_and(|g| g.0 == "done") }
    })
    .await;
    assert_eq!(
        gate_state(cid.clone()).await.unwrap().1.as_deref(),
        Some("passed")
    );
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/cl/launches/{cid}/run-items"),
            &tester,
            json!({"caseNumbers": [b]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let list = items(&s, "cl", &cid).await;
    set_status(&s, "cl", &tester, &item_of(&list, b), "failed").await;
    let (st, closed) = s
        .post(
            &format!("/api/v1/projects/cl/launches/{cid}/manual/close"),
            &tester,
            json!({"untestedTo": "leave"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{closed}");
    assert_eq!(closed["status"], "finished");
    let cid3 = cid.clone();
    wait_for("gate evaluated again after close", || {
        let f = gate_state(cid3.clone());
        async move {
            f.await
                .is_some_and(|g| g.0 == "done" && g.1.as_deref() == Some("failed"))
        }
    })
    .await;
    let (st, g) = s
        .get(
            &format!("/api/v1/projects/cl/launches/{cid}/gate"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(g["status"], "failed", "{g}");
    audit(&s, &["manualRun.close", "manualRun.reopen"]).await;
}

#[tokio::test]
async fn partition_drop_keeps_a_closed_manual_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_paused(
        &db,
        &[
            "history.rollup",
            "retention.apply",
            "partitions.ensure",
            "analysis.indexes",
        ],
    )
    .await;
    s.project("pd").await;
    let p = pid(&db, "pd").await;
    let store = fastproof_store::Store::new(db.pool.clone());
    store.ensure_partitions(8, 1).await.unwrap();
    let n = case(&db, p, "Old", vec![step("a", "", vec![])]).await;
    let plan = managed(
        &s,
        "pd",
        ADMIN_TOKEN,
        json!({"name": "P", "caseNumbers": [n]}),
    )
    .await;
    let started = start(
        &s,
        "pd",
        ADMIN_TOKEN,
        plan["id"].as_str().unwrap(),
        json!({"name": "old run", "assign": "none", "includeAutomated": false}),
    )
    .await;
    let lid = started["launch"]["id"].as_str().unwrap().to_string();
    assert_eq!(started["ciHint"], "", "no CI part without automated cases");
    let list = items(&s, "pd", &lid).await;
    let iid = item_of(&list, n);
    put_step(
        &s,
        "pd",
        ADMIN_TOKEN,
        &iid,
        "1",
        json!({"status": "failed", "actual": "no"}),
    )
    .await;
    set_status(&s, "pd", ADMIN_TOKEN, &iid, "failed").await;
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/pd/launches/{lid}/manual/close"),
            ADMIN_TOKEN,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let auto = s.launch("pd", Some("auto")).await;
    let aid = auto["id"].as_str().unwrap().to_string();
    for t in ["launches"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update {t} set created_at = now() - interval '125 days', started_at = now() - interval '125 days', \
             status = 'finished', rolled_up_at = now() where id = any($1::uuid[])"
        )))
        .bind(vec![lid.clone(), aid.clone()])
        .execute(&db.pool)
        .await
        .unwrap();
    }
    for t in ["results", "attempts", "run_items"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update {t} set launch_created_at = (select created_at from launches where id = $1::uuid) \
             where launch_id = $1::uuid"
        )))
        .bind(&lid)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let month: String = sqlx::query_scalar(
        "select 'results_p' || to_char(created_at, 'YYYYMM') from launches where id = $1::uuid",
    )
    .bind(&lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let in_month: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from {month} where launch_id = $1::uuid"
    )))
    .bind(&lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(in_month, 1, "the projected result moved into the old month");
    let projects = store.all_project_settings().await.unwrap();
    let dropped = store.drop_rolled_partitions(&projects).await.unwrap();
    assert!(dropped.contains(&month), "{dropped:?}");
    let left: Vec<String> =
        sqlx::query_scalar("select id::text from launches where id = any($1::uuid[])")
            .bind(vec![lid.clone(), aid.clone()])
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        left,
        vec![lid.clone()],
        "the closed manual launch stays, the automated one goes"
    );
    let l = s.launch_get("pd", &lid).await;
    assert_eq!(l["manual"]["failed"], 1);
    let (got, _) = item(&s, "pd", &iid).await;
    assert!(got["resultId"].is_null());
    assert_eq!(got["status"], "failed");
    assert_eq!(got["stepResults"][0]["actual"], "no");
    let (st, report) = s
        .get(
            &format!("/api/v1/projects/pd/launches/{lid}/manual-report?format=json"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert_eq!(report["items"][0]["status"], "failed");
}

#[tokio::test]
async fn permissions_and_idor_for_every_route() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pa").await;
    s.project("pb").await;
    let pa = pid(&db, "pa").await;
    let pb = pid(&db, "pb").await;
    let viewer = s.token("v", "viewer", &["pa"]).await;
    let tester = s.token("t", "tester", &["pa"]).await;
    let uploader = s.token("u", "uploader", &["pa"]).await;
    let editor = s.token("e", "editor", &["pa"]).await;
    let outsider = s.token("o", "editor", &["pb"]).await;
    let tname = me(&s, &tester).await;
    let n = case(&db, pa, "A", vec![step("x", "", vec![])]).await;
    let n2 = case(&db, pa, "B", vec![step("y", "", vec![])]).await;
    let nb = case(&db, pb, "Other", vec![step("z", "", vec![])]).await;
    let plan = managed(
        &s,
        "pa",
        ADMIN_TOKEN,
        json!({"name": "P", "caseNumbers": [n, n2]}),
    )
    .await;
    let plan_id = plan["id"].as_str().unwrap().to_string();
    let started = start(
        &s,
        "pa",
        ADMIN_TOKEN,
        &plan_id,
        json!({"name": "run", "assign": "none", "includeAutomated": true}),
    )
    .await;
    let lid = started["launch"]["id"].as_str().unwrap().to_string();
    let list = items(&s, "pa", &lid).await;
    let iid = item_of(&list, n);
    let iid2 = item_of(&list, n2);
    let plan_b = managed(
        &s,
        "pb",
        ADMIN_TOKEN,
        json!({"name": "PB", "caseNumbers": [nb]}),
    )
    .await;
    let started_b = start(
        &s,
        "pb",
        ADMIN_TOKEN,
        plan_b["id"].as_str().unwrap(),
        json!({"name": "run b", "assign": "none", "includeAutomated": true}),
    )
    .await;
    let lid_b = started_b["launch"]["id"].as_str().unwrap().to_string();
    let iid_b = item_of(&items(&s, "pb", &lid_b).await, nb);

    let a = "/api/v1/projects/pa";
    let routes: Vec<(Method, String, Option<Value>, &str)> = vec![
        (Method::GET, format!("{a}/test-plans"), None, "cases:read"),
        (
            Method::POST,
            format!("{a}/test-plans"),
            Some(json!({"kind": "managed", "name": "new"})),
            "plans:write",
        ),
        (
            Method::GET,
            format!("{a}/test-plans/{plan_id}"),
            None,
            "cases:read",
        ),
        (
            Method::PATCH,
            format!("{a}/test-plans/{plan_id}"),
            Some(json!({"name": "x"})),
            "plans:write",
        ),
        (
            Method::DELETE,
            format!("{a}/test-plans/{plan_id}"),
            None,
            "plans:write",
        ),
        (
            Method::GET,
            format!("{a}/test-plans/{plan_id}/items"),
            None,
            "cases:read",
        ),
        (
            Method::POST,
            format!("{a}/test-plans/{plan_id}/items"),
            Some(json!({"remove": [n]})),
            "plans:write",
        ),
        (
            Method::POST,
            format!("{a}/test-plans/{plan_id}/sync?dryRun=true"),
            Some(json!({})),
            "plans:write",
        ),
        (
            Method::GET,
            format!("{a}/test-plans/{plan_id}/testplan.json"),
            None,
            "launches:read",
        ),
        (
            Method::POST,
            format!("{a}/test-plans/{plan_id}/launches"),
            Some(json!({"name": "x", "assign": "me", "includeAutomated": true})),
            "runs:execute",
        ),
        (
            Method::GET,
            format!("{a}/launches/{lid}/testplan.json"),
            None,
            "launches:read",
        ),
        (
            Method::POST,
            format!("{a}/launches/{lid}/run-items"),
            Some(json!({"caseNumbers": [n]})),
            "runs:execute",
        ),
        (
            Method::GET,
            format!("{a}/launches/{lid}/run-items"),
            None,
            "launches:read",
        ),
        (Method::GET, format!("{a}/run-items"), None, "launches:read"),
        (
            Method::GET,
            format!("{a}/run-items/{iid}"),
            None,
            "launches:read",
        ),
        (
            Method::PUT,
            format!("{a}/run-items/{iid}/steps/1"),
            Some(json!({"status": "passed"})),
            "runs:execute",
        ),
        (
            Method::PATCH,
            format!("{a}/run-items/{iid}"),
            Some(json!({"comment": "c"})),
            "runs:execute",
        ),
        (
            Method::POST,
            format!("{a}/run-items/{iid}/defect"),
            Some(json!({"comment": "c"})),
            "runs:execute",
        ),
        (
            Method::GET,
            format!("{a}/run-items/{iid}/new-issue-url"),
            None,
            "launches:read",
        ),
        (
            Method::POST,
            format!("{a}/run-items/{iid}/rerun"),
            Some(json!({})),
            "runs:execute",
        ),
        (
            Method::POST,
            format!("{a}/launches/{lid}/manual/close"),
            Some(json!({"untestedTo": "leave"})),
            "runs:execute",
        ),
        (
            Method::POST,
            format!("{a}/launches/{lid}/manual/reopen"),
            Some(json!({})),
            "plans:write",
        ),
        (
            Method::GET,
            format!("{a}/launches/{lid}/manual-report"),
            None,
            "launches:read",
        ),
    ];
    let holds = |role: &str, perm: &str| match role {
        "viewer" => matches!(perm, "cases:read" | "launches:read"),
        "tester" => matches!(perm, "cases:read" | "launches:read" | "runs:execute"),
        "uploader" => matches!(perm, "cases:read" | "launches:read"),
        _ => true,
    };
    for (m, path, body, perm) in &routes {
        for (role, token) in [
            ("viewer", &viewer),
            ("tester", &tester),
            ("uploader", &uploader),
        ] {
            if holds(role, perm) {
                continue;
            }
            let (st, b) = s.call(m.clone(), path, Some(token), body.clone()).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{role} {m} {path}: {b}");
        }
        let (st, b) = s.call(m.clone(), path, Some(&outsider), body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "outsider {m} {path}: {b}");
        let (st, _) = s.call(m.clone(), path, None, body.clone()).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "anonymous {m} {path}");
        if m == Method::GET {
            for token in [&viewer, &tester, &uploader, &editor] {
                let (st, b) = s.call(Method::GET, path, Some(token), None).await;
                assert!(
                    st == StatusCode::OK || st == StatusCode::CONFLICT,
                    "{m} {path}: {st} {b}"
                );
            }
        }
    }
    let r = s
        .req(
            Method::POST,
            &format!("{a}/run-items/{iid}/attachments"),
            Some(&viewer),
        )
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    let (st, b) = put_step(&s, "pa", &tester, &iid, "1", json!({"status": "failed"})).await;
    assert_eq!(st, StatusCode::OK, "tester executes: {b}");
    let (st, b) = s
        .post(
            &format!("{a}/run-items/{iid2}/defect"),
            &tester,
            json!({"comment": "c"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "not the tester's item: {b}");
    let failed = set_status(&s, "pa", &tester, &iid, "failed").await;
    assert_eq!(failed["executedBy"].as_str(), Some(tname.as_str()));
    let (st, d) = s
        .post(
            &format!("{a}/run-items/{iid}/defect"),
            &tester,
            json!({"comment": "known gateway bug", "issue": {"url": "https://jira.example.com/browse/PAY-1", "key": "PAY-1"}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["defect"]["source"], "user");
    assert_eq!(d["defect"]["issueKey"], "PAY-1");
    let (_, r) = s
        .get(
            &format!("{a}/results/{}", d["resultId"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(r["defect"]["comment"], "known gateway bug");
    let (st, b) = s
        .post(
            &format!("{a}/run-items/{iid2}/defect"),
            &editor,
            json!({"comment": "c"}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("run_item_not_final")),
        "results:triage passes the ownership check"
    );
    let (st, _, _) = send(
        &s,
        Method::PATCH,
        &format!("{a}/test-plans/{plan_id}"),
        Some(&tester),
        Some("\"r1\""),
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a tester does not edit plans");

    let b = "/api/v1/projects/pb";
    let idor: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, format!("{b}/test-plans/{plan_id}"), None),
        (
            Method::PATCH,
            format!("{b}/test-plans/{plan_id}"),
            Some(json!({"name": "x"})),
        ),
        (Method::DELETE, format!("{b}/test-plans/{plan_id}"), None),
        (Method::GET, format!("{b}/test-plans/{plan_id}/items"), None),
        (
            Method::POST,
            format!("{b}/test-plans/{plan_id}/items"),
            Some(json!({"remove": [n]})),
        ),
        (
            Method::POST,
            format!("{b}/test-plans/{plan_id}/sync"),
            Some(json!({})),
        ),
        (
            Method::GET,
            format!("{b}/test-plans/{plan_id}/testplan.json"),
            None,
        ),
        (
            Method::POST,
            format!("{b}/test-plans/{plan_id}/launches"),
            Some(json!({"name": "x", "assign": "none", "includeAutomated": true})),
        ),
        (
            Method::GET,
            format!("{b}/launches/{lid}/testplan.json"),
            None,
        ),
        (
            Method::POST,
            format!("{b}/launches/{lid}/run-items"),
            Some(json!({"caseNumbers": [nb]})),
        ),
        (Method::GET, format!("{b}/launches/{lid}/run-items"), None),
        (Method::GET, format!("{b}/run-items/{iid}"), None),
        (
            Method::PUT,
            format!("{b}/run-items/{iid}/steps/1"),
            Some(json!({"status": "passed"})),
        ),
        (
            Method::PATCH,
            format!("{b}/run-items/{iid}"),
            Some(json!({"comment": "c"})),
        ),
        (
            Method::POST,
            format!("{b}/run-items/{iid}/defect"),
            Some(json!({"comment": "c"})),
        ),
        (
            Method::GET,
            format!("{b}/run-items/{iid}/new-issue-url"),
            None,
        ),
        (
            Method::POST,
            format!("{b}/run-items/{iid}/rerun"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("{b}/launches/{lid}/manual/close"),
            Some(json!({"untestedTo": "leave"})),
        ),
        (
            Method::POST,
            format!("{b}/launches/{lid}/manual/reopen"),
            Some(json!({})),
        ),
        (
            Method::GET,
            format!("{b}/launches/{lid}/manual-report"),
            None,
        ),
    ];
    for (m, path, body) in idor {
        let (st, e, _) = send(
            &s,
            m.clone(),
            &path,
            Some(ADMIN_TOKEN),
            Some("\"r1\""),
            body,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} {path}: {e}");
    }
    let r = s
        .req(
            Method::POST,
            &format!("{b}/run-items/{iid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let (st, e) = s
        .post(
            &format!("{b}/launches/{lid_b}/run-items"),
            ADMIN_TOKEN,
            json!({"planId": plan_id}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a plan of another project: {e}");
    let (st, e) = s
        .post(
            &format!("{b}/launches/{lid_b}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [n2 + 100]}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let r = s
        .req(
            Method::POST,
            &format!("{a}/run-items/{iid2}/attachments"),
            Some(&tester),
        )
        .header("x-fastproof-attachment-name", "shot.png")
        .header("content-type", "image/png")
        .body(vec![1u8, 2, 3])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let att: Value = r.json().await.unwrap();
    let (st, e) = put_step(
        &s,
        "pb",
        ADMIN_TOKEN,
        &iid_b,
        "1",
        json!({"status": "failed", "attachmentIds": [att["id"]]}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "an attachment of another project: {e}"
    );
    let (st, ack) = put_step(
        &s,
        "pa",
        &tester,
        &iid2,
        "1",
        json!({"status": "failed", "attachmentIds": [att["id"]]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ack}");
    let (got, _) = item(&s, "pa", &iid2).await;
    assert_eq!(got["stepResults"][0]["attachments"][0]["name"], "shot.png");
    assert_eq!(got["stepResults"][0]["attachments"][0]["size"], 3);
    let folder_b: Uuid = Uuid::now_v7();
    sqlx::query("insert into case_folders (id, project_id, name, path_ids, depth) values ($1, $2, 'F', array[$1], 1)")
        .bind(folder_b)
        .bind(pb)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, e) = s
        .post(
            &format!("{a}/test-plans"),
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "f", "filter": {"folderIds": [folder_b]}}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a folder of another project: {e}"
    );
}

#[tokio::test]
async fn limits_of_plans_and_runs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MAX_PLAN_CASES", "2"),
            ("FASTPROOF_TM_MAX_RUN_ITEMS", "2"),
            ("FASTPROOF_TM_MAX_BULK", "3"),
            ("FASTPROOF_TM_MAX_STEP_ATTACHMENTS", "1"),
            ("FASTPROOF_TM_MAX_MARKDOWN_BYTES", "10"),
            ("FASTPROOF_TM_MAX_ATTACHMENT_BYTES", "4"),
        ],
    )
    .await;
    s.project("lm").await;
    let p = pid(&db, "lm").await;
    let mut ns = Vec::new();
    for i in 0..4 {
        ns.push(case(&db, p, &format!("c{i}"), vec![step("s", "", vec![])]).await);
    }
    let base = "/api/v1/projects/lm/test-plans";
    let (st, e) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "x", "caseNumbers": &ns[..3]}),
        )
        .await;
    assert_eq!(
        (st, e["details"]["limit"].as_str()),
        (StatusCode::CONFLICT, Some("maxPlanCases")),
        "{e}"
    );
    let (st, e) = s
        .post(
            base,
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "x", "caseNumbers": &ns}),
        )
        .await;
    assert_eq!(
        (st, e["details"]["limit"].as_str()),
        (StatusCode::CONFLICT, Some("maxBulk")),
        "{e}"
    );
    let plan = managed(
        &s,
        "lm",
        ADMIN_TOKEN,
        json!({"name": "ok", "caseNumbers": &ns[..2]}),
    )
    .await;
    let (st, e) = s
        .post(
            &format!("{base}/{}/items", plan["id"].as_str().unwrap()),
            ADMIN_TOKEN,
            json!({"add": [ns[2]]}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    let l = s.launch("lm", None).await;
    let lid = l["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/lm/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": &ns[..2]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, e) = s
        .post(
            &format!("/api/v1/projects/lm/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [ns[2]]}),
        )
        .await;
    assert_eq!(
        (st, e["details"]["limit"].as_str()),
        (StatusCode::CONFLICT, Some("maxRunItems")),
        "{e}"
    );
    let iid = item_of(&items(&s, "lm", lid).await, ns[0]);
    let (st, e) = put_step(
        &s,
        "lm",
        ADMIN_TOKEN,
        &iid,
        "1",
        json!({"status": "failed", "actual": "x".repeat(11)}),
    )
    .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_markdown")),
        "{e}"
    );
    let ids = [Uuid::now_v7(), Uuid::now_v7()];
    let (st, e) = put_step(
        &s,
        "lm",
        ADMIN_TOKEN,
        &iid,
        "1",
        json!({"status": "failed", "attachmentIds": ids}),
    )
    .await;
    assert_eq!(
        (st, e["details"]["limit"].as_str()),
        (StatusCode::CONFLICT, Some("maxStepAttachments")),
        "{e}"
    );
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/lm/run-items/{iid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .body("12345")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let (st, e) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/lm/run-items/{iid}"),
            Some(ADMIN_TOKEN),
            Some(json!({"status": "nope"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
}

#[tokio::test]
async fn parallel_step_writers_on_one_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_DATABASE_MAX_CONNECTIONS", "16")]).await;
    s.project("par").await;
    let p = pid(&db, "par").await;
    const WRITERS: usize = 50;
    const STEPS: usize = 10;
    let mut numbers = Vec::new();
    for i in 0..WRITERS {
        let steps = (0..STEPS)
            .map(|k| step(&format!("step {k}"), "", vec![]))
            .collect();
        numbers.push(case(&db, p, &format!("case {i}"), steps).await);
    }
    let tester = s.token("t", "tester", &["par"]).await;
    let plan = managed(
        &s,
        "par",
        ADMIN_TOKEN,
        json!({"name": "P", "caseNumbers": numbers}),
    )
    .await;
    let started = start(
        &s,
        "par",
        &tester,
        plan["id"].as_str().unwrap(),
        json!({"name": "par", "assign": "me", "includeAutomated": false}),
    )
    .await;
    let lid = started["launch"]["id"].as_str().unwrap().to_string();
    let list = items(&s, "par", &lid).await;
    assert_eq!(list.len(), WRITERS);
    let ids: Vec<String> = list
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let base = s.base.clone();
    let t0 = Instant::now();
    let mut tasks = Vec::new();
    for (w, id) in ids.iter().enumerate() {
        let (base, id, tester, shared) = (base.clone(), id.clone(), tester.clone(), ids[0].clone());
        tasks.push(tokio::spawn(async move {
            let c = Server::client_only(&base);
            let (st, _) = c.get(&format!("/api/v1/projects/par/run-items/{id}"), &tester).await;
            assert_eq!(st, StatusCode::OK);
            let mut lat: Vec<(&'static str, Duration)> = Vec::new();
            let mut rev = 0;
            for k in 1..=STEPS {
                let t = Instant::now();
                let (st, ack) = put_step(&c, "par", &tester, &id, &k.to_string(),
                    json!({"status": if k == STEPS && w % 5 == 0 { "failed" } else { "passed" }, "actual": format!("w{w} s{k}")})).await;
                lat.push((if k == 1 { "first step" } else { "step" }, t.elapsed()));
                assert_eq!(st, StatusCode::OK, "{ack}");
                rev = ack["revision"].as_i64().unwrap();
            }
            let t = Instant::now();
            let (st, b, _) = send(&c, Method::PATCH, &format!("/api/v1/projects/par/run-items/{id}"), Some(&tester),
                Some(&format!("\"r{rev}\"")), Some(json!({"status": if w % 5 == 0 { "failed" } else { "passed" }}))).await;
            lat.push(("final status", t.elapsed()));
            assert_eq!(st, StatusCode::OK, "{b}");
            let t = Instant::now();
            let (st, b) = put_step(&c, "par", &tester, &shared, "1", json!({"status": "passed", "comment": format!("w{w}")})).await;
            lat.push(("shared item", t.elapsed()));
            assert_eq!(st, StatusCode::OK, "{b}");
            lat
        }));
    }
    let mut lat: Vec<(&'static str, Duration)> = Vec::new();
    for t in tasks {
        lat.extend(t.await.unwrap());
    }
    let wall = t0.elapsed();
    let pct = |kind: Option<&str>| {
        let mut v: Vec<Duration> = lat
            .iter()
            .filter(|(k, _)| kind.is_none_or(|w| *k == w))
            .map(|x| x.1)
            .collect();
        v.sort();
        (
            v.len(),
            v[v.len() / 2],
            v[v.len() * 95 / 100],
            *v.last().unwrap(),
        )
    };
    eprintln!(
        "parallel writers: {WRITERS} writers, {} requests in {wall:?}",
        lat.len()
    );
    for kind in [
        None,
        Some("step"),
        Some("first step"),
        Some("final status"),
        Some("shared item"),
    ] {
        let (n, p50, p95, max) = pct(kind);
        eprintln!(
            "  {:<12} n={n:<4} p50 {p50:?}, p95 {p95:?}, max {max:?}",
            kind.unwrap_or("all")
        );
    }
    let (rows, done): (i64, i64) = sqlx::query_as(
        "select (select count(*) from run_step_results s join run_items i on i.id = s.run_item_id where i.launch_id = $1::uuid), \
           (select count(*) from run_items where launch_id = $1::uuid and status in ('passed', 'failed'))",
    )
    .bind(&lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (rows as usize, done as usize),
        (WRITERS * STEPS, WRITERS),
        "no lost writes"
    );
    let l = s.launch_get("par", &lid).await;
    assert_eq!(l["counts"]["total"].as_i64(), Some(WRITERS as i64));
    assert_eq!(l["counts"]["failed"].as_i64(), Some((WRITERS / 5) as i64));
    assert_eq!(
        l["manual"]["passed"].as_i64(),
        Some((WRITERS - WRITERS / 5) as i64)
    );
    let (got, _) = item(&s, "par", &ids[0]).await;
    let comment = got["stepResults"][0]["comment"].as_str().unwrap();
    assert!(comment.starts_with('w'), "last writer wins: {comment}");
    let shared_rev = got["revision"].as_i64().unwrap();
    assert_eq!(
        shared_rev as usize,
        1 + STEPS + 1 + WRITERS,
        "every write bumped the revision once"
    );
    let (attempt_steps,): (i64,) = sqlx::query_as(
        "select count(*) from attempts where launch_id = $1::uuid and steps_z is not null",
    )
    .bind(&lid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(attempt_steps as usize, WRITERS);
}

#[tokio::test]
async fn start_from_a_plan_of_5000_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("big").await;
    let p = pid(&db, "big").await;
    const N: i64 = 5000;
    let body = CaseBody {
        description: String::new(),
        preconditions: "p".into(),
        steps: (0..5)
            .map(|k| step(&format!("s{k}"), "e", vec![]))
            .collect(),
        links: Vec::new(),
        parameters: None,
    };
    let snapshot = CaseSnapshot {
        name: "case".into(),
        status: CaseStatus::Active,
        folder_id: None,
        body: body.clone(),
        fields: Default::default(),
        tags: Vec::new(),
    };
    sqlx::query(
        "with s as (update projects set case_seq = case_seq + $3 where id = $1 returning case_seq) \
         insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by) \
         select gen_random_uuid(), $1, (select case_seq from s) - $3 + g, 'Case ' || g, 'active', 'ui', $2, 't', 't' \
         from generate_series(1, $3) g",
    )
    .bind(p)
    .bind(serde_json::to_value(&body).unwrap())
    .bind(N)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, snapshot_z, created_by) \
         select id, 1, $1, 'ui', $2, 't' from cases where project_id = $1",
    )
    .bind(p)
    .bind(fastproof_store::codec::encode(&snapshot).unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let numbers: Vec<i64> = (1..=N).collect();
    let t = Instant::now();
    let plan = managed(
        &s,
        "big",
        ADMIN_TOKEN,
        json!({"name": "Big", "caseNumbers": numbers}),
    )
    .await;
    let plan_ms = t.elapsed();
    assert_eq!(plan["counts"]["total"], N);
    let t = Instant::now();
    let started = start(
        &s,
        "big",
        ADMIN_TOKEN,
        plan["id"].as_str().unwrap(),
        json!({"name": "big run", "assign": "me", "includeAutomated": true}),
    )
    .await;
    let took = t.elapsed();
    eprintln!(
        "plan of {N} cases created in {plan_ms:?}; launch with {N} run items started in {took:?}"
    );
    assert_eq!(started["launch"]["manual"]["total"], N);
    assert!(took < Duration::from_secs(30), "{took:?}");
    let identities: i64 = sqlx::query_scalar(
        "select count(*) from test_cases where project_id = $1 and kind = 'manual'",
    )
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(identities, N);
    let lid = started["launch"]["id"].as_str().unwrap();
    let (st, page) = s
        .get(
            &format!("/api/v1/projects/big/launches/{lid}/run-items?limit=500&status=untested"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(page["items"].as_array().unwrap().len(), 500);
    assert!(page["next"].is_string());
}

#[tokio::test]
async fn report_issue_link_and_cli() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rep").await;
    let p = pid(&db, "rep").await;
    fastproof_store::Store::new(db.pool.clone())
        .merge_project_settings(
            p,
            json!({"issueTrackers": [
                {"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}", "keyRegex": "[A-Z]+-[0-9]+"},
                {"type": "github", "urlTemplate": "https://github.com/org/app/issues/{key}", "keyRegex": "#?[0-9]+"}]}),
        )
        .await
        .unwrap();
    let n = case(
        &db,
        p,
        "=HYPERLINK(\"x\")",
        vec![step("Open", "Works", vec![])],
    )
    .await;
    let a = case(&db, p, "Auto", vec![]).await;
    automate(&db, p, a, "suite.auto", Some("77")).await;
    let plan = managed(
        &s,
        "rep",
        ADMIN_TOKEN,
        json!({"name": "R", "caseNumbers": [n, a]}),
    )
    .await;
    let plan_id = plan["id"].as_str().unwrap().to_string();
    let started = start(
        &s,
        "rep",
        ADMIN_TOKEN,
        &plan_id,
        json!({"name": "report", "externalId": "ci 42; rm -rf /", "assign": "none", "includeAutomated": true}),
    )
    .await;
    assert!(
        started["ciHint"]
            .as_str()
            .unwrap()
            .contains("--launch 'ci 42; rm -rf /'"),
        "{}",
        started["ciHint"]
    );
    let lid = started["launch"]["id"].as_str().unwrap().to_string();
    let iid = item_of(&items(&s, "rep", &lid).await, n);
    put_step(
        &s,
        "rep",
        ADMIN_TOKEN,
        &iid,
        "1",
        json!({"status": "failed", "actual": "-1 & broken"}),
    )
    .await;
    set_status(&s, "rep", ADMIN_TOKEN, &iid, "failed").await;

    let (st, u) = s
        .get(
            &format!("/api/v1/projects/rep/run-items/{iid}/new-issue-url"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let url = u["url"].as_str().unwrap();
    assert_eq!(
        url, "https://jira.example.com/secure/CreateIssue!default.jspa",
        "Jira without projectId and issueTypeId: the create page"
    );
    assert_eq!(u["prefilled"], false);
    assert!(
        u["prefill"]["description"]
            .as_str()
            .unwrap()
            .contains("Actual: -1 & broken"),
        "{u}"
    );
    let (_, u) = s
        .get(
            &format!("/api/v1/projects/rep/run-items/{iid}/new-issue-url?tracker=github"),
            ADMIN_TOKEN,
        )
        .await;
    assert!(
        u["url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/org/app/issues/new?title=")
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/rep/run-items/{iid}/new-issue-url?tracker=gitlab"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/rep/launches/{lid}/manual-report"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    let csv = r.text().await.unwrap();
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert!(lines[0].starts_with("\"case\",\"name\""), "{csv}");
    assert!(
        lines[1].contains("\"'=HYPERLINK(\"\"x\"\")\""),
        "formula cells are prefixed: {csv}"
    );
    assert!(lines[1].contains("1 failed: -1 & broken"), "{csv}");
    let (st, j) = s
        .get(
            &format!("/api/v1/projects/rep/launches/{lid}/manual-report?format=json"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(j["items"][0]["steps"][0]["expected"], "Works");
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/rep/launches/{lid}/manual-report?format=xml"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let cli = |args: Vec<String>| {
        let base = s.base.clone();
        async move {
            let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_fastproof"))
                .arg("testplan")
                .args(["--url", &base, "--project", "rep"])
                .args(&args)
                .env("FASTPROOF_TOKEN", ADMIN_TOKEN)
                .env_remove("FASTPROOF_LAUNCH")
                .env_remove("FASTPROOF_PROJECT")
                .output()
                .await
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
            )
        }
    };
    let (code, out, err) = cli(vec!["--plan".into(), plan_id.clone()]).await;
    assert_eq!(code, Some(0), "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["tests"], json!([{"id": "77", "selector": "suite.auto"}]));
    let (code, out, err) = cli(vec!["--launch".into(), "ci 42; rm -rf /".into()]).await;
    assert_eq!(code, Some(0), "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["tests"][0]["selector"], "suite.auto");
    let (code, _, _) = cli(vec!["--launch".into(), "missing".into()]).await;
    assert_ne!(code, Some(0));
}

#[tokio::test]
async fn cases_api_identity_and_filter_match_the_case_list() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("mix").await;
    let p = pid(&db, "mix").await;
    let base = "/api/v1/projects/mix/cases";
    let mut numbers = Vec::new();
    for (name, tags) in [
        ("Pay", json!(["smoke", "pay"])),
        ("Refund", json!(["pay"])),
        ("Login", json!(["smoke"])),
    ] {
        let (st, c) = s
            .post(
                base,
                ADMIN_TOKEN,
                json!({"name": name, "body": {"steps": [{"action": "Do", "expected": "Done"}]}, "tags": tags}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{c}");
        numbers.push(c["number"].as_i64().unwrap());
    }
    let filter = json!({"tags": ["smoke", "pay"]});
    let (st, listed) = s
        .get(&format!("{base}?tag=smoke&tag=pay"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{listed}");
    let mut want: Vec<i64> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["number"].as_i64().unwrap())
        .collect();
    want.sort();
    let plan = managed(
        &s,
        "mix",
        ADMIN_TOKEN,
        json!({"name": "F", "filter": filter}),
    )
    .await;
    let (_, plan_items) = s
        .get(
            &format!(
                "/api/v1/projects/mix/test-plans/{}/items",
                plan["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
        )
        .await;
    let mut got: Vec<i64> = plan_items["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["caseNumber"].as_i64().unwrap())
        .collect();
    got.sort();
    assert_eq!(got, want, "a plan filter matches what the case list shows");
    assert_eq!(got, vec![numbers[0]]);

    let before: Vec<(Uuid, i64)> = sqlx::query_as(
        "select t.id, c.number from test_cases t join cases c on c.id = t.case_id \
         where t.project_id = $1 and t.kind = 'manual' order by c.number",
    )
    .bind(p)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        before.len(),
        3,
        "the cases API created the manual identities"
    );
    let l = s.launch("mix", None).await;
    let lid = l["id"].as_str().unwrap();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/mix/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": numbers}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let list = items(&s, "mix", lid).await;
    let done = set_status(
        &s,
        "mix",
        ADMIN_TOKEN,
        &item_of(&list, numbers[1]),
        "passed",
    )
    .await;
    let tc: Uuid = sqlx::query_scalar("select test_case_id from results where id = $1::uuid")
        .bind(done["resultId"].as_str().unwrap())
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar(
        "select count(*) from test_cases where project_id = $1 and kind = 'manual'",
    )
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(after, 3, "run items reuse the identities");
    assert_eq!(
        tc, before[1].0,
        "the result lands on the case's manual identity"
    );
    let (st, c) = s.get(&format!("{base}/{}", numbers[1]), ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["identities"].as_array().unwrap().len(), 1);
}
