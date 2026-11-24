//! One project's held launch row (a merge holds its launches for its whole first phase) does
//! not hold up other projects: the merge job, the gate queue and mute expiry leave that
//! project for a later run instead of waiting for it while rows of other projects stay
//! locked. Skipped without `FASTPROOF_TEST_DATABASE_URL`.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const T0: i64 = 1_795_000_000_000;

fn res(id: &str, name: &str, status: &str, start: i64) -> Value {
    json!({"id": id, "name": name, "fullName": format!("com.example.Suite#{name}"),
        "status": status, "start": start, "stop": start + 5})
}

async fn shard(s: &Server, project: &str, name: &str, items: Vec<Value>) -> String {
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches"),
            ADMIN_TOKEN,
            json!({"name": name, "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let l = b["id"].as_str().unwrap().to_string();
    let (st, out) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{l}/results"),
            ADMIN_TOKEN,
            Value::Array(items),
        )
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{out}");
    let (st, b) = s
        .post(
            &format!("/api/v1/projects/{project}/launches/{l}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    l
}

async fn merged(s: &Server, db: &TestDb, key: &str) -> Value {
    s.project(key).await;
    let a = shard(s, key, "a", vec![res("1", "x", "passed", T0)]).await;
    let b = shard(s, key, "b", vec![res("2", "x", "failed", T0 + 9)]).await;
    let (st, m) = s
        .post(
            &format!("/api/v1/projects/{key}/launches/merge"),
            ADMIN_TOKEN,
            json!({"launchIds": [a, b]}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{m}");
    sqlx::query(
        "insert into launch_merge_work (merge_id, project_id, series_id) \
         select m.id, m.project_id, r.series_id from launch_merges m join results r on r.launch_id = m.target_id \
         where m.id = $1",
    )
    .bind(m["id"].as_str().unwrap().parse::<Uuid>().unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    m
}

async fn held(db: &TestDb, launch: &str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut hold = db.pool.begin().await.unwrap();
    sqlx::query("select id from launches where id = $1 for update")
        .bind(launch.parse::<Uuid>().unwrap())
        .execute(&mut *hold)
        .await
        .unwrap();
    hold
}

async fn work_left(db: &TestDb, merge: &Value) -> i64 {
    sqlx::query_scalar("select count(*) from launch_merge_work where merge_id = $1")
        .bind(merge["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// The target row of one project's merge is held (as a merge, an undo or a manual run of that
/// project holds it): the job leaves that merge for its next run at once, finishes the merge
/// of the other project, and does not count the held one as a failure. Before, the job waited
/// 5 s five times on the held row, and the gate queue (one statement over the launches of
/// every project) waited on it too while it held the other project's target, so that merge
/// timed out as well.
#[tokio::test]
async fn a_held_target_does_not_hold_up_the_merges_of_other_projects() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    let first = merged(&s, &db, "i1").await;
    let second = merged(&s, &db, "i2").await;
    let hold = held(&db, first["targetId"].as_str().unwrap()).await;
    let store = Store::new(db.pool.clone());
    let started = Instant::now();
    let r = store.merge_maintenance().await.unwrap();
    let took = started.elapsed();
    let (left_first, left_second) = (work_left(&db, &first).await, work_left(&db, &second).await);
    hold.rollback().await.unwrap();
    assert_eq!((r.analysed, r.busy, r.errors), (1, 1, 0), "{r:?}");
    assert!(
        left_first > 0 && left_second == 0,
        "{left_first} {left_second}"
    );
    assert!(took < Duration::from_secs(10), "the job took {took:?}");
    let r = store.merge_maintenance().await.unwrap();
    assert_eq!((r.analysed, r.busy, r.errors), (1, 0, 0), "{r:?}");
    assert_eq!(work_left(&db, &first).await, 0);
}

/// Gate evaluations are queued for the launches of other projects while one launch row is
/// held; the held launch waits in the consumer's pending list and is queued after.
#[tokio::test]
async fn gates_are_queued_around_a_held_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    let store = Store::new(db.pool.clone());
    let consumer = "isolation-test";
    store
        .consume_finished_launches(consumer, Duration::ZERO, 500)
        .await
        .unwrap();
    s.project("g1").await;
    s.project("g2").await;
    let a = shard(&s, "g1", "a", vec![res("1", "x", "passed", T0)]).await;
    let b = shard(&s, "g2", "b", vec![res("1", "x", "passed", T0)]).await;
    let hold = held(&db, &a).await;
    let consumed = tokio::time::timeout(
        Duration::from_secs(10),
        store.consume_finished_launches(consumer, Duration::ZERO, 500),
    )
    .await;
    let pending: Vec<Uuid> =
        sqlx::query_scalar("select pending from integration_cursors where name = $1")
            .bind(consumer)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    // The other project's launch row is free again: a merge there can lock it.
    let mut other = db.pool.begin().await.unwrap();
    sqlx::query("set local lock_timeout = '2s'")
        .execute(&mut *other)
        .await
        .unwrap();
    let locked = sqlx::query("select id from launches where id = $1 for update")
        .bind(b.parse::<Uuid>().unwrap())
        .execute(&mut *other)
        .await;
    other.rollback().await.unwrap();
    hold.rollback().await.unwrap();
    assert!(
        consumed.is_ok_and(|r| r.is_ok()),
        "the gate queue waited on the held launch"
    );
    assert_eq!(pending, vec![a.parse::<Uuid>().unwrap()]);
    assert!(locked.is_ok(), "{locked:?}");
    let gate = |l: String| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from launch_gates where launch_id = $1")
                .bind(l.parse::<Uuid>().unwrap())
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(gate(b.clone()).await, 1);
    store
        .consume_finished_launches(consumer, Duration::ZERO, 500)
        .await
        .unwrap();
    let pending: Vec<Uuid> =
        sqlx::query_scalar("select pending from integration_cursors where name = $1")
            .bind(consumer)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(pending.is_empty(), "{pending:?}");
    assert_eq!(gate(a.clone()).await, 1);
}

/// Mutes of other projects end while a launch row of one project is held; that project's
/// mutes end at the next run.
#[tokio::test]
async fn mutes_of_other_projects_end_around_a_held_launch() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["mutes.expire"]).await;
    let store = Store::new(db.pool.clone());
    s.project("m1").await;
    s.project("m2").await;
    let a = shard(&s, "m1", "a", vec![res("1", "x", "failed", T0)]).await;
    let b = shard(&s, "m2", "b", vec![res("1", "x", "failed", T0)]).await;
    for l in [&a, &b] {
        let l = l.parse::<Uuid>().unwrap();
        sqlx::query(
            "insert into series_mutes (series_id, project_id, until) \
             select r.series_id, l.project_id, now() - interval '1 minute' \
             from results r join launches l on l.id = r.launch_id where r.launch_id = $1",
        )
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("update results set muted = true where launch_id = $1")
            .bind(l)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let muted = |l: String| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("select muted from results where launch_id = $1")
                .bind(l.parse::<Uuid>().unwrap())
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let hold = held(&db, &a).await;
    let expired = tokio::time::timeout(Duration::from_secs(10), store.expire_mutes()).await;
    let (still_a, still_b) = (muted(a.clone()).await, muted(b.clone()).await);
    hold.rollback().await.unwrap();
    assert!(
        matches!(expired, Ok(Ok(1))),
        "mute expiry waited on the held launch: {expired:?}"
    );
    assert!(still_a && !still_b, "{still_a} {still_b}");
    assert_eq!(store.expire_mutes().await.unwrap(), 1);
    assert!(!muted(a.clone()).await);
    assert_eq!(s.launch_get("m1", &a).await["counts"]["muted"], 0);
}
