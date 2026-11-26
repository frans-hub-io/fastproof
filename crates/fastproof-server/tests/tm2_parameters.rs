//! Parameters over HTTP: the generator and iteration routes with their permissions,
//! `If-Match`, IDOR across projects, the gate on case writes, a run of a parameterized case
//! with its iterations, the derived item status, the audit trail and the live event, and the
//! series of an iteration next to an uploaded Allure result with the same parameters.

mod common;

use std::io::Write;
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

fn base(key: &str) -> String {
    format!("/api/v1/projects/{key}")
}

fn checkout(rows: Value) -> Value {
    json!({"name": "Checkout", "body": {
        "preconditions": "Open the shop in ${browser}",
        "steps": [{"action": "Add a book"}, {"action": "Pay in ${currency}", "expected": "Paid in ${currency}"}],
        "parameters": {"columns": ["browser", "currency"], "rows": rows,
            "generator": {"mode": "all", "values": {"browser": ["firefox", "chromium"], "currency": ["EUR"]}}}}})
}

fn two_rows() -> Value {
    json!([{"id": "r_ff", "values": {"browser": "firefox", "currency": "EUR"}, "enabled": true},
           {"id": "r_cr", "values": {"browser": "chromium", "currency": "EUR"}, "enabled": true}])
}

async fn create_case(s: &Server, key: &str, body: Value) -> Reply {
    send(
        s,
        Method::POST,
        &format!("{}/cases", base(key)),
        Some(ADMIN_TOKEN),
        None,
        Some(body),
    )
    .await
}

