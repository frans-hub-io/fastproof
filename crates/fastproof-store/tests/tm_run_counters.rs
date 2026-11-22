//! Launch counters under manual writes: step writes lock only the run item, status changes add
//! their change of the counters to the launch row at the end of their transaction. These tests
//! check that the counters always equal a full recount: for every status transition, and after
//! many testers, an automated ingest into the same launch, mutes and a close ran at once
//! (without deadlocks). They need `FASTPROOF_TEST_DATABASE_URL`; without it they pass without
//! doing anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fastproof_model::{IdentityInput, ParsedResult, SourceFormat, Status, TestIdentity};
use fastproof_store::analysis::triage::{MuteRequest, TriageRequest};
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::launches::LaunchStart;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::projection::CountDelta;
use fastproof_store::tm::{
    self, CaseBody, CaseStep, ManualUntestedTo, RunItemPatch, RunStatus, RunStepWrite, StepStatus,
    TmError, TmLimits,
};
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
    async fn new(connections: u32) -> Option<(Self, Store)> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(connections)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        fastproof_store::migrate(&pool).await.unwrap();
        let store = Store::new(pool.clone());
        Some((Self { admin, name, pool }, store))
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

async fn project(store: &Store, key: &str) -> Project {
    match store.create_project(key, key, "").await.unwrap() {
        CreateProject::Created(p) => *p,
        CreateProject::Exists => panic!("project {key} exists"),
    }
}

