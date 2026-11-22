//! Security rules of test management over HTTP and MCP: `trace_actions` never returns typed
//! values (real Playwright 1.65 trace), code sync trusts only launches every contributor of
//! which may sync and finished launches take results only from their creator or a syncing
//! caller, the AI review rules on REST, caps on zip exports, separate worker pools for
//! imports and exports, import jobs behind `cases:import`, heavy slots for routes that
//! multiply work, the cap on pending suggestions, and the NDJSON report of created fields.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb, read};
use fastproof_store::Store;
use fastproof_store::tm::{TmJobKind, TmLimits};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn state(s: &Server) -> &fastproof_server::state::AppState {
    &s.running.as_ref().unwrap().state
}

async fn auth_of(s: &Server, token: &str) -> franshub_auth::Auth {
    let mut h = axum::http::HeaderMap::new();
    h.insert("authorization", format!("Bearer {token}").parse().unwrap());
    state(s)
        .platform
        .authenticate(&h, &axum::http::Method::GET)
        .await
        .unwrap()
}

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

fn actions_of_files(files: &[&str]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut open = HashMap::new();
    for f in files {
        let text = String::from_utf8(read(f)).unwrap();
        fastproof_server::mcp_tm::actions_of(&text, &mut out, &mut open);
    }
    out
}

