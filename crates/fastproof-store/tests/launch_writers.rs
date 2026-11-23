//! Several writers of one launch at once (a big launch spread over Kafka partitions, CI shards
//! posting into one launch): they do not wait for each other unless they store the same test,
//! the launch counters and result times stay equal to a recount, every attempt belongs to its
//! result, and triage, rule tasks and mute expiry running meanwhile meet no deadlock.
//!
//! Needs `FASTPROOF_TEST_DATABASE_URL`; without it the tests pass without doing anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Stage, Status, TestIdentity};
use fastproof_store::analysis::rules::{
    CompiledRule, MuteAction, NewRule, RuleAction, RuleChange, RuleMatch,
};
use fastproof_store::analysis::triage::{MuteRequest, TriageRequest};
use fastproof_store::ingest::{LaunchRef, NewAttempt, persist_attempts_in, reserve_attempt_seq};
use fastproof_store::launches::LaunchStart;
use fastproof_store::projects::CreateProject;
use fastproof_store::{Project, Store};
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
            .max_connections(16)
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

const BASE: i64 = 1_795_000_000_000;

fn attempt(test: usize, key: &str, status: Status, start: i64) -> NewAttempt {
    let full_name = format!("shop.Cart#t{test}");
    let display = format!("t{test}");
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: &full_name,
        display_name: &display,
        ..Default::default()
    });
    NewAttempt {
        client_key: key.to_string(),
        result: ParsedResult {
            name: identity.display_name.clone(),
            full_name,
            identity,
            status,
            start: Some(start),
            stop: Some(start + 10),
            format: SourceFormat::AllureResults,
            ..Default::default()
        },
    }
}

fn running(test: usize, key: &str, start: i64) -> NewAttempt {
    let mut a = attempt(test, key, Status::Unknown, start);
    a.result.stage = Stage::Running;
    a.result.stop = None;
    a
}

async fn project(store: &Store) -> Project {
    let CreateProject::Created(p) = store.create_project("shop", "Shop", "").await.unwrap() else {
        panic!("exists")
    };
    *p
}

