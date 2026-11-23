//! Launch merges on a real PostgreSQL: shards into a new launch, a re-run into the original,
//! attempt and flakiness semantics, transitions and series statistics, hidden sources, undo,
//! idempotency, late attempts, final merges and tombstones, permissions, limits and the CLI.
//! Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeMap;

use clap::Parser;
use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Status, TestIdentity};
use fastproof_store::Store;
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

const T0: i64 = 1_795_000_000_000;

fn res(id: &str, name: &str, status: &str, start: i64) -> Value {
    let mut v = json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"),
        "status": status, "start": start, "stop": start + 5});
    if status == "failed" {
        v["error"] = json!({"message": format!("{name} failed")});
    }
    v
}

async fn launch(
    s: &Server,
    project: &str,
    name: &str,
    token: &str,
    external: Option<&str>,
) -> String {
    let mut body = json!({"name": name, "vcs": {"branch": "main"}});
    if let Some(e) = external {
        body["externalId"] = json!(e);
    }
    let (st, b) = s
        .post(&format!("/api/v1/projects/{project}/launches"), token, body)
        .await;
    assert!(st.is_success(), "{b}");
    b["id"].as_str().unwrap().to_string()
}

async fn post(s: &Server, project: &str, lid: &str, token: &str, items: Vec<Value>) {
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/results"),
            token,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    assert!(out["rejected"].as_array().unwrap().is_empty(), "{out}");
}

