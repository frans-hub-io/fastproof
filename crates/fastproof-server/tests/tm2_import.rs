//! Import and export of shared steps, parameters and requirements (night 2): NDJSON v2 with a
//! round trip into an empty project, v1 files, shared steps and requirement links from the
//! foreign formats (TestRail, Qase, the Allure TestOps API against a local stub), repeated
//! upserts that reuse what exists, the CSV expansion, the new limits and the closed gate.
//!
//! Parameters are written by `P` and requirements by `R`: the tests write those rows directly
//! and run the parameter assertions only where the case validation accepts parameters.

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn fixture(name: &str) -> Vec<u8> {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fastproof-tm-import/tests/fixtures")
        .join(name);
    std::fs::read(p).unwrap()
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "fastproofboundary7MA4YWxkTrZu0gW";
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

async fn start_import(
    s: &Server,
    key: &str,
    file: &[u8],
    name: &str,
    options: Value,
) -> (StatusCode, Value) {
    let o = options.to_string();
    let (ct, body) = multipart(&[("options", None, o.as_bytes()), ("file", Some(name), file)]);
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
    let st = r.status();
    let t = r.text().await.unwrap();
    (st, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

async fn wait_job(s: &Server, key: &str, id: &str, want: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (st, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        let status = b["status"].as_str().unwrap_or("").to_string();
        if want.contains(&status.as_str()) {
            return b;
        }
        assert!(
            !["failed", "cancelled", "done"].contains(&status.as_str()),
            "job ended {status}: {b}"
        );
        assert!(Instant::now() < deadline, "job {id} stuck: {b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn import(s: &Server, key: &str, file: &[u8], name: &str, options: Value) -> (Value, Value) {
    let (st, j) = start_import(s, key, file, name, options).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(s, key, &id, &["ready"]).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let done = wait_job(s, key, &id, &["done"]).await;
    (ready["report"].clone(), done["report"].clone())
}

async fn export(s: &Server, key: &str, format: &str) -> String {
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/{key}/cases/export?format={format}"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    r.text().await.unwrap()
}

fn records(nd: &str) -> Vec<Value> {
    nd.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

async fn project_id(pool: &PgPool, key: &str) -> Uuid {
    sqlx::query_scalar("select id from projects where key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn post_ok(s: &Server, path: &str, body: Value) -> Value {
    let (st, b) = s.post(path, ADMIN_TOKEN, body).await;
    assert!(st.is_success(), "{path}: {st} {b}");
    b
}

async fn shared(s: &Server, key: &str, name: &str, steps: Value) -> Value {
    post_ok(
        s,
        &format!("/api/v1/projects/{key}/shared-steps"),
        json!({"name": name, "description": "Shared *text*", "steps": steps}),
    )
    .await
}

async fn case(s: &Server, key: &str, name: &str, steps: Value) -> Value {
    post_ok(
        s,
        &format!("/api/v1/projects/{key}/cases"),
        json!({"name": name, "body": {"steps": steps}}),
    )
    .await
}

fn reference(ss: &Value) -> Value {
    json!({"id": "", "shared": {"id": ss["id"], "number": ss["number"]}})
}

/// A requirement row (the tree of `R` is not here yet): `parent` is `(id, path, depth)`.
async fn requirement(
    pool: &PgPool,
    project: Uuid,
    number: i64,
    title: &str,
    kind: &str,
    parent: Option<(Uuid, Vec<Uuid>, i16)>,
    issue_key: Option<&str>,
) -> Uuid {
    let id = Uuid::now_v7();
    let (pid, mut path, depth) = match parent {
        Some((p, path, d)) => (Some(p), path, d + 1),
        None => (None, Vec::new(), 1),
    };
    path.push(id);
    sqlx::query(
        "insert into requirements (id, project_id, number, title, description, type, parent_id, path_ids, depth, \
           tracker_id, issue_key, created_by) values ($1, $2, $3, $4, 'About **it**', $5, $6, $7, $8, \
           case when $9::text is null then null else 'jira' end, $9, 'test')",
    )
    .bind(id)
    .bind(project)
    .bind(number)
    .bind(title)
    .bind(kind)
    .bind(pid)
    .bind(&path)
    .bind(depth)
    .bind(issue_key)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "update projects set requirement_seq = greatest(requirement_seq, $2) where id = $1",
    )
    .bind(project)
    .bind(number)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn link(
    pool: &PgPool,
    project: Uuid,
    req: Uuid,
    case_number: i64,
    origin: &str,
    status: &str,
) {
    sqlx::query(
        "insert into requirement_links (requirement_id, case_id, project_id, origin, status, created_by) \
         select $1, c.id, $2, $4, $5, 'test' from cases c where c.project_id = $2 and c.number = $3",
    )
    .bind(req)
    .bind(project)
    .bind(case_number)
    .bind(origin)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

async fn count(pool: &PgPool, sql: &'static str, project: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Gives case `number` two parameter rows (one switched off) through the API, so P's
/// validation and gate apply as for any write.
async fn set_parameters(s: &Server, key: &str, number: i64) {
    let (st, c) = s
        .get(
            &format!("/api/v1/projects/{key}/cases/{number}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let mut body = c["body"].clone();
    body["parameters"] = json!({"columns": ["card"], "rows": [
        {"id": "r_one", "values": {"card": "visa"}, "enabled": true},
        {"id": "r_two", "values": {"card": "amex"}, "enabled": false}]});
    let r = s
        .req(
            Method::PUT,
            &format!("/api/v1/projects/{key}/cases/{number}"),
            Some(ADMIN_TOKEN),
        )
        .header("if-match", format!("\"r{}\"", c["revision"]))
        .json(&json!({"name": c["name"], "body": body}))
        .send()
        .await
        .unwrap();
    let st = r.status();
    assert_eq!(st, StatusCode::OK, "{}", r.text().await.unwrap());
}

#[tokio::test]
async fn ndjson_v2_round_trips_into_an_empty_project() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("src").await;
    s.project("dst").await;
    let login = shared(
        &s,
        "src",
        "Log in",
        json!([{"action": "Open the page", "expected": "The form"}, {"action": "Sign in", "steps": [{"action": "Type the user"}]}]),
    )
    .await;
    let logout = shared(&s, "src", "Log out", json!([{"action": "Click out"}])).await;
    let a = case(
        &s,
        "src",
        "Buy",
        json!([reference(&login), {"action": "Pay with ${card}", "expected": "Paid"}]),
    )
    .await;
    case(
        &s,
        "src",
        "Refund",
        json!([{"action": "Ask"}, reference(&logout), reference(&login)]),
    )
    .await;
    case(&s, "src", "Plain", json!([{"action": "Look"}])).await;
    let pid = project_id(&db.pool, "src").await;
    sqlx::query(
        "insert into shared_step_keys (project_id, kind, value, shared_step_id) values ($1, 'external', 'login-1', $2)",
    )
    .bind(pid)
    .bind(Uuid::parse_str(login["id"].as_str().unwrap()).unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let epic = requirement(&db.pool, pid, 1, "Checkout", "epic", None, None).await;
    let child = requirement(
        &db.pool,
        pid,
        2,
        "Pay by card",
        "story",
        Some((epic, vec![epic], 1)),
        Some("SHOP-9"),
    )
    .await;
    requirement(&db.pool, pid, 3, "Unlinked", "requirement", None, None).await;
    link(
        &db.pool,
        pid,
        child,
        a["number"].as_i64().unwrap(),
        "ui",
        "active",
    )
    .await;
    link(&db.pool, pid, epic, 2, "ai", "proposed").await;
    set_parameters(&s, "src", 1).await;

    let nd = export(&s, "src", "ndjson").await;
    let rec = records(&nd);
    assert_eq!(rec[0]["format"], "fastproof-cases");
    assert_eq!(rec[0]["version"], 2);
    let types: Vec<&str> = rec[1..]
        .iter()
        .map(|r| r["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "sharedStep",
            "sharedStep",
            "requirement",
            "requirement",
            "requirement",
            "case",
            "case",
            "case"
        ],
        "shared steps, then requirements parents first, then cases"
    );
    assert_eq!(
        rec[1]["keys"],
        json!([{"kind": "external", "value": "login-1"}])
    );
    assert_eq!(
        rec[6]["steps"][0]["shared"],
        json!({"kind": "number", "value": "1"})
    );
    assert_eq!(rec[6]["requirements"][0]["key"], "REQ-2");
    assert_eq!(rec[7]["requirements"][0]["status"], "proposed");
    let nd_text = nd.to_string();
    assert!(
        !nd_text.contains("shared\":{\"id"),
        "no ids of the exporting project leak"
    );

    let (dry, applied) = import(
        &s,
        "dst",
        nd.as_bytes(),
        "export.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly"}),
    )
    .await;
    assert_eq!(dry["create"], 3, "{dry}");
    assert_eq!(applied["create"], 3, "{applied}");
    assert!(
        applied["errors"].as_array().unwrap().is_empty(),
        "{applied}"
    );
    let did = project_id(&db.pool, "dst").await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            did
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirements where project_id = $1",
            did
        )
        .await,
        3
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirement_links where project_id = $1",
            did
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_step_usages where project_id = $1",
            did
        )
        .await,
        3,
        "usages follow the references"
    );
    let (_, usages) = s
        .get("/api/v1/projects/dst/shared-steps/1/usages", ADMIN_TOKEN)
        .await;
    assert_eq!(usages["items"].as_array().unwrap().len(), 2, "{usages}");

    let again = export(&s, "dst", "ndjson").await;
    let (a, b) = (records(&nd), records(&again));
    assert_eq!(a.len(), b.len());
    for (x, y) in a[1..].iter().zip(&b[1..]) {
        assert_eq!(x, y, "the records of the round trip differ");
    }
    assert_eq!(b[0]["version"], 2);

    let csv = export(&s, "dst", "csv").await;
    assert!(
        csv.lines().next().unwrap().contains(",Links,Parameters"),
        "{csv}"
    );
    assert!(
        csv.contains("Open the page") && csv.contains("Click out"),
        "shared steps are expanded into text: {csv}"
    );
    let (j, _) = (
        post_ok(
            &s,
            "/api/v1/projects/dst/cases/exports",
            json!({"format": "zip"}),
        )
        .await,
        (),
    );
    let url = j["downloadUrl"].as_str().unwrap().to_string();
    wait_job(&s, "dst", j["id"].as_str().unwrap(), &["done"]).await;
    let r = s
        .req(Method::GET, &url, Some(ADMIN_TOKEN))
        .send()
        .await
        .unwrap();
    let bytes = r.bytes().await.unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut z.by_name("cases.ndjson").unwrap(), &mut text).unwrap();
    assert_eq!(
        records(&text)[1..],
        b[1..],
        "the export job writes the same records"
    );
    let mut csv = String::new();
    std::io::Read::read_to_string(&mut z.by_name("cases.csv").unwrap(), &mut csv).unwrap();
    assert!(csv.contains("Click out"), "{csv}");

    assert_eq!(rec[6]["parameters"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(a[6], b[6], "parameters round trip");
}

#[tokio::test]
async fn a_partial_export_carries_what_its_cases_use() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("part").await;
    let used = shared(&s, "part", "Used", json!([{"action": "a"}])).await;
    shared(&s, "part", "Unused", json!([{"action": "b"}])).await;
    case(&s, "part", "Uses", json!([reference(&used)])).await;
    case(&s, "part", "Free", json!([{"action": "c"}])).await;
    let pid = project_id(&db.pool, "part").await;
    let root = requirement(&db.pool, pid, 1, "Root", "epic", None, None).await;
    let leaf = requirement(
        &db.pool,
        pid,
        2,
        "Leaf",
        "story",
        Some((root, vec![root], 1)),
        None,
    )
    .await;
    requirement(&db.pool, pid, 3, "Other", "story", None, None).await;
    link(&db.pool, pid, leaf, 1, "ui", "active").await;
    let r = s
        .req(
            Method::GET,
            "/api/v1/projects/part/cases/export?format=ndjson&q=Uses",
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let rec = records(&r.text().await.unwrap());
    let kinds: Vec<(&str, &str)> = rec[1..]
        .iter()
        .map(|r| {
            (
                r["type"].as_str().unwrap(),
                r["name"].as_str().or(r["title"].as_str()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("sharedStep", "Used"),
            ("requirement", "Root"),
            ("requirement", "Leaf"),
            ("case", "Uses")
        ],
        "only the referenced shared step and the covered requirement with its ancestor"
    );
}

#[tokio::test]
async fn a_repeated_upsert_reuses_shared_steps_and_requirements() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("up").await;
    s.project("up2").await;
    let ss = shared(&s, "up", "Log in", json!([{"action": "Open"}])).await;
    case(&s, "up", "Buy", json!([reference(&ss)])).await;
    let pid = project_id(&db.pool, "up").await;
    let epic = requirement(&db.pool, pid, 1, "Epic", "epic", None, None).await;
    requirement(
        &db.pool,
        pid,
        2,
        "Keyed",
        "story",
        Some((epic, vec![epic], 1)),
        Some("SHOP-5"),
    )
    .await;
    let nd = export(&s, "up", "ndjson").await;
    let opts = json!({"format": "fastproof-ndjson", "mode": "upsert"});
    import(&s, "up2", nd.as_bytes(), "a.ndjson", opts.clone()).await;
    let id2 = project_id(&db.pool, "up2").await;
    let shared_q = "select count(*) from shared_steps where project_id = $1";
    let req_q = "select count(*) from requirements where project_id = $1";
    assert_eq!(count(&db.pool, shared_q, id2).await, 1);
    assert_eq!(count(&db.pool, req_q, id2).await, 2);
    let edited = nd
        .replace("\"Open\"", "\"Open the page\"")
        .replace("Keyed", "Keyed again");
    import(&s, "up2", edited.as_bytes(), "b.ndjson", opts).await;
    assert_eq!(
        count(&db.pool, shared_q, id2).await,
        1,
        "matched by number and name"
    );
    assert_eq!(
        count(&db.pool, req_q, id2).await,
        2,
        "matched by the issue key"
    );
    let (_, v) = s
        .get("/api/v1/projects/up2/shared-steps/1/versions", ADMIN_TOKEN)
        .await;
    assert_eq!(
        v["items"].as_array().unwrap().len(),
        2,
        "upsert changed the content: {v}"
    );
    let title: String = sqlx::query_scalar(
        "select title from requirements where project_id = $1 and issue_key = 'SHOP-5'",
    )
    .bind(id2)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(title, "Keyed again");
    let (_, v) = s
        .get("/api/v1/projects/up2/shared-steps/1/usages", ADMIN_TOKEN)
        .await;
    assert_eq!(
        v["items"].as_array().unwrap().len(),
        2,
        "both copies of the case use it: {v}"
    );
}

#[tokio::test]
async fn v1_files_still_import() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("old").await;
    let (_, applied) = import(
        &s,
        "old",
        &fixture("fastproof.ndjson"),
        "old.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly", "unknownFieldValues": "create"}),
    )
    .await;
    assert_eq!(applied["create"], 2, "{applied}");
    let (st, c) = s.get("/api/v1/projects/old/cases/5", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["name"], "Exported case");
}

#[tokio::test]
async fn testrail_shared_steps_and_references() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tr").await;
    let pid = project_id(&db.pool, "tr").await;
    requirement(&db.pool, pid, 1, "Shop", "epic", None, Some("SHOP-1")).await;
    let file = fixture("testrail-shared.csv");
    let plain: String = String::from_utf8(file.clone())
        .unwrap()
        .lines()
        .map(|l| l.rsplit_once(',').unwrap().0.to_string() + "\n")
        .collect();
    let (_, applied) = import(
        &s,
        "tr",
        plain.as_bytes(),
        "old.csv",
        json!({"format": "testrail-csv", "mode": "upsert"}),
    )
    .await;
    assert_eq!(applied["create"], 2, "{applied}");
    sqlx::query("update cases set tags = array['imported-shared-step'] where project_id = $1")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    let (c, _) = s.get("/api/v1/projects/tr/cases/1", ADMIN_TOKEN).await;
    assert_eq!(c, StatusCode::OK);
    let (dry, applied) = import(
        &s,
        "tr",
        &file,
        "cases.csv",
        json!({"format": "testrail-csv", "mode": "upsert"}),
    )
    .await;
    assert_eq!(dry["update"], 2, "inlined steps become references: {dry}");
    assert_eq!(applied["update"], 2, "{applied}");
    assert_eq!(applied["create"], 0);
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        1
    );
    let (_, ss) = s
        .get("/api/v1/projects/tr/shared-steps/1", ADMIN_TOKEN)
        .await;
    assert_eq!(ss["name"], "Shared step 5");
    assert_eq!(ss["origin"], "import");
    assert_eq!(ss["steps"].as_array().unwrap().len(), 2);
    assert_eq!(ss["steps"][0]["expected"], "Logged in");
    let (_, buy) = s.get("/api/v1/projects/tr/cases/1", ADMIN_TOKEN).await;
    let steps = buy["body"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{buy}");
    assert!(steps[0]["shared"].is_null() && steps[0]["action"] == "Open the shop");
    assert_eq!(steps[1]["shared"]["number"], 1);
    assert_eq!(steps[2]["action"], "Pay");
    assert_eq!(
        buy["tags"],
        json!([]),
        "the inlined tag goes with the inlined steps"
    );
    let (_, refund) = s.get("/api/v1/projects/tr/cases/2", ADMIN_TOKEN).await;
    assert_eq!(refund["body"]["steps"][0]["shared"]["number"], 1);
    let key: Option<String> = sqlx::query_scalar(
        "select value from shared_step_keys where project_id = $1 and kind = 'testrail'",
    )
    .bind(pid)
    .fetch_optional(&db.pool)
    .await
    .unwrap();
    assert_eq!(key.as_deref(), Some("5"));
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirement_links where project_id = $1",
            pid
        )
        .await,
        2,
        "SHOP-1 exists and both cases name it; SHOP-2 is no requirement"
    );
    let (_, again) = import(
        &s,
        "tr",
        &file,
        "cases.csv",
        json!({"format": "testrail-csv", "mode": "upsert"}),
    )
    .await;
    assert_eq!(again["update"], 0, "{again}");
    assert_eq!(again["skip"], 2);
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        1
    );
    let (_, ss) = s
        .get("/api/v1/projects/tr/shared-steps/1", ADMIN_TOKEN)
        .await;
    assert_eq!(ss["version"], 1, "an unchanged upsert writes no version");
    let csv = export(&s, "tr", "csv").await;
    assert!(
        csv.contains("Enter the password"),
        "CSV expands the reference: {csv}"
    );
}

const QASE: &str = r#"{"code":"SHOP","suites":[{"id":1,"title":"Cart","cases":[
  {"id":7,"title":"Pay","steps":[
    {"action":"Open","expected_result":"ok"},
    {"action":"Log in","shared_step_hash":"h1","shared_step_nested_steps":[{"action":"Type user"},{"action":"Type pass"}]},
    {"action":"Pay as {{User role}}","expected_result":"Paid"}],
   "params":{"single":{"User role":["admin","guest"]},"group":[{"browser":"chrome","os":"linux"},{"browser":"firefox","os":"mac"}]}},
  {"id":8,"title":"Other","steps":[{"action":"Log in","shared_step_hash":"h1"}]}]}]}"#;