/// A launch with a run item for case `n`; returns `(launch id, item id)`.
async fn start(s: &Server, key: &str, n: i64) -> (String, String) {
    let launch = s.launch(key, None).await;
    let lid = launch["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("{}/launches/{lid}/run-items", base(key)),
            ADMIN_TOKEN,
            json!({"caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, items) = s
        .get(
            &format!("{}/launches/{lid}/run-items", base(key)),
            ADMIN_TOKEN,
        )
        .await;
    (lid, items["items"][0]["id"].as_str().unwrap().to_string())
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

#[tokio::test]
async fn routes_check_authentication_permission_and_project() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pa").await;
    s.project("pb").await;
    let viewer = s.token("vic", "viewer", &["pa"]).await;
    let tester = s.token("tess", "tester", &["pa"]).await;
    let editor_b = s.token("edb", "editor", &["pb"]).await;
    let c = create_case(&s, "pa", checkout(two_rows())).await;
    assert_eq!(c.status, StatusCode::CREATED, "{}", c.body);
    let n = c.body["number"].as_i64().unwrap();
    let (_, item) = start(&s, "pa", n).await;
    let gen_body = json!({"mode": "pairwise", "values": {"a": ["1", "2"], "b": ["x", "y"]}});
    let step = json!({"status": "passed"});
    let patch = json!({"comment": "x"});
    // (method, path suffix, body, needs If-Match, minimum role)
    let routes: Vec<(Method, String, Value, bool, &str)> = vec![
        (
            Method::POST,
            format!("/cases/{n}/parameters/generate"),
            gen_body.clone(),
            false,
            "write",
        ),
        (
            Method::PUT,
            format!("/run-items/{item}/iterations/r_ff/steps/1"),
            step.clone(),
            false,
            "execute",
        ),
        (
            Method::PATCH,
            format!("/run-items/{item}/iterations/r_ff"),
            patch.clone(),
            true,
            "execute",
        ),
    ];
    for (method, suffix, body, needs_match, role) in &routes {
        let path = format!("{}{suffix}", base("pa"));
        let m = needs_match.then_some("\"r1\"");
        let anon = send(&s, method.clone(), &path, None, m, Some(body.clone())).await;
        assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{method} {suffix}");
        let foreign = send(
            &s,
            method.clone(),
            &path,
            Some(&editor_b),
            m,
            Some(body.clone()),
        )
        .await;
        assert_eq!(foreign.status, StatusCode::FORBIDDEN, "{method} {suffix}");
        let nowhere = send(
            &s,
            method.clone(),
            &format!("{}{suffix}", base("zz")),
            Some(&editor_b),
            m,
            Some(body.clone()),
        )
        .await;
        assert_eq!(nowhere.status, StatusCode::FORBIDDEN, "{method} {suffix}");
        let r = send(
            &s,
            method.clone(),
            &path,
            Some(&viewer),
            m,
            Some(body.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "viewer {method} {suffix}");
        let r = send(
            &s,
            method.clone(),
            &path,
            Some(&tester),
            m,
            Some(body.clone()),
        )
        .await;
        if *role == "write" {
            assert_eq!(r.status, StatusCode::FORBIDDEN, "tester {method} {suffix}");
        } else {
            assert_ne!(
                r.status,
                StatusCode::FORBIDDEN,
                "tester {method} {suffix}: {}",
                r.body
            );
        }
    }
    // Ids of another project are not reachable: the item of "pa" through "pb", a case
    // number that only exists in "pa".
    let pb_admin = |m: Method, suffix: String, body: Value| {
        let s = &s;
        async move {
            send(
                s,
                m,
                &format!("{}{suffix}", base("pb")),
                Some(ADMIN_TOKEN),
                Some("\"r1\""),
                Some(body),
            )
            .await
        }
    };
    let r = pb_admin(
        Method::PUT,
        format!("/run-items/{item}/iterations/r_ff/steps/1"),
        step.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    let r = pb_admin(
        Method::PATCH,
        format!("/run-items/{item}/iterations/r_ff"),
        patch.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    let r = pb_admin(
        Method::POST,
        format!("/cases/{n}/parameters/generate"),
        gen_body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
    for bad in ["x", "0", "-1", "1.5"] {
        let r = send(
            &s,
            Method::POST,
            &format!("{}/cases/{bad}/parameters/generate", base("pa")),
            Some(ADMIN_TOKEN),
            None,
            Some(gen_body.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
    }
    for bad_row in ["r_nope", "nope", "r_a%20b"] {
        let r = send(
            &s,
            Method::PUT,
            &format!(
                "{}/run-items/{item}/iterations/{bad_row}/steps/1",
                base("pa")
            ),
            Some(ADMIN_TOKEN),
            None,
            Some(step.clone()),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad_row}: {}", r.body);
    }
}

#[tokio::test]
async fn generator_gate_and_validation() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pg").await;
    let plain = create_case(&s, "pg", json!({"name": "Plain"})).await;
    let n = plain.body["number"].as_i64().unwrap();
    let path = format!("{}/cases/{n}/parameters/generate", base("pg"));
    let (st, b) = s
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"mode": "all", "values": {"browser": ["firefox", "chromium"], "currency": ["EUR", "USD", "UAH"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let rows = b["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    assert!(
        rows.iter()
            .all(|r| r["enabled"] == true && r["id"].as_str().unwrap().starts_with("r_"))
    );
    let (st, pw) = s
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"mode": "pairwise", "values": {"a": ["1", "2", "3"], "b": ["1", "2", "3"], "c": ["1", "2", "3"], "d": ["1", "2", "3"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{pw}");
    assert!(pw["rows"].as_array().unwrap().len() < 81);
    let (st, e) = s
        .post(
            &path,
            ADMIN_TOKEN,
            json!({"mode": "all", "values": {"a": (0..40).map(|i| i.to_string()).collect::<Vec<_>>(),
                "b": (0..40).map(|i| i.to_string()).collect::<Vec<_>>()}}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("limit_reached")),
        "{e}"
    );
    for bad in [
        json!({"mode": "all", "values": {}}),
        json!({"mode": "all", "values": {"1x": ["a"]}}),
        json!({"mode": "all", "values": {"a": []}}),
        json!({"mode": "nope", "values": {"a": ["1"]}}),
        json!({"mode": "all", "values": {"a": ["1"]}, "extra": 1}),
    ] {
        let (st, e) = s.post(&path, ADMIN_TOKEN, bad.clone()).await;
        assert!(
            st == StatusCode::BAD_REQUEST || st == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {st} {e}"
        );
    }

    let unknown = create_case(
        &s,
        "pg",
        json!({"name": "U", "body": {"steps": [{"action": "Pay in ${price}"}],
            "parameters": {"columns": ["currency"], "rows": []}}}),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
    assert_eq!(unknown.body["code"], "invalid_case");
    assert_eq!(unknown.body["details"][0]["path"], "body.steps[0].action");

    let ok = create_case(&s, "pg", checkout(two_rows())).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
    assert_eq!(ok.body["body"]["parameters"]["rows"][1]["id"], "r_cr");
    let ver = s
        .get(
            &format!("{}/cases/{}/versions/1", base("pg"), ok.body["number"]),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(ver.0, StatusCode::OK, "{}", ver.1);

    let closed = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "closed")]).await;
    let r = create_case(&closed, "pg", checkout(two_rows())).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["code"], "upgrade_in_progress");
    assert_eq!(r.body["details"]["what"], "case parameters");
    let r = create_case(&closed, "pg", json!({"name": "Still fine"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
}

fn allure_zip(browser: &str, currency: &str) -> Vec<u8> {
    let result = json!({"uuid": "r1", "historyId": "h1", "name": "checkout", "fullName": "shop.CheckoutTest#checkout",
        "status": "passed", "start": 1795249075967i64, "stop": 1795249076967i64,
        "parameters": [{"name": "currency", "value": currency}, {"name": "browser", "value": browser}]});
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("r1-result.json", o).unwrap();
        z.write_all(result.to_string().as_bytes()).unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn run_iterations_project_on_the_allure_parameter_series() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pr").await;
    let b = base("pr");
    let tester = s.token("tess", "tester", &["pr"]).await;
    let c = create_case(&s, "pr", checkout(two_rows())).await;
    let n = c.body["number"].as_i64().unwrap();
    let (lid, item) = start(&s, "pr", n).await;
    let (st, list) = s
        .get(&format!("{b}/launches/{lid}/run-items"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["items"][0]["iterationCount"], 2);

    let got = send(
        &s,
        Method::GET,
        &format!("{b}/run-items/{item}"),
        Some(&tester),
        None,
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(got.body["parameterColumns"], json!(["browser", "currency"]));
    assert_eq!(got.body["iterations"][0]["rowId"], "r_ff");
    assert_eq!(got.body["iterations"][1]["values"]["browser"], "chromium");
    assert!(
        got.body["stepsHtml"][1]["action"]
            .as_str()
            .unwrap()
            .contains("${currency}"),
        "{}",
        got.body["stepsHtml"]
    );
    let etag = got.etag.clone().unwrap();

    let r = send(
        &s,
        Method::PUT,
        &format!("{b}/run-items/{item}/steps/1"),
        Some(&tester),
        None,
        Some(json!({"status": "passed"})),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("parameterized_item"))
    );
    let r = send(
        &s,
        Method::PUT,
        &format!("{b}/run-items/{item}/iterations/r_ff/steps/2"),
        Some(&tester),
        None,
        Some(json!({"status": "passed", "actual": "Paid"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["iterationStatus"], "in_progress");
    assert_eq!(r.body["itemStatus"], "in_progress");
    let rev = r.body["revision"].as_i64().unwrap();
    assert!(
        format!("\"r{rev}\"") != etag,
        "a step write moves the item revision"
    );

    let path = format!("{b}/run-items/{item}/iterations/r_ff");
    let r = send(
        &s,
        Method::PATCH,
        &path,
        Some(&tester),
        None,
        Some(json!({"status": "passed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &s,
        Method::PATCH,
        &path,
        Some(&tester),
        Some(&etag),
        Some(json!({"status": "passed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    let m = format!("\"r{rev}\"");
    let r = send(
        &s,
        Method::PATCH,
        &path,
        Some(&tester),
        Some(&m),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    let r = send(
        &s,
        Method::PATCH,
        &path,
        Some(&tester),
        Some(&m),
        Some(json!({"status": "passed", "untestedStepsTo": "skipped"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["iterations"][0]["status"], "passed");
    assert_eq!(r.body["status"], "in_progress");
    let m = r.etag.clone().unwrap();
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/run-items/{item}"),
        Some(&tester),
        Some(&m),
        Some(json!({"status": "failed"})),
    )
    .await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (StatusCode::CONFLICT, Some("parameterized_item"))
    );
    let r = send(
        &s,
        Method::PATCH,
        &format!("{b}/run-items/{item}/iterations/r_cr"),
        Some(&tester),
        Some(&m),
        Some(json!({"status": "failed", "comment": "card declined"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        r.body["status"], "failed",
        "a failed iteration fails the item"
    );

    let results = s.results("pr", &lid, "").await;
    assert_eq!(results.len(), 2, "{results:?}");
    let audit = audit_has(&s, &["runIteration.update"]).await;
    let a = audit
        .iter()
        .find(|i| i["action"] == "runIteration.update")
        .unwrap();
    assert_eq!(a["detail"]["rowId"], "r_cr", "{a}");

    let ev: Vec<Value> = sqlx::query_scalar(
        "select payload from events where type = 'runIteration.updated' and project = 'pr' order by id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(ev.len() >= 3, "{ev:?}");
    let last = ev.last().unwrap();
    assert_eq!(
        (
            last["itemId"].as_str(),
            last["rowId"].as_str(),
            last["status"].as_str()
        ),
        (Some(item.as_str()), Some("r_cr"), Some("failed"))
    );
    assert_eq!(last["launchId"], lid.as_str());
    assert!(last["revision"].as_i64().is_some());

    // An automated test with the same @Parameters lands on the same parameters hash.
    let auto = s.launch("pr", None).await;
    let alid = auto["id"].as_str().unwrap().to_string();
    let u = s
        .ingest("pr", &alid, allure_zip("firefox", "EUR"), "")
        .await;
    assert_eq!(u["status"], "done", "{u}");
    let manual: String = sqlx::query_scalar(
        "select s.parameters_hash from run_iterations it join series s on s.id = it.series_id \
         where it.row_id = 'r_ff'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let automated: Vec<String> = sqlx::query_scalar(
        "select s.parameters_hash from results r join series s on s.id = r.series_id \
         join launches l on l.id = r.launch_id where l.id = $1::uuid",
    )
    .bind(&alid)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(automated, vec![manual.clone()]);
    let other: String = sqlx::query_scalar(
        "select s.parameters_hash from run_iterations it join series s on s.id = it.series_id \
         where it.row_id = 'r_cr'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_ne!(other, manual);
}