/// H1: the runner's `test.trace` of a real Playwright 1.65 run has `Fill "<value>"` and
/// `Type "<text>"` titles; neither they nor the library events return a typed value.
#[test]
fn trace_actions_never_return_typed_values() {
    let all = actions_of_files(&[
        "playwright-trace/test.trace",
        "playwright-trace/0-trace.trace",
        "playwright-trace/1-trace.trace",
    ]);
    let text = serde_json::to_string(&all).unwrap();
    for secret in [
        "hunter2-SECRET",
        "typed-SECRET2",
        "Fill \"",
        "Type \"",
        "Press \"",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    for a in &all {
        let n = a["apiName"].as_str().unwrap();
        assert!(
            n.split('.').count() == 2 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'),
            "apiName from a fixed list: {n}"
        );
    }
    let fill = all
        .iter()
        .find(|a| a["apiName"] == "Frame.fill")
        .unwrap_or_else(|| panic!("the library fill stays: {text}"));
    assert_eq!(fill["selector"], "internal:label=\"Password\"i");
    assert!(all.iter().any(|a| a["apiName"] == "Frame.type"), "{text}");
    assert!(all.iter().any(|a| a["apiName"] == "Frame.press"), "{text}");

    let lines = [
        json!({"type": "before", "callId": "c@1", "startTime": 1.0, "class": "Frame", "method": "fill",
            "title": "Fill \"pw-SECRET3\"", "params": {"selector": "#p", "value": "pw-SECRET3"}}),
        json!({"type": "after", "callId": "c@1", "endTime": 9.0, "error": {"message":
            "Timeout 5000ms exceeded.\nCall log:\n  - fill(\"pw-SECRET3\")"}}),
        json!({"type": "before", "callId": "c@2", "startTime": 10.0, "class": "Keyboard", "method": "type",
            "apiName": "keyboard.type", "params": {"text": "kb-SECRET4"}}),
        json!({"type": "after", "callId": "c@2", "endTime": 11.0, "error": {"message": "typing kb-SECRET4 failed"}}),
        json!({"type": "before", "callId": "c@3", "startTime": 12.0, "class": "Frame", "method": "goto",
            "apiName": "Fill \"x-SECRET5\"", "params": {"url": "https://shop.example.com/"}}),
        json!({"type": "after", "callId": "c@3", "endTime": 13.0}),
        json!({"type": "action", "metadata": {"apiName": "Type \"old-SECRET6\"", "type": "Frame", "method": "type",
            "params": {"selector": "#u", "text": "old-SECRET6"}, "startTime": 1.0, "endTime": 2.0,
            "error": {"error": {"message": "elementHandle.type(\"old-SECRET6\") failed"}}}}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let mut out = Vec::new();
    let mut open = HashMap::new();
    fastproof_server::mcp_tm::actions_of(&text, &mut out, &mut open);
    let t = serde_json::to_string(&out).unwrap();
    for secret in ["SECRET3", "SECRET4", "SECRET5", "SECRET6"] {
        assert!(!t.contains(secret), "{secret} leaked: {t}");
    }
    assert_eq!(out.len(), 4, "{t}");
    assert_eq!(out[0]["status"], "failed");
    assert!(
        out[0]["error"]
            .as_str()
            .unwrap()
            .starts_with("Timeout 5000ms exceeded.")
    );
    assert_eq!(out[1]["apiName"], "keyboard.type");
    assert_eq!(out[2]["apiName"], "Frame.goto");
    assert_eq!(out[2]["url"], "https://shop.example.com/");
    assert_eq!(out[3]["apiName"], "Frame.type");
}

fn result(name: &str, full: &str) -> Value {
    json!({"id": full, "name": name, "fullName": full, "status": "passed", "start": 1795249075967i64,
        "stop": 1795249076967i64})
}

async fn sync(db: &TestDb) -> fastproof_store::tm::sync::SyncStats {
    Store::new(db.pool.clone())
        .run_case_sync("test", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap()
}

fn mr_access_file(dir: &std::path::Path) -> String {
    let p = dir.join("access.yaml");
    std::fs::write(
        &p,
        "roles:\n  - name: mr-uploader\n    permissions: [\"projects:read\", \"launches:read\", \"launches:write\", \"cases:read\"]\n",
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

async fn case_names(s: &Server, key: &str) -> Vec<String> {
    let (st, b) = s
        .get(
            &format!("/api/v1/projects/{key}/cases?limit=100"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

/// H2: a merge-request token (the docs' `mr-uploader` role, no `cases:sync`) cannot change
/// cases through the trusted launch of the main pipeline, finished or still running.
#[tokio::test]
async fn mr_token_cannot_change_cases_through_a_trusted_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = mr_access_file(dir.path());
    let s = Server::start_with(&db, &[("FASTPROOF_ACCESS_FILE", &file)]).await;
    db.hold_jobs(&["cases.sync", "cases.backfill"]).await;
    s.project("mr").await;
    let ci = s.token("ci", "uploader", &["mr"]).await;
    let mr = s.token("mr-pipeline", "mr-uploader", &["mr"]).await;
    let base = "/api/v1/projects/mr";

    let (st, l) = s
        .post(
            &format!("{base}/launches"),
            &ci,
            json!({"name": "ci", "externalId": "pipeline-100", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let lid = l["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("{base}/launches/{lid}/results"),
            &ci,
            json!([result("adds an item", "shop.Cart#adds")]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let (st, b) = s
        .post(&format!("{base}/launches/{lid}/finish"), &ci, json!({}))
        .await;
    assert!(st.is_success(), "{b}");
    let st1 = sync(&db).await;
    assert_eq!(st1.created, 1, "{st1:?}");

    let (st, joined) = s
        .post(
            &format!("{base}/launches"),
            &mr,
            json!({"name": "mr", "externalId": "pipeline-100", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{joined}");
    assert_eq!(
        joined["id"],
        lid.as_str(),
        "joining by externalId still works"
    );
    let evil = json!([
        result("PWNED by an MR pipeline", "shop.Cart#adds"),
        result("injected test", "evil.Branch#only")
    ]);
    let (st, b) = s
        .post(&format!("{base}/launches/{lid}/results"), &mr, evil.clone())
        .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "a finished launch refuses results: {b}"
    );
    assert_eq!(b["code"], "launch_finished", "{b}");
    let r = s
        .req(
            Method::POST,
            &format!("{base}/launches/{lid}/uploads?format=junit"),
            Some(&mr),
        )
        .header("content-type", "application/xml")
        .body("<testsuite name=\"x\"><testcase name=\"y\" classname=\"evil\"/></testsuite>")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT, "and uploads");
    let (st, b) = s
        .post(&format!("{base}/launches/{lid}/finish"), &mr, json!({}))
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "and a second finish: {b}");
    let (st, b) = s
        .post(
            &format!("{base}/launches/{lid}/results"),
            &ci,
            json!([result("adds an item", "shop.Cart#adds")]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "the creator still may: {b}");
    let (st, b) = s
        .post(&format!("{base}/launches/{lid}/finish"), &ci, json!({}))
        .await;
    assert!(st.is_success(), "{b}");
    let st2 = sync(&db).await;
    assert_eq!((st2.created, st2.updated), (0, 0), "{st2:?}");
    assert_eq!(case_names(&s, "mr").await, vec!["adds an item"]);

    let (st, l) = s
        .post(
            &format!("{base}/launches"),
            &ci,
            json!({"name": "ci", "externalId": "pipeline-101", "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{l}");
    let lid2 = l["id"].as_str().unwrap().to_string();
    let (st, b) = s
        .post(
            &format!("{base}/launches/{lid2}/results"),
            &ci,
            json!([result("removes an item", "shop.Cart#removes")]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let (st, b) = s
        .post(&format!("{base}/launches/{lid2}/results"), &mr, evil)
        .await;
    assert_eq!(
        st,
        StatusCode::ACCEPTED,
        "a running launch takes MR results: {b}"
    );
    let (st, b) = s
        .post(&format!("{base}/launches/{lid2}/finish"), &ci, json!({}))
        .await;
    assert!(st.is_success(), "{b}");
    let st3 = sync(&db).await;
    assert_eq!(
        (st3.created, st3.updated, st3.linked),
        (0, 0, 0),
        "a launch with an MR contributor is not trusted: {st3:?}"
    );
    assert_eq!(case_names(&s, "mr").await, vec!["adds an item"]);
    let (st, u) = s
        .get(
            &format!("{base}/test-identities?unlinked=true"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let unlinked = u.to_string();
    assert!(
        unlinked.contains("evil.Branch#only") && unlinked.contains("shop.Cart#removes"),
        "{unlinked}"
    );
}

/// M1: the AI rules hold on REST too: the principal that created an AI draft (here an
/// agent's editor token over MCP) can neither review nor activate it, and nobody accepts
/// their own suggestion; another person can.
#[tokio::test]
async fn ai_rules_hold_on_rest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ai").await;
    let agent = s.token("copilot", "editor", &["ai"]).await;
    let person = s.token("ann", "editor", &["ai"]).await;
    let base = "/api/v1/projects/ai";
    let (err, d) = mcp_tool(
        &s,
        &agent,
        "create_case_draft",
        json!({"project": "ai", "name": "Logs in", "steps": [{"action": "Open"}], "agent": "copilot"}),
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
    let tag = get.etag.clone().unwrap();

    let r = send(
        &s,
        Method::POST,
        &format!("{base}/cases/{n}/review"),
        &agent,
        Some(&tag),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.body["code"], "ai_self_review", "{}", r.body);
    let r = send(
        &s,
        Method::PATCH,
        &format!("{base}/cases/{n}"),
        &agent,
        Some(&tag),
        Some(json!({"status": "active"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    let mut put = get.body.clone();
    put["status"] = json!("active");
    let w = json!({"name": put["name"], "status": "active", "body": put["body"], "folderId": put["folderId"]});
    let r = send(
        &s,
        Method::PUT,
        &format!("{base}/cases/{n}"),
        &agent,
        Some(&tag),
        Some(w),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/cases/bulk"),
        &agent,
        None,
        Some(json!({"numbers": [n], "action": "setStatus", "value": "active"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["updated"], 0, "{}", r.body);
    assert_eq!(r.body["skipped"][0]["code"], "ai_self_review", "{}", r.body);
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

    let rev = r.body["revision"].as_i64().unwrap();
    let (err, p) = mcp_tool(
        &s,
        &agent,
        "propose_case_update",
        json!({"project": "ai", "number": n, "baseRevision": rev, "name": "Logs in with a password",
            "rationale": "clearer", "agent": "copilot"}),
    )
    .await;
    assert!(!err, "{p}");
    let sid = p["suggestionId"].as_str().unwrap().to_string();
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/case-suggestions/{sid}/accept"),
        &agent,
        Some(&format!("\"r{rev}\"")),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.body["code"], "ai_self_review", "{}", r.body);

    let r = send(
        &s,
        Method::POST,
        &format!("{base}/case-suggestions/{sid}/accept"),
        &person,
        Some(&format!("\"r{rev}\"")),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let tag = r.etag.clone().unwrap();
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
    assert_eq!(r.body["status"], "active");
    assert_eq!(
        r.body["aiUnreviewed"], false,
        "activating by another person is the review"
    );

    let (err, d) = mcp_tool(
        &s,
        &agent,
        "create_case_draft",
        json!({"project": "ai", "name": "Logs out", "agent": "copilot"}),
    )
    .await;
    assert!(!err, "{d}");
    let n2 = d["number"].as_i64().unwrap();
    let r = send(
        &s,
        Method::GET,
        &format!("{base}/cases/{n2}"),
        &person,
        None,
        None,
    )
    .await;
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/cases/{n2}/review"),
        &person,
        r.etag.as_deref(),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["aiUnreviewed"], false);
}

async fn hold_tm_job_claims(db: &TestDb) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('fastproof:tm:jobs', 0))")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

/// M2: zip exports are capped per caller (2) and per project (4) while queued or running;
/// imports are not held back by them.
#[tokio::test]
async fn zip_exports_are_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ex").await;
    let v1 = s.token("v1", "viewer", &["ex"]).await;
    let v2 = s.token("v2", "viewer", &["ex"]).await;
    let v3 = s.token("v3", "viewer", &["ex"]).await;
    let held = hold_tm_job_claims(&db).await;
    let path = "/api/v1/projects/ex/cases/exports";
    let body = json!({"format": "zip", "attachments": true});
    for _ in 0..2 {
        let (st, b) = s.post(path, &v1, body.clone()).await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    }
    let (st, b) = s.post(path, &v1, body.clone()).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "export_busy", "{b}");
    for _ in 0..2 {
        let (st, b) = s.post(path, &v2, body.clone()).await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    }
    let (st, b) = s.post(path, &v3, body.clone()).await;
    assert_eq!(st, StatusCode::CONFLICT, "the project's cap: {b}");
    assert_eq!(b["code"], "export_busy", "{b}");
    held.rollback().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let n: i64 = sqlx::query_scalar(
            "select count(*) from tm_jobs where kind = 'export' and status in ('queued', 'running')",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if n == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "exports did not finish");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (st, b) = s.post(path, &v3, body).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
}

/// M2: imports and exports are claimed from separate pools, so queued exports never hold
/// an import back.
#[tokio::test]
async fn imports_are_claimed_apart_from_exports() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.jobs"]).await;
    s.project("pool").await;
    let p = state(&s).store.project("pool").await.unwrap().unwrap();
    let store = &state(&s).store;
    for i in 0..4 {
        store
            .create_tm_job(
                uuid::Uuid::now_v7(),
                p.id,
                &p.key,
                TmJobKind::Export,
                &json!({"filter": {}, "attachments": false, "tokenSha256": "00", "downloaded": false}),
                &format!("u{i}"),
            )
            .await
            .unwrap();
    }
    let import = uuid::Uuid::now_v7();
    store
        .create_tm_job(import, p.id, &p.key, TmJobKind::Import, &json!({}), "admin")
        .await
        .unwrap();
    let job = store
        .claim_tm_job("t-imports", TmJobKind::Import)
        .await
        .unwrap()
        .expect("the import is claimed");
    assert_eq!(job.id, import);
    let job = store
        .claim_tm_job("t-exports", TmJobKind::Export)
        .await
        .unwrap()
        .expect("an export is claimed");
    assert_eq!(job.kind, TmJobKind::Export);
}

/// L3: `/tm-jobs/{id}` of an import needs `cases:import`, like `/imports/{id}`.
#[tokio::test]
async fn import_jobs_need_cases_import() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["cases.jobs"]).await;
    s.project("ij").await;
    let viewer = s.token("v", "viewer", &["ij"]).await;
    let p = state(&s).store.project("ij").await.unwrap().unwrap();
    let id = uuid::Uuid::now_v7();
    state(&s)
        .store
        .create_tm_job(id, p.id, &p.key, TmJobKind::Import, &json!({}), "admin")
        .await
        .unwrap();
    let (st, b) = s
        .get(&format!("/api/v1/projects/ij/tm-jobs/{id}"), &viewer)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    let (st, b) = s
        .get(&format!("/api/v1/projects/ij/tm-jobs/{id}"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// L4 and M5: routes that multiply work take a heavy slot of the caller (429 while the
/// caller's slots are taken), `trace_actions` and zip export creation included.
#[tokio::test]
async fn work_multiplying_routes_take_heavy_slots() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "1")]).await;
    s.project("hv").await;
    let auth = auth_of(&s, ADMIN_TOKEN).await;
    let held = state(&s).guards.heavy(&auth).await.unwrap();
    let id = uuid::Uuid::now_v7();
    let base = "/api/v1/projects/hv";
    let calls: Vec<(Method, String, Option<Value>)> = vec![
        (
            Method::POST,
            format!("{base}/test-plans/{id}/launches"),
            Some(json!({"name": "r", "assign": "none", "includeAutomated": false})),
        ),
        (
            Method::POST,
            format!("{base}/launches/{id}/run-items"),
            Some(json!({"caseNumbers": [1]})),
        ),
        (
            Method::POST,
            format!("{base}/test-plans/{id}/items"),
            Some(json!({"add": [1]})),
        ),
        (Method::POST, format!("{base}/test-plans/{id}/sync"), None),
        (
            Method::POST,
            format!("{base}/test-plans"),
            Some(json!({"kind": "managed", "name": "p", "filter": {"statuses": ["active"]}})),
        ),
        (
            Method::DELETE,
            format!("{base}/case-folders/{id}?mode=archiveCases"),
            None,
        ),
        (
            Method::POST,
            format!("{base}/cases/exports"),
            Some(json!({"format": "zip"})),
        ),
    ];
    for (m, path, body) in calls {
        let r = send(&s, m.clone(), &path, ADMIN_TOKEN, None, body).await;
        assert_eq!(
            r.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{m} {path}: {}",
            r.body
        );
    }
    let (err, r) = mcp_tool(
        &s,
        ADMIN_TOKEN,
        "trace_actions",
        json!({"project": "hv", "resultId": id}),
    )
    .await;
    assert!(err, "{r}");
    assert!(r.to_string().contains("too many"), "{r}");
    drop(held);
    let r = send(
        &s,
        Method::POST,
        &format!("{base}/test-plans/{id}/sync"),
        ADMIN_TOKEN,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
}

/// L5: at most 20 pending suggestions per case.
#[tokio::test]
async fn pending_suggestions_are_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("sg").await;
    let agent = s.token("agent", "editor", &["sg"]).await;
    let (st, c) = s
        .post(
            "/api/v1/projects/sg/cases",
            ADMIN_TOKEN,
            json!({"name": "Pays", "body": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    let n = c["number"].as_i64().unwrap();
    let rev = c["revision"].as_i64().unwrap();
    for i in 0..21 {
        let (err, r) = mcp_tool(
            &s,
            &agent,
            "propose_case_update",
            json!({"project": "sg", "number": n, "baseRevision": rev, "name": format!("Pays {i}"),
                "rationale": "r"}),
        )
        .await;
        if i < 20 {
            assert!(!err, "{i}: {r}");
        } else {
            assert!(err, "{r}");
            assert!(r.to_string().contains("limit_reached"), "{r}");
        }
    }
}

async fn wait_job(s: &Server, key: &str, id: &str, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (st, b) = s
            .get(&format!("/api/v1/projects/{key}/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        if b["status"] == want {
            return b;
        }
        assert!(
            !["failed", "cancelled"].contains(&b["status"].as_str().unwrap_or("")),
            "{b}"
        );
        assert!(Instant::now() < deadline, "{b}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Blackbox 11: with `unknownFieldValues: create`, the NDJSON header's fields are reported as
/// fields to create, not as left out.
#[tokio::test]
async fn ndjson_report_lists_created_fields() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("nd").await;
    let nd = [
        json!({"format": "fastproof-cases", "version": 1, "fields": [
            {"key": "platform", "name": "Platform", "type": "select", "options": ["web"]}]}),
        json!({"name": "Opens", "fields": {"platform": ["web"]}}),
    ]
    .iter()
    .map(|l| format!("{l}\n"))
    .collect::<String>();
    let boundary = "fastproofboundary7MA4YWxkTrZu0gW";
    let opts =
        json!({"format": "fastproof-ndjson", "mode": "createOnly", "unknownFieldValues": "create"});
    let mut body = Vec::new();
    write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\n\r\n{opts}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"cases.ndjson\"\r\n\r\n{nd}\r\n\
         --{boundary}--\r\n"
    )
    .unwrap();
    let r = s
        .req(
            Method::POST,
            "/api/v1/projects/nd/imports",
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
    let j: Value = r.json().await.unwrap();
    let id = j["id"].as_str().unwrap().to_string();
    let ready = wait_job(&s, "nd", &id, "ready").await;
    assert_eq!(ready["report"]["unknownFields"], json!([]), "{ready}");
    assert_eq!(ready["report"]["newFields"], json!(["platform"]), "{ready}");
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/nd/imports/{id}/apply"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let done = wait_job(&s, "nd", &id, "done").await;
    assert_eq!(done["report"]["unknownFields"], json!([]), "{done}");
    assert_eq!(done["report"]["newFields"], json!(["platform"]), "{done}");
    let (_, c) = s.get("/api/v1/projects/nd/cases/1", ADMIN_TOKEN).await;
    assert_eq!(c["fields"]["platform"], "web", "{c}");
}

/// L5 (verification): a bulk action writes a version only for the cases it changes.
#[tokio::test]
async fn bulk_writes_versions_only_for_changed_cases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("bv").await;
    for (name, tags) in [("A", json!(["smoke"])), ("B", json!([]))] {
        let (st, c) = s
            .post(
                "/api/v1/projects/bv/cases",
                ADMIN_TOKEN,
                json!({"name": name, "body": {}, "tags": tags}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{c}");
    }
    let r = send(
        &s,
        Method::POST,
        "/api/v1/projects/bv/cases/bulk",
        ADMIN_TOKEN,
        None,
        Some(json!({"numbers": [1, 2], "action": "addTags", "value": ["smoke"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["updated"], 1, "{}", r.body);
    assert_eq!(
        r.body["skipped"],
        json!([{"number": 1, "code": "unchanged"}])
    );
    let versions: Vec<(i64, i64)> = sqlx::query_as(
        "select c.number, count(v.*) from cases c join case_versions v on v.case_id = c.id \
         group by c.number order by c.number",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(versions, vec![(1, 1), (2, 2)]);
}