#[tokio::test]
async fn qase_shared_steps_by_hash_and_parameter_rows() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("qa").await;
    let (dry, applied) = import(
        &s,
        "qa",
        QASE.as_bytes(),
        "q.json",
        json!({"format": "qase-json", "mode": "upsert"}),
    )
    .await;
    let pid = project_id(&db.pool, "qa").await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        1
    );
    let number: i64 =
        sqlx::query_scalar("select number from cases where project_id = $1 and name = 'Other'")
            .bind(pid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let (_, other) = s
        .get(&format!("/api/v1/projects/qa/cases/{number}"), ADMIN_TOKEN)
        .await;
    assert_eq!(other["body"]["steps"][0]["shared"]["number"], 1, "{other}");
    assert_eq!(applied["create"], 2, "{dry} {applied}");
    let (_, pay) = s.get("/api/v1/projects/qa/cases/1", ADMIN_TOKEN).await;
    assert_eq!(pay["name"], "Pay");
    let p = &pay["body"]["parameters"];
    assert_eq!(p["columns"], json!(["User_role", "browser", "os"]));
    assert_eq!(p["rows"].as_array().unwrap().len(), 4);
    assert_eq!(pay["body"]["steps"][2]["action"], "Pay as ${User_role}");
    let (_, again) = import(
        &s,
        "qa",
        QASE.as_bytes(),
        "q.json",
        json!({"format": "qase-json", "mode": "upsert"}),
    )
    .await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        1
    );
    assert_eq!(again["update"], 0, "{again}");
}