/// A launch with a manual run of `cases` cases of `steps` steps each; returns the launch and
/// its run items in case order.
async fn manual_launch(
    db: &TestDb,
    store: &Store,
    p: &Project,
    cases: usize,
    steps: usize,
) -> (LaunchRef, Vec<Uuid>) {
    let mut numbers = Vec::new();
    for c in 0..cases {
        let body = CaseBody {
            description: "d".into(),
            preconditions: String::new(),
            steps: (0..steps)
                .map(|s| CaseStep {
                    id: format!("s_{}", s + 1),
                    action: format!("do {s}"),
                    expected: "ok".into(),
                    ..Default::default()
                })
                .collect(),
            links: vec![],
        };
        let mut conn = db.pool.acquire().await.unwrap();
        let row = tm::testutil::insert_case(&mut conn, p.id, &format!("case {c}"), &body, None)
            .await
            .unwrap();
        numbers.push(row.number);
    }
    let (l, _) = store
        .start_launch(
            p,
            &LaunchStart {
                name: "combined".into(),
                ..Default::default()
            },
            "tester",
        )
        .await
        .unwrap();
    let added = store
        .add_run_items(
            p,
            l.id,
            &tm::RunItemsAdd {
                case_numbers: Some(numbers),
                plan_id: None,
                assignee: None,
            },
            &TmLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(added.added, cases as i64);
    let items: Vec<Uuid> = sqlx::query_scalar(
        "select ri.id from run_items ri join cases c on c.id = ri.case_id where ri.launch_id = $1 order by c.number",
    )
    .bind(l.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    (
        LaunchRef {
            id: l.id,
            project_id: p.id,
            project_key: p.key.clone(),
            created_at: l.created_at,
        },
        items,
    )
}

async fn counters(pool: &PgPool, launch: Uuid) -> CountDelta {
    let r: (i32, i32, i32, i32, i32, i32, i32, i32, i32, i32) = sqlx::query_as(
        "select count_total, count_passed, count_failed, count_broken, count_skipped, count_unknown, \
           count_flaky, count_muted, count_untested, count_in_progress from launches where id = $1",
    )
    .bind(launch)
    .fetch_one(pool)
    .await
    .unwrap();
    CountDelta {
        total: r.0,
        passed: r.1,
        failed: r.2,
        broken: r.3,
        skipped: r.4,
        unknown: r.5,
        flaky: r.6,
        muted: r.7,
        untested: r.8,
        in_progress: r.9,
    }
}

/// The counters a full recount gives (computed in a transaction that is rolled back).
async fn recounted(pool: &PgPool, launch: Uuid) -> CountDelta {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("select 1 from launches where id = $1 for update")
        .bind(launch)
        .execute(&mut *tx)
        .await
        .unwrap();
    Store::recount_launch(&mut tx, launch).await.unwrap();
    let r: (i32, i32, i32, i32, i32, i32, i32, i32, i32, i32) = sqlx::query_as(
        "select count_total, count_passed, count_failed, count_broken, count_skipped, count_unknown, \
           count_flaky, count_muted, count_untested, count_in_progress from launches where id = $1",
    )
    .bind(launch)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    CountDelta {
        total: r.0,
        passed: r.1,
        failed: r.2,
        broken: r.3,
        skipped: r.4,
        unknown: r.5,
        flaky: r.6,
        muted: r.7,
        untested: r.8,
        in_progress: r.9,
    }
}

/// Counters equal a full recount, and every attempt of the launch has its own `seq` within
/// `attempt_seq`.
async fn assert_exact(pool: &PgPool, launch: Uuid, when: &str) {
    assert_eq!(
        counters(pool, launch).await,
        recounted(pool, launch).await,
        "counters differ from a recount {when}"
    );
    let (n, distinct, min, max, seq): (i64, i64, Option<i64>, Option<i64>, i64) = sqlx::query_as(
        "select count(*), count(distinct a.seq), min(a.seq), max(a.seq), \
           (select attempt_seq from launches where id = $1) from attempts a where a.launch_id = $1",
    )
    .bind(launch)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(n, distinct, "attempt seq values repeat {when}");
    if n > 0 {
        assert!(min.unwrap() >= 1, "an attempt kept seq 0 {when}");
        assert!(
            max.unwrap() <= seq,
            "an attempt seq beyond attempt_seq {when}"
        );
    }
}

fn diff(a: &CountDelta, b: &CountDelta) -> CountDelta {
    CountDelta {
        total: a.total - b.total,
        passed: a.passed - b.passed,
        failed: a.failed - b.failed,
        broken: a.broken - b.broken,
        skipped: a.skipped - b.skipped,
        unknown: a.unknown - b.unknown,
        flaky: a.flaky - b.flaky,
        muted: a.muted - b.muted,
        untested: a.untested - b.untested,
        in_progress: a.in_progress - b.in_progress,
    }
}

fn code(e: &TmError) -> &str {
    match e {
        TmError::NotFound(_) => "not_found",
        TmError::Invalid { code, .. }
        | TmError::Conflict { code, .. }
        | TmError::Forbidden { code, .. } => code,
        TmError::Stale { .. } => "precondition_failed",
        TmError::Other(_) => "other",
    }
}

fn step(status: StepStatus) -> RunStepWrite {
    RunStepWrite {
        status,
        actual: None,
        comment: None,
        attachment_ids: None,
    }
}

fn to(status: RunStatus) -> RunItemPatch {
    RunItemPatch {
        status: Some(status),
        ..Default::default()
    }
}

/// The current revision of an item.
async fn revision(pool: &PgPool, item: Uuid) -> i64 {
    sqlx::query_scalar("select revision from run_items where id = $1")
        .bind(item)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Brings an untested item (first execution) to `status`.
async fn reach(store: &Store, pool: &PgPool, p: &Project, item: Uuid, status: RunStatus) {
    let lim = TmLimits::default();
    match status {
        RunStatus::Untested => {}
        RunStatus::InProgress => {
            store
                .put_run_step(p, item, "1", &step(StepStatus::Passed), "ann", &lim)
                .await
                .unwrap();
        }
        s => {
            let rev = revision(pool, item).await;
            store
                .patch_run_item(p, item, rev, &to(s), "ann", &lim)
                .await
                .unwrap();
        }
    }
}

/// The result status a final run status projects to.
fn projected(s: RunStatus) -> Option<&'static str> {
    match s {
        RunStatus::Passed => Some("passed"),
        RunStatus::Failed => Some("failed"),
        RunStatus::Blocked => Some("broken"),
        RunStatus::Skipped => Some("skipped"),
        RunStatus::Untested | RunStatus::InProgress => None,
    }
}

#[tokio::test]
async fn every_status_transition_changes_the_counters_by_its_delta() {
    let Some((db, store)) = TestDb::new(6).await else {
        return;
    };
    let p = project(&store, "trans").await;
    let pairs: Vec<(RunStatus, RunStatus)> = RunStatus::ALL
        .iter()
        .flat_map(|a| RunStatus::ALL.iter().map(move |b| (*a, *b)))
        .filter(|(a, b)| a != b)
        .collect();
    let (launch, items) = manual_launch(&db, &store, &p, pairs.len() * 2 + 4, 2).await;
    assert_exact(&db.pool, launch.id, "after adding items").await;
    let lim = TmLimits::default();
    // The second half runs on muted series: their results are muted, and count_muted moves too.
    let muted_series: Vec<Uuid> =
        sqlx::query_scalar("select series_id from run_items where id = any($1) order by series_id")
            .bind(&items[pairs.len()..pairs.len() * 2])
            .fetch_all(&db.pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into series_mutes (series_id, project_id, reason, created_by) \
         select s, $2, 'known', 'ann' from unnest($1::uuid[]) as s",
    )
    .bind(&muted_series)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut checked = 0;
    for (k, &item) in items[..pairs.len() * 2].iter().enumerate() {
        let (from, target) = pairs[k % pairs.len()];
        let muted = k >= pairs.len();
        reach(&store, &db.pool, &p, item, from).await;
        assert_exact(&db.pool, launch.id, &format!("after reaching {from}")).await;
        let before = counters(&db.pool, launch.id).await;
        let rev = revision(&db.pool, item).await;
        store
            .patch_run_item(&p, item, rev, &to(target), "ann", &lim)
            .await
            .unwrap();
        let after = counters(&db.pool, launch.id).await;
        let mut want = CountDelta::item_change(from, target);
        let result = |s: RunStatus| {
            projected(s).map(|st| tm::projection::CountedResult {
                status: st.into(),
                flaky_in_run: false,
                muted,
            })
        };
        want.add(&CountDelta::result_change(
            result(from).as_ref(),
            result(target).as_ref(),
        ));
        assert_eq!(
            diff(&after, &before),
            want,
            "{from} -> {target} (muted series: {muted})"
        );
        assert_exact(&db.pool, launch.id, &format!("after {from} -> {target}")).await;
        checked += 1;
    }
    assert_eq!(checked, 60);

    // A step on an untested item starts it; a step on a finished item projects it again and
    // leaves the counters alone.
    let fresh = items[pairs.len() * 2];
    let before = counters(&db.pool, launch.id).await;
    store
        .put_run_step(&p, fresh, "1", &step(StepStatus::Failed), "bob", &lim)
        .await
        .unwrap();
    assert_eq!(
        diff(&counters(&db.pool, launch.id).await, &before),
        CountDelta::item_change(RunStatus::Untested, RunStatus::InProgress)
    );
    reach(&store, &db.pool, &p, fresh, RunStatus::Failed).await;
    let before = counters(&db.pool, launch.id).await;
    let seq_before: i64 = sqlx::query_scalar("select attempt_seq from launches where id = $1")
        .bind(launch.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    store
        .put_run_step(&p, fresh, "2", &step(StepStatus::Passed), "bob", &lim)
        .await
        .unwrap();
    assert_eq!(counters(&db.pool, launch.id).await, before);
    let seq_after: i64 = sqlx::query_scalar("select attempt_seq from launches where id = $1")
        .bind(launch.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(seq_after, seq_before, "the stored attempt keeps its seq");

    // A rerun keeps the earlier result: only the item counters move; the next final status
    // replaces the result, and going back shows the earlier execution again.
    let rev = revision(&db.pool, fresh).await;
    let before = counters(&db.pool, launch.id).await;
    store.rerun_run_item(&p, fresh, rev, "bob").await.unwrap();
    assert_eq!(
        diff(&counters(&db.pool, launch.id).await, &before),
        CountDelta::item_change(RunStatus::Failed, RunStatus::Untested)
    );
    assert_exact(&db.pool, launch.id, "after a rerun").await;
    reach(&store, &db.pool, &p, fresh, RunStatus::InProgress).await;
    reach(&store, &db.pool, &p, fresh, RunStatus::Passed).await;
    assert_exact(&db.pool, launch.id, "after a rerun passed").await;
    let rev = revision(&db.pool, fresh).await;
    let before = counters(&db.pool, launch.id).await;
    store
        .patch_run_item(&p, fresh, rev, &to(RunStatus::Untested), "bob", &lim)
        .await
        .unwrap();
    let mut want = CountDelta::item_change(RunStatus::Passed, RunStatus::Untested);
    want.passed = -1;
    want.failed = 1;
    assert_eq!(diff(&counters(&db.pool, launch.id).await, &before), want);
    assert_exact(
        &db.pool,
        launch.id,
        "after going back to the earlier execution",
    )
    .await;

    // Comment and assignee changes leave the counters alone; a closed run refuses changes.
    let other = items[pairs.len() * 2 + 1];
    let before = counters(&db.pool, launch.id).await;
    let rev = revision(&db.pool, other).await;
    store
        .patch_run_item(
            &p,
            other,
            rev,
            &RunItemPatch {
                comment: Some("later".into()),
                assignee: Some(Some("cid".into())),
                ..Default::default()
            },
            "ann",
            &lim,
        )
        .await
        .unwrap();
    assert_eq!(counters(&db.pool, launch.id).await, before);
    store
        .close_manual_run(&p, launch.id, ManualUntestedTo::Leave)
        .await
        .unwrap();
    assert_exact(&db.pool, launch.id, "after close").await;
    let rev = revision(&db.pool, other).await;
    let e = store
        .patch_run_item(&p, other, rev, &to(RunStatus::Passed), "ann", &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "manual_run_closed");
    let e = store
        .put_run_step(&p, other, "1", &step(StepStatus::Passed), "ann", &lim)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "manual_run_closed");
    store.reopen_manual_run(&p, launch.id).await.unwrap();
    store
        .close_manual_run(&p, launch.id, ManualUntestedTo::Skipped)
        .await
        .unwrap();
    assert_exact(&db.pool, launch.id, "after closing with skipped").await;
    let open: i64 = sqlx::query_scalar(
        "select count(*) from run_items where launch_id = $1 and status in ('untested', 'in_progress')",
    )
    .bind(launch.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(open, 0);
}

fn automated(i: usize, attempt: usize) -> ParsedResult {
    let full_name = format!("shop.Suite{}#case{i}", i % 7);
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name: &full_name,
        display_name: &format!("case {i}"),
        ..Default::default()
    });
    // Every fifth test fails first and passes on its retry (flaky in the run).
    let status = if i.is_multiple_of(5) && attempt == 0 {
        Status::Failed
    } else if i.is_multiple_of(11) {
        Status::Broken
    } else {
        Status::Passed
    };
    let start = 1_795_000_000_000 + (attempt * 10_000 + i) as i64;
    ParsedResult {
        source_uuid: Some(format!("u{i}-{attempt}")),
        name: identity.display_name.clone(),
        full_name,
        identity,
        status,
        start: Some(start),
        stop: Some(start + 5),
        format: SourceFormat::AllureResults,
        ..Default::default()
    }
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

/// A small deterministic generator for the testers' choices.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

const TESTERS: usize = 12;
const ITEMS: usize = 96;
const STEPS: usize = 6;
const AUTOMATED: usize = 240;

/// One tester's work on one item: every step, a final status, and sometimes a step on the
/// finished item, a way back and forward again, or a rerun and a second execution. Stops
/// quietly at `manual_run_closed` when `closing`.
async fn execute(
    store: &Store,
    p: &Project,
    item: Uuid,
    rng: &mut Rng,
    closing: bool,
) -> Result<(), String> {
    let lim = TmLimits::default();
    let finals = [
        RunStatus::Passed,
        RunStatus::Failed,
        RunStatus::Blocked,
        RunStatus::Skipped,
    ];
    let res = async {
        let mut rev = 0;
        for s in 1..=STEPS {
            let st = if rng.below(8) == 0 {
                StepStatus::Failed
            } else {
                StepStatus::Passed
            };
            rev = store
                .put_run_step(p, item, &s.to_string(), &step(st), "t", &lim)
                .await?
                .revision;
        }
        let f = finals[rng.below(4) as usize];
        rev = store
            .patch_run_item(p, item, rev, &to(f), "t", &lim)
            .await?
            .revision;
        match rng.below(4) {
            0 => {
                rev = store
                    .put_run_step(p, item, "1", &step(StepStatus::Blocked), "t", &lim)
                    .await?
                    .revision;
            }
            1 => {
                let back = if rng.below(2) == 0 {
                    RunStatus::InProgress
                } else {
                    RunStatus::Untested
                };
                rev = store
                    .patch_run_item(p, item, rev, &to(back), "t", &lim)
                    .await?
                    .revision;
                rev = store
                    .patch_run_item(p, item, rev, &to(finals[rng.below(4) as usize]), "t", &lim)
                    .await?
                    .revision;
            }
            2 => {
                store.rerun_run_item(p, item, rev, "t").await?;
                rev = store
                    .put_run_step(p, item, "1", &step(StepStatus::Passed), "t", &lim)
                    .await?
                    .revision;
                if rng.below(2) == 0 {
                    rev = store
                        .patch_run_item(p, item, rev, &to(finals[rng.below(4) as usize]), "t", &lim)
                        .await?
                        .revision;
                }
            }
            _ => {}
        }
        let _ = rev;
        Ok::<(), TmError>(())
    }
    .await;
    match res {
        Ok(()) => Ok(()),
        Err(e) if closing && code(&e) == "manual_run_closed" => Ok(()),
        Err(e) => Err(format!("{item}: {} {e:?}", code(&e))),
    }
}

#[tokio::test]
async fn concurrent_testers_ingest_mutes_and_close_keep_counters_exact() {
    let Some((db, store)) = TestDb::new(TESTERS as u32 + 8).await else {
        return;
    };
    let p = Arc::new(project(&store, "busy").await);
    let (launch, items) = manual_launch(&db, &store, &p, ITEMS, STEPS).await;
    let items = Arc::new(items);
    let dl0 = deadlocks(&db.pool).await;

    // Phase 1: testers, an automated ingest (with retries: flaky results) and a muter that
    // mutes and unmutes manual results until the testers are done, all at once.
    let busy = Arc::new(AtomicUsize::new(TESTERS));
    let mut tasks = Vec::new();
    for t in 0..TESTERS {
        let store = store.clone();
        let p = p.clone();
        let items = items.clone();
        let busy = busy.clone();
        tasks.push(tokio::spawn(async move {
            let mut rng = Rng(t as u64 + 1);
            let mut errors = Vec::new();
            for item in items.iter().skip(t).step_by(TESTERS) {
                if let Err(e) = execute(&store, &p, *item, &mut rng, false).await {
                    errors.push(e);
                }
            }
            busy.fetch_sub(1, Ordering::SeqCst);
            errors
        }));
    }
    {
        let store = store.clone();
        let launch = launch.clone();
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            for attempt in 0..2 {
                let ids: Vec<usize> = (0..AUTOMATED)
                    .filter(|i| attempt == 0 || i.is_multiple_of(5))
                    .collect();
                for part in ids.chunks(40) {
                    let batch: Vec<NewAttempt> = part
                        .iter()
                        .map(|&i| NewAttempt {
                            client_key: format!("r:t{i}:{attempt}"),
                            result: automated(i, attempt),
                        })
                        .collect();
                    if let Err(e) = store.persist_attempts(&launch, None, batch).await {
                        errors.push(format!("ingest: {e:#}"));
                    }
                    tokio::task::yield_now().await;
                }
            }
            errors
        }));
    }
    {
        let store = store.clone();
        let p = p.clone();
        let pool = db.pool.clone();
        let launch_id = launch.id;
        let busy = busy.clone();
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            let mut round = 0usize;
            while busy.load(Ordering::SeqCst) > 0 || round < 12 {
                round += 1;
                // Manual results only: triage locks results before the launch row, ingest
                // the launch row before its (automated) results.
                let ids: Vec<Uuid> = sqlx::query_scalar(
                    "select id from results where launch_id = $1 and run_item_id is not null \
                     order by id desc offset $2 limit 6",
                )
                .bind(launch_id)
                .bind((round % 3) as i64 * 3)
                .fetch_all(&pool)
                .await
                .unwrap();
                if ids.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    continue;
                }
                let req = TriageRequest {
                    result_ids: Some(ids),
                    mute: (round % 3 != 2).then(|| MuteRequest {
                        until: None,
                        reason: "noisy".into(),
                    }),
                    unmute: (round % 3 == 2).then_some(true),
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
    let mut errors = Vec::new();
    for t in tasks {
        errors.extend(t.await.unwrap());
    }
    assert!(errors.is_empty(), "{:?}", &errors[..errors.len().min(5)]);
    assert_exact(&db.pool, launch.id, "after testers, ingest and mutes").await;
    let c = counters(&db.pool, launch.id).await;
    assert!(c.total >= AUTOMATED as i32, "{c:?}");
    assert!(c.flaky > 0 && c.muted > 0, "{c:?}");

    // Phase 2: a rerun of every item while the run is closed, with `skipped` and then (after a
    // reopen) with `leave`; writers that lose the race get `manual_run_closed`.
    for mode in [ManualUntestedTo::Skipped, ManualUntestedTo::Leave] {
        if mode == ManualUntestedTo::Leave {
            store.reopen_manual_run(&p, launch.id).await.unwrap();
        }
        let executions: i64 = sqlx::query_scalar(
            "select sum(execution_no)::bigint from run_items where launch_id = $1",
        )
        .bind(launch.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let mut tasks = Vec::new();
        for t in 0..TESTERS {
            let store = store.clone();
            let p = p.clone();
            let items = items.clone();
            let pool = db.pool.clone();
            tasks.push(tokio::spawn(async move {
                let mut rng = Rng(100 + t as u64);
                let mut errors = Vec::new();
                for item in items.iter().skip(t).step_by(TESTERS) {
                    let rev = revision(&pool, *item).await;
                    match store.rerun_run_item(&p, *item, rev, "t").await {
                        Ok(_) => {}
                        Err(e) if code(&e) == "manual_run_closed" => break,
                        Err(e) => {
                            errors.push(format!("rerun: {} {e:?}", code(&e)));
                            break;
                        }
                    }
                    if let Err(e) = execute(&store, &p, *item, &mut rng, true).await {
                        errors.push(e);
                    }
                }
                errors
            }));
        }
        let closer = {
            let store = store.clone();
            let p = p.clone();
            let pool = db.pool.clone();
            let launch_id = launch.id;
            tokio::spawn(async move {
                loop {
                    let now: i64 = sqlx::query_scalar(
                        "select sum(execution_no)::bigint from run_items where launch_id = $1",
                    )
                    .bind(launch_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                    if now - executions >= (ITEMS / 3) as i64 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                store
                    .close_manual_run(&p, launch_id, mode)
                    .await
                    .map_err(|e| format!("close: {} {e:?}", code(&e)))?;
                // What the items look like right after the close: no change may follow.
                let statuses: Vec<(Uuid, String, i32)> = sqlx::query_as(
                    "select id, status, execution_no from run_items where launch_id = $1 order by id",
                )
                .bind(launch_id)
                .fetch_all(&pool)
                .await
                .unwrap();
                Ok::<_, String>(statuses)
            })
        };
        let mut errors = Vec::new();
        for t in tasks {
            errors.extend(t.await.unwrap());
        }
        let at_close = closer.await.unwrap();
        assert!(errors.is_empty(), "{:?}", &errors[..errors.len().min(5)]);
        let at_close = at_close.unwrap();
        assert_exact(
            &db.pool,
            launch.id,
            &format!("after a close ({mode}) among writers"),
        )
        .await;
        let now: Vec<(Uuid, String, i32)> = sqlx::query_as(
            "select id, status, execution_no from run_items where launch_id = $1 order by id",
        )
        .bind(launch.id)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(now, at_close, "an item changed after the close ({mode})");
        if mode == ManualUntestedTo::Skipped {
            let open = now
                .iter()
                .filter(|r| r.1 == "untested" || r.1 == "in_progress")
                .count();
            assert_eq!(open, 0, "the close left items open");
        }
    }
    assert_eq!(
        deadlocks(&db.pool).await - dl0,
        0,
        "PostgreSQL detected deadlocks"
    );
}

/// Triage that mutes and unmutes automated results while an ingest writes new attempts of the
/// same tests into the launch: triage takes the launch row before the results, in the order
/// ingest takes them, so neither waits for the other in a cycle (no deadlock at all, not only
/// none that reaches the caller), and `count_muted` equals a recount afterwards.
#[tokio::test]
async fn triage_of_automated_results_does_not_deadlock_with_ingest() {
    let Some((db, store)) = TestDb::new(12).await else {
        return;
    };
    let p = Arc::new(project(&store, "auto").await);
    let (l, _) = store
        .start_launch(
            &p,
            &LaunchStart {
                name: "ci".into(),
                ..Default::default()
            },
            "ci",
        )
        .await
        .unwrap();
    let launch = LaunchRef {
        id: l.id,
        project_id: p.id,
        project_key: p.key.clone(),
        created_at: l.created_at,
    };
    let batch = |attempt: usize, part: &[usize]| -> Vec<NewAttempt> {
        part.iter()
            .map(|&i| NewAttempt {
                client_key: format!("a:t{i}:{attempt}"),
                result: automated(i, attempt),
            })
            .collect()
    };
    let all: Vec<usize> = (0..AUTOMATED).collect();
    store
        .persist_attempts(&launch, None, batch(0, &all))
        .await
        .unwrap();
    let dl0 = deadlocks(&db.pool).await;
    const ATTEMPTS: usize = 6;
    let ingesting = Arc::new(AtomicUsize::new(1));
    let mut tasks = Vec::new();
    {
        let store = store.clone();
        let launch = launch.clone();
        let ingesting = ingesting.clone();
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            for attempt in 1..=ATTEMPTS {
                let mut ids: Vec<usize> = (0..AUTOMATED).collect();
                if attempt.is_multiple_of(2) {
                    ids.reverse();
                }
                for part in ids.chunks(40) {
                    if let Err(e) = store
                        .persist_attempts(&launch, None, batch(attempt, part))
                        .await
                    {
                        errors.push(format!("ingest: {e:#}"));
                    }
                }
            }
            ingesting.fetch_sub(1, Ordering::SeqCst);
            errors
        }));
    }
    for m in 0..2u64 {
        let store = store.clone();
        let p = p.clone();
        let pool = db.pool.clone();
        let ingesting = ingesting.clone();
        let launch_id = launch.id;
        tasks.push(tokio::spawn(async move {
            let mut errors = Vec::new();
            let mut round = 0u64;
            while ingesting.load(Ordering::SeqCst) > 0 || round < 4 {
                round += 1;
                let ids: Vec<Uuid> =
                    sqlx::query_scalar("select id from results where launch_id = $1 order by id")
                        .bind(launch_id)
                        .fetch_all(&pool)
                        .await
                        .unwrap();
                let mute = (round + m).is_multiple_of(2);
                let req = TriageRequest {
                    result_ids: Some(ids),
                    mute: mute.then(|| MuteRequest {
                        until: None,
                        reason: "noisy".into(),
                    }),
                    unmute: (!mute).then_some(true),
                    comment: Some(format!("round {round}")),
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
    let mut errors = Vec::new();
    for t in tasks {
        errors.extend(t.await.unwrap());
    }
    assert!(errors.is_empty(), "{:?}", &errors[..errors.len().min(5)]);
    assert_eq!(
        deadlocks(&db.pool).await,
        dl0,
        "PostgreSQL broke a deadlock between triage and ingest"
    );
    assert_exact(&db.pool, launch.id, "after triage and ingest").await;
    let attempts: i64 = sqlx::query_scalar("select count(*) from attempts where launch_id = $1")
        .bind(launch.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(attempts as usize, AUTOMATED * (ATTEMPTS + 1));
}