async fn launch(store: &Store, p: &Project) -> LaunchRef {
    let (l, _) = store
        .start_launch(
            p,
            &LaunchStart {
                name: "run".into(),
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

/// Opens a transaction that has persisted `attempts` and is not committed yet.
async fn open_writer(
    pool: &PgPool,
    l: &LaunchRef,
    attempts: Vec<NewAttempt>,
) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut conn = pool.acquire().await.unwrap();
    let seq0 = reserve_attempt_seq(&mut conn, l.id, attempts.len())
        .await
        .unwrap();
    drop(conn);
    let mut tx = pool.begin().await.unwrap();
    persist_attempts_in(&mut tx, l, None, attempts, seq0)
        .await
        .unwrap();
    tx
}

type Counters = (i32, i32, i32, i32, i32, i32, i32, i32);
type Times = (Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>);

async fn counters(pool: &PgPool, l: Uuid) -> (Counters, Times) {
    let c: Counters = sqlx::query_as(
        "select count_total, count_passed, count_failed, count_broken, count_skipped, count_unknown, \
           count_flaky, count_muted from launches where id = $1",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    let t: Times =
        sqlx::query_as("select first_result_at, last_result_at from launches where id = $1")
            .bind(l)
            .fetch_one(pool)
            .await
            .unwrap();
    (c, t)
}

async fn recounted(pool: &PgPool, l: Uuid) -> (Counters, Times) {
    let c: Counters = sqlx::query_as(
        "select count(*)::int, (count(*) filter (where status = 'passed'))::int, \
           (count(*) filter (where status = 'failed'))::int, (count(*) filter (where status = 'broken'))::int, \
           (count(*) filter (where status = 'skipped'))::int, (count(*) filter (where status = 'unknown'))::int, \
           (count(*) filter (where flaky_in_run))::int, (count(*) filter (where muted))::int \
         from results where launch_id = $1",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    let t: Times = sqlx::query_as(
        "select min(started_at) filter (where run_item_id is null), \
           max(coalesce(stopped_at, started_at)) filter (where run_item_id is null) \
         from results where launch_id = $1",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    (c, t)
}

/// Counters and times equal a recount; attempts belong to results of their own series, each
/// result counts its attempts and shows its latest one; `seq` is unique and handed out.
async fn assert_consistent(pool: &PgPool, l: Uuid) {
    assert_eq!(
        counters(pool, l).await,
        recounted(pool, l).await,
        "counters differ from a recount"
    );
    let orphans: i64 = sqlx::query_scalar(
        "select count(*) from attempts a where a.launch_id = $1 and not exists (select 1 from results r \
           where r.id = a.result_id and r.series_id = a.series_id)",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(orphans, 0, "attempts without their result");
    let wrong: i64 = sqlx::query_scalar(
        "select count(*) from results r where r.launch_id = $1 and (r.attempt_count <> \
           (select count(*) from attempts a where a.result_id = r.id) or r.status <> \
           (select a.status from attempts a where a.result_id = r.id \
            order by (a.started_at is not null) desc, a.started_at desc nulls last, a.seq desc limit 1))",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(wrong, 0, "results that do not match their attempts");
    let (n, distinct, max, seq): (i64, i64, Option<i64>, i64) = sqlx::query_as(
        "select count(*), count(distinct a.seq), max(a.seq), (select attempt_seq from launches where id = $1) \
         from attempts a where a.launch_id = $1",
    )
    .bind(l)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, distinct, "attempt seq values repeat");
    assert!(max.unwrap_or(0) <= seq, "an attempt seq beyond attempt_seq");
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

/// A writer storing other tests of the launch waits for another writer only at the launch row,
/// which writers take for two short statements: the reservation of attempt numbers before
/// their transaction and the counter update right before their commit. Nothing else of the
/// other writer (its test cases, series, results, statistics) holds it up.
#[tokio::test]
async fn writers_of_other_tests_wait_only_at_the_launch_row() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p).await;
    let a = open_writer(&db.pool, &l, vec![attempt(1, "r:a", Status::Failed, BASE)]).await;
    let task = tokio::spawn({
        let store = store.clone();
        let l = l.clone();
        async move {
            store
                .persist_attempts(&l, None, vec![attempt(2, "r:b", Status::Passed, BASE - 5)])
                .await
        }
    });
    let mut waiting = None;
    for _ in 0..100 {
        waiting = sqlx::query_scalar::<_, String>(
            "select query from pg_stat_activity where datname = current_database() \
             and wait_event_type = 'Lock' and pid <> pg_backend_pid()",
        )
        .fetch_optional(&db.pool)
        .await
        .unwrap();
        if waiting.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let waiting = waiting.expect("the second writer did not wait for the open one");
    assert!(
        waiting.starts_with("update launches set attempt_seq")
            || waiting.contains("with old as (select first_result_at"),
        "the second writer waits at another statement: {waiting}"
    );
    a.commit().await.unwrap();
    let b = task.await.unwrap().unwrap();
    assert_eq!(b.accepted, 1);
    let ((total, passed, failed, ..), (first, _)) = counters(&db.pool, l.id).await;
    assert_eq!((total, passed, failed), (2, 1, 1));
    assert_eq!(first.map(|t| t.timestamp_millis()), Some(BASE - 5));
    assert_consistent(&db.pool, l.id).await;
}

/// Two writers of the same test take turns: the second waits for (or runs again after) the
/// first, and the result counts both attempts.
#[tokio::test]
async fn writers_of_the_same_test_take_turns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p).await;
    // A new result (no row yet) and an existing one.
    store
        .persist_attempts(&l, None, vec![attempt(2, "r:x0", Status::Failed, BASE)])
        .await
        .unwrap();
    let a = open_writer(
        &db.pool,
        &l,
        vec![
            attempt(1, "r:a", Status::Failed, BASE),
            attempt(2, "r:x1", Status::Failed, BASE + 100),
        ],
    )
    .await;
    let task = tokio::spawn({
        let store = store.clone();
        let l = l.clone();
        async move {
            store
                .persist_attempts(
                    &l,
                    None,
                    vec![
                        attempt(1, "r:b", Status::Passed, BASE + 1000),
                        attempt(2, "r:x2", Status::Passed, BASE + 1000),
                    ],
                )
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !task.is_finished(),
        "the second writer of a test did not wait"
    );
    a.commit().await.unwrap();
    task.await.unwrap().unwrap();
    let rows: Vec<(String, i32, bool)> = sqlx::query_as(
        "select status, attempt_count, flaky_in_run from results where launch_id = $1 order by name",
    )
    .bind(l.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![("passed".into(), 2, true), ("passed".into(), 3, true)]
    );
    assert_consistent(&db.pool, l.id).await;
}

const TESTS: usize = 120;
const WRITERS: usize = 8;

/// Eight writers store three attempts of each of 120 tests, every attempt of a test from
/// another writer, some first reported running by one writer and completed by another, later
/// attempts moving the earliest result time: counters and times equal a recount at the end.
#[tokio::test]
async fn many_writers_of_one_launch_count_exactly() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p).await;
    let mut tasks = Vec::new();
    for w in 0..WRITERS {
        let store = store.clone();
        let l = l.clone();
        tasks.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            for t in 0..TESTS {
                for k in 0..3usize {
                    if (t + k) % WRITERS != w {
                        continue;
                    }
                    let start = BASE + t as i64 * 10 + k as i64 * 100_000;
                    let status = [Status::Failed, Status::Broken, Status::Passed][k];
                    if k == 1 && t % 3 == 0 {
                        // Reported running here, completed by the next writer.
                        mine.push(running(t, &format!("r:t{t}:{k}"), start));
                    } else {
                        mine.push(attempt(t, &format!("r:t{t}:{k}"), status, start));
                    }
                }
                if t % 3 == 0 && (t + 2) % WRITERS == w {
                    let start = BASE + t as i64 * 10 + 100_000;
                    mine.push(attempt(t, &format!("r:t{t}:1"), Status::Broken, start));
                }
            }
            for part in mine.chunks(7) {
                store
                    .persist_attempts(&l, None, part.to_vec())
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_consistent(&db.pool, l.id).await;
    let (n, running): (i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where status = 'running') from attempts where launch_id = $1",
    )
    .bind(l.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n as usize, TESTS * 3);
    let ((total, passed, ..), _) = counters(&db.pool, l.id).await;
    assert_eq!((total, passed), (TESTS as i32, TESTS as i32));
    assert_eq!(running, 0, "a running attempt was not completed");
}

/// Ingest of new attempts into a launch while triage mutes and unmutes its results, a rule
/// task with a mute action applies to it and mute expiry lifts mutes: every writer locks the
/// result rows in id order and the launch row last, so PostgreSQL never has to break a
/// deadlock, and `count_muted` equals a recount.
#[tokio::test]
async fn triage_rule_tasks_and_mute_expiry_do_not_deadlock_with_ingest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = Arc::new(project(&store).await);
    let l = launch(&store, &p).await;
    let first: Vec<NewAttempt> = (0..TESTS)
        .map(|t| attempt(t, &format!("r:t{t}:0"), Status::Failed, BASE + t as i64))
        .collect();
    store.persist_attempts(&l, None, first).await.unwrap();
    let RuleChange::Done(rule) = store
        .create_rule(
            p.id,
            &NewRule {
                name: "mute all".into(),
                enabled: Some(false),
                order: None,
                matcher: RuleMatch::default(),
                action: RuleAction {
                    mute: Some(MuteAction { days: Some(1) }),
                    ..Default::default()
                },
            },
            "ann",
        )
        .await
        .unwrap()
    else {
        panic!("rule")
    };
    let rule = Arc::new(CompiledRule::compile(*rule).unwrap());
    let dl0 = deadlocks(&db.pool).await;
    let done = Arc::new(AtomicBool::new(false));
    let mut tasks: Vec<tokio::task::JoinHandle<Vec<String>>> = Vec::new();
    {
        let store = store.clone();
        let l = l.clone();
        let done = done.clone();
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            for round in 1..=5usize {
                let mut ids: Vec<usize> = (0..TESTS).collect();
                if round % 2 == 0 {
                    ids.reverse();
                }
                for part in ids.chunks(30) {
                    let batch: Vec<NewAttempt> = part
                        .iter()
                        .map(|&t| {
                            attempt(
                                t,
                                &format!("r:t{t}:{round}"),
                                Status::Failed,
                                BASE + t as i64 + round as i64 * 1000,
                            )
                        })
                        .collect();
                    if let Err(e) = store.persist_attempts(&l, None, batch).await {
                        errors.push(format!("ingest: {e:#}"));
                    }
                }
            }
            done.store(true, Ordering::SeqCst);
            errors
        }));
    }
    {
        let store = store.clone();
        let p = p.clone();
        let pool = db.pool.clone();
        let done = done.clone();
        let launch_id = l.id;
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            let mut round = 0u32;
            while !done.load(Ordering::SeqCst) || round < 3 {
                round += 1;
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "select id from results where launch_id = $1 order by id desc",
                )
                .bind(launch_id)
                .fetch_all(&pool)
                .await
                .unwrap();
                let req = TriageRequest {
                    result_ids: Some(ids),
                    mute: Some(MuteRequest {
                        until: Some(Utc::now() - chrono::Duration::seconds(1)),
                        reason: "ended".into(),
                    }),
                    ..Default::default()
                };
                match store.triage(p.id, &p.key, &req, "ann").await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => errors.push(format!("triage refused: {e:?}")),
                    Err(e) => errors.push(format!("triage: {e:#}")),
                }
            }
            errors
        }));
    }
    {
        let store = store.clone();
        let p = p.clone();
        let l = l.clone();
        let done = done.clone();
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            let mut round = 0u32;
            while !done.load(Ordering::SeqCst) || round < 3 {
                round += 1;
                if let Err(e) = store.apply_rule_to_launch(p.id, &p.key, l.id, &rule).await {
                    errors.push(format!("rule task: {e:#}"));
                }
                if let Err(e) = store.expire_mutes().await {
                    errors.push(format!("mute expiry: {e:#}"));
                }
            }
            errors
        }));
    }
    let mut errors = Vec::new();
    for t in tasks {
        errors.extend(t.await.unwrap());
    }
    assert!(errors.is_empty(), "{:?}", &errors[..errors.len().min(5)]);
    assert_eq!(
        deadlocks(&db.pool).await,
        dl0,
        "PostgreSQL broke a deadlock"
    );
    assert_consistent(&db.pool, l.id).await;
}