#[tokio::test]
async fn a_closed_gate_stops_a_v2_import_with_new_records() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let closed = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "closed")]).await;
    closed.project("gt").await;
    let file = fixture("fastproof-v2.ndjson");
    let (st, j) = start_import(
        &closed,
        "gt",
        &file,
        "v2.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly"}),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let failed = loop {
        let (_, b) = closed
            .get(&format!("/api/v1/projects/gt/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        if b["status"] == "failed" {
            break b;
        }
        assert_ne!(b["status"], "ready", "the dry run must not pass: {b}");
        assert!(Instant::now() < deadline, "{b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        failed["report"]["errors"][0]["code"], "upgrade_in_progress",
        "{failed}"
    );
    let pid = project_id(&db.pool, "gt").await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirements where project_id = $1",
            pid
        )
        .await,
        0
    );
    let cases_only = "{\"format\":\"fastproof-cases\",\"version\":2}\n{\"type\":\"case\",\"name\":\"Only a case\"}\n";
    let (_, applied) = import(
        &closed,
        "gt",
        cases_only.as_bytes(),
        "cases.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly"}),
    )
    .await;
    assert_eq!(
        applied["create"], 1,
        "a v2 file of cases alone writes nothing new: {applied}"
    );
}

#[tokio::test]
async fn the_new_record_types_have_their_own_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_MAX_SHARED_STEPS", "1"),
            ("FASTPROOF_TM_MAX_REQUIREMENTS", "1"),
        ],
    )
    .await;
    s.project("lim").await;
    let two_shared = "{\"format\":\"fastproof-cases\",\"version\":2}\n\
        {\"type\":\"sharedStep\",\"name\":\"a\",\"steps\":[{\"action\":\"x\"}]}\n\
        {\"type\":\"sharedStep\",\"name\":\"b\",\"steps\":[{\"action\":\"x\"}]}\n";
    let (st, j) = start_import(
        &s,
        "lim",
        two_shared.as_bytes(),
        "a.ndjson",
        json!({"format": "fastproof-ndjson"}),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, b) = s
            .get(
                &format!("/api/v1/projects/lim/tm-jobs/{}", j["id"].as_str().unwrap()),
                ADMIN_TOKEN,
            )
            .await;
        if b["status"] == "failed" {
            assert_eq!(b["report"]["errors"][0]["code"], "limit_reached", "{b}");
            assert!(
                b["report"]["errors"][0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("maxSharedSteps"),
                "{b}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "{b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let plain = Server::start(&db).await;
    plain.project("bad").await;
    let bad = "{\"format\":\"fastproof-cases\",\"version\":2}\n\
        {\"type\":\"sharedStep\",\"name\":\"\",\"steps\":[{\"action\":\"x\"}]}\n\
        {\"type\":\"sharedStep\",\"name\":\"deep\",\"steps\":[{\"action\":\"x\",\"steps\":[{\"action\":\"y\",\"steps\":[{\"action\":\"z\"}]}]}]}\n\
        {\"type\":\"requirement\",\"title\":\"<script>\\u0007\",\"kind\":\"Bad Type\"}\n";
    let (dry, _) = {
        let (st, j) = start_import(
            &plain,
            "bad",
            bad.as_bytes(),
            "b.ndjson",
            json!({"format": "fastproof-ndjson"}),
        )
        .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{j}");
        let r = wait_job(&plain, "bad", j["id"].as_str().unwrap(), &["ready"]).await;
        (r["report"].clone(), ())
    };
    let rows: Vec<i64> = dry["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["row"].as_i64().unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![2, 3, 4],
        "every bad record is a row error: {dry}"
    );
}

#[tokio::test]
async fn links_to_a_full_requirement_are_left_out_with_a_warning() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_MAX_REQUIREMENT_CASES", "1")]).await;
    s.project("full").await;
    let file = "{\"format\":\"fastproof-cases\",\"version\":2}\n\
        {\"type\":\"requirement\",\"number\":1,\"title\":\"R\"}\n\
        {\"type\":\"case\",\"name\":\"A\",\"requirements\":[{\"key\":\"REQ-1\"},{\"key\":\"REQ-404\"}]}\n\
        {\"type\":\"case\",\"name\":\"B\",\"requirements\":[{\"key\":\"REQ-1\"}]}\n";
    let (_, applied) = import(
        &s,
        "full",
        file.as_bytes(),
        "f.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly"}),
    )
    .await;
    assert_eq!(applied["create"], 2, "{applied}");
    let pid = project_id(&db.pool, "full").await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirement_links where project_id = $1",
            pid
        )
        .await,
        1
    );
    assert_eq!(applied["warnings"][0]["code"], "limit_reached", "{applied}");
}

