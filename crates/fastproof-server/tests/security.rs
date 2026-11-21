//! Regression tests of the core security fixes: bounded NDJSON and JSON parsing, the dashboard
//! date range, database and per-caller limits, search windows, log chunk sizes, event stream
//! caps and re-checks, test plan caps, list filter lengths, stalled bodies and spool caps, the
//! compiled-rule cache, reuse of deleted project keys, the attachment GC race and per-upload
//! attachment metadata. Gated on `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn state(s: &Server) -> &fastproof_server::state::AppState {
    &s.running.as_ref().unwrap().state
}

async fn launch_id(s: &Server, project: &str) -> String {
    s.launch(project, None).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn result(id: &str, labels: usize) -> Value {
    let labels: Vec<Value> = (0..labels)
        .map(|i| json!({"name": "tag", "value": format!("v{i}")}))
        .collect();
    json!({"id": id, "name": id, "status": "passed", "labels": labels})
}

async fn post_raw(s: &Server, path: &str, ct: &str, body: reqwest::Body) -> (StatusCode, Value) {
    let r = s
        .req(Method::POST, path, Some(ADMIN_TOKEN))
        .header("content-type", ct)
        .body(body)
        .send()
        .await
        .unwrap();
    let st = r.status();
    (st, r.json().await.unwrap_or(Value::Null))
}

fn chunked(bytes: Vec<u8>, size: usize) -> reqwest::Body {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(size).map(|c| Ok(c.to_vec())).collect();
    reqwest::Body::wrap_stream(futures::stream::iter(chunks))
}

/// B2: a long NDJSON line sent in small pieces is scanned once (the old reader searched the
/// whole buffer again for every piece: quadratic), and a line has its own size cap.
#[tokio::test]
async fn ndjson_lines_are_scanned_once_and_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("nd").await;
    let lid = launch_id(&s, "nd").await;
    let path = format!("/api/v1/projects/nd/launches/{lid}/results");
    let mut big = result("big", 0);
    big["description"] = json!("d".repeat(6 * 1024 * 1024));
    big["stdout"] = json!("o".repeat(6 * 1024 * 1024));
    let mut body = serde_json::to_vec(&big).unwrap();
    body.push(b'\n');
    let started = Instant::now();
    let (st, out) = post_raw(&s, &path, "application/x-ndjson", chunked(body, 1024)).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["accepted"], 1, "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a 12 MiB line in 1 KiB pieces took {:?}",
        started.elapsed()
    );
    let line = vec![b'x'; 17 * 1024 * 1024];
    let (st, out) = post_raw(&s, &path, "application/x-ndjson", chunked(line, 64 * 1024)).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    assert_eq!(out["details"]["limit"], "max_document_bytes", "{out}");
    assert_eq!(out["details"]["max"], 16 * 1024 * 1024, "{out}");
}

/// B4: JSON and NDJSON bodies are bounded by one value budget per request before they are
/// parsed (the old code parsed every document into a `Value` first).
#[tokio::test]
async fn json_bodies_have_one_value_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_cfg(&db, &[], |c| c.limits.max_json_values = 2000).await;
    s.project("jv").await;
    let lid = launch_id(&s, "jv").await;
    let path = format!("/api/v1/projects/jv/launches/{lid}/results");
    let lines: String = (0..100)
        .map(|i| format!("{}\n", result(&format!("r{i}"), 20)))
        .collect();
    let (st, out) = post_raw(&s, &path, "application/x-ndjson", lines.into()).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    assert_eq!(out["details"]["limit"], "max_json_values", "{out}");
    let arr = Value::Array((0..100).map(|i| result(&format!("a{i}"), 20)).collect());
    let (st, out) = s.post(&path, ADMIN_TOKEN, arr).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    assert_eq!(out["details"]["limit"], "max_json_values", "{out}");
    let ok = Value::Array(vec![
        result("ok1", 2),
        json!({"id": "bad"}),
        result("ok2", 2),
    ]);
    let (st, out) = s.post(&path, ADMIN_TOKEN, ok).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["accepted"], 2, "{out}");
    assert_eq!(out["rejected"][0]["index"], 1, "{out}");
    let logs = format!("/api/v1/projects/jv/launches/{lid}/logs");
    let lines: String = (0..400)
        .map(|i| {
            format!(
                "{}\n",
                json!({"resultId": "r", "attempt": 0, "seq": i, "ts": 1, "level": "info", "stream": "stdout", "message": "m"})
            )
        })
        .collect();
    let (st, out) = post_raw(&s, &logs, "application/x-ndjson", lines.into()).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    assert_eq!(out["details"]["limit"], "max_json_values", "{out}");
}