/// Attachment content an upload stored after its launch was deleted is recorded without a
/// link (a blob without a row would never be deleted), then collected like any unlinked
/// attachment: its blob is queued for deletion. A hash another launch uses keeps its row.
#[tokio::test]
async fn attachments_of_a_deleted_launch_are_collected() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p).await;
    let item = |sha: &str| fastproof_store::attachments::NewAttachment {
        sha256: sha.into(),
        size: 3,
        content_type: "text/plain".into(),
        original_name: Some("out.txt".into()),
        blob_key: format!("projects/shop/attachments/{}/{sha}", &sha[..2]),
    };
    store
        .insert_attachments(p.id, l.id, &[item("aa01")])
        .await
        .unwrap();
    store
        .keep_unlinked_attachments(p.id, &[item("aa01"), item("bb02"), item("bb02")])
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from attachments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(
        store
            .collect_attachments(Duration::ZERO, 100)
            .await
            .unwrap(),
        1
    );
    let queued: Vec<String> = sqlx::query_scalar("select prefix from blob_deletions")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        queued,
        vec!["projects/shop/attachments/bb/bb02".to_string()]
    );
}

/// Results stored after their launch finished (a Kafka writer catching up after the reporter
/// finished the launch) queue the launch for code sync again, so cases see them; a sync in
/// progress keeps its lease and the launch is taken again once it ends.
#[tokio::test]
async fn late_results_queue_code_sync_again() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let l = launch(&store, &p).await;
    store
        .persist_attempts(&l, None, vec![attempt(1, "r:a", Status::Passed, BASE)])
        .await
        .unwrap();
    let queued = |pool: PgPool, id: Uuid| async move {
        sqlx::query_as::<_, (Option<String>, bool)>(
            "select lease_owner, coalesce(lease_until > now(), false) from case_sync_queue where launch_id = $1",
        )
        .bind(id)
        .fetch_optional(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        queued(db.pool.clone(), l.id).await,
        None,
        "a running launch is not queued"
    );
    store
        .finish_launch(p.id, &p.key, l.id, "finished")
        .await
        .unwrap();
    assert!(queued(db.pool.clone(), l.id).await.is_some());
    // The sync ran and took the launch off the queue; a late result arrives.
    sqlx::query("delete from case_sync_queue where launch_id = $1")
        .bind(l.id)
        .execute(&db.pool)
        .await
        .unwrap();
    store
        .persist_attempts(&l, None, vec![attempt(2, "r:b", Status::Passed, BASE)])
        .await
        .unwrap();
    assert_eq!(queued(db.pool.clone(), l.id).await, Some((None, false)));
    // A sync in progress: it keeps its lease, the launch stays queued after it.
    sqlx::query(
        "update case_sync_queue set lease_owner = 'w1', lease_until = now() + interval '10 minutes' where launch_id = $1",
    )
    .bind(l.id)
    .execute(&db.pool)
    .await
    .unwrap();
    store
        .persist_attempts(&l, None, vec![attempt(3, "r:c", Status::Passed, BASE)])
        .await
        .unwrap();
    assert_eq!(queued(db.pool.clone(), l.id).await, Some((None, true)));
    let done =
        sqlx::query("delete from case_sync_queue where launch_id = $1 and lease_owner = 'w1'")
            .bind(l.id)
            .execute(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        done.rows_affected(),
        0,
        "the finished sync must not take the late results off the queue"
    );
}

/// Results of a launch whose monthly partition is gone (retention dropped it while the launch
/// stayed, as closed manual runs do) go into a partition of their month again, not into the
/// default partition that retention never drops.
#[tokio::test]
async fn results_of_an_old_launch_do_not_land_in_the_default_partition() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let p = project(&store).await;
    let mut l = launch(&store, &p).await;
    let old = Utc::now() - chrono::Duration::days(800);
    sqlx::query("update launches set created_at = $2 where id = $1")
        .bind(l.id)
        .bind(old)
        .execute(&db.pool)
        .await
        .unwrap();
    l.created_at = old;
    let part = format!("results_p{}", old.format("%Y%m"));
    let exists = |pool: PgPool, name: String| async move {
        sqlx::query_scalar::<_, bool>("select to_regclass($1) is not null")
            .bind(name)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert!(!exists(db.pool.clone(), part.clone()).await);
    store
        .persist_attempts(&l, None, vec![attempt(1, "r:a", Status::Passed, BASE)])
        .await
        .unwrap();
    assert!(
        exists(db.pool.clone(), part.clone()).await,
        "{part} was not created"
    );
    for t in ["results_default", "attempts_default"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {t}")))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "rows in {t}");
    }
    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {part}")))
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
}
