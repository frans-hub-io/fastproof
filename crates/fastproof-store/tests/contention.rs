//! Concurrent persist transactions of launches that share test identities: every launch of a
//! project upserts the same test cases and series, in shuffled order. They must not deadlock
//! (PostgreSQL's deadlock counter stays put), fail, or lose or duplicate rows.
//!
//! Needs `FASTPROOF_TEST_DATABASE_URL`; without it the test passes without doing anything.

use std::time::Instant;

use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Status, Step, TestIdentity};
use fastproof_store::Store;
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::launches::LaunchStart;
use fastproof_store::projects::CreateProject;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const LAUNCHES: usize = 12;
const TESTS: usize = 600;
const CHUNK: usize = 150;

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
            .max_connections(LAUNCHES as u32 + 4)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

fn result(i: usize, launch: usize) -> ParsedResult {
    let full_name = format!("shop.Suite{}#case{i}", i % 30);
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: &full_name,
        display_name: &format!("case {i}"),
        ..Default::default()
    });
    let status = if (i + launch).is_multiple_of(9) {
        Status::Failed
    } else {
        Status::Passed
    };
    ParsedResult {
        source_uuid: Some(format!("u{launch}-{i}")),
        name: identity.display_name.clone(),
        full_name,
        identity,
        status,
        start: Some(1_795_000_000_000 + i as i64),
        stop: Some(1_795_000_000_010 + i as i64),
        steps: vec![Step {
            name: "step".into(),
            status,
            ..Default::default()
        }],
        format: SourceFormat::AllureResults,
        ..Default::default()
    }
}

/// The order a launch reports its tests in: a shuffle seeded by the launch.
fn shuffled(launch: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..TESTS).collect();
    let mut s = 0x9e37_79b9_7f4a_7c15u64 ^ (launch as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
    v
}

async fn deadlocks(pool: &PgPool) -> i64 {
    let _ = sqlx::query("select pg_stat_force_next_flush()")
        .execute(pool)
        .await;
    sqlx::query_scalar("select deadlocks from pg_stat_database where datname = current_database()")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_launches_with_shared_tests_do_not_deadlock() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    let store = Store::new(db.pool.clone());
    let CreateProject::Created(project) = store.create_project("shop", "Shop", "").await.unwrap()
    else {
        panic!("exists")
    };
    let mut launches = Vec::new();
    for i in 0..LAUNCHES {
        let (l, _) = store
            .start_launch(
                &project,
                &LaunchStart {
                    name: format!("run {i}"),
                    ..Default::default()
                },
                "tester",
            )
            .await
            .unwrap();
        launches.push(LaunchRef {
            id: l.id,
            project_id: project.id,
            project_key: project.key.clone(),
            created_at: l.created_at,
        });
    }
    let before = deadlocks(&db.pool).await;
    let started = Instant::now();
    let tasks: Vec<_> = launches
        .iter()
        .enumerate()
        .map(|(li, launch)| {
            let store = store.clone();
            let launch = launch.clone();
            tokio::spawn(async move {
                let order = shuffled(li);
                let mut errors = Vec::new();
                for part in order.chunks(CHUNK) {
                    let attempts: Vec<NewAttempt> = part
                        .iter()
                        .map(|&i| NewAttempt {
                            client_key: format!("r:t{i}"),
                            result: result(i, li),
                        })
                        .collect();
                    if let Err(e) = store.persist_attempts(&launch, None, attempts).await {
                        errors.push(format!("{e:#}"));
                    }
                }
                errors
            })
        })
        .collect();
    let mut errors = Vec::new();
    for t in tasks {
        errors.extend(t.await.unwrap());
    }
    let elapsed = started.elapsed();
    let after = deadlocks(&db.pool).await;
    eprintln!(
        "{LAUNCHES} launches x {TESTS} shared tests in chunks of {CHUNK}: {:.2} s, {} errors, {} deadlocks",
        elapsed.as_secs_f64(),
        errors.len(),
        after - before
    );
    assert!(
        errors.is_empty(),
        "persist failed: {:?}",
        &errors[..errors.len().min(3)]
    );
    assert_eq!(after - before, 0, "PostgreSQL detected deadlocks");
    let count = |sql: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count("select count(*) from test_cases").await, TESTS as i64);
    assert_eq!(count("select count(*) from series").await, TESTS as i64);
    assert_eq!(
        count("select count(*) from attempts").await,
        (LAUNCHES * TESTS) as i64
    );
    assert_eq!(
        count("select count(*) from results").await,
        (LAUNCHES * TESTS) as i64
    );
    assert_eq!(
        count("select count(*) from series_stats").await,
        TESTS as i64
    );
    assert_eq!(
        count("select coalesce(sum(count_total), 0)::bigint from launches").await,
        (LAUNCHES * TESTS) as i64
    );
}