/// B3: extreme days are refused instead of overflowing (the old handler panicked and the
/// connection was dropped).
#[tokio::test]
async fn dashboard_days_are_range_checked() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("dash").await;
    for q in [
        "from=262142-12-01&to=262142-12-31",
        "to=-262143-01-30",
        "from=0001-01-01&to=0001-01-31",
    ] {
        let r = s
            .req(
                Method::GET,
                &format!("/api/v1/projects/dash/dashboard?{q}"),
                Some(ADMIN_TOKEN),
            )
            .send()
            .await
            .expect("the server answers");
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{q}");
    }
    let (st, b) = s
        .get(
            "/api/v1/projects/dash/dashboard?from=2026-01-01&to=2026-01-31",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// B8: the dashboard's `series_daily (project_id, day)` index, the statement timeout of pooled
/// connections (none for migrations), and per-caller limits on heavy reads.
#[tokio::test]
async fn database_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_DATABASE_STATEMENT_TIMEOUT_SECONDS", "7"),
            ("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "1"),
        ],
    )
    .await;
    let n: i64 = sqlx::query_scalar(
        "select count(*) from pg_indexes where tablename = 'series_daily' and indexname = 'series_daily_project_day'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
    let pool = state(&s).store.pool().clone();
    let t: String = sqlx::query_scalar("show statement_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(t, "7s");
    let free = fastproof_server::db::unbounded(&pool).await.unwrap();
    let t: String = sqlx::query_scalar("show statement_timeout")
        .fetch_one(&free)
        .await
        .unwrap();
    assert_eq!(t, "0");
    free.close().await;

    s.project("heavy").await;
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        "authorization",
        format!("Bearer {ADMIN_TOKEN}").parse().unwrap(),
    );
    let auth = state(&s)
        .platform
        .authenticate(&h, &axum::http::Method::GET)
        .await
        .unwrap();
    let held = state(&s).guards.heavy(&auth).await.unwrap();
    let (st, b) = s.get("/api/v1/projects/heavy/dashboard", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    let other = s.token("other", "viewer", &["heavy"]).await;
    let (st, b) = s.get("/api/v1/projects/heavy/dashboard", &other).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    drop(held);
    let (st, b) = s.get("/api/v1/projects/heavy/dashboard", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// B9: log searches have a bounded time window for every backend.
#[tokio::test]
async fn log_search_window_is_bounded() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("win").await;
    let (st, b) = s
        .get(
            "/api/v1/projects/win/search/logs?q=x&from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    let (st, b) = s
        .get(
            "/api/v1/projects/win/search/logs?q=x&from=2026-01-01T00:00:00Z&to=2026-01-20T00:00:00Z",
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

/// B10: stored chunks are bounded in bytes, and reads page through them.
#[tokio::test]
async fn log_chunks_are_bounded_in_bytes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("chunks").await;
    let lid = launch_id(&s, "chunks").await;
    let lines: Vec<Value> = (0..40)
        .map(|i| json!({"resultId": "r", "attempt": 0, "seq": i, "ts": 1, "level": "info", "stream": "stdout",
            "message": format!("{i}:{}", "m".repeat(60 * 1024))}))
        .collect();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/chunks/launches/{lid}/logs"),
            ADMIN_TOKEN,
            Value::Array(lines),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    let (n, max): (i64, i32) =
        sqlx::query_as("select count(*), max(bytes) from attempt_log_chunks")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(n >= 3, "{n} chunks");
    assert!(max as usize <= fastproof_store::logs::CHUNK_BYTES, "{max}");
    let mut seqs = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let q = before.map(|b| format!("&before={b}")).unwrap_or_default();
        let (st, page) = s
            .get(
                &format!("/api/v1/projects/chunks/launches/{lid}/logs?limit=7{q}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{page}");
        let mut got: Vec<u64> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["seq"].as_u64().unwrap())
            .collect();
        got.reverse();
        seqs.extend(got);
        match page["next"].as_str() {
            Some(n) => before = Some(n.to_string()),
            None => break,
        }
    }
    assert_eq!(seqs, (0..40).rev().collect::<Vec<u64>>());
}

async fn open_stream(s: &Server, path: &str, token: &str) -> reqwest::Response {
    s.req(Method::GET, path, Some(token)).send().await.unwrap()
}

/// B11: one caller can hold only a few open event streams.
#[tokio::test]
async fn event_streams_are_capped_per_caller() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_SSE_MAX_PER_CALLER", "2")]).await;
    s.project("sse").await;
    let lid = launch_id(&s, "sse").await;
    let events = "/api/v1/projects/sse/events";
    let logs = format!("/api/v1/projects/sse/launches/{lid}/logs/stream");
    let a = open_stream(&s, events, ADMIN_TOKEN).await;
    assert_eq!(a.status(), StatusCode::OK);
    let b = open_stream(&s, &logs, ADMIN_TOKEN).await;
    assert_eq!(b.status(), StatusCode::OK);
    let c = open_stream(&s, events, ADMIN_TOKEN).await;
    assert_eq!(c.status(), StatusCode::TOO_MANY_REQUESTS);
    let other = s.token("other-sse", "viewer", &["sse"]).await;
    let d = open_stream(&s, &logs, &other).await;
    assert_eq!(d.status(), StatusCode::OK);
    drop(a);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let e = open_stream(&s, events, ADMIN_TOKEN).await;
        if e.status() == StatusCode::OK {
            break;
        }
        assert!(Instant::now() < deadline, "the slot was not released");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A7: open streams end once their token is revoked.
#[tokio::test]
async fn streams_end_when_the_token_is_revoked() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_SSE_RECHECK_SECONDS", "1")]).await;
    s.project("rev").await;
    let lid = launch_id(&s, "rev").await;
    let (st, t) = s
        .post(
            "/api/v1/tokens",
            ADMIN_TOKEN,
            json!({"name": "watcher", "role": "viewer", "scopes": ["rev"], "expiresInDays": 1}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    let token = t["token"].as_str().unwrap().to_string();
    let id = t["id"].as_str().unwrap().to_string();
    let mut streams = Vec::new();
    for path in [
        "/api/v1/projects/rev/events".to_string(),
        format!("/api/v1/projects/rev/launches/{lid}/logs/stream"),
    ] {
        let r = open_stream(&s, &path, &token).await;
        assert_eq!(r.status(), StatusCode::OK);
        streams.push(r.bytes_stream());
    }
    let (st, b) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/tokens/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success(), "{st} {b}");
    for mut body in streams {
        let ended = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(c) = body.next().await {
                if c.is_err() {
                    break;
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "the stream outlived the revoked token");
    }
}

/// B13: test plans are capped per project and expire.
#[tokio::test]
async fn test_plans_are_capped_and_expire() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("plans").await;
    let pid: uuid::Uuid = sqlx::query_scalar("select id from projects where key = 'plans'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into test_plans (id, project_id, name, created_at) \
         select gen_random_uuid(), $1, 'p' || g, now() - interval '100 days' from generate_series(1, $2) g",
    )
    .bind(pid)
    .bind(fastproof_store::analysis::plans::MAX_PLANS_PER_PROJECT as i32)
    .execute(&db.pool)
    .await
    .unwrap();
    let body = json!({"name": "one more", "testCaseIds": []});
    let (st, b) = s
        .post(
            "/api/v1/projects/plans/test-plans",
            ADMIN_TOKEN,
            body.clone(),
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{b}");
    let r = state(&s)
        .store
        .retention_apply(Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        r.test_plans_deleted,
        fastproof_store::analysis::plans::MAX_PLANS_PER_PROJECT as u64
    );
    let (st, b) = s
        .post("/api/v1/projects/plans/test-plans", ADMIN_TOKEN, body)
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
}

/// B13: text filters of lists are bounded.
#[tokio::test]
async fn list_filters_are_bounded() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lq").await;
    let lid = launch_id(&s, "lq").await;
    let long = "q".repeat(300);
    for path in [
        format!("/api/v1/projects/lq/launches/{lid}/results?q={long}"),
        format!("/api/v1/projects/lq/launches?q={long}"),
    ] {
        let (st, b) = s.get(&path, ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{path}: {b}");
    }
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/lq/launches/{lid}/results?q=ok"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
}

/// B13: a body that stops sending fails after the idle timeout and releases its spool slot,
/// which other uploads wait for.
#[tokio::test]
async fn stalled_bodies_time_out_and_spools_are_capped() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_BODY_IDLE_TIMEOUT_SECONDS", "2"),
            ("FASTPROOF_MAX_CONCURRENT_SPOOLS", "1"),
        ],
    )
    .await;
    s.project("idle").await;
    let lid = launch_id(&s, "idle").await;
    let path = format!("/api/v1/projects/idle/launches/{lid}/attachments");
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(1);
    tx.send(Ok(b"first bytes".to_vec())).await.unwrap();
    let stalled = s
        .req(Method::POST, &path, Some(ADMIN_TOKEN))
        .header("content-type", "text/plain")
        .timeout(Duration::from_secs(20))
        .body(reqwest::Body::wrap_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ));
    let started = Instant::now();
    let a = tokio::spawn(async move { stalled.send().await.map(|r| r.status()) });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let b_started = Instant::now();
    let b = s
        .req(Method::POST, &path, Some(ADMIN_TOKEN))
        .header("content-type", "text/plain")
        .body("second")
        .send()
        .await
        .unwrap();
    assert_eq!(b.status(), StatusCode::CREATED);
    assert!(
        b_started.elapsed() >= Duration::from_millis(800),
        "the second upload did not wait for the spool slot"
    );
    let a = a.await.unwrap().expect("the stalled request is answered");
    assert_eq!(a, StatusCode::BAD_REQUEST);
    assert!(started.elapsed() < Duration::from_secs(15));
    drop(tx);
}

