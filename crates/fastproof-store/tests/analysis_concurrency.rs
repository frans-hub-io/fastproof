//! The analysis of launches stored at the same time, and the log budget of deleted launches:
//! verdicts (transition, flakiness) and series statistics follow the order in which launches
//! commit, two launches of a series without statistics both count, history skips other
//! branches and debug runs beyond the history depth, and a deleted launch gets no log budget.
//!
//! Needs `FASTPROOF_TEST_DATABASE_URL`; without it the tests pass without doing anything.

use std::time::Duration;

use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Status, TestIdentity};
use fastproof_store::Project;
use fastproof_store::Store;
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::launches::LaunchStart;
use fastproof_store::projects::CreateProject;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

#[path = "support/testdb.rs"]
mod testdb;

struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        fastproof_store::migrate(&pool).await.unwrap();
        Some(Self { admin, name, pool })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

fn attempt(test: &str, status: Status) -> NewAttempt {
    let full_name = format!("shop.Checkout#{test}");
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: &full_name,
        display_name: test,
        ..Default::default()
    });
    NewAttempt {
        client_key: format!("r:{test}"),
        result: ParsedResult {
            name: identity.display_name.clone(),
            full_name,
            identity,
            status,
            start: Some(1_795_000_000_000),
            stop: Some(1_795_000_000_010),
            format: SourceFormat::AllureResults,
            ..Default::default()
        },
    }
}

async fn project(store: &Store) -> Project {
    let CreateProject::Created(p) = store.create_project("shop", "Shop", "").await.unwrap() else {
        panic!("exists")
    };
    *p
}

async fn launch(store: &Store, p: &Project, debug: bool) -> LaunchRef {
    let (l, _) = store
        .start_launch(
            p,
            &LaunchStart {
                name: "run".into(),
                debug: Some(debug),
                ..Default::default()
            },
            "tester",
        )
        .await
        .unwrap();
    LaunchRef {
        id: l.id,
        project_id: p.id,
        project_key: p.key.clone(),
        created_at: l.created_at,
    }
}

async fn transition(pool: &PgPool, launch: Uuid) -> Option<String> {
    sqlx::query_scalar("select transition from results where launch_id = $1")
        .bind(launch)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn ring(pool: &PgPool) -> (String, i32) {
    sqlx::query_as("select last_statuses, runs_tracked from series_stats")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Launch A (older, failed) is stored but not committed while launch B (newer, passed) is
/// analysed: B waits for A's statistics, then judges by a history that includes A (fixed), as
/// it would had it started after A committed.
#[tokio::test]
async fn a_launch_waiting_for_another_judges_by_its_result() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    for _ in 0..3 {
        let l = launch(&store, &p, false).await;
        store
            .persist_attempts(&l, None, vec![attempt("pays", Status::Passed)])
            .await
            .unwrap();
    }
    let a = launch(&store, &p, false).await;
    let b = launch(&store, &p, false).await;
    let mut tx = db.pool.begin().await.unwrap();
    fastproof_store::ingest::persist_attempts_in(
        &mut tx,
        &a,
        None,
        vec![attempt("pays", Status::Failed)],
    )
    .await
    .unwrap();
    let task = tokio::spawn({
        let store = store.clone();
        let b = b.clone();
        async move {
            store
                .persist_attempts(&b, None, vec![attempt("pays", Status::Passed)])
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!task.is_finished(), "B waits for the statistics A holds");
    tx.commit().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        transition(&db.pool, a.id).await.as_deref(),
        Some("regressed")
    );
    assert_eq!(
        transition(&db.pool, b.id).await.as_deref(),
        Some("fixed"),
        "B saw A"
    );
    assert_eq!(ring(&db.pool).await, ("PPPFP".to_string(), 5));
}

/// A series that has no statistics yet (only a debug run so far) stored by two launches at the
/// same time: both runs are counted.
#[tokio::test]
async fn two_launches_of_a_series_without_statistics_both_count() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let d = launch(&store, &p, true).await;
    store
        .persist_attempts(&d, None, vec![attempt("pays", Status::Passed)])
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from series_stats")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "debug runs keep no statistics");
    let a = launch(&store, &p, false).await;
    let b = launch(&store, &p, false).await;
    let mut tx = db.pool.begin().await.unwrap();
    fastproof_store::ingest::persist_attempts_in(
        &mut tx,
        &a,
        None,
        vec![attempt("pays", Status::Failed)],
    )
    .await
    .unwrap();
    let task = tokio::spawn({
        let store = store.clone();
        let b = b.clone();
        async move {
            store
                .persist_attempts(&b, None, vec![attempt("pays", Status::Passed)])
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    tx.commit().await.unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(ring(&db.pool).await, ("FP".to_string(), 2));
}

/// The latest results of a series come from debug runs (as many as the history depth): the
/// history reaches past them to the earlier run of the branch.
#[tokio::test]
async fn history_reaches_past_debug_runs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let first = launch(&store, &p, false).await;
    store
        .persist_attempts(&first, None, vec![attempt("pays", Status::Passed)])
        .await
        .unwrap();
    for _ in 0..12 {
        let d = launch(&store, &p, true).await;
        store
            .persist_attempts(&d, None, vec![attempt("pays", Status::Failed)])
            .await
            .unwrap();
    }
    let last = launch(&store, &p, false).await;
    store
        .persist_attempts(&last, None, vec![attempt("pays", Status::Failed)])
        .await
        .unwrap();
    assert_eq!(
        transition(&db.pool, last.id).await.as_deref(),
        Some("regressed"),
        "the run before the debug runs counts"
    );
    assert_eq!(ring(&db.pool).await, ("PF".to_string(), 2));
}

/// The log budget of a launch: charged up to the limit, refused beyond it, and refused (not an
/// error) once the launch is deleted.
#[tokio::test]
async fn log_budget_refuses_deleted_launches() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p, false).await;
    assert!(store.charge_log_bytes(l.id, 60, 100).await.unwrap());
    assert!(!store.charge_log_bytes(l.id, 60, 100).await.unwrap());
    assert!(store.charge_log_bytes(l.id, 40, 100).await.unwrap());
    assert!(
        !store
            .charge_log_bytes(Uuid::now_v7(), 10, 100)
            .await
            .unwrap()
    );
    assert!(store.delete_launch(p.id, &p.key, l.id).await.unwrap());
    assert!(!store.charge_log_bytes(l.id, 1, 100).await.unwrap());
}