async fn finish(s: &Server, project: &str, lid: &str, token: &str) {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{lid}/finish"),
            token,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

async fn by_name(s: &Server, project: &str, lid: &str) -> BTreeMap<String, Value> {
    s.results(project, lid, "")
        .await
        .into_iter()
        .map(|r| (r["name"].as_str().unwrap().to_string(), r))
        .collect()
}

async fn merge(s: &Server, project: &str, token: &str, body: Value) -> (StatusCode, Value) {
    s.post(
        &format!("/api/v1/projects/{project}/launches/merge"),
        token,
        body,
    )
    .await
}

async fn list_ids(s: &Server, project: &str) -> Vec<String> {
    let (st, b) = s
        .get(&format!("/api/v1/projects/{project}/launches"), ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap().to_string())
        .collect()
}

async fn triage(s: &Server, project: &str, result: &str, comment: &str) {
    let (_, types) = s
        .get(
            &format!("/api/v1/projects/{project}/defect-types"),
            ADMIN_TOKEN,
        )
        .await;
    let pb = types["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == "productBug" && t["builtIn"] == true)
        .unwrap()["id"]
        .clone();
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/triage"),
            ADMIN_TOKEN,
            json!({"resultIds": [result], "defectTypeId": pb, "comment": comment}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

async fn stats(db: &TestDb, project: &str, name: &str) -> (String, i32) {
    sqlx::query_as(
        "select st.last_statuses, st.runs_tracked from series_stats st join series se on se.id = st.series_id \
         join test_cases tc on tc.id = se.test_case_id join projects p on p.id = tc.project_id \
         where p.key = $1 and tc.display_name = $2",
    )
    .bind(project)
    .bind(name)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn shards_merge_into_a_new_launch_and_undo() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("mg").await;

    let base = launch(&s, "mg", "nightly", ADMIN_TOKEN, None).await;
    post(
        &s,
        "mg",
        &base,
        ADMIN_TOKEN,
        vec![res("b1", "t1", "passed", T0), res("b2", "t2", "passed", T0)],
    )
    .await;
    finish(&s, "mg", &base, ADMIN_TOKEN).await;

    let a = launch(&s, "mg", "nightly shard 1", ADMIN_TOKEN, Some("ci-7-1")).await;
    post(
        &s,
        "mg",
        &a,
        ADMIN_TOKEN,
        vec![
            res("1", "t1", "failed", T0 + 1000),
            res("2", "t2", "failed", T0 + 1000),
        ],
    )
    .await;
    finish(&s, "mg", &a, ADMIN_TOKEN).await;
    let t1 = by_name(&s, "mg", &a).await["t1"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    triage(&s, "mg", &t1, "known checkout bug").await;
    let b = launch(&s, "mg", "nightly shard 2", ADMIN_TOKEN, Some("ci-7-2")).await;
    post(
        &s,
        "mg",
        &b,
        ADMIN_TOKEN,
        vec![
            res("1", "t3", "passed", T0 + 2000),
            res("2", "t2", "passed", T0 + 3000),
        ],
    )
    .await;
    finish(&s, "mg", &b, ADMIN_TOKEN).await;
    assert_eq!(stats(&db, "mg", "t2").await, ("PFP".into(), 3));

    let (st, preview) = merge(
        &s,
        "mg",
        ADMIN_TOKEN,
        json!({"launchIds": [a, b], "dryRun": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{preview}");
    assert_eq!(preview["status"], "preview");
    assert_eq!(preview["counts"]["attempts"], 4);
    assert_eq!(preview["counts"]["sharedSeries"], 1);
    assert!(preview["id"].is_null());
    assert_eq!(
        list_ids(&s, "mg").await.len(),
        3,
        "a preview changes nothing"
    );

    let (st, m) = merge(
        &s,
        "mg",
        ADMIN_TOKEN,
        json!({"launchIds": [a, b], "name": "nightly 7"}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert_eq!(m["status"], "active");
    assert_eq!(m["createdTarget"], true);
    assert_eq!(m["counts"]["attempts"], 4);
    assert_eq!(
        m["counts"]["rekeyed"], 2,
        "client ids 1 and 2 name other tests in shard 2"
    );
    assert_eq!(m["counts"]["results"], 3);
    assert!(m["undoUntil"].is_string());
    let target = m["targetId"].as_str().unwrap().to_string();
    let merge_id = m["id"].as_str().unwrap().to_string();

    let t = s.launch_get("mg", &target).await;
    assert_eq!(t["name"], "nightly 7");
    assert_eq!(t["status"], "finished");
    assert_eq!(t["counts"]["total"], 3);
    assert_eq!(t["counts"]["passed"], 2);
    assert_eq!(t["counts"]["failed"], 1);
    assert_eq!(t["counts"]["flaky"], 1);
    assert_eq!(t["vcs"]["branch"], "main");
    let r = by_name(&s, "mg", &target).await;
    assert_eq!(
        r["t2"]["status"], "passed",
        "the later attempt is the result"
    );
    assert_eq!(r["t2"]["attemptCount"], 2);
    assert_eq!(r["t2"]["flakyInRun"], true);
    assert_eq!(r["t1"]["status"], "failed");
    assert_eq!(r["t1"]["transition"], "regressed");
    assert_eq!(
        r["t1"]["defect"]["source"], "user",
        "triage follows the winning attempt"
    );
    assert_eq!(r["t1"]["defect"]["comment"], "known checkout bug");
    assert_eq!(
        r["t3"]["transition"], "new",
        "the hidden shards are not history"
    );
    assert!(r["t2"].get("transition").is_none());

    let ids = list_ids(&s, "mg").await;
    assert_eq!(ids, vec![target.clone(), base.clone()]);
    let hidden = s.launch_get("mg", &a).await;
    assert_eq!(hidden["mergedInto"], target.as_str());
    assert_eq!(stats(&db, "mg", "t2").await, ("PP".into(), 2));
    assert_eq!(stats(&db, "mg", "t3").await, ("P".into(), 1));

    let (st, e) = s
        .post(
            &format!("/api/v1/projects/mg/launches/{a}/results"),
            ADMIN_TOKEN,
            json!([res("9", "t9", "passed", T0)]),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "launch_merged");
    assert_eq!(e["details"]["mergedInto"], target.as_str());
    let (st, e) = s
        .post(
            &format!("/api/v1/projects/mg/launches/{a}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_merged"))
    );
    let joined = launch(&s, "mg", "nightly shard 1", ADMIN_TOKEN, Some("ci-7-1")).await;
    assert_eq!(
        joined, target,
        "the CI id of a shard joins the merged launch"
    );

    let (st, again) = merge(
        &s,
        "mg",
        ADMIN_TOKEN,
        json!({"launchIds": [b, a], "name": "nightly 7"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["id"], m["id"]);
    assert_eq!(by_name(&s, "mg", &target).await.len(), 3);
    let (st, e) = merge(&s, "mg", ADMIN_TOKEN, json!({"launchIds": [a, base]})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_merged"))
    );

    let (st, ms) = s
        .get(
            &format!("/api/v1/projects/mg/launches/{a}/merges"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ms["items"][0]["id"], m["id"]);
    assert_eq!(ms["items"][0]["sources"].as_array().unwrap().len(), 2);

    let (st, u) = s
        .post(
            &format!("/api/v1/projects/mg/merges/{merge_id}/undo"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    assert_eq!(u["status"], "undone");
    assert_eq!(u["undoneBy"], "admin");
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/mg/launches/{target}"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let mut ids = list_ids(&s, "mg").await;
    ids.sort();
    let mut want = vec![a.clone(), b.clone(), base.clone()];
    want.sort();
    assert_eq!(ids, want);
    assert!(s.launch_get("mg", &a).await.get("mergedInto").is_none());
    assert_eq!(stats(&db, "mg", "t2").await, ("PFP".into(), 3));
    assert_eq!(by_name(&s, "mg", &a).await["t1"]["status"], "failed");
    let (st, e) = s
        .post(
            &format!("/api/v1/projects/mg/merges/{merge_id}/undo"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_not_active"))
    );
    let (_, p) = s.get("/api/v1/projects/mg", ADMIN_TOKEN).await;
    assert_eq!(p["launchCount"], 3);
}

#[tokio::test]
async fn rerun_merges_into_the_original_with_a_running_target() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("rr").await;
    let original = launch(&s, "rr", "build 12", ADMIN_TOKEN, None).await;
    post(
        &s,
        "rr",
        &original,
        ADMIN_TOKEN,
        vec![
            res("o1", "login", "failed", T0),
            res("o2", "cart", "passed", T0),
        ],
    )
    .await;
    let login = by_name(&s, "rr", &original).await["login"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    triage(&s, "rr", &login, "flaky login").await;
    let rerun = launch(&s, "rr", "build 12 rerun", ADMIN_TOKEN, None).await;
    post(
        &s,
        "rr",
        &rerun,
        ADMIN_TOKEN,
        vec![res("o1", "login", "passed", T0 + 60_000)],
    )
    .await;

    let (st, e) = merge(
        &s,
        "rr",
        ADMIN_TOKEN,
        json!({"launchIds": [original, rerun], "into": original}),
    )
    .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_running")),
        "{e}"
    );
    finish(&s, "rr", &rerun, ADMIN_TOKEN).await;

    let (st, m) = merge(
        &s,
        "rr",
        ADMIN_TOKEN,
        json!({"launchIds": [original, rerun], "into": original}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert_eq!(m["createdTarget"], false);
    assert_eq!(m["targetId"], original.as_str());
    assert_eq!(
        m["counts"]["rekeyed"], 1,
        "the re-run reused the client id o1 for a new attempt"
    );
    let r = by_name(&s, "rr", &original).await;
    assert_eq!(r["login"]["status"], "passed");
    assert_eq!(r["login"]["attemptCount"], 2);
    assert_eq!(r["login"]["flakyInRun"], true);
    assert_eq!(
        r["login"]["defect"]["comment"], "flaky login",
        "{}",
        r["login"]
    );
    let l = s.launch_get("rr", &original).await;
    assert_eq!(l["status"], "running", "the target keeps running");
    assert_eq!(l["counts"]["passed"], 2);
    assert_eq!(l["counts"]["failed"], 0);

    post(
        &s,
        "rr",
        &original,
        ADMIN_TOKEN,
        vec![res("o3", "login", "failed", T0 + 120_000)],
    )
    .await;
    let r = by_name(&s, "rr", &original).await;
    assert_eq!(
        r["login"]["attemptCount"], 3,
        "ingest after the merge adds attempts"
    );
    assert_eq!(r["login"]["status"], "failed");

    let (st, u) = s
        .post(
            &format!(
                "/api/v1/projects/rr/merges/{}/undo",
                m["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let r = by_name(&s, "rr", &original).await;
    assert_eq!(r["login"]["attemptCount"], 2, "the copied attempt leaves");
    assert_eq!(r["login"]["status"], "failed");
    assert_eq!(by_name(&s, "rr", &rerun).await["login"]["status"], "passed");
    assert!(list_ids(&s, "rr").await.contains(&rerun));
    let _ = db;
}

#[tokio::test]
async fn busy_late_final_and_deleted_targets() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("bz").await;
    let store = Store::new(db.pool.clone());
    let a = launch(&s, "bz", "a", ADMIN_TOKEN, None).await;
    post(&s, "bz", &a, ADMIN_TOKEN, vec![res("1", "x", "passed", T0)]).await;
    finish(&s, "bz", &a, ADMIN_TOKEN).await;
    let b = launch(&s, "bz", "b", ADMIN_TOKEN, None).await;
    post(&s, "bz", &b, ADMIN_TOKEN, vec![res("2", "y", "passed", T0)]).await;
    finish(&s, "bz", &b, ADMIN_TOKEN).await;

    sqlx::query("insert into uploads (id, project_id, launch_id, status) select $1, project_id, id, 'queued' from launches where id = $2")
        .bind(Uuid::now_v7())
        .bind(Uuid::parse_str(&b).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, e) = merge(&s, "bz", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_busy")),
        "{e}"
    );
    assert_eq!(e["details"]["launchIds"][0], b.as_str());
    sqlx::query("update uploads set status = 'done' where launch_id = $1")
        .bind(Uuid::parse_str(&b).unwrap())
        .execute(&db.pool)
        .await
        .unwrap();

    let (st, m) = merge(&s, "bz", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let target = m["targetId"].as_str().unwrap().to_string();
    let mid = Uuid::parse_str(m["id"].as_str().unwrap()).unwrap();

    let project = store.project("bz").await.unwrap().unwrap();
    let la: (Uuid, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select id, created_at from launches where id = $1")
            .bind(Uuid::parse_str(&a).unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: "com.example.Late#late",
        display_name: "late",
        ..Default::default()
    });
    let late = ParsedResult {
        source_uuid: Some("late-1".into()),
        name: "late".into(),
        full_name: "com.example.Late#late".into(),
        identity,
        status: Status::Passed,
        start: Some(T0 + 10),
        stop: Some(T0 + 20),
        ..Default::default()
    };
    store
        .persist_attempts(
            &LaunchRef {
                id: la.0,
                project_id: project.id,
                project_key: "bz".into(),
                created_at: la.1,
            },
            None,
            vec![NewAttempt {
                client_key: "u:allure:late-1".into(),
                result: late,
            }],
        )
        .await
        .unwrap();
    assert!(!by_name(&s, "bz", &target).await.contains_key("late"));
    let rep = store.merge_maintenance().await.unwrap();
    assert_eq!(rep.synced, 1);
    let r = by_name(&s, "bz", &target).await;
    assert_eq!(
        r["late"]["status"], "passed",
        "late attempts reach the target"
    );
    let (_, mm) = s
        .get(&format!("/api/v1/projects/bz/merges/{mid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(mm["counts"]["lateAttempts"], 1);
    assert_eq!(store.merge_maintenance().await.unwrap().synced, 0);

    sqlx::query("update launch_merges set undo_until = now() - interval '1 minute' where id = $1")
        .bind(mid)
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, e) = s
        .post(
            &format!("/api/v1/projects/bz/merges/{mid}/undo"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_expired"))
    );
    assert_eq!(store.merge_maintenance().await.unwrap().finalized, 1);
    let (_, mm) = s
        .get(&format!("/api/v1/projects/bz/merges/{mid}"), ADMIN_TOKEN)
        .await;
    assert_eq!(mm["status"], "final");
    let left: i64 = sqlx::query_scalar("select count(*) from results where launch_id = any($1)")
        .bind(vec![
            Uuid::parse_str(&a).unwrap(),
            Uuid::parse_str(&b).unwrap(),
        ])
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "finalized sources keep no results");
    assert_eq!(s.launch_get("bz", &a).await["mergedInto"], target.as_str());
    assert_eq!(by_name(&s, "bz", &target).await.len(), 3);

    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/bz/launches/{target}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(store.merge_maintenance().await.unwrap().orphans_deleted, 2);
    let (st, _) = s
        .get(&format!("/api/v1/projects/bz/launches/{a}"), ADMIN_TOKEN)
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "sources go with their deleted target"
    );

    let c = launch(&s, "bz", "c", ADMIN_TOKEN, None).await;
    post(&s, "bz", &c, ADMIN_TOKEN, vec![res("3", "z", "passed", T0)]).await;
    finish(&s, "bz", &c, ADMIN_TOKEN).await;
    let d = launch(&s, "bz", "d", ADMIN_TOKEN, None).await;
    post(
        &s,
        "bz",
        &d,
        ADMIN_TOKEN,
        vec![res("4", "z", "failed", T0 + 5)],
    )
    .await;
    finish(&s, "bz", &d, ADMIN_TOKEN).await;
    let (st, f) = merge(
        &s,
        "bz",
        ADMIN_TOKEN,
        json!({"launchIds": [c, d], "final": true}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{f}");
    assert_eq!(
        (f["status"].as_str(), f["final"].as_bool()),
        (Some("active"), Some(true))
    );
    assert!(f["undoUntil"].is_null());
    let (st, e) = s
        .post(
            &format!(
                "/api/v1/projects/bz/merges/{}/undo",
                f["id"].as_str().unwrap()
            ),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_final"))
    );
    let r = by_name(&s, "bz", f["targetId"].as_str().unwrap()).await;
    assert_eq!(r["z"]["attemptCount"], 2);
    assert_eq!(r["z"]["status"], "failed");
}

#[tokio::test]
async fn permissions_limits_and_mixed_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pm").await;
    let viewer = s.token("viewer", "viewer", &["pm"]).await;
    let ci = s.token("ci", "uploader", &["pm"]).await;
    let editor = s.token("alice", "editor", &["pm"]).await;

    let mine1 = launch(&s, "pm", "shard", &ci, None).await;
    post(&s, "pm", &mine1, &ci, vec![res("1", "a", "passed", T0)]).await;
    finish(&s, "pm", &mine1, &ci).await;
    let mine2 = launch(&s, "pm", "shard", &ci, None).await;
    post(&s, "pm", &mine2, &ci, vec![res("2", "b", "passed", T0)]).await;
    finish(&s, "pm", &mine2, &ci).await;
    let other = launch(&s, "pm", "manual", ADMIN_TOKEN, None).await;
    post(
        &s,
        "pm",
        &other,
        ADMIN_TOKEN,
        vec![res("3", "c", "passed", T0)],
    )
    .await;
    finish(&s, "pm", &other, ADMIN_TOKEN).await;

    let (st, _) = s
        .call(
            Method::POST,
            "/api/v1/projects/pm/launches/merge",
            None,
            Some(json!({"launchIds": [mine1, mine2]})),
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = merge(&s, "pm", &viewer, json!({"launchIds": [mine1, mine2]})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = merge(&s, "nope", &viewer, json!({"launchIds": [mine1, mine2]})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "same answer whether the project exists or not"
    );
    let (st, _) = merge(&s, "pm", &ci, json!({"launchIds": [mine1, other]})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a CI token merges only its own launches"
    );
    let (st, _) = merge(
        &s,
        "pm",
        &editor,
        json!({"launchIds": [mine1, mine2], "final": true}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a merge without undo needs settings:write"
    );
    for bad in [
        json!({"launchIds": [mine1]}),
        json!({"launchIds": [mine1, mine1]}),
        json!({"launchIds": [mine1, mine2], "into": other}),
        json!({"launchIds": [mine1, mine2], "name": ""}),
    ] {
        let (st, e) = merge(&s, "pm", ADMIN_TOKEN, bad.clone()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad} {e}");
    }
    let (st, _) = merge(
        &s,
        "pm",
        ADMIN_TOKEN,
        json!({"launchIds": [mine1, mine2], "extra": 1}),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields are refused"
    );
    let (st, _) = merge(
        &s,
        "pm",
        ADMIN_TOKEN,
        json!({"launchIds": [mine1, Uuid::now_v7()]}),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, m) = merge(&s, "pm", &ci, json!({"launchIds": [mine1, mine2]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let (st, _) = s
        .get(
            &format!("/api/v1/projects/pm/merges/{}", m["id"].as_str().unwrap()),
            &viewer,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = s
        .post(
            &format!(
                "/api/v1/projects/pm/merges/{}/undo",
                m["id"].as_str().unwrap()
            ),
            &viewer,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .post(
            &format!(
                "/api/v1/projects/pm/merges/{}/undo",
                m["id"].as_str().unwrap()
            ),
            &ci,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "the one who merged may undo");

    let (st, m) = merge(
        &s,
        "pm",
        &editor,
        json!({"launchIds": [mine1, other], "into": other}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let (st, _) = s
        .post(
            &format!(
                "/api/v1/projects/pm/merges/{}/undo",
                m["id"].as_str().unwrap()
            ),
            &editor,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let dbg = {
        let (st, b) = s
            .post(
                "/api/v1/projects/pm/launches",
                ADMIN_TOKEN,
                json!({"name": "dbg", "debug": true, "vcs": {"branch": "main"}}),
            )
            .await;
        assert!(st.is_success());
        b["id"].as_str().unwrap().to_string()
    };
    finish(&s, "pm", &dbg, ADMIN_TOKEN).await;
    let (st, e) = merge(&s, "pm", ADMIN_TOKEN, json!({"launchIds": [mine1, dbg]})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_mixed"))
    );
    let (st, b) = s
        .post(
            "/api/v1/projects/pm/launches",
            ADMIN_TOKEN,
            json!({"name": "dev", "vcs": {"branch": "dev"}}),
        )
        .await;
    assert!(st.is_success());
    let dev = b["id"].as_str().unwrap().to_string();
    finish(&s, "pm", &dev, ADMIN_TOKEN).await;
    let (st, e) = merge(&s, "pm", ADMIN_TOKEN, json!({"launchIds": [mine1, dev]})).await;
    assert_eq!(
        (st, e["details"]["field"].as_str()),
        (StatusCode::CONFLICT, Some("branch"))
    );

    let mut s2 = Server::start_with(&db, &[("FASTPROOF_MERGE_MAX_ATTEMPTS", "1")]).await;
    let (st, e) = merge(&s2, "pm", ADMIN_TOKEN, json!({"launchIds": [mine1, mine2]})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("merge_too_large"))
    );
    s2.stop().await;
}

#[tokio::test]
async fn cli_merge_undo_and_rerun_of() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cm").await;
    let tf = s.dir.path().join("token");
    std::fs::write(&tf, ADMIN_TOKEN).unwrap();
    let run = |args: Vec<String>| async move {
        let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
        fastproof_server::cli::run(cli).await
    };
    let common = |cmd: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = vec!["fastproof".into()];
        v.extend(cmd.iter().map(|s| s.to_string()));
        v.extend([
            "--url".into(),
            s.base.clone(),
            "--token-file".into(),
            tf.to_string_lossy().into(),
            "-p".into(),
            "cm".into(),
        ]);
        v
    };
    let a = launch(&s, "cm", "a", ADMIN_TOKEN, None).await;
    post(&s, "cm", &a, ADMIN_TOKEN, vec![res("1", "x", "failed", T0)]).await;
    finish(&s, "cm", &a, ADMIN_TOKEN).await;
    let b = launch(&s, "cm", "b", ADMIN_TOKEN, None).await;
    post(&s, "cm", &b, ADMIN_TOKEN, vec![res("2", "y", "passed", T0)]).await;
    finish(&s, "cm", &b, ADMIN_TOKEN).await;

    assert!(
        run(common(&["launch", "merge", "1", "#2", "--final"]))
            .await
            .is_err(),
        "--final needs --yes"
    );
    assert_eq!(
        run(common(&["launch", "merge", "1", "#2", "--dry-run"]))
            .await
            .unwrap(),
        0
    );
    assert_eq!(list_ids(&s, "cm").await.len(), 2);
    assert_eq!(
        run(common(&["launch", "merge", "#1", "2", "--into", "1"]))
            .await
            .unwrap(),
        0
    );
    assert_eq!(list_ids(&s, "cm").await, vec![a.clone()]);
    let (_, ms) = s
        .get(
            &format!("/api/v1/projects/cm/launches/{a}/merges"),
            ADMIN_TOKEN,
        )
        .await;
    let mid = ms["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        run(common(&["launch", "undo-merge", &mid])).await.unwrap(),
        0
    );
    assert_eq!(list_ids(&s, "cm").await.len(), 2);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rerun.xml");
    std::fs::write(
        &file,
        r#"<testsuite name="s"><testcase classname="com.example.Suite" name="x" time="0.1"/></testsuite>"#,
    )
    .unwrap();
    assert_eq!(
        run(common(&[
            "upload",
            file.to_str().unwrap(),
            "--rerun-of",
            "#1",
            "--wait",
            "--no-ci"
        ]))
        .await
        .unwrap(),
        0
    );
    let l = s.launch_get("cm", &a).await;
    assert!(l["counts"]["total"].as_i64().unwrap() >= 1, "{l}");
    let (_, ups) = s
        .get(
            &format!("/api/v1/projects/cm/launches/{a}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        ups["items"].as_array().unwrap().len(),
        1,
        "the re-run went into launch #1"
    );
}

#[tokio::test]
async fn ingest_into_the_target_while_merging() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("cc").await;
    let target = launch(&s, "cc", "running", ADMIN_TOKEN, None).await;
    post(
        &s,
        "cc",
        &target,
        ADMIN_TOKEN,
        vec![res("t0", "shared", "failed", T0)],
    )
    .await;
    let mut sources = Vec::new();
    for i in 0..3 {
        let l = launch(&s, "cc", &format!("shard {i}"), ADMIN_TOKEN, None).await;
        let items: Vec<Value> = (0..40)
            .map(|j| {
                res(
                    &format!("s{i}-{j}"),
                    &format!("s{i}t{j}"),
                    "passed",
                    T0 + 10 + j,
                )
            })
            .chain([res(
                &format!("s{i}-x"),
                "shared",
                "passed",
                T0 + 1000 * (i + 1),
            )])
            .collect();
        post(&s, "cc", &l, ADMIN_TOKEN, items).await;
        finish(&s, "cc", &l, ADMIN_TOKEN).await;
        sources.push(l);
    }
    let base = s.base.clone();
    let tgt = target.clone();
    let writer = tokio::spawn(async move {
        let c = Server::client_only(&base);
        for k in 0..20 {
            post(
                &c,
                "cc",
                &tgt,
                ADMIN_TOKEN,
                vec![res(
                    &format!("live-{k}"),
                    &format!("live{k}"),
                    "passed",
                    T0 + 5000 + k,
                )],
            )
            .await;
        }
    });
    let mut ids = sources.clone();
    ids.push(target.clone());
    let (st, m) = merge(
        &s,
        "cc",
        ADMIN_TOKEN,
        json!({"launchIds": ids, "into": target}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    writer.await.unwrap();
    let r = by_name(&s, "cc", &target).await;
    assert_eq!(
        r.len(),
        1 + 3 * 40 + 20,
        "every result of the shards and of the concurrent writer is there"
    );
    assert_eq!(r["shared"]["attemptCount"], 4);
    assert_eq!(r["shared"]["status"], "passed");
    assert_eq!(r["shared"]["flakyInRun"], true);
    let l = s.launch_get("cc", &target).await;
    assert_eq!(l["counts"]["total"], 141);
    assert_eq!(l["counts"]["passed"], 141);
}