/// B13: a deleted rule leaves the compiled-rule cache.
#[tokio::test]
async fn deleted_rules_leave_the_compiled_cache() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rc").await;
    let (st, rule) = s
        .post(
            "/api/v1/projects/rc/rules",
            ADMIN_TOKEN,
            json!({"name": "refused", "match": {"messageRegex": "refused"}, "action": {"comment": "infra"}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{rule}");
    let id: uuid::Uuid = rule["id"].as_str().unwrap().parse().unwrap();
    let lid = launch_id(&s, "rc").await;
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/rc/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([{"id": "f1", "name": "f1", "status": "failed", "error": {"message": "connection refused"}}]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert!(fastproof_store::analysis::rules::compiled_cached(id));
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/rc/rules/{id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success());
    assert!(!fastproof_store::analysis::rules::compiled_cached(id));
}

fn access_file(dir: &std::path::Path) -> String {
    let p = dir.join("access.yaml");
    std::fs::write(
        &p,
        "bindings:\n  - subject: group:qa\n    role: viewer\n    scopes: [bound]\n",
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

/// A1: a deleted project's key does not hand its tokens, access-file bindings or audit
/// records to a new project with the same key.
#[tokio::test]
async fn deleted_project_keys_start_clean() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = access_file(dir.path());
    let s = Server::start_with(&db, &[("FASTPROOF_ACCESS_FILE", &file)]).await;
    for key in ["reuse", "bound"] {
        s.project(key).await;
    }
    let old = s.token("old-ci", "uploader", &["reuse"]).await;
    let wide = s.token("wide", "viewer", &["*"]).await;
    for key in ["reuse", "bound"] {
        let (st, _) = s
            .call(
                Method::DELETE,
                &format!("/api/v1/projects/{key}"),
                Some(ADMIN_TOKEN),
                None,
            )
            .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
    }
    fastproof_server::scheduler::purge_blobs(state(&s))
        .await
        .unwrap();
    let (st, b) = s
        .post(
            "/api/v1/projects",
            ADMIN_TOKEN,
            json!({"key": "bound", "name": "again"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    s.project("reuse").await;
    let (st, _) = s.get("/api/v1/projects/reuse", &old).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "the old token was revoked");
    let (st, _) = s.get("/api/v1/projects/reuse", &wide).await;
    assert_eq!(st, StatusCode::OK, "tokens for every project stay");
    let auditor = s.token("auditor", "admin", &["reuse"]).await;
    let (st, a) = s.get("/api/v1/audit?scope=reuse", &auditor).await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let actions: Vec<&str> = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    assert!(
        !actions.contains(&"project.delete"),
        "records of the deleted project: {actions:?}"
    );
}

async fn upload_attachment(
    s: &Server,
    project: &str,
    lid: &str,
    ct: &str,
    name: &str,
    body: &'static str,
) -> Value {
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{project}/launches/{lid}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", ct)
        .header("x-fastproof-attachment-name", name)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    r.json().await.unwrap()
}

async fn content(s: &Server, project: &str, id: &str) -> (StatusCode, String, String, String) {
    let r = s
        .req(
            Method::GET,
            &format!("/api/v1/projects/{project}/attachments/{id}/content"),
            Some(ADMIN_TOKEN),
        )
        .send()
        .await
        .unwrap();
    let st = r.status();
    let h = |k: &str| {
        r.headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (ct, cd) = (h("content-type"), h("content-disposition"));
    (st, ct, cd, r.text().await.unwrap_or_default())
}

/// D1: content uploaded again after the GC took its row keeps its blob (the old job deleted
/// the blob after the row, so a re-upload in between lost its content).
#[tokio::test]
async fn attachment_gc_keeps_reuploaded_content() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("gc").await;
    let l1 = launch_id(&s, "gc").await;
    upload_attachment(&s, "gc", &l1, "text/plain", "a.txt", "same bytes").await;
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/gc/launches/{l1}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success());
    sqlx::query("update attachments set last_seen_at = now() - interval '2 days'")
        .execute(&db.pool)
        .await
        .unwrap();
    let queued = state(&s)
        .store
        .collect_attachments(Duration::from_secs(86_400), 1000)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    let l2 = launch_id(&s, "gc").await;
    let again = upload_attachment(&s, "gc", &l2, "text/plain", "a.txt", "same bytes").await;
    fastproof_server::scheduler::purge_blobs(state(&s))
        .await
        .unwrap();
    let (st, _, _, text) = content(&s, "gc", again["id"].as_str().unwrap()).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(text, "same bytes");
    let left: i64 = sqlx::query_scalar("select count(*) from blob_deletions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// D3: identical bytes uploaded with another type and name keep their own type and name.
#[tokio::test]
async fn attachment_metadata_is_per_upload() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("meta").await;
    let lid = launch_id(&s, "meta").await;
    let a = upload_attachment(&s, "meta", &lid, "text/plain", "notes.txt", "{\"k\":1}").await;
    let b = upload_attachment(
        &s,
        "meta",
        &lid,
        "application/json",
        "data.json",
        "{\"k\":1}",
    )
    .await;
    assert_ne!(a["id"], b["id"]);
    assert_eq!(a["sha256"], b["sha256"]);
    let (_, ct, cd, _) = content(&s, "meta", a["id"].as_str().unwrap()).await;
    assert_eq!(ct, "text/plain");
    assert!(cd.contains("notes.txt"), "{cd}");
    let (st, ct, cd, text) = content(&s, "meta", b["id"].as_str().unwrap()).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ct, "application/json");
    assert!(cd.contains("data.json"), "{cd}");
    assert_eq!(text, "{\"k\":1}");
    let rows: i64 = sqlx::query_scalar("select count(*) from attachments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the bytes are stored once");
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/meta/launches/{lid}/results"),
            ADMIN_TOKEN,
            json!([{"id": "r1", "name": "r1", "status": "passed",
                "attachments": [{"id": b["id"].as_str().unwrap()}]}]),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["accepted"], 1, "{out}");
}