#[tokio::test]
async fn five_thousand_cases_with_references_and_links_import_in_batches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("big").await;
    let mut file = String::from("{\"format\":\"fastproof-cases\",\"version\":2}\n");
    for i in 1..=100 {
        file += &format!(
            "{{\"type\":\"sharedStep\",\"number\":{i},\"keys\":[{{\"kind\":\"external\",\"value\":\"s{i}\"}}],\"name\":\"Shared {i}\",\"steps\":[{{\"action\":\"a{i}\"}}]}}\n"
        );
    }
    for i in 1..=50 {
        file += &format!("{{\"type\":\"requirement\",\"number\":{i},\"title\":\"Req {i}\"}}\n");
    }
    for i in 1..=5000 {
        file += &format!(
            "{{\"type\":\"case\",\"number\":{i},\"name\":\"Case {i}\",\"steps\":[{{\"shared\":{{\"kind\":\"number\",\"value\":\"{}\"}}}},{{\"action\":\"own {i}\"}},{{\"shared\":{{\"kind\":\"number\",\"value\":\"{}\"}}}}],\"requirements\":[{{\"key\":\"REQ-{}\"}},{{\"key\":\"REQ-{}\"}}]}}\n",
            i % 100 + 1,
            (i * 7) % 100 + 1,
            i % 50 + 1,
            (i * 3) % 50 + 1
        );
    }
    let t = Instant::now();
    let (dry, applied) = import(
        &s,
        "big",
        file.as_bytes(),
        "big.ndjson",
        json!({"format": "fastproof-ndjson", "mode": "createOnly"}),
    )
    .await;
    eprintln!(
        "5000 cases, 100 shared steps, 50 requirements: dry run and apply took {:?}",
        t.elapsed()
    );
    assert_eq!(dry["create"], 5000, "{dry}");
    assert_eq!(applied["create"], 5000, "{applied}");
    assert!(
        applied["errors"].as_array().unwrap().is_empty(),
        "{applied}"
    );
    let pid = project_id(&db.pool, "big").await;
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_step_usages where project_id = $1",
            pid
        )
        .await,
        9_900
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirement_links where project_id = $1",
            pid
        )
        .await,
        9_800
    );
}

