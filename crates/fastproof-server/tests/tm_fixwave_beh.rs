//! Behaviour fixes of test management after the stage 2a reviews, over HTTP: `PUT` keeps the
//! folder; a partial `caseSync` merges; imports set required-field drafts, a status option and
//! fields for unknown columns; manual work does not stretch the launch duration; run items keep
//! the executed name; parent steps follow a blocked child; the defect rule checks the executor;
//! `caseNumbers` skip automated cases; Jira links need project and issue type ids; `/people`;
//! launches of a plan; plan folders include subfolders; reruns keep earlier executions.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

async fn send(
    s: &Server,
    m: Method,
    path: &str,
    token: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut rb = s.req(m, path, Some(token));
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

fn step(action: &str, children: Value) -> Value {
    json!({"action": action, "expected": format!("{action} works"), "steps": children})
}

async fn create_case(s: &Server, key: &str, body: Value) -> i64 {
    let (st, b) = s
        .post(&format!("/api/v1/projects/{key}/cases"), ADMIN_TOKEN, body)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["number"].as_i64().unwrap()
}

async fn get_case(s: &Server, key: &str, n: i64) -> (Value, String) {
    let (st, b, etag) = send(
        s,
        Method::GET,
        &format!("/api/v1/projects/{key}/cases/{n}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    (b, etag.unwrap())
}

async fn folder(s: &Server, key: &str, name: &str, parent: Option<&str>) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/case-folders"),
            ADMIN_TOKEN,
            json!({"name": name, "parentId": parent}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["id"].as_str().unwrap().to_string()
}

async fn plan(s: &Server, key: &str, body: Value) -> String {
    let mut body = body;
    body["kind"] = json!("managed");
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/test-plans"),
            ADMIN_TOKEN,
            body,
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["id"].as_str().unwrap().to_string()
}

async fn start(s: &Server, key: &str, token: &str, plan: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/test-plans/{plan}/launches"),
            token,
            json!({"name": "regression", "assign": "none", "includeAutomated": false}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    b["launch"]["id"].as_str().unwrap().to_string()
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

async fn item(s: &Server, key: &str, id: &str) -> (Value, String) {
    let (st, b, etag) = send(
        s,
        Method::GET,
        &format!("/api/v1/projects/{key}/run-items/{id}"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    (b, etag.unwrap())
}

async fn patch_item(
    s: &Server,
    key: &str,
    token: &str,
    id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (_, etag) = item(s, key, id).await;
    let (st, b, _) = send(
        s,
        Method::PATCH,
        &format!("/api/v1/projects/{key}/run-items/{id}"),
        token,
        Some(&etag),
        Some(body),
    )
    .await;
    (st, b)
}

async fn put_step(s: &Server, key: &str, token: &str, id: &str, path: &str, body: Value) -> Value {
    let (st, b) = s
        .call(
            Method::PUT,
            &format!("/api/v1/projects/{key}/run-items/{id}/steps/{path}"),
            Some(token),
            Some(body),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

async fn me(s: &Server, token: &str) -> String {
    let (st, b) = s.get("/api/v1/me", token).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["name"].as_str().unwrap().to_string()
}

async fn audit_has(s: &Server, action: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (st, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::OK, "{a}");
        if let Some(i) = a["items"]
            .as_array()
            .and_then(|v| v.iter().find(|i| i["action"] == action))
        {
            return i.clone();
        }
        assert!(Instant::now() < deadline, "audit lacks {action}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn put_settings(s: &Server, key: &str, body: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        &format!("/api/v1/projects/{key}/settings"),
        Some(ADMIN_TOKEN),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn put_without_folder_id_keeps_the_folder() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("fk").await;
    let orders = folder(&s, "fk", "Orders", None).await;
    let n = create_case(
        &s,
        "fk",
        json!({"name": "Pay", "folderId": orders, "body": {"description": "old"}}),
    )
    .await;
    let (c, etag) = get_case(&s, "fk", n).await;
    assert_eq!(c["folderId"], json!(orders), "GET shows folderId: {c}");
    let mut body = c["body"].clone();
    body["description"] = json!("new");
    let (st, b, etag2) = send(
        &s,
        Method::PUT,
        &format!("/api/v1/projects/fk/cases/{n}"),
        ADMIN_TOKEN,
        Some(&etag),
        Some(json!({"name": c["name"], "body": body})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["folderId"], json!(orders), "left out keeps it: {b}");
    assert_eq!(b["folder"]["id"], json!(orders));
    assert_eq!(b["body"]["description"], "new");
    let (st, b, _) = send(
        &s,
        Method::PUT,
        &format!("/api/v1/projects/fk/cases/{n}"),
        ADMIN_TOKEN,
        etag2.as_deref(),
        Some(json!({"name": "Pay", "folderId": null, "body": body})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b["folderId"].is_null(), "null moves to the root: {b}");
    assert!(b["folder"].is_null());
}

#[tokio::test]
async fn partial_case_sync_keeps_other_keys() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cs").await;
    let (st, b) = put_settings(
        &s,
        "cs",
        json!({"caseSync": {"enabled": true, "branches": ["main"], "createCases": false,
            "rootFolder": "From code", "fields": {"name": "case", "tags": "case"}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = put_settings(
        &s,
        "cs",
        json!({"caseSync": {"branches": ["develop", "release/*"], "fields": {"tags": "code"}}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let cs = &b["caseSync"];
    assert_eq!(cs["branches"], json!(["develop", "release/*"]));
    assert_eq!(cs["createCases"], false, "kept: {cs}");
    assert_eq!(cs["rootFolder"], "From code");
    assert_eq!(cs["fields"]["name"], "case", "fields merge by field: {cs}");
    assert_eq!(cs["fields"]["tags"], "code");
    let (st, b) = put_settings(&s, "cs", json!({"caseSync": {"rootFolder": null}})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["caseSync"]["rootFolder"], "Automated", "null = default");
    assert_eq!(b["caseSync"]["createCases"], false);
    let (st, b) = put_settings(&s, "cs", json!({"caseSync": {"rootFolder": " "}})).await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "merged value is validated: {b}"
    );
    let (st, b) = put_settings(&s, "cs", json!({"caseSync": null})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["caseSync"]["createCases"], true, "null resets the area");
}

fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    use std::io::Write;
    let boundary = "fastproofboundaryFixwaveB0";
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

/// Dry run, then apply; returns `(dry report, applied report)`.
async fn import(s: &Server, key: &str, file: &[u8], options: Value) -> (Value, Value) {
    let o = options.to_string();
    let (ct, body) = multipart(&[
        ("options", None, o.as_bytes()),
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
    let st = r.status();
    let j: Value = r.json().await.unwrap();
    assert_eq!(st, StatusCode::ACCEPTED, "{j}");
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(s, key, &id, "ready").await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{key}/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let done = wait_job(s, key, &id, "done").await;
    (ready["report"].clone(), done["report"].clone())
}

const TESTRAIL: &str = "ID,Title,Section,Section Hierarchy,Priority,Type,References,Estimate,Milestone,Automation Type,Component\n\
C1,Login,Login,Accounts > Login,High,Functional,\"DH-1, DH-2\",600s,R1,None,Auth\n\
C2,Logout,Login,Accounts > Login,Low,Regression,,,R1,Ranorex,Auth\n\
C3,Reset,Passwords,Accounts > Passwords,Medium,Functional,DH-3,1m,R2,None,Mail\n";

#[tokio::test]
async fn testrail_import_keeps_unknown_columns_and_drafts_required_gaps() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tri").await;
    let (st, b) = s
        .post(
            "/api/v1/projects/tri/case-fields",
            ADMIN_TOKEN,
            json!({"key": "component_owner", "name": "Component owner", "type": "text", "required": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let (dry, applied) = import(
        &s,
        "tri",
        TESTRAIL.as_bytes(),
        json!({"format": "testrail-csv", "mode": "createOnly", "unknownFieldValues": "create"}),
    )
    .await;
    assert_eq!(dry["create"], 3, "required fields fail no row: {dry}");
    assert_eq!(dry["errors"], json!([]), "{dry}");
    let warned: Vec<&str> = dry["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(warned, ["required_fields_missing"; 3], "{dry}");
    let created: Vec<&str> = dry["createdFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    for col in [
        "Type",
        "References",
        "Estimate",
        "Milestone",
        "Automation type",
    ] {
        assert!(created.contains(&col), "{col} will be created: {dry}");
    }
    assert_eq!(
        dry["unknownFields"],
        json!([]),
        "created fields are not unknown: {dry}"
    );
    assert_eq!(applied["create"], 3, "{applied}");
    assert_eq!(applied["errors"], json!([]), "{applied}");
    let (_, defs) = s.get("/api/v1/projects/tri/case-fields", ADMIN_TOKEN).await;
    let def = |name: &str| {
        defs["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("no field {name}: {defs}"))
            .clone()
    };
    let ty = def("Type");
    assert_eq!(ty["type"], "select", "{ty}");
    let opts: Vec<&str> = ty["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert_eq!(opts, ["Functional", "Regression"]);
    assert_eq!(def("Automation type")["key"], "automation_type");
    let (list_st, list) = s
        .get("/api/v1/projects/tri/cases?limit=10", ADMIN_TOKEN)
        .await;
    assert_eq!(list_st, StatusCode::OK);
    let first = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Login")
        .unwrap()
        .clone();
    assert_eq!(first["status"], "draft", "required gap → draft: {first}");
    assert_eq!(first["fields"]["type"], "Functional", "{first}");
    assert_eq!(first["fields"]["references"], "DH-1, DH-2");
    assert_eq!(first["fields"]["milestone"], "R1");
    assert_eq!(
        first["fields"]["component"], "Auth",
        "Component maps to the built-in field"
    );
    assert_eq!(
        applied["createdFields"].as_array().unwrap().len(),
        5,
        "{applied}"
    );

    s.project("trs").await;
    let (_, applied) = import(
        &s,
        "trs",
        TESTRAIL.as_bytes(),
        json!({"format": "testrail-csv", "mode": "createOnly", "unknownFieldValues": "skip",
            "status": "draft"}),
    )
    .await;
    assert_eq!(applied["create"], 3, "{applied}");
    let (_, list) = s
        .get("/api/v1/projects/trs/cases?limit=10", ADMIN_TOKEN)
        .await;
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["status"] == "draft"),
        "status option: {list}"
    );
    assert!(
        list["items"][0]["fields"]["type"].is_null(),
        "skip creates no fields"
    );
}

fn result(id: &str, name: &str, start: i64, stop: i64) -> Value {
    json!({"id": id, "name": name, "fullName": format!("tests.{name}"), "status": "passed",
        "start": start, "stop": stop, "labels": []})
}

#[tokio::test]
async fn manual_work_does_not_stretch_the_launch_duration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dur").await;
    let n = create_case(
        &s,
        "dur",
        json!({"name": "Manual check", "body": {"steps": [step("look", json!([]))]}}),
    )
    .await;
    let l = s.launch("dur", Some("ci-1")).await;
    let lid = l["id"].as_str().unwrap().to_string();
    let t0 = 1_795_249_075_967i64;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/dur/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([
                result("a", "a", t0, t0 + 1000),
                result("b", "b", t0 + 9000, t0 + 10_000)
            ]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while s.launch_get("dur", &lid).await["counts"]["total"] != 2 {
        assert!(Instant::now() < deadline, "results never counted");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/dur/launches/{lid}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let (st, added) = s
        .post(
            &format!("/api/v1/projects/dur/launches/{lid}/run-items"),
            ADMIN_TOKEN,
            json!({"caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{added}");
    let iid = items(&s, "dur", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = patch_item(&s, "dur", ADMIN_TOKEN, &iid, json!({"status": "passed"})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let l = s.launch_get("dur", &lid).await;
    assert_eq!(
        l["counts"]["total"], 3,
        "the manual result is in the launch: {l}"
    );
    assert_eq!(
        l["durationMs"], 10_000,
        "the duration covers the automated results only: {l}"
    );
    assert!(l["manual"]["startedAt"].is_string(), "{l}");
    assert!(l["manual"]["finishedAt"].is_string(), "{l}");
    let (st, list) = s.get("/api/v1/projects/dur/launches", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let row = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == json!(lid))
        .unwrap()
        .clone();
    assert_eq!(
        row["manual"]["passed"], 1,
        "the launch list carries manual: {row}"
    );
    assert_eq!(row["durationMs"], 10_000);
}

#[tokio::test]
async fn run_items_keep_the_executed_name() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("nm").await;
    let n = create_case(
        &s,
        "nm",
        json!({"name": "Login with valid password", "body": {"steps": [step("type", json!([]))]}}),
    )
    .await;
    let p = plan(&s, "nm", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "nm", ADMIN_TOKEN, &p).await;
    let (_, etag) = get_case(&s, "nm", n).await;
    let (st, b, _) = send(
        &s,
        Method::PATCH,
        &format!("/api/v1/projects/nm/cases/{n}"),
        ADMIN_TOKEN,
        Some(&etag),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (c, etag) = get_case(&s, "nm", n).await;
    let (st, b, _) = send(
        &s,
        Method::PUT,
        &format!("/api/v1/projects/nm/cases/{n}"),
        ADMIN_TOKEN,
        Some(&etag),
        Some(json!({"name": "Login with a valid password", "body": c["body"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let list = items(&s, "nm", &lid).await;
    assert_eq!(list[0]["name"], "Login with valid password", "{list:?}");
    let iid = list[0]["id"].as_str().unwrap().to_string();
    let (got, _) = item(&s, "nm", &iid).await;
    assert_eq!(got["name"], "Login with valid password");
    assert_eq!(got["caseVersion"], 1);
    let (st, b) = patch_item(
        &s,
        "nm",
        ADMIN_TOKEN,
        &iid,
        json!({"status": "passed", "assignee": me(&s, ADMIN_TOKEN).await}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, report) = s
        .get(
            &format!("/api/v1/projects/nm/launches/{lid}/manual-report?format=json"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(report["items"][0]["name"], "Login with valid password");
}

#[tokio::test]
async fn parent_steps_follow_a_blocked_child_and_untested_steps_suggest_in_progress() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ps").await;
    let n = create_case(
        &s,
        "ps",
        json!({"name": "Nested", "body": {"steps": [
            step("parent", json!([step("child", json!([]))])), step("second", json!([]))]}}),
    )
    .await;
    let p = plan(&s, "ps", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "ps", ADMIN_TOKEN, &p).await;
    let iid = items(&s, "ps", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ack = put_step(
        &s,
        "ps",
        ADMIN_TOKEN,
        &iid,
        "2",
        json!({"status": "passed"}),
    )
    .await;
    assert_eq!(ack["suggestedStatus"], "in_progress", "1.1 untested: {ack}");
    put_step(
        &s,
        "ps",
        ADMIN_TOKEN,
        &iid,
        "1.1",
        json!({"status": "blocked"}),
    )
    .await;
    put_step(
        &s,
        "ps",
        ADMIN_TOKEN,
        &iid,
        "2",
        json!({"status": "failed"}),
    )
    .await;
    let (st, b) = patch_item(
        &s,
        "ps",
        ADMIN_TOKEN,
        &iid,
        json!({"status": "failed", "untestedStepsTo": "skipped"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let recorded: Vec<(String, String)> = b["stepResults"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        !recorded.iter().any(|(p, st)| p == "1" && st == "skipped"),
        "the parent is not forced to skipped: {recorded:?}"
    );
    let (_, report) = s
        .get(
            &format!("/api/v1/projects/ps/launches/{lid}/manual-report?format=json"),
            ADMIN_TOKEN,
        )
        .await;
    let steps: Vec<(&str, &str)> = report["items"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["path"].as_str().unwrap(), s["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        steps,
        [("1", "blocked"), ("1.1", "blocked"), ("2", "failed")],
        "{report}"
    );
}

#[tokio::test]
async fn defects_follow_the_executor_and_name_the_permission() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("df").await;
    let t1 = s.token("df-t1", "tester", &["df"]).await;
    let t2 = s.token("df-t2", "tester", &["df"]).await;
    let n = create_case(
        &s,
        "df",
        json!({"name": "Pay", "body": {"steps": [step("pay", json!([]))]}}),
    )
    .await;
    let p = plan(&s, "df", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "df", ADMIN_TOKEN, &p).await;
    let iid = items(&s, "df", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = patch_item(&s, "df", &t1, &iid, json!({"status": "failed"})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let defect = |token: String| {
        let s = &s;
        let iid = iid.clone();
        async move {
            s.post(
                &format!("/api/v1/projects/df/run-items/{iid}/defect"),
                &token,
                json!({"comment": "declined"}),
            )
            .await
        }
    };
    let (st, b) = defect(t2.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(b["details"]["required"], "results:triage", "{b}");
    let t2_name = me(&s, &t2).await;
    let (st, b) = patch_item(&s, "df", &t2, &iid, json!({"assignee": t2_name})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = defect(t2.clone()).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "taking the item over does not make t2 its executor: {b}"
    );
    let (st, b) = defect(t1.clone()).await;
    assert_eq!(st, StatusCode::OK, "the executor may: {b}");
}

#[tokio::test]
async fn case_numbers_skip_automated_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("an").await;
    let manual = create_case(&s, "an", json!({"name": "By hand", "body": {}})).await;
    let auto = create_case(&s, "an", json!({"name": "By code", "body": {}})).await;
    sqlx::query(
        "update cases set automated = true where number = $1 and project_id = (select id from projects where key = 'an')",
    )
    .bind(auto)
    .execute(&db.pool)
    .await
    .unwrap();
    let l = s.launch("an", Some("nightly")).await;
    let lid = l["id"].as_str().unwrap();
    let path = format!("/api/v1/projects/an/launches/{lid}/run-items");
    let (st, b) = s
        .post(&path, ADMIN_TOKEN, json!({"caseNumbers": [manual, auto]}))
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        (b["added"].as_i64(), b["skipped"].as_i64()),
        (Some(1), Some(1)),
        "{b}"
    );
    assert_eq!(
        b["skips"],
        json!([{"caseNumber": auto, "code": "automated"}]),
        "{b}"
    );
    let (st, b) = s
        .post(&path, ADMIN_TOKEN, json!({"caseNumbers": [manual]}))
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["skips"],
        json!([{"caseNumber": manual, "code": "in_launch"}])
    );
    assert_eq!(items(&s, "an", lid).await.len(), 1);
}

#[tokio::test]
async fn jira_links_need_project_and_issue_type_ids() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ji").await;
    let n = create_case(
        &s,
        "ji",
        json!({"name": "Pay", "body": {"steps": [step("pay", json!([]))]}}),
    )
    .await;
    let p = plan(&s, "ji", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "ji", ADMIN_TOKEN, &p).await;
    let iid = items(&s, "ji", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    put_step(
        &s,
        "ji",
        ADMIN_TOKEN,
        &iid,
        "1",
        json!({"status": "failed", "actual": "500"}),
    )
    .await;
    let tracker = json!({"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}",
        "keyRegex": "[A-Z]+-[0-9]+"});
    let (st, b) = put_settings(&s, "ji", json!({"issueTrackers": [tracker]})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let url = format!("/api/v1/projects/ji/run-items/{iid}/new-issue-url");
    let (st, b) = s.get(&url, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["url"],
        "https://jira.example.com/secure/CreateIssue!default.jspa"
    );
    assert_eq!(b["prefilled"], false);
    assert!(
        b["prefill"]["title"].as_str().unwrap().starts_with("TC-"),
        "{b}"
    );
    assert!(
        b["prefill"]["description"]
            .as_str()
            .unwrap()
            .contains("Actual: 500"),
        "{b}"
    );
    let mut with_ids = tracker.clone();
    with_ids["projectId"] = json!("10000");
    with_ids["issueTypeId"] = json!("10004");
    let (st, b) = put_settings(&s, "ji", json!({"issueTrackers": [with_ids]})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s.get(&url, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(
        b["url"].as_str().unwrap().starts_with(
            "https://jira.example.com/secure/CreateIssueDetails!init.jspa?pid=10000&issuetype=10004&summary=TC-"
        ),
        "{b}"
    );
    assert_eq!(b["prefilled"], true);
    let mut bad = tracker.clone();
    bad["projectId"] = json!("WEB");
    let (st, b) = put_settings(&s, "ji", json!({"issueTrackers": [bad]})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let mut gh = tracker;
    gh["type"] = json!("github");
    gh["urlTemplate"] = json!("https://github.com/org/app/issues/{key}");
    gh["keyRegex"] = json!("#?[0-9]+");
    let (st, _) = put_settings(&s, "ji", json!({"issueTrackers": [gh]})).await;
    assert_eq!(st, StatusCode::OK);
    let (_, b) = s.get(&url, ADMIN_TOKEN).await;
    assert_eq!(b["prefilled"], true);
    assert!(b["prefill"].is_null(), "{b}");
}

#[tokio::test]
async fn people_lists_bound_principals_and_seen_assignees() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pp").await;
    s.project("other").await;
    let tester = s.token("pp-tester", "tester", &["pp"]).await;
    s.token("pp-viewer", "viewer", &["pp"]).await;
    let outsider = s.token("other-tester", "tester", &["other"]).await;
    let n = create_case(&s, "pp", json!({"name": "A", "body": {}})).await;
    let p = plan(&s, "pp", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "pp", ADMIN_TOKEN, &p).await;
    let iid = items(&s, "pp", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, b) = patch_item(
        &s,
        "pp",
        ADMIN_TOKEN,
        &iid,
        json!({"assignee": "maria@example.org"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, b) = s.get("/api/v1/projects/pp/people", &tester).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let names: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["principal"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"pp-tester"), "{b}");
    assert!(names.contains(&"maria@example.org"), "seen assignee: {b}");
    assert!(!names.contains(&"pp-viewer"), "viewers cannot execute: {b}");
    assert!(
        !names.contains(&"other-tester"),
        "another project's token: {b}"
    );
    let (_, b) = s.get("/api/v1/projects/pp/people?q=MARIA", &tester).await;
    assert_eq!(b["items"], json!([{"principal": "maria@example.org"}]));
    let (st, _) = s.get("/api/v1/projects/pp/people", &outsider).await;
    assert!(
        st == StatusCode::FORBIDDEN || st == StatusCode::NOT_FOUND,
        "{st}"
    );
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/pp/people?q={}", "x".repeat(201)),
            &tester,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .call(Method::GET, "/api/v1/projects/pp/people", None, None)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn plan_launches_and_recursive_plan_folders() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pl").await;
    s.project("pm").await;
    let top = folder(&s, "pl", "Top", None).await;
    let sub = folder(&s, "pl", "Sub", Some(&top)).await;
    let a = create_case(
        &s,
        "pl",
        json!({"name": "In top", "folderId": top, "body": {}}),
    )
    .await;
    let b_ = create_case(
        &s,
        "pl",
        json!({"name": "In sub", "folderId": sub, "body": {}}),
    )
    .await;
    let (st, created) = s
        .post(
            "/api/v1/projects/pl/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "Folder plan", "filter": {"folderIds": [top]}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{created}");
    assert_eq!(created["filter"]["recursive"], true, "{created}");
    let pid = created["id"].as_str().unwrap().to_string();
    let (_, its) = s
        .get(
            &format!("/api/v1/projects/pl/test-plans/{pid}/items?limit=100"),
            ADMIN_TOKEN,
        )
        .await;
    let nums: Vec<i64> = its["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["caseNumber"].as_i64().unwrap())
        .collect();
    assert_eq!(
        nums,
        [a, b_],
        "a plan folder includes its subfolders: {its}"
    );
    let lid = start(&s, "pl", ADMIN_TOKEN, &pid).await;
    let path = format!("/api/v1/projects/pl/test-plans/{pid}/launches");
    let (st, l) = s.get(&path, ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["items"].as_array().unwrap().len(), 1);
    assert_eq!(l["items"][0]["id"], json!(lid));
    assert_eq!(l["items"][0]["manual"]["total"], 2, "{l}");
    assert_eq!(l["items"][0]["testPlanId"], json!(pid));
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/pm/test-plans/{pid}/launches"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "another project's plan");
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/pl/test-plans/{}/launches", Uuid::now_v7()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let viewer = s.token("pl-viewer", "viewer", &["pl"]).await;
    let (st, _) = s.get(&path, &viewer).await;
    assert_eq!(st, StatusCode::OK, "launches:read is enough");
    let other = s.token("pm-viewer", "viewer", &["pm"]).await;
    let (st, _) = s.get(&path, &other).await;
    assert!(
        st == StatusCode::FORBIDDEN || st == StatusCode::NOT_FOUND,
        "{st}"
    );
}

#[tokio::test]
async fn rerun_needs_if_match_and_keeps_earlier_executions_after_a_partition_drop() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["history.rollup", "retention.apply", "partitions.ensure"])
        .await;
    s.project("rr").await;
    let (st, b) = put_settings(
        &s,
        "rr",
        json!({"retention": {"rawDays": 30, "manualDays": 0}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let store = fastproof_store::Store::new(db.pool.clone());
    store.ensure_partitions(8, 1).await.unwrap();
    let tester = s.token("rr-tester", "tester", &["rr"]).await;
    let n = create_case(
        &s,
        "rr",
        json!({"name": "Pay", "body": {"steps": [step("pay", json!([]))]}}),
    )
    .await;
    let p = plan(&s, "rr", json!({"name": "P", "caseNumbers": [n]})).await;
    let lid = start(&s, "rr", ADMIN_TOKEN, &p).await;
    let iid = items(&s, "rr", &lid).await[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    put_step(
        &s,
        "rr",
        &tester,
        &iid,
        "1",
        json!({"status": "failed", "actual": "first try"}),
    )
    .await;
    let (st, b) = patch_item(
        &s,
        "rr",
        &tester,
        &iid,
        json!({"status": "failed", "comment": "c1"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let rerun = format!("/api/v1/projects/rr/run-items/{iid}/rerun");
    let (st, b, _) = send(&s, Method::POST, &rerun, &tester, None, None).await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "{b}");
    let (st, b, _) = send(&s, Method::POST, &rerun, &tester, Some("\"r999\""), None).await;
    assert_eq!(st, StatusCode::PRECONDITION_FAILED, "{b}");
    let (_, etag) = item(&s, "rr", &iid).await;
    let (st, b, _) = send(&s, Method::POST, &rerun, &tester, Some(&etag), None).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["executionNo"], 2);
    assert_eq!(b["stepResults"], json!([]));
    let a = audit_has(&s, "runItem.rerun").await;
    assert_eq!(a["detail"]["executionNo"], 1, "{a}");
    assert_eq!(a["detail"]["from"], "failed", "{a}");
    put_step(
        &s,
        "rr",
        &tester,
        &iid,
        "1",
        json!({"status": "passed", "actual": "second try"}),
    )
    .await;
    let (st, b) = patch_item(&s, "rr", &tester, &iid, json!({"status": "passed"})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/rr/launches/{lid}/manual/close"),
            ADMIN_TOKEN,
            json!({"untestedTo": "skipped"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    sqlx::query(
        "update launches set created_at = now() - interval '125 days', started_at = now() - interval '125 days', \
         status = 'finished', rolled_up_at = now() where id = $1::uuid",
    )
    .bind(&lid)
    .execute(&db.pool)
    .await
    .unwrap();
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
    let projects = store.all_project_settings().await.unwrap();
    let dropped = store.drop_rolled_partitions(&projects).await.unwrap();
    let still_there: bool =
        sqlx::query_scalar("select exists (select 1 from pg_class where relname = $1)")
            .bind(&month)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        dropped.contains(&month) || !still_there,
        "the partition is dropped by this call or by the server's own retention job first: {dropped:?}"
    );
    let attempts: i64 =
        sqlx::query_scalar("select count(*) from attempts where launch_id = $1::uuid")
            .bind(&lid)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 0, "the raw attempts are gone");

    let (st, ex) = s
        .get(
            &format!("/api/v1/projects/rr/run-items/{iid}/executions"),
            &tester,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{ex}");
    let first = &ex["items"][0];
    assert_eq!(first["executionNo"], 1, "{ex}");
    assert_eq!(first["status"], "failed");
    assert_eq!(first["comment"], "c1");
    assert_eq!(first["executedBy"], json!(me(&s, &tester).await));
    assert_eq!(first["stepResults"][0]["actual"], "first try", "{ex}");
    assert_eq!(first["rerunBy"], json!(me(&s, &tester).await));
    assert!(ex["next"].is_null());
    let (got, _) = item(&s, "rr", &iid).await;
    assert_eq!(got["stepResults"][0]["actual"], "second try");
    assert_eq!(got["status"], "passed");
    let (st, _) = s
        .get(
            &format!(
                "/api/v1/projects/rr/run-items/{}/executions",
                Uuid::now_v7()
            ),
            &tester,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    s.project("rs").await;
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/rs/run-items/{iid}/executions"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "IDOR");
}
