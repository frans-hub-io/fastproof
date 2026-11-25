//! TM next foundation over HTTP: the settings gate for new formats while an older replica
//! runs, `GET /api/v1/admin/replicas`, `cases:review` for starting runs with unapproved
//! versions, and the refusals of formats whose areas are not built yet.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn put_settings(s: &Server, key: &str, token: &str, body: Value) -> (StatusCode, Value) {
    s.call(
        Method::PUT,
        &format!("/api/v1/projects/{key}/settings"),
        Some(token),
        Some(body),
    )
    .await
}

fn rerun_with_variables() -> Value {
    json!({"rerun": {"provider": "webhook", "url": "https://ci.example.com/hooks/fastproof",
        "refs": ["main", "release/*"],
        "variables": [{"name": "BROWSER", "values": ["chromium", "firefox"], "default": "chromium",
            "description": "Browser"}, {"name": "SUITE"}]}})
}

/// Pretends the database was upgraded: every migration before the gate ran two days ago.
async fn upgraded(db: &TestDb) {
    fastproof_store::migrate(&db.pool).await.unwrap();
    sqlx::query(
        "update fastproof_schema_migrations set installed_on = installed_on - interval '2 days' where version < 911",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn old_replica(db: &TestDb, seen_minutes_ago: i32) {
    sqlx::query(
        "insert into replica_levels (instance, tm_level, version, seen_at) \
         values ('old-replica', 1, '0.2.0-dev2', now() - make_interval(mins => $1)) \
         on conflict (instance) do update set seen_at = excluded.seen_at",
    )
    .bind(seen_minutes_ago)
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn settings_with_new_formats_wait_for_the_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    upgraded(&db).await;
    old_replica(&db, 0).await;
    let a = Server::start(&db).await;
    a.project("gate").await;

    let (st, b) = put_settings(&a, "gate", ADMIN_TOKEN, rerun_with_variables()).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "upgrade_in_progress");
    assert_eq!(
        b["details"],
        json!({"what": "rerun.variables", "requiredLevel": 2})
    );
    for body in [
        json!({"caseSync": {"requirementLinkTypes": ["req"]}}),
        json!({"notifications": [{"id": "n1", "kind": "webhook", "target": "https://hooks.example.com/x",
            "on": ["caseReviewRequested"]}]}),
    ] {
        let (st, b) = put_settings(&a, "gate", ADMIN_TOKEN, body.clone()).await;
        assert_eq!(st, StatusCode::CONFLICT, "{body} {b}");
        assert_eq!(b["code"], "upgrade_in_progress", "{b}");
    }
    let (st, b) = put_settings(
        &a,
        "gate",
        ADMIN_TOKEN,
        json!({"flaky": {"historyDepth": 12}, "caseReview": {"mode": "manual", "approvals": 2,
            "resetOn": "content", "allowSelfApproval": false, "imports": "review"}}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "formats old replicas keep as they are: {b}"
    );
    assert_eq!(b["caseReview"]["approvals"], 2);

    let (st, r) = a.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(
        (r["gateOpen"].as_bool(), r["mode"].as_str()),
        (Some(false), Some("auto"))
    );
    assert!(r["settleUntil"].is_string(), "{r}");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, r) = a.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
        let levels: Vec<i64> = r["replicas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["tmLevel"].as_i64().unwrap())
            .collect();
        if levels.contains(&1) && levels.contains(&2) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "this replica never reported: {r}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Without a settle time only the old replica holds the gate, until it is gone.
    let b_srv = Server::start_with(&db, &[("FASTPROOF_TM_GATE_SETTLE_SECONDS", "0")]).await;
    let (st, b) = put_settings(&b_srv, "gate", ADMIN_TOKEN, rerun_with_variables()).await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    old_replica(&db, 3).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (st, b) = put_settings(&b_srv, "gate", ADMIN_TOKEN, rerun_with_variables()).await;
        if st == StatusCode::OK {
            assert_eq!(b["rerun"]["variables"][0]["name"], "BROWSER", "{b}");
            assert_eq!(b["rerun"]["refs"], json!(["main", "release/*"]));
            break;
        }
        assert_eq!(st, StatusCode::CONFLICT, "{b}");
        assert!(Instant::now() < deadline, "the gate never opened");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // `FASTPROOF_TM_GATE=open` skips the check.
    old_replica(&db, 0).await;
    let c = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    let (st, b) = put_settings(
        &c,
        "gate",
        ADMIN_TOKEN,
        json!({"caseSync": {"requirementLinkTypes": ["requirement", "story"]}}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["caseSync"]["requirementLinkTypes"],
        json!(["requirement", "story"])
    );
    let (_, r) = c.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
    assert_eq!(
        (r["gateOpen"].as_bool(), r["mode"].as_str()),
        (Some(true), Some("open"))
    );

    // Invalid values are refused before the gate.
    for (body, what) in [
        (
            json!({"rerun": {"provider": "webhook", "url": "https://ci.example.com/h",
                "variables": [{"name": "FASTPROOF_X"}]}}),
            "FASTPROOF_",
        ),
        (
            json!({"rerun": {"provider": "webhook", "url": "https://ci.example.com/h",
                "variables": [{"name": "A", "values": ["x"], "default": "y"}]}}),
            "default",
        ),
        (
            json!({"rerun": {"provider": "webhook", "url": "https://ci.example.com/h",
                "refs": ["bad//glob"]}}),
            "rerun.refs",
        ),
        (
            json!({"caseReview": {"mode": "all", "approvals": 9, "resetOn": "any",
                "allowSelfApproval": true, "imports": "bypass"}}),
            "approvals",
        ),
        (
            json!({"caseWorkflow": {"statuses": [{"key": "Draft", "name": "D", "category": "draft"}],
                "manual": {"initial": "Draft", "transitions": []},
                "automated": {"initial": "Draft", "transitions": []}}}),
            "key",
        ),
    ] {
        let (st, b) = put_settings(&c, "gate", ADMIN_TOKEN, body.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body} {b}");
        assert!(b["message"].as_str().unwrap().contains(what), "{b}");
    }
}

#[tokio::test]
async fn replicas_need_global_settings_write() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rep").await;
    let scoped_admin = s.token("scoped", "admin", &["rep"]).await;
    let editor = s.token("ed", "editor", &["rep"]).await;
    for t in [&scoped_admin, &editor] {
        let (st, b) = s.get("/api/v1/admin/replicas", t).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    }
    let (st, _) = s
        .call(Method::GET, "/api/v1/admin/replicas", None, None)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, r) = s.get("/api/v1/admin/replicas", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["gateOpen"], true, "a fresh install is open: {r}");
    assert!(r["settleUntil"].is_null());
}

#[tokio::test]
async fn unapproved_runs_need_cases_review() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rev").await;
    let tester = s.token("t", "tester", &["rev"]).await;
    let editor = s.token("e", "editor", &["rev"]).await;
    let (st, opts) = s.get("/api/v1/tokens/options", ADMIN_TOKEN).await;
    assert_eq!(st, StatusCode::OK);
    let editor_role = opts["roles"]
        .as_array()
        .and_then(|r| r.iter().find(|x| x["name"] == "editor"))
        .cloned()
        .unwrap_or_default();
    assert!(
        editor_role.to_string().contains("cases:review"),
        "editor gains cases:review: {opts}"
    );
    let (st, c) = s
        .post(
            "/api/v1/projects/rev/cases",
            ADMIN_TOKEN,
            json!({"name": "Pay", "body": {"steps": [{"action": "Pay"}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    assert!(c["state"].is_null() && c["reviewState"].is_null() && c["approvedVersion"].is_null());
    assert_eq!(c["sharedSteps"], json!([]));
    let n = c["number"].as_i64().unwrap();
    let (st, plan) = s
        .post(
            "/api/v1/projects/rev/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "p", "caseNumbers": [n]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    let plan_id = plan["id"].as_str().unwrap();
    let start = |ext: &str, unapproved: bool| json!({"name": "run", "externalId": ext, "includeUnapproved": unapproved});
    let path = format!("/api/v1/projects/rev/test-plans/{plan_id}/launches");
    let (st, b) = s.post(&path, &tester, start("t-1", true)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    let (st, b) = s.post(&path, &tester, start("t-2", false)).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert_eq!(b["skips"], json!([]));
    assert_eq!(b["launches"][0]["id"], b["launch"]["id"]);
    assert_eq!(b["dispatches"], json!([]));
    let (st, b) = s.post(&path, &editor, start("e-1", true)).await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    let launch = b["launch"]["id"].as_str().unwrap().to_string();
    let items = format!("/api/v1/projects/rev/launches/{launch}/run-items");
    let (st, b) = s
        .post(
            &items,
            &tester,
            json!({"caseNumbers": [n], "includeUnapproved": true}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    let (st, b) = s
        .post(
            &items,
            &editor,
            json!({"caseNumbers": [n], "includeUnapproved": true}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");

    let (st, b) = s
        .post(
            &path,
            &editor,
            json!({"name": "ci", "externalId": "ci-1", "includeManual": false}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
    assert!(b["launch"]["manual"].is_null(), "no run items: {b}");
}

#[tokio::test]
async fn formats_of_later_areas_are_refused_until_built() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("later").await;
    let (st, b) = s
        .post(
            "/api/v1/projects/later/cases",
            ADMIN_TOKEN,
            json!({"name": "S", "body": {"steps": [{"id": "",
                "shared": {"id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b", "number": 1}}]}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "invalid_case");
    let (st, b) = s
        .post(
            "/api/v1/projects/later/cases",
            ADMIN_TOKEN,
            json!({"name": "P", "body": {"parameters": {"columns": ["a"], "rows": []}}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "invalid_case");
    let (st, b) = s
        .post(
            "/api/v1/projects/later/test-plans",
            ADMIN_TOKEN,
            json!({"kind": "managed", "name": "q", "query": "status ~ active"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "invalid_query");
    let (st, b) = s
        .post(
            "/api/v1/projects/later/cases/bulk",
            ADMIN_TOKEN,
            json!({"query": "status ~ active", "action": "archive"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    assert_eq!(b["code"], "invalid_query");
    let (st, b) = s
        .post(
            "/api/v1/projects/later/cases/bulk",
            ADMIN_TOKEN,
            json!({"numbers": [1], "action": "setState", "value": "draft"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no workflow: {b}");
}
