//! Launch merges, second round: code-sync trust, hidden launches and retention, late log
//! lines and writes after finalization, the job's isolation, stable owner keys, previews
//! without locks, busy projects, triage on a created target, ingest ownership, logs of rekeyed
//! attempts, fingerprints, copied triage, numbers of merged launches, write guards, the log
//! budget, and ingest of other launches during a merge. Skipped without
//! `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use clap::Parser;
use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Status, TestIdentity};
use fastproof_store::Store;
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::merges::{LaunchMergeRequest, MergeCaller, MergeError, MergeOptions};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

const T0: i64 = 1_795_000_000_000;

fn res(id: &str, name: &str, status: &str, start: i64) -> Value {
    let mut v = json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"),
        "status": status, "start": start, "stop": start + 5});
    if status == "failed" {
        v["error"] = json!({"message": format!("{name} failed"),
            "trace": format!("java.lang.AssertionError: {name}\n\tat com.example.Suite.{name}(Suite.java:7)")});
    }
    v
}

async fn launch(s: &Server, project: &str, name: &str, token: &str) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            token,
            json!({"name": name, "vcs": {"branch": "main"}}),
        )
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

async fn shard(s: &Server, project: &str, name: &str, token: &str, items: Vec<Value>) -> String {
    let l = launch(s, project, name, token).await;
    post(s, project, &l, token, items).await;
    finish(s, project, &l, token).await;
    l
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

async fn undo(s: &Server, project: &str, token: &str, id: &Value) -> (StatusCode, Value) {
    s.post(
        &format!(
            "/api/v1/projects/{project}/merges/{}/undo",
            id.as_str().unwrap()
        ),
        token,
        json!({}),
    )
    .await
}

fn uuid(v: &str) -> Uuid {
    v.parse().unwrap()
}

async fn expire(db: &TestDb, merge: &Value) {
    sqlx::query("update launch_merges set undo_until = now() - interval '1 minute' where id = $1")
        .bind(uuid(merge.as_str().unwrap()))
        .execute(&db.pool)
        .await
        .unwrap();
}

fn caller(key: &str, can_sync: bool) -> MergeCaller {
    MergeCaller {
        name: key.into(),
        key: key.into(),
        can_sync,
        can_triage: true,
    }
}

fn late_result(name: &str, start: i64) -> ParsedResult {
    let full = format!("com.example.Late#{name}");
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: &full,
        display_name: name,
        ..Default::default()
    });
    ParsedResult {
        source_uuid: Some(format!("{name}-1")),
        name: name.into(),
        full_name: full,
        identity,
        status: Status::Passed,
        start: Some(start),
        stop: Some(start + 10),
        ..Default::default()
    }
}