mod stub {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use serde_json::{Value, json};

    pub const TOKEN: &str = "testops-api-token-SECRET-1234";
    pub const JWT: &str = "jwt-value-SECRET-5678";

    #[derive(Clone, Default)]
    pub struct Seen(pub Arc<Mutex<Vec<String>>>);

    fn authed(h: &HeaderMap) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {JWT}"))
    }

    async fn token() -> Response {
        axum::Json(json!({"access_token": JWT})).into_response()
    }

    async fn search(h: HeaderMap) -> Response {
        if !authed(&h) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        axum::Json(json!({"content": [{"id": 601}, {"id": 602}], "last": true})).into_response()
    }

    async fn case(Path(id): Path<i64>) -> Response {
        axum::Json(json!({"id": id, "name": format!("Case {id}"), "automated": false}))
            .into_response()
    }

    async fn step(State(seen): State<Seen>, Path(id): Path<i64>) -> Response {
        seen.0.lock().unwrap().push(format!("step {id}"));
        let v = if id == 601 {
            json!({"root": {"children": [1, 4]}, "scenarioSteps": {
                "1": {"body": "Log in", "sharedStepId": 31, "children": [2, 3]},
                "2": {"body": "Type user"}, "3": {"body": "Type pass"}, "4": {"body": "Pay"}}})
        } else {
            json!({"root": {"children": [5]}, "scenarioSteps": {
                "5": {"body": "Log in", "sharedStepId": 31, "children": [6]}, "6": {"body": "Type user"}}})
        };
        axum::Json(v).into_response()
    }

    async fn shared(State(seen): State<Seen>, Path(id): Path<i64>) -> Response {
        seen.0.lock().unwrap().push(format!("shared {id}"));
        if id != 31 {
            return StatusCode::NOT_FOUND.into_response();
        }
        axum::Json(json!({"id": 31, "name": "Log in", "description": "<p>Common</p>"}))
            .into_response()
    }

    async fn shared_step(Path(id): Path<i64>) -> Response {
        let _ = id;
        axum::Json(json!({"root": {"children": [1, 2]}, "scenarioSteps": {
            "1": {"body": "Type user"}, "2": {"body": "Type pass"}}}))
        .into_response()
    }

    async fn parameter(Path(id): Path<i64>) -> Response {
        if id == 601 {
            return axum::Json(json!([{"name": "Browser", "values": ["chrome", "firefox"]},
                {"name": "OS", "values": [{"name": "linux"}, {"name": "mac"}]}]))
            .into_response();
        }
        StatusCode::NOT_FOUND.into_response()
    }

    async fn issues() -> Response {
        axum::Json(json!([{"name": "SHOP-1", "url": "https://tracker.example.com/SHOP-1"}]))
            .into_response()
    }

    async fn empty() -> Response {
        axum::Json(json!([])).into_response()
    }

    async fn none() -> Response {
        axum::Json(json!({"content": []})).into_response()
    }

    pub async fn start() -> (String, Seen) {
        let seen = Seen::default();
        let app = Router::new()
            .route("/api/uaa/oauth/token", post(token))
            .route("/api/rs/testcase/__search", get(search))
            .route("/api/rs/testcase/{id}", get(case))
            .route("/api/rs/testcase/{id}/step", get(step))
            .route("/api/rs/testcase/{id}/parameter", get(parameter))
            .route("/api/rs/testcase/{id}/issue", get(issues))
            .route("/api/rs/testcase/{id}/link", get(empty))
            .route("/api/rs/testcase/{id}/cfv", get(empty))
            .route("/api/rs/testcase/attachment", get(none))
            .route("/api/rs/sharedstep/{id}", get(shared))
            .route("/api/rs/sharedstep/{id}/step", get(shared_step))
            .with_state(seen.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(l, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    pub fn log(seen: &Seen) -> Vec<String> {
        seen.0.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    pub fn unused(_: Value) {}
}

#[tokio::test]
async fn testops_shared_steps_parameters_and_issue_links() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (base, seen) = stub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("secrets.key");
    std::fs::write(&key, format!("{KEY_HEX}\n")).unwrap();
    let port = base.rsplit(':').next().unwrap().to_string();
    let allow = format!("127.0.0.1:{port}");
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_EGRESS_ALLOW", &allow),
            ("FASTPROOF_SECRETS_KEY_FILE", key.to_str().unwrap()),
            ("FASTPROOF_TESTOPS_ALLOW_HTTP", "true"),
        ],
    )
    .await;
    s.project("to").await;
    let pid = project_id(&db.pool, "to").await;
    requirement(&db.pool, pid, 1, "Shop", "epic", None, Some("SHOP-1")).await;
    let (st, j) = s
        .post(
            "/api/v1/projects/to/imports",
            ADMIN_TOKEN,
            json!({"source": "allure-testops", "baseUrl": base, "token": stub::TOKEN,
            "remoteProjectId": 7, "mode": "upsert"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    wait_job(&s, "to", &id, &["ready"]).await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/to/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let done = wait_job(&s, "to", &id, &["done"]).await;
    assert_eq!(done["report"]["create"], 2, "{done}");
    assert_eq!(
        stub::log(&seen)
            .iter()
            .filter(|l| *l == "shared 31")
            .count(),
        1,
        "a shared step is fetched once: {:?}",
        stub::log(&seen)
    );
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from shared_steps where project_id = $1",
            pid
        )
        .await,
        1
    );
    let (_, ss) = s
        .get("/api/v1/projects/to/shared-steps/1", ADMIN_TOKEN)
        .await;
    assert_eq!(ss["name"], "Log in");
    assert_eq!(ss["steps"].as_array().unwrap().len(), 2);
    let (_, c) = s.get("/api/v1/projects/to/cases/602", ADMIN_TOKEN).await;
    assert_eq!(c["body"]["steps"].as_array().unwrap().len(), 1, "{c}");
    assert_eq!(c["body"]["steps"][0]["shared"]["number"], 1);
    assert_eq!(c["tags"], json!([]));
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirement_links where project_id = $1",
            pid
        )
        .await,
        2,
        "SHOP-1 is a requirement"
    );
    let (_, c) = s.get("/api/v1/projects/to/cases/601", ADMIN_TOKEN).await;
    assert_eq!(c["body"]["parameters"]["rows"].as_array().unwrap().len(), 4);
    assert_eq!(c["body"]["steps"][0]["shared"]["number"], 1);
    assert_eq!(c["body"]["steps"][1]["action"], "Pay");
}

