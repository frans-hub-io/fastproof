//! FPQL over HTTP: authentication, permissions and IDOR of the query routes, `query` on the
//! case list, bulk actions, exports and plans, saved queries per caller, and the export job
//! with a query waiting for the replica gate.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn call(
    s: &Server,
    m: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    s.call(m, path, token, body).await
}

async fn seed(s: &Server, key: &str) {
    s.project(key).await;
    for (i, (name, tags)) in [
        ("Login page", json!(["smoke"])),
        ("Checkout total", json!(["api"])),
        ("Refund flow", json!([])),
    ]
    .into_iter()
    .enumerate()
    {
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/{key}/cases"),
                ADMIN_TOKEN,
                json!({"name": name, "status": if i == 2 { "draft" } else { "active" }, "tags": tags,
                    "body": {"steps": [{"action": "press"}]}}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{b}");
    }
}

fn numbers(list: &Value) -> Vec<i64> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["number"].as_i64().unwrap())
        .collect()
}

fn enc(q: &str) -> String {
    url::form_urlencoded::byte_serialize(q.as_bytes()).collect()
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
async fn query_routes_check_callers_and_queries() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    seed(&s, "qa").await;
    s.project("qb").await;
    let viewer = s.token("val", "viewer", &["qa"]).await;

    let validate = json!({"entity": "cases", "query": "status = active"});
    for (m, path, body) in [
        (
            Method::GET,
            "/api/v1/projects/qa/query/schema?entity=cases",
            None,
        ),
        (
            Method::POST,
            "/api/v1/projects/qa/query/validate",
            Some(validate.clone()),
        ),
        (
            Method::GET,
            "/api/v1/projects/qa/saved-queries?entity=cases",
            None,
        ),
        (
            Method::GET,
            "/api/v1/projects/qa/cases?query=status%20%3D%20active",
            None,
        ),
    ] {
        let (st, _) = call(&s, m.clone(), path, None, body.clone()).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}");
        let other = path.replace("/qa/", "/qb/");
        let (st, b) = call(&s, m.clone(), &other, Some(&viewer), body.clone()).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{other}: {b}");
        let missing = path.replace("/qa/", "/nope/");
        let (st2, _) = call(&s, m.clone(), &missing, Some(&viewer), body.clone()).await;
        assert_eq!(
            st2,
            StatusCode::FORBIDDEN,
            "same answer for a project that does not exist"
        );
        let (st, b) = call(&s, m, path, Some(&viewer), body).await;
        assert_eq!(st, StatusCode::OK, "{path}: {b}");
    }

    let (_, schema) = s.get("/api/v1/projects/qa/query/schema", &viewer).await;
    assert_eq!(schema["entity"], "cases");
    assert!(
        schema["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "priority")
    );
    let (st, b) = s
        .get(
            "/api/v1/projects/qa/query/schema?entity=requirements",
            &viewer,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "requirement queries exist since night 2: {b}"
    );
    assert_eq!(b["entity"], "requirements");
    let (st, b) = s
        .get("/api/v1/projects/qa/query/schema?entity=launches", &viewer)
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");

    let (st, v) = s
        .post("/api/v1/projects/qa/query/validate", &viewer, validate)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (
            v["ok"].as_bool(),
            v["count"].as_i64(),
            v["normalized"].as_str()
        ),
        (Some(true), Some(2), Some("status = active"))
    );
    let (st, v) = s
        .post(
            "/api/v1/projects/qa/query/validate",
            &viewer,
            json!({"entity": "cases", "query": "name = \"日本\" and stat = x"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "an invalid query is 200 with ok: false");
    assert_eq!(v["ok"], false);
    assert_eq!(
        (
            v["error"]["position"].as_i64(),
            v["error"]["length"].as_i64()
        ),
        (Some(16), Some(4))
    );
    let (st, _) = s
        .post(
            "/api/v1/projects/qa/query/validate",
            &viewer,
            json!({"entity": "cases", "query": "x", "extra": 1}),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let (st, l) = s
        .get(
            &format!(
                "/api/v1/projects/qa/cases?query={}",
                enc("tag = smoke or status = draft")
            ),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(numbers(&l), vec![1, 3]);
    let (st, l) = s
        .get(
            &format!(
                "/api/v1/projects/qa/cases?status=active&query={}",
                enc("tag != smoke")
            ),
            &viewer,
        )
        .await;
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![2]));
    let (st, e) = s
        .get(
            &format!("/api/v1/projects/qa/cases?query={}", enc("status = nope")),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(e["code"], "invalid_query");
    assert_eq!(
        (
            e["details"]["position"].as_i64(),
            e["details"]["length"].as_i64()
        ),
        (Some(9), Some(4))
    );
    assert!(
        e["details"]["expected"]
            .as_array()
            .unwrap()
            .contains(&json!("draft"))
    );
    let long = format!("name = \"{}\"", "x".repeat(5000));
    let (st, e) = s
        .get(
            &format!("/api/v1/projects/qa/cases?query={}", enc(&long)),
            &viewer,
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );

    let (st, csv) = {
        let r = s
            .req(
                Method::GET,
                &format!(
                    "/api/v1/projects/qa/cases/export?format=csv&query={}",
                    enc("status = active")
                ),
                Some(&viewer),
            )
            .send()
            .await
            .unwrap();
        (r.status(), r.text().await.unwrap())
    };
    assert_eq!(st, StatusCode::OK);
    assert!(
        csv.contains("Login page")
            && csv.contains("Checkout total")
            && !csv.contains("Refund flow"),
        "{csv}"
    );
}

#[tokio::test]
async fn bulk_and_plans_by_query() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    seed(&s, "qp").await;
    let viewer = s.token("qpv", "viewer", &["qp"]).await;
    let editor = s.token("qpe", "editor", &["qp"]).await;

    let body = json!({"query": "status = active", "action": "addTags", "value": ["q"]});
    let (st, _) = s
        .post("/api/v1/projects/qp/cases/bulk", &viewer, body.clone())
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, r) = s
        .post("/api/v1/projects/qp/cases/bulk", &editor, body)
        .await;
    assert_eq!(
        (st, r["updated"].as_i64()),
        (StatusCode::OK, Some(2)),
        "{r}"
    );
    let (st, r) = s
        .post(
            "/api/v1/projects/qp/cases/bulk",
            &editor,
            json!({"query": "tag = q", "numbers": [1], "action": "archive"}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, r) = s
        .post(
            "/api/v1/projects/qp/cases/bulk",
            &editor,
            json!({"query": "tag ~ q", "action": "archive"}),
        )
        .await;
    assert_eq!(
        (st, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
    audit_has(&s, &["case.bulk"]).await;

    let (st, plan) = s
        .post(
            "/api/v1/projects/qp/test-plans",
            &editor,
            json!({"kind": "managed", "name": "Smoke", "query": "tag = smoke"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{plan}");
    assert_eq!(
        (plan["query"].as_str(), plan["counts"]["total"].as_i64()),
        (Some("tag = smoke"), Some(1))
    );
    let id = plan["id"].as_str().unwrap().to_string();
    let (st, e) = s
        .post(
            "/api/v1/projects/qp/test-plans",
            &editor,
            json!({"kind": "managed", "name": "Both", "query": "tag = smoke", "filter": {"tags": ["api"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let r = s
        .req(
            Method::PATCH,
            &format!("/api/v1/projects/qp/test-plans/{id}"),
            Some(&editor),
        )
        .header("If-Match", format!("\"r{}\"", plan["revision"]))
        .json(&json!({"query": "tag in [smoke, api]"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (st, d) = s
        .post(
            &format!("/api/v1/projects/qp/test-plans/{id}/sync?dryRun=true"),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["added"], json!([2]));
}

#[tokio::test]
async fn saved_queries_per_caller() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    seed(&s, "qs").await;
    s.project("qt").await;
    let viewer = s.token("sqv", "viewer", &["qs"]).await;
    let viewer2 = s.token("sqw", "viewer", &["qs"]).await;
    let editor = s.token("sqe", "editor", &["qs"]).await;
    let base = "/api/v1/projects/qs/saved-queries";

    let (st, mine) = s
        .post(
            base,
            &viewer,
            json!({"entity": "cases", "name": "Mine", "query": "tag = smoke"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{mine}");
    assert_eq!(
        (mine["mine"].as_bool(), mine["shared"].as_bool()),
        (Some(true), Some(false))
    );
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .post(
            base,
            &viewer,
            json!({"entity": "cases", "name": "Team", "query": "tag = smoke", "shared": true}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "sharing needs cases:write");
    let (st, e) = s
        .post(
            base,
            &viewer,
            json!({"entity": "cases", "name": "Bad", "query": "tag ~ a"}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
    let (st, team) = s
        .post(
            base,
            &editor,
            json!({"entity": "cases", "name": "Team", "query": "status = draft", "shared": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{team}");
    let team_id = team["id"].as_str().unwrap().to_string();
    let (st, dup) = s
        .post(
            base,
            &editor,
            json!({"entity": "cases", "name": "Team", "query": "status = draft"}),
        )
        .await;
    assert_eq!(
        (st, dup["code"].as_str()),
        (StatusCode::CONFLICT, Some("saved_query_name_taken"))
    );

    let (_, l) = s.get(&format!("{base}?entity=cases"), &viewer2).await;
    let names: Vec<&str> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Team"]);
    let (_, l) = s.get(base, &viewer).await;
    assert_eq!(l["items"].as_array().unwrap().len(), 2);

    // Another caller's personal query is 404, whatever their permissions.
    for t in [&viewer2, &editor] {
        let (st, _) = call(
            &s,
            Method::PATCH,
            &format!("{base}/{mine_id}"),
            Some(t),
            Some(json!({"name": "X"})),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(
            &s,
            Method::DELETE,
            &format!("{base}/{mine_id}"),
            Some(t),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
    let (st, l) = s
        .get(
            &format!("/api/v1/projects/qs/cases?query={}", enc("filter = Mine")),
            &viewer2,
        )
        .await;
    assert_eq!(
        (st, l["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
    let (st, l) = s
        .get(
            &format!("/api/v1/projects/qs/cases?query={}", enc("filter = Mine")),
            &viewer,
        )
        .await;
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![1]));

    // A shared query: anybody reads it, changes need cases:write.
    let (st, _) = call(
        &s,
        Method::PATCH,
        &format!("{base}/{team_id}"),
        Some(&viewer),
        Some(json!({"query": "status = active"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("{base}/{team_id}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, l) = s
        .get(
            &format!("/api/v1/projects/qs/cases?query={}", enc("filter = Team")),
            &viewer,
        )
        .await;
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![3]));
    let (st, p) = call(
        &s,
        Method::PATCH,
        &format!("{base}/{mine_id}"),
        Some(&viewer),
        Some(json!({"query": "filter = Mine"})),
    )
    .await;
    assert_eq!(
        (st, p["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query")),
        "a query naming itself"
    );

    // IDOR: the id of another project's query.
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/projects/qt/saved-queries/{team_id}"),
        Some(ADMIN_TOKEN),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("{base}/{team_id}"),
        Some(&editor),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("{base}/not-a-uuid"),
        Some(&editor),
        None,
    )
    .await;
    assert!(
        st == StatusCode::BAD_REQUEST || st == StatusCode::NOT_FOUND,
        "{st}"
    );
    audit_has(&s, &["savedQuery.create", "savedQuery.delete"]).await;
    let (_, a) = s.get("/api/v1/audit?limit=500", ADMIN_TOKEN).await;
    let personal = a["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["action"] == "savedQuery.create" && i["detail"]["name"] == "Mine")
        .cloned()
        .unwrap_or_default();
    assert!(
        personal["detail"].get("query").is_none(),
        "personal queries are audited without the text: {personal}"
    );
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
    sqlx::query(
        "insert into replica_levels (instance, tm_level, version, seen_at) values ('old-replica', 1, '0.2.0-dev2', now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn export_job_with_a_query_waits_for_the_gate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    upgraded(&db).await;
    let mut a = Server::start(&db).await;
    seed(&a, "qx").await;
    let base = "/api/v1/projects/qx/cases/exports";
    let (st, b) = a
        .post(
            base,
            ADMIN_TOKEN,
            json!({"format": "zip", "query": "status = active"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["code"], "upgrade_in_progress");
    assert_eq!(
        b["details"],
        json!({"what": "export.query", "requiredLevel": 2})
    );
    let (st, b) = a
        .post(
            base,
            ADMIN_TOKEN,
            json!({"format": "zip", "filter": {"statuses": ["active"]}}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "a filter is an old format: {b}");
    let (st, b) = a
        .post(
            base,
            ADMIN_TOKEN,
            json!({"format": "zip", "query": "x", "filter": {}}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{b}");
    a.stop().await;

    let b2 = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    let (st, job) = b2
        .post(
            base,
            ADMIN_TOKEN,
            json!({"format": "zip", "query": "status = draft"}),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, j) = b2
            .get(&format!("/api/v1/projects/qx/tm-jobs/{id}"), ADMIN_TOKEN)
            .await;
        if j["status"] == "done" {
            assert_eq!(j["progress"]["total"], 1, "{j}");
            break;
        }
        assert!(j["status"] != "failed", "{j}");
        assert!(Instant::now() < deadline, "{j}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (st, b) = b2
        .post(
            base,
            ADMIN_TOKEN,
            json!({"format": "zip", "query": "status = nope"}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
}

/// Personal saved queries belong to the caller's owner key, not to its name: a new token that
/// reuses the name of a revoked one (or a token named like a user) does not see, change or run
/// them.
#[tokio::test]
async fn personal_saved_queries_follow_the_caller_not_the_name() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_TM_GATE", "open")]).await;
    seed(&s, "qo").await;
    let base = "/api/v1/projects/qo/saved-queries";
    let (st, first) = s
        .post(
            "/api/v1/tokens",
            ADMIN_TOKEN,
            json!({"name": "twin", "role": "viewer", "scopes": ["qo"], "expiresInDays": 1}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{first}");
    let first_token = first["token"].as_str().unwrap().to_string();
    let (st, mine) = s
        .post(
            base,
            &first_token,
            json!({"entity": "cases", "name": "Mine", "query": "tag = smoke"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{mine}");
    assert_eq!(mine["owner"].as_str(), Some("twin"));
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let first_id = first["id"]
        .as_str()
        .or(first["item"]["id"].as_str())
        .unwrap_or_else(|| panic!("token id in {first}"))
        .to_string();
    let (st, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/tokens/{first_id}"),
        Some(ADMIN_TOKEN),
        None,
    )
    .await;
    assert!(st.is_success(), "{st}");

    let second = s.token("twin", "viewer", &["qo"]).await;
    let (_, l) = s.get(&format!("{base}?entity=cases"), &second).await;
    assert_eq!(l["items"], json!([]), "{l}");
    for (m, body) in [
        (Method::PATCH, Some(json!({"query": "status = draft"}))),
        (Method::DELETE, None),
    ] {
        let (st, _) = call(&s, m, &format!("{base}/{mine_id}"), Some(&second), body).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }
    let (st, e) = s
        .get(
            &format!("/api/v1/projects/qo/cases?query={}", enc("filter = Mine")),
            &second,
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
    let (st, v) = s
        .post(
            "/api/v1/projects/qo/query/validate",
            &second,
            json!({"entity": "cases", "query": "filter = Mine"}),
        )
        .await;
    assert_eq!((st, v["ok"].as_bool()), (StatusCode::OK, Some(false)));
    let (st, own) = s
        .post(
            base,
            &second,
            json!({"entity": "cases", "name": "Mine", "query": "status = draft"}),
        )
        .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "the same name is free for another caller: {own}"
    );
    let (st, l) = s
        .get(
            &format!("/api/v1/projects/qo/cases?query={}", enc("filter = Mine")),
            &second,
        )
        .await;
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![3]));
    let (st, l) = s
        .get(
            &format!(
                "/api/v1/projects/qo/cases?query={}",
                enc("createdBy = me()")
            ),
            &second,
        )
        .await;
    assert_eq!(
        (st, numbers(&l)),
        (StatusCode::OK, vec![]),
        "me() stays the name"
    );
    let rows: Vec<String> =
        sqlx::query_scalar("select owner_key from saved_queries order by created_at")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|k| k.starts_with("token:")) && rows[0] != rows[1],
        "{rows:?}"
    );
}

/// A managed plan created from a query is a multiplying write like one created from a filter:
/// it takes the per-caller write rate (and the heavy guard).
#[tokio::test]
async fn plan_by_query_takes_the_write_rate() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_GATE", "open"),
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "1"),
        ],
    )
    .await;
    seed(&s, "qr").await;
    let editor = s.token("qre", "editor", &["qr"]).await;
    let (st, p) = s
        .post(
            "/api/v1/projects/qr/test-plans",
            &editor,
            json!({"kind": "managed", "name": "One", "query": "tag = smoke"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{p}");
    let (st, e) = s
        .post(
            "/api/v1/projects/qr/test-plans",
            &editor,
            json!({"kind": "managed", "name": "Two", "query": "tag = api"}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")),
        "{e}"
    );
}

/// The case list with a query is a heavy read: a caller runs at most
/// `FASTPROOF_HEAVY_QUERIES_PER_CALLER` of them at once.
#[tokio::test]
async fn case_list_with_a_query_takes_the_heavy_guard() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_GATE", "open"),
            ("FASTPROOF_HEAVY_QUERIES_PER_CALLER", "1"),
        ],
    )
    .await;
    seed(&s, "qh").await;
    let viewer = s.token("qhv", "viewer", &["qh"]).await;
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("lock table cases in access exclusive mode")
        .execute(&mut *lock)
        .await
        .unwrap();
    let slow = {
        let c = Server::client_only(&s.base);
        let v = viewer.clone();
        tokio::spawn(async move {
            c.get(
                &format!("/api/v1/projects/qh/cases?query={}", enc("status = active")),
                &v,
            )
            .await
        })
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity where datname = current_database() \
             and wait_event_type = 'Lock' and query ilike '%from cases%'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        if waiting > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the first list never waited on the lock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/qh/cases?query={}", enc("status = draft")),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    lock.rollback().await.unwrap();
    let (st, l) = slow.await.unwrap();
    assert_eq!((st, numbers(&l)), (StatusCode::OK, vec![1, 2]));
}