async fn persist_late(db: &TestDb, store: &Store, project: &str, launch: &str, name: &str) {
    let p = store.project(project).await.unwrap().unwrap();
    let at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select created_at from launches where id = $1")
            .bind(uuid(launch))
            .fetch_one(&db.pool)
            .await
            .unwrap();
    store
        .persist_attempts(
            &LaunchRef {
                id: uuid(launch),
                project_id: p.id,
                project_key: project.into(),
                created_at: at,
            },
            None,
            vec![NewAttempt {
                client_key: format!("u:allure:{name}-1"),
                result: late_result(name, T0 + 10),
            }],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn code_sync_trust_follows_sources_and_caller() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("tr").await;
    let store = Store::new(db.pool.clone());
    let p = store.project("tr").await.unwrap().unwrap();
    let x = shard(
        &s,
        "tr",
        "ci",
        ADMIN_TOKEN,
        vec![res("1", "a", "passed", T0)],
    )
    .await;
    let src = shard(
        &s,
        "tr",
        "crafted",
        ADMIN_TOKEN,
        vec![res("2", "b", "passed", T0)],
    )
    .await;
    sqlx::query("update launches set sync_blocked = true where id = $1")
        .bind(uuid(&src))
        .execute(&db.pool)
        .await
        .unwrap();
    let req = LaunchMergeRequest {
        launch_ids: vec![uuid(&x), uuid(&src)],
        into: Some(uuid(&x)),
        ..Default::default()
    };
    let e = store
        .merge_launches(
            &p,
            &req,
            &caller("user:bob", false),
            MergeOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            MergeError::Conflict {
                code: "launch_finished",
                ..
            }
        ),
        "{e:?}"
    );
    let owner: Option<String> =
        sqlx::query_scalar("select created_by_key from launches where id = $1")
            .bind(uuid(&x))
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let owner = owner.expect("the creating caller's key is recorded");
    store
        .merge_launches(&p, &req, &caller(&owner, true), MergeOptions::default())
        .await
        .unwrap();
    let blocked: bool = sqlx::query_scalar("select sync_blocked from launches where id = $1")
        .bind(uuid(&x))
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        blocked,
        "an untrusted source makes the target untrusted for code sync"
    );

    let c = shard(
        &s,
        "tr",
        "c",
        ADMIN_TOKEN,
        vec![res("3", "c", "passed", T0)],
    )
    .await;
    let d = shard(
        &s,
        "tr",
        "d",
        ADMIN_TOKEN,
        vec![res("4", "d", "passed", T0)],
    )
    .await;
    let (m, _) = store
        .merge_launches(
            &p,
            &LaunchMergeRequest {
                launch_ids: vec![uuid(&c), uuid(&d)],
                ..Default::default()
            },
            &caller("user:bob", false),
            MergeOptions::default(),
        )
        .await
        .unwrap();
    let (blocked, trusted): (bool, bool) =
        sqlx::query_as("select sync_blocked, created_by_can_sync from launches where id = $1")
            .bind(m.target_id.unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        blocked && !trusted,
        "a caller without cases:sync makes an untrusted target"
    );
}

#[tokio::test]
async fn hidden_launches_retention_refs_and_missing_sources() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges", "retention.apply"]).await;
    s.project("hd").await;
    let store = Store::new(db.pool.clone());
    let a = shard(
        &s,
        "hd",
        "a",
        ADMIN_TOKEN,
        vec![res("1", "x", "passed", T0)],
    )
    .await;
    let b = shard(
        &s,
        "hd",
        "b",
        ADMIN_TOKEN,
        vec![res("2", "y", "passed", T0)],
    )
    .await;
    let other = shard(
        &s,
        "hd",
        "other",
        ADMIN_TOKEN,
        vec![res("3", "z", "passed", T0)],
    )
    .await;
    let att = Uuid::now_v7();
    let reference = Uuid::now_v7();
    sqlx::query(
        "insert into attachments (id, project_id, sha256, size, content_type, blob_key) \
         select $1, project_id, 'sha-merge-ref', 3, 'text/plain', 'k' from launches where id = $2",
    )
    .bind(att)
    .bind(uuid(&a))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into attachment_refs (id, attachment_id, launch_id, content_type, original_name) \
         values ($1, $2, $3, 'text/x-log', 'run.log')",
    )
    .bind(reference)
    .bind(att)
    .bind(uuid(&a))
    .execute(&db.pool)
    .await
    .unwrap();
    let (st, m) = merge(&s, "hd", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let target = m["targetId"].as_str().unwrap().to_string();

    let (st, e) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/hd/launches/{a}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_merged"))
    );
    let (st, set) = s
        .call(
            Method::PUT,
            "/api/v1/projects/hd/settings",
            Some(ADMIN_TOKEN),
            Some(json!({"retention": {"rawDays": 3650, "launchesKeep": 1, "keepImportant": true, "manualDays": 0}})),
        )
        .await;
    assert!(st.is_success(), "{set}");
    let (st, preview) = s
        .get("/api/v1/projects/hd/retention/preview", ADMIN_TOKEN)
        .await;
    assert_eq!(st, StatusCode::OK, "{preview}");
    let ids: Vec<&str> = preview["launchIds"]
        .as_array()
        .map(|v| v.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(preview["launches"], 1, "{preview}");
    assert!(
        !ids.contains(&a.as_str()) && !ids.contains(&b.as_str()),
        "{preview}"
    );
    let (_, p) = s.get("/api/v1/projects/hd", ADMIN_TOKEN).await;
    assert_eq!(p["launchCount"], 2);

    expire(&db, &m["id"]).await;
    assert_eq!(store.merge_maintenance().await.unwrap().finalized, 1);
    let moved: Uuid = sqlx::query_scalar("select launch_id from attachment_refs where id = $1")
        .bind(reference)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        moved,
        uuid(&target),
        "attachment references move to the target"
    );

    let c = shard(
        &s,
        "hd",
        "c",
        ADMIN_TOKEN,
        vec![res("4", "w", "passed", T0)],
    )
    .await;
    let (st, m2) = merge(&s, "hd", ADMIN_TOKEN, json!({"launchIds": [c, other]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m2}");
    let proj = store.project("hd").await.unwrap().unwrap();
    assert!(store.delete_launch(proj.id, "hd", uuid(&c)).await.unwrap());
    let (st, e) = undo(&s, "hd", ADMIN_TOKEN, &m2["id"]).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_source_gone")),
        "{e}"
    );
}