/// `REQ-<n>` in a file names the requirement with that number in the file (I's L9): never a
/// requirement of the project by its number, and never shadowed by the number a record got in
/// the project when the file's numbers are not kept. Records are created through R's functions
/// (R's numbering, depth and event), and the importing project already has requirements.
#[tokio::test]
async fn requirement_keys_name_the_numbers_of_the_file() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rk").await;
    for t in ["Old one", "Old two"] {
        post_ok(&s, "/api/v1/projects/rk/requirements", json!({"title": t})).await;
    }
    // In the file A is REQ-4 and B REQ-7; here they become REQ-3 and REQ-4.
    let file = "{\"format\":\"fastproof-cases\",\"version\":2}\n\
        {\"type\":\"requirement\",\"number\":4,\"title\":\"A\",\"kind\":\"epic\"}\n\
        {\"type\":\"requirement\",\"number\":7,\"title\":\"B\",\"kind\":\"story\",\"parent\":4,\"externalState\":\"Open\"}\n\
        {\"type\":\"case\",\"name\":\"Uses A\",\"steps\":[{\"action\":\"a\"}],\"requirements\":[{\"key\":\"REQ-4\"}]}\n\
        {\"type\":\"case\",\"name\":\"Uses one\",\"steps\":[{\"action\":\"b\"}],\"requirements\":[{\"key\":\"REQ-1\"}]}\n";
    let (_, done) = import(
        &s,
        "rk",
        file.as_bytes(),
        "r.ndjson",
        json!({"format": "fastproof-ndjson"}),
    )
    .await;
    assert_eq!(done["create"], 2, "{done}");
    let (_, a) = s
        .get("/api/v1/projects/rk/requirements/3", ADMIN_TOKEN)
        .await;
    assert_eq!(a["title"], "A", "{a}");
    assert_eq!(a["type"], "epic");
    assert_eq!(a["origin"], "import");
    assert_eq!(a["depth"], 1);
    let (_, b) = s
        .get("/api/v1/projects/rk/requirements/4", ADMIN_TOKEN)
        .await;
    assert_eq!(b["title"], "B", "{b}");
    assert_eq!(b["depth"], 2);
    assert_eq!(b["path"].as_array().unwrap().len(), 2);
    assert_eq!(b["parentId"], a["id"]);
    let (_, cases) = s
        .get("/api/v1/projects/rk/requirements/3/cases", ADMIN_TOKEN)
        .await;
    let names: Vec<&str> = cases["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, vec!["Uses A"], "REQ-4 of the file is A: {cases}");
    for n in [1, 2, 4] {
        let (_, cs) = s
            .get(
                &format!("/api/v1/projects/rk/requirements/{n}/cases"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(
            cs["items"].as_array().unwrap().len(),
            0,
            "REQ-{n} of the project is not named by the file: {cs}"
        );
    }
    assert_eq!(
        count(
            &db.pool,
            "select count(*) from requirements where project_id = $1 and external_state = 'Open'",
            project_id(&db.pool, "rk").await
        )
        .await,
        1
    );
}