#[tokio::test]
async fn late_logs_and_writes_after_finalization_reach_the_target() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("lt").await;
    let store = Store::new(db.pool.clone());
    let a = shard(
        &s,
        "lt",
        "a",
        ADMIN_TOKEN,
        vec![res("1", "x", "passed", T0)],
    )
    .await;
    let b = shard(
        &s,
        "lt",
        "b",
        ADMIN_TOKEN,
        vec![res("2", "y", "passed", T0)],
    )
    .await;
    let (st, m) = merge(
        &s,
        "lt",
        ADMIN_TOKEN,
        json!({"launchIds": [a, b], "final": true}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let target = m["targetId"].as_str().unwrap().to_string();
    sqlx::query(
        "insert into attempt_log_chunks (project_id, launch_id, result_key, attempt, first_seq, last_seq, \
           line_count, bytes, lines_z) select project_id, id, '1', 0, 1, 2, 2, 2, '\\x00' from launches where id = $1",
    )
    .bind(uuid(&a))
    .execute(&db.pool)
    .await
    .unwrap();
    let r = store.merge_maintenance().await.unwrap();
    assert_eq!(r.synced, 1, "a log watermark moved: {r:?}");
    let chunks: i64 =
        sqlx::query_scalar("select count(*) from attempt_log_chunks where launch_id = $1")
            .bind(uuid(&target))
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(chunks, 1);

    expire(&db, &m["id"]).await;
    assert_eq!(store.merge_maintenance().await.unwrap().finalized, 1);
    persist_late(&db, &store, "lt", &a, "after").await;
    let r = store.merge_maintenance().await.unwrap();
    assert_eq!(r.synced, 1, "{r:?}");
    assert_eq!(
        by_name(&s, "lt", &target).await["after"]["status"],
        "passed"
    );
    let left: i64 = sqlx::query_scalar("select count(*) from results where launch_id = $1")
        .bind(uuid(&a))
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "the tombstone stays empty");
    let (_, mm) = s
        .get(
            &format!("/api/v1/projects/lt/merges/{}", m["id"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(mm["status"], "final");
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/projects/lt/launches/{target}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(store.merge_maintenance().await.unwrap().orphans_deleted, 2);
    let (_, mm) = s
        .get(
            &format!("/api/v1/projects/lt/merges/{}", m["id"].as_str().unwrap()),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        mm["status"], "deleted",
        "a final merge whose target is deleted"
    );
}

#[tokio::test]
async fn the_job_isolates_failures_and_skips_deleted_projects() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    let store = Store::new(db.pool.clone());
    let mut merges = Vec::new();
    for key in ["j1", "j2", "j3"] {
        s.project(key).await;
        let a = shard(&s, key, "a", ADMIN_TOKEN, vec![res("1", "x", "passed", T0)]).await;
        let b = shard(
            &s,
            key,
            "b",
            ADMIN_TOKEN,
            vec![res("2", "x", "failed", T0 + 9)],
        )
        .await;
        let (st, m) = merge(&s, key, ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
        assert_eq!(st, StatusCode::CREATED, "{m}");
        sqlx::query(
            "insert into launch_merge_work (merge_id, project_id, series_id) \
             select m.id, m.project_id, r.series_id from launch_merges m join results r on r.launch_id = m.target_id \
             where m.id = $1",
        )
        .bind(uuid(m["id"].as_str().unwrap()))
        .execute(&db.pool)
        .await
        .unwrap();
        merges.push(m);
    }
    sqlx::query("update projects set deleted_at = now() where key = 'j3'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut hold = db.pool.begin().await.unwrap();
    sqlx::query(
        "select st.series_id from series_stats st join results r on r.series_id = st.series_id \
         where r.launch_id = $1 for update of st",
    )
    .bind(uuid(merges[0]["targetId"].as_str().unwrap()))
    .execute(&mut *hold)
    .await
    .unwrap();
    let r = store.merge_maintenance().await.unwrap();
    hold.rollback().await.unwrap();
    assert_eq!(
        (r.errors, r.busy),
        (0, 1),
        "held rows leave the merge for the next run: {r:?}"
    );
    assert_eq!(r.analysed, 1, "the second project went on: {r:?}");
    let left: Vec<String> = sqlx::query_scalar(
        "select distinct p.key from launch_merge_work w join projects p on p.id = w.project_id order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(left, vec!["j1".to_string(), "j3".to_string()]);
    let r = store.merge_maintenance().await.unwrap();
    assert_eq!((r.errors, r.analysed), (0, 1), "{r:?}");
}

#[tokio::test]
async fn owners_are_compared_by_stable_keys() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("ow").await;
    let mint = || async {
        let (st, b) = s
            .post(
                "/api/v1/tokens",
                ADMIN_TOKEN,
                json!({"name": "ci", "role": "uploader", "scopes": ["ow"], "expiresInDays": 1}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{b}");
        (
            b["token"].as_str().unwrap().to_string(),
            b["id"].as_str().unwrap().to_string(),
        )
    };
    let (old, old_id) = mint().await;
    let a = shard(&s, "ow", "a", &old, vec![res("1", "x", "passed", T0)]).await;
    let b = shard(&s, "ow", "b", &old, vec![res("2", "y", "passed", T0)]).await;
    let (st, m) = merge(&s, "ow", &old, json!({"launchIds": [a, b]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let (st, _) = s
        .call(
            Method::DELETE,
            &format!("/api/v1/tokens/{old_id}"),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert!(st.is_success());
    let (new, _) = mint().await;
    let (st, _) = undo(&s, "ow", &new, &m["id"]).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a new token with the old name is someone else"
    );
    let c = shard(
        &s,
        "ow",
        "c",
        ADMIN_TOKEN,
        vec![res("3", "x", "passed", T0)],
    )
    .await;
    let d = shard(&s, "ow", "d", &new, vec![res("4", "y", "passed", T0)]).await;
    let (st, _) = merge(&s, "ow", &new, json!({"launchIds": [c, d]})).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "launches of someone else need triage"
    );
    let e = shard(&s, "ow", "e", &new, vec![res("5", "x", "passed", T0)]).await;
    let (st, m2) = merge(&s, "ow", &new, json!({"launchIds": [d, e]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m2}");
    let (st, _) = undo(&s, "ow", &new, &m2["id"]).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn previews_take_no_locks_and_busy_projects_answer_at_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("pv").await;
    let store = Store::new(db.pool.clone());
    let p = store.project("pv").await.unwrap().unwrap();
    let a = shard(
        &s,
        "pv",
        "a",
        ADMIN_TOKEN,
        vec![res("1", "x", "passed", T0)],
    )
    .await;
    let b = shard(
        &s,
        "pv",
        "b",
        ADMIN_TOKEN,
        vec![res("2", "y", "passed", T0)],
    )
    .await;
    let mut hold = db.pool.begin().await.unwrap();
    sqlx::query("select id from launches where id = $1 for update")
        .bind(uuid(&a))
        .execute(&mut *hold)
        .await
        .unwrap();
    let started = Instant::now();
    let (st, pv) = merge(
        &s,
        "pv",
        ADMIN_TOKEN,
        json!({"launchIds": [a, b], "dryRun": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{pv}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "a preview does not wait for row locks"
    );
    hold.rollback().await.unwrap();

    let mut hold = db.pool.begin().await.unwrap();
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 7720))")
        .bind(format!("fastproof-merge:{}", p.id))
        .execute(&mut *hold)
        .await
        .unwrap();
    let started = Instant::now();
    let (st, e) = merge(&s, "pv", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("merge_busy"))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    hold.rollback().await.unwrap();
}

#[tokio::test]
async fn undoing_a_triaged_target_needs_triage() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("tg").await;
    let ci = s.token("ci", "uploader", &["tg"]).await;
    let a = shard(&s, "tg", "a", &ci, vec![res("1", "x", "failed", T0)]).await;
    let b = shard(&s, "tg", "b", &ci, vec![res("2", "y", "passed", T0)]).await;
    let (st, m) = merge(&s, "tg", &ci, json!({"launchIds": [a, b]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let target = m["targetId"].as_str().unwrap();
    let rid = by_name(&s, "tg", target).await["x"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, types) = s.get("/api/v1/projects/tg/defect-types", ADMIN_TOKEN).await;
    let pb = types["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == "productBug")
        .unwrap()["id"]
        .clone();
    let (st, _) = s
        .post(
            "/api/v1/projects/tg/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [rid], "defectTypeId": pb, "comment": "real bug"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, e) = undo(&s, "tg", &ci, &m["id"]).await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("merge_target_triaged")),
        "{e}"
    );
    let (st, _) = undo(&s, "tg", ADMIN_TOKEN, &m["id"]).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn ingest_ownership_fingerprints_and_copied_triage_on_undo() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("un").await;
    let x = launch(&s, "un", "original", ADMIN_TOKEN).await;
    post(
        &s,
        "un",
        &x,
        ADMIN_TOKEN,
        vec![res("o1", "login", "failed", T0)],
    )
    .await;
    let src = launch(&s, "un", "rerun", ADMIN_TOKEN).await;
    post(
        &s,
        "un",
        &src,
        ADMIN_TOKEN,
        vec![
            res("k", "pending", "running", T0 + 100),
            res("r1", "login", "failed", T0 + 50),
            res("r2", "checkout", "failed", T0 + 60),
        ],
    )
    .await;
    finish(&s, "un", &src, ADMIN_TOKEN).await;
    let login_src = by_name(&s, "un", &src).await["login"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, types) = s.get("/api/v1/projects/un/defect-types", ADMIN_TOKEN).await;
    let pb = types["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["group"] == "productBug")
        .unwrap()["id"]
        .clone();
    let (st, _) = s
        .post(
            "/api/v1/projects/un/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [login_src], "defectTypeId": pb, "comment": "from the re-run"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let fp_before: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "select first_launch_id, last_launch_id from failure_fingerprints order by fingerprint",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();

    let (st, m) = merge(
        &s,
        "un",
        ADMIN_TOKEN,
        json!({"launchIds": [x, src], "into": x}),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let r = by_name(&s, "un", &x).await;
    assert_eq!(r["login"]["defect"]["comment"], "from the re-run");
    post(
        &s,
        "un",
        &x,
        ADMIN_TOKEN,
        vec![res("k", "pending", "passed", T0 + 100)],
    )
    .await;

    let (st, u) = undo(&s, "un", ADMIN_TOKEN, &m["id"]).await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let r = by_name(&s, "un", &x).await;
    assert_eq!(
        r["pending"]["status"], "passed",
        "the target's own write took the copied running attempt over and keeps it"
    );
    assert_ne!(
        r["login"]["defect"]["comment"], "from the re-run",
        "a decision copied from a source leaves with the undo"
    );
    assert!(!r.contains_key("checkout"));
    let fp_after: Vec<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "select first_launch_id, last_launch_id from failure_fingerprints order by fingerprint",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        fp_before, fp_after,
        "fingerprints point where they did before"
    );
}

#[tokio::test]
async fn logs_follow_rekeyed_attempts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    s.project("lg").await;
    let mut launches = Vec::new();
    for (name, test) in [("a", "alpha"), ("b", "beta")] {
        let l = launch(&s, "lg", name, ADMIN_TOKEN).await;
        post(
            &s,
            "lg",
            &l,
            ADMIN_TOKEN,
            vec![res("1", test, "passed", T0)],
        )
        .await;
        let (st, b) = s
            .post(
                &format!("/api/v1/projects/lg/launches/{l}/logs"),
                ADMIN_TOKEN,
                json!([{"resultId": "1", "attempt": 0, "seq": 1, "ts": T0, "level": "info", "stream": "stdout",
                    "message": format!("{test} says hello")}]),
            )
            .await;
        assert_eq!(st, StatusCode::ACCEPTED, "{b}");
        finish(&s, "lg", &l, ADMIN_TOKEN).await;
        launches.push(l);
    }
    let (st, m) = merge(&s, "lg", ADMIN_TOKEN, json!({"launchIds": launches})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert_eq!(m["counts"]["rekeyed"], 1);
    let target = m["targetId"].as_str().unwrap();
    for test in ["alpha", "beta"] {
        let rid = by_name(&s, "lg", target).await[test]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (st, logs) = s
            .get(
                &format!("/api/v1/projects/lg/launches/{target}/logs?resultId={rid}"),
                ADMIN_TOKEN,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{logs}");
        let lines: Vec<&str> = logs["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|l| l["message"].as_str())
            .collect();
        assert_eq!(lines, vec![format!("{test} says hello").as_str()], "{logs}");
    }
}

#[tokio::test]
async fn guards_numbers_and_the_log_budget() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(&db, &[("FASTPROOF_LOG_MAX_BYTES_PER_LAUNCH", "1024")]).await;
    s.project("gd").await;
    let a = shard(
        &s,
        "gd",
        "a",
        ADMIN_TOKEN,
        vec![res("1", "x", "failed", T0)],
    )
    .await;
    let b = shard(
        &s,
        "gd",
        "b",
        ADMIN_TOKEN,
        vec![res("2", "y", "passed", T0)],
    )
    .await;
    let a_result = by_name(&s, "gd", &a).await["x"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query("insert into launch_log_bytes (launch_id, bytes) values ($1, 2000)")
        .bind(uuid(&a))
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, e) = merge(&s, "gd", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(
        (st, e["code"].as_str(), e["details"]["limit"].as_str()),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            Some("merge_too_large"),
            Some("logBytes")
        )
    );
    sqlx::query("delete from launch_log_bytes where launch_id = $1")
        .bind(uuid(&a))
        .execute(&db.pool)
        .await
        .unwrap();
    let (st, m) = merge(&s, "gd", ADMIN_TOKEN, json!({"launchIds": [a, b]})).await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    let target = m["targetId"].as_str().unwrap().to_string();

    let (st, e) = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/gd/launches/{a}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", "application/octet-stream")
        .header("x-fastproof-attachment-name", "a.txt")
        .body("abc")
        .send()
        .await
        .map(|r| r.status())
        .map(|st| (st, ()))
        .unwrap();
    assert_eq!(st, StatusCode::CONFLICT, "{e:?}");
    let (st, e) = s
        .call(
            Method::PATCH,
            &format!("/api/v1/projects/gd/launches/{a}"),
            Some(ADMIN_TOKEN),
            Some(json!({"important": true})),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_merged"))
    );
    let (st, e) = s
        .post(
            "/api/v1/projects/gd/triage",
            ADMIN_TOKEN,
            json!({"resultIds": [a_result], "comment": "x"}),
        )
        .await;
    assert_eq!(
        (st, e["code"].as_str()),
        (StatusCode::CONFLICT, Some("launch_merged"))
    );

    let tf = s.dir.path().join("token");
    std::fs::write(&tf, ADMIN_TOKEN).unwrap();
    let number = s.launch_get("gd", &a).await["number"].as_i64().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rerun.xml");
    std::fs::write(
        &file,
        r#"<testsuite name="s"><testcase classname="com.example.Suite" name="z" time="0.1"/></testsuite>"#,
    )
    .unwrap();
    let args: Vec<String> = [
        "fastproof",
        "upload",
        file.to_str().unwrap(),
        "--rerun-of",
        &format!("#{number}"),
        "--wait",
        "--no-ci",
        "--url",
        &s.base,
        "--token-file",
        tf.to_str().unwrap(),
        "-p",
        "gd",
    ]
    .iter()
    .map(|v| v.to_string())
    .collect();
    let cli = fastproof_server::cli::Cli::try_parse_from(args).unwrap();
    assert_eq!(fastproof_server::cli::run(cli).await.unwrap(), 0);
    let (_, ups) = s
        .get(
            &format!("/api/v1/projects/gd/launches/{target}/uploads"),
            ADMIN_TOKEN,
        )
        .await;
    assert_eq!(
        ups["items"].as_array().unwrap().len(),
        1,
        "the number of a merged launch resolves to its target"
    );
}

#[tokio::test]
async fn ingest_of_other_launches_is_not_blocked_by_a_merge() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("nb").await;
    let tests: Vec<Value> = (0..200)
        .map(|i| res(&format!("a{i}"), &format!("t{i}"), "passed", T0 + i))
        .collect();
    let a = shard(&s, "nb", "a", ADMIN_TOKEN, tests.clone()).await;
    let b = shard(&s, "nb", "b", ADMIN_TOKEN, tests.clone()).await;
    let other = launch(&s, "nb", "other", ADMIN_TOKEN).await;

    // Hold the merge in its first phase: it waits for this lock when it copies containers,
    // after it copied the attempts of all the shared tests.
    let mut hold = db.pool.begin().await.unwrap();
    sqlx::query("lock table launch_containers in exclusive mode")
        .execute(&mut *hold)
        .await
        .unwrap();
    let base = s.base.clone();
    let (a2, b2) = (a.clone(), b.clone());
    let merging = tokio::spawn(async move {
        let c = Server::client_only(&base);
        merge(&c, "nb", ADMIN_TOKEN, json!({"launchIds": [a2, b2]})).await
    });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!merging.is_finished(), "the merge waits for the held lock");
    let started = Instant::now();
    let items: Vec<Value> = (0..200)
        .map(|i| res(&format!("o{i}"), &format!("t{i}"), "failed", T0 + 1000 + i))
        .collect();
    post(&s, "nb", &other, ADMIN_TOKEN, items).await;
    let took = started.elapsed();
    assert!(
        !merging.is_finished(),
        "the merge is still in its first phase"
    );
    hold.rollback().await.unwrap();
    let (st, m) = merging.await.unwrap();
    assert_eq!(st, StatusCode::CREATED, "{m}");
    assert!(
        took < Duration::from_secs(3),
        "ingest of the same tests into another launch took {took:?} during a merge"
    );
}
