//! TM next foundation on a real PostgreSQL: the upgrade from 0820 to the `09xx` block, the
//! replica gate, the denormalized last result of cases, the runnable version and skip rules,
//! and the stored formats that old replicas read. They need `FASTPROOF_TEST_DATABASE_URL`;
//! without it they pass without doing anything.

use std::time::Duration;

use chrono::{DateTime, Utc};
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::launches::LaunchStart;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::runs::RunItemFilter;
use fastproof_store::tm::{
    self, CaseBody, CaseReviewSettings, CaseStep, CaseWorkflowSettings, ReviewMode, RunItemPatch,
    RunItemSkipCode, RunItemsAdd, RunStatus, StartPlanLaunch, TestPlanWrite, TmError, TmGateMode,
    TmJobKind, TmLimits,
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
    async fn new() -> Option<Self> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }

    async fn migrated() -> Option<(Self, Store)> {
        let db = Self::new().await?;
        fastproof_store::migrate(&db.pool).await.unwrap();
        let store = Store::new(db.pool.clone());
        Some((db, store))
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

fn body(action: &str) -> CaseBody {
    CaseBody {
        steps: vec![CaseStep {
            id: "s_1".into(),
            action: action.into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn case_in(store: &Store, p: &Project, name: &str) -> tm::CaseRow {
    let mut conn = store.pool().acquire().await.unwrap();
    tm::testutil::insert_case(&mut conn, p.id, name, &body(name), None)
        .await
        .unwrap()
}

/// A finished launch `minutes_ago` old; `can_sync` is what its creator could.
async fn launch(
    db: &TestDb,
    p: &Project,
    minutes_ago: i32,
    can_sync: bool,
) -> (Uuid, DateTime<Utc>) {
    let id = Uuid::now_v7();
    let number: i64 = sqlx::query_scalar(
        "update projects set launch_seq = launch_seq + 1 where id = $1 returning launch_seq",
    )
    .bind(p.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let at: DateTime<Utc> = sqlx::query_scalar(
        "insert into launches (id, project_id, number, name, status, created_at, started_at, created_by_can_sync) \
         values ($1, $2, $5, 'l', 'finished', \
           now() - make_interval(mins => $3), now() - make_interval(mins => $3), $4) returning created_at",
    )
    .bind(id)
    .bind(p.id)
    .bind(minutes_ago)
    .bind(can_sync)
    .bind(number)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (id, at)
}

/// A code identity linked to the case.
async fn identity(db: &TestDb, p: &Project, case: &tm::CaseRow, full_name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, kind, case_id) \
         values ($1, $2, md5($3), $3, $3, 'code', $4)",
    )
    .bind(id)
    .bind(p.id)
    .bind(full_name)
    .bind(case.id)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

/// A result of an identity in a launch, stopped `minutes_ago`.
async fn result(
    db: &TestDb,
    p: &Project,
    l: (Uuid, DateTime<Utc>),
    tc: Uuid,
    status: &str,
    minutes_ago: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, \
           status, stopped_at) values ($1, $2, $3, $4, $5, $6, 'r', 'r', $7, now() - make_interval(mins => $8))",
    )
    .bind(id)
    .bind(l.1)
    .bind(p.id)
    .bind(l.0)
    .bind(tc)
    .bind(Uuid::now_v7())
    .bind(status)
    .bind(minutes_ago)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn last(
    db: &TestDb,
    case: Uuid,
) -> (Option<String>, Option<String>, Option<Uuid>, Option<Uuid>) {
    sqlx::query_as(
        "select last_status, last_source, last_launch_id, last_result_id from cases where id = $1",
    )
    .bind(case)
    .fetch_one(&db.pool)
    .await
    .unwrap()
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

/// Runs every queued `last_status` job to its end.
async fn run_last_status_jobs(store: &Store) -> usize {
    let mut n = 0;
    while let Some(job) = store
        .claim_tm_job("test-owner", TmJobKind::LastStatus)
        .await
        .unwrap()
    {
        store
            .run_last_status_job(job.id, "test-owner")
            .await
            .unwrap();
        n += 1;
    }
    n
}

#[tokio::test]
async fn upgrade_from_0820_keeps_data_and_old_queries() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrator()
        .run_to(820, &db.pool)
        .await
        .unwrap();
    let pid = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name, case_seq) values ($1, 'old', 'Old', 2)")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    let cid = Uuid::now_v7();
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by) \
         values ($1, $2, 1, 'Pay', 'active', 'ui', $3, 'u', 'u')",
    )
    .bind(cid)
    .bind(pid)
    .bind(serde_json::to_value(body("pay")).unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let snap = tm::CaseSnapshot {
        name: "Pay".into(),
        status: tm::CaseStatus::Active,
        folder_id: None,
        body: body("pay"),
        fields: Default::default(),
        tags: Vec::new(),
    };
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, snapshot_z, created_by) \
         values ($1, 1, $2, 'ui', $3, 'u')",
    )
    .bind(cid)
    .bind(pid)
    .bind(fastproof_store::codec::encode(&snap).unwrap())
    .execute(&db.pool)
    .await
    .unwrap();
    let plan = Uuid::now_v7();
    sqlx::query(
        "insert into test_plans (id, project_id, name, kind) values ($1, $2, 'p', 'managed')",
    )
    .bind(plan)
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("insert into test_plan_items (plan_id, case_id) values ($1, $2)")
        .bind(plan)
        .bind(cid)
        .execute(&db.pool)
        .await
        .unwrap();
    let lid = Uuid::now_v7();
    let created: DateTime<Utc> = sqlx::query_scalar(
        "insert into launches (id, project_id, number, name, status, test_plan_id, manual_state) \
         values ($1, $2, 1, 'run', 'running', $3, 'open') returning created_at",
    )
    .bind(lid)
    .bind(pid)
    .bind(plan)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let item = Uuid::now_v7();
    sqlx::query(
        "insert into run_items (id, project_id, launch_id, launch_created_at, case_id, case_version, series_id) \
         values ($1, $2, $3, $4, $5, 1, $6)",
    )
    .bind(item)
    .bind(pid)
    .bind(lid)
    .bind(created)
    .bind(cid)
    .bind(Uuid::now_v7())
    .execute(&db.pool)
    .await
    .unwrap();

    let mut applied = fastproof_store::migrate(&db.pool).await.unwrap();
    applied.sort();
    assert_eq!(applied, (900..=911).collect::<Vec<i64>>());
    fastproof_store::check_schema(&db.pool).await.unwrap();
    let invalid: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint where conname in \
         ('cases_review_state_check', 'cases_last_source_check', 'tm_jobs_kind_check') and not convalidated",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(invalid.is_empty(), "{invalid:?}");
    let indexes: Vec<(String, bool)> = sqlx::query_as(
        "select c.relname::text, i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid \
         where c.relname in ('cases_review_queue', 'cases_last_status', 'cases_state') order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            ("cases_last_status".to_string(), true),
            ("cases_review_queue".to_string(), true),
            ("cases_state".to_string(), true),
        ]
    );
    let (state, review, last, pins, iterations): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
        i32,
    ) = sqlx::query_as(
        "select c.state, c.review_state, c.last_status, ri.shared_pins, ri.iterations \
         from cases c join run_items ri on ri.case_id = c.id where c.id = $1",
    )
    .bind(cid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (state, review, last, pins, iterations),
        (None, None, None, None, 0)
    );

    // Writes in the shape of the previous release still work (explicit column lists).
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by) \
         values ($1, $2, 2, 'Refund', 'draft', 'ui', '{}', 'u', 'u')",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into tm_jobs (id, project_id, kind, params) values ($1, $2, 'backfill', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    let bad = sqlx::query(
        "insert into tm_jobs (id, project_id, kind, params) values ($1, $2, 'other', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .execute(&db.pool)
    .await;
    assert!(bad.is_err());

    let store = Store::new(db.pool.clone());
    let list = store
        .list_cases(
            pid,
            &tm::cases::CaseQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(list.items.len(), 2);
    assert!(
        list.items
            .iter()
            .all(|c| c.last_result.is_none() && c.state.is_none())
    );
    let view = store.case_view(pid, 1).await.unwrap();
    assert!(view.shared_steps.is_empty() && view.approved_version.is_none());
    let ri = store.run_item(pid, item).await.unwrap();
    assert_eq!((ri.case_version, ri.summary.iteration_count), (1, 0));
    let json = serde_json::to_string(&ri).unwrap();
    assert_eq!(json.matches("\"iterations\":").count(), 1, "{json}");
    assert!(json.contains("\"iterationCount\":0") && json.contains("\"sharedPins\":[]"));
    assert_eq!(ri.steps.len(), 1);
    let tp = store.plan_testplan(pid, plan).await;
    assert!(
        tp.is_err(),
        "a plan without automated cases has no testplan"
    );
}

#[tokio::test]
async fn gate_follows_replicas_and_the_settle_time() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let fresh = store.tm_gate(Duration::from_secs(600)).await.unwrap();
    assert!(fresh.open, "a fresh install is open at once: {fresh:?}");
    assert_eq!(fresh.settle_until, None);

    // An upgrade: every migration before the gate ran two days ago, the gate one just now.
    sqlx::query(
        "update fastproof_schema_migrations set installed_on = installed_on - interval '2 days' where version < 911",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let g = store.tm_gate(Duration::from_secs(600)).await.unwrap();
    assert!(!g.open);
    let until = g.settle_until.expect("settle time");
    assert!(until > Utc::now() + chrono::Duration::seconds(500));
    assert!(store.tm_gate(Duration::ZERO).await.unwrap().open);

    store.replica_heartbeat("new-a", "0.3.0").await.unwrap();
    assert!(store.tm_gate(Duration::ZERO).await.unwrap().open);
    sqlx::query("insert into replica_levels (instance, tm_level, version) values ('old-b', 1, '0.2.0-dev2')")
        .execute(&db.pool)
        .await
        .unwrap();
    let g = store.tm_gate(Duration::ZERO).await.unwrap();
    assert!(!g.open);
    assert_eq!(g.behind, vec!["old-b".to_string()]);
    sqlx::query(
        "update replica_levels set seen_at = now() - interval '3 minutes' where instance = 'old-b'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(store.tm_gate(Duration::ZERO).await.unwrap().open);
    sqlx::query(
        "update replica_levels set seen_at = now() - interval '2 days' where instance = 'old-b'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    store.replica_heartbeat("new-a", "0.3.0").await.unwrap();
    let levels = store.replica_levels().await.unwrap();
    assert_eq!(levels.len(), 1);
    assert_eq!(
        (levels[0].instance.as_str(), levels[0].tm_level),
        ("new-a", 2)
    );

    let mut conn = db.pool.acquire().await.unwrap();
    let limits = |mode, settle| TmLimits {
        gate_mode: mode,
        gate_settle_seconds: settle,
        ..TmLimits::default()
    };
    let e = tm::gate::require(&mut conn, &limits(TmGateMode::Closed, 0), "rerun.variables")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "upgrade_in_progress");
    let TmError::Conflict { details, .. } = e else {
        panic!("a conflict")
    };
    assert_eq!(
        details.unwrap(),
        serde_json::json!({"what": "rerun.variables", "requiredLevel": 2})
    );
    tm::gate::reset_cache();
    tm::gate::require(&mut conn, &limits(TmGateMode::Open, 600), "x")
        .await
        .unwrap();
    let e = tm::gate::require(&mut conn, &limits(TmGateMode::Auto, 600), "x")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "upgrade_in_progress");
    tm::gate::require(&mut conn, &limits(TmGateMode::Auto, 0), "x")
        .await
        .unwrap();
}

#[tokio::test]
async fn last_result_follows_launches_manual_results_and_deletes() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "last").await;
    let limits = TmLimits::default();
    let c1 = case_in(&store, &p, "c1").await;
    let t1 = identity(&db, &p, &c1, "a.T1").await;
    let t1b = identity(&db, &p, &c1, "a.T1b").await;

    // An untrusted launch still moves the last result.
    let l1 = launch(&db, &p, 60, false).await;
    let r1 = result(&db, &p, l1, t1, "failed", 50).await;
    let (trusted, _) = store.sync_launch(l1.0, &limits).await.unwrap();
    assert!(!trusted);
    assert_eq!(
        last(&db, c1.id).await,
        (
            Some("failed".into()),
            Some("automated".into()),
            Some(l1.0),
            Some(r1)
        )
    );

    // With caseSync off, a newer launch (another identity of the case) moves it on.
    let mut s = ProjectSettings::default();
    s.case_sync.enabled = false;
    store.put_project_settings(p.id, &s).await.unwrap();
    let l2 = launch(&db, &p, 30, true).await;
    let r2 = result(&db, &p, l2, t1b, "passed", 20).await;
    store.sync_launch(l2.0, &limits).await.unwrap();
    assert_eq!(last(&db, c1.id).await.3, Some(r2));

    // A late sync of an older launch never moves it back.
    let l0 = launch(&db, &p, 120, true).await;
    result(&db, &p, l0, t1, "broken", 110).await;
    store.sync_launch(l0.0, &limits).await.unwrap();
    assert_eq!(last(&db, c1.id).await.3, Some(r2));

    // Deleting the launch clears the case and queues a recompute from what is left.
    assert!(store.delete_launch(p.id, &p.key, l2.0).await.unwrap());
    assert_eq!(last(&db, c1.id).await, (None, None, None, None));
    assert_eq!(run_last_status_jobs(&store).await, 1);
    assert_eq!(last(&db, c1.id).await.3, Some(r1));

    // The backfill job fills cases whose results came before the columns.
    let c2 = case_in(&store, &p, "c2").await;
    let t2 = identity(&db, &p, &c2, "a.T2").await;
    let r3 = result(&db, &p, l1, t2, "skipped", 40).await;
    assert_eq!(last(&db, c2.id).await.3, None);
    sqlx::query("delete from tm_jobs where kind = 'last_status'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store.enqueue_last_status_jobs().await.unwrap(),
        0,
        "a fresh install needs no backfill"
    );
    sqlx::query(
        "update fastproof_schema_migrations set installed_on = installed_on - interval '2 days' where version < 911",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(store.enqueue_last_status_jobs().await.unwrap(), 1);
    assert_eq!(run_last_status_jobs(&store).await, 1);
    assert_eq!(
        store.enqueue_last_status_jobs().await.unwrap(),
        0,
        "a job is done for the project"
    );
    sqlx::query("delete from tm_jobs where kind = 'last_status'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(store.enqueue_last_status_jobs().await.unwrap(), 1);
    assert_eq!(store.enqueue_last_status_jobs().await.unwrap(), 0);
    assert_eq!(run_last_status_jobs(&store).await, 1);
    assert_eq!(
        last(&db, c2.id).await,
        (
            Some("skipped".into()),
            Some("automated".into()),
            Some(l1.0),
            Some(r3)
        )
    );
    let (status, done): (String, serde_json::Value) = sqlx::query_as(
        "select status, progress from tm_jobs where kind = 'last_status' order by created_at desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(status, "done");
    assert_eq!(done["done"], 2);

    // A manual result sets it through the projection, and clearing the status removes it.
    let m = case_in(&store, &p, "manual").await;
    let lm = launch(&db, &p, 5, true).await;
    sqlx::query("update launches set status = 'running', manual_state = 'open' where id = $1")
        .bind(lm.0)
        .execute(&db.pool)
        .await
        .unwrap();
    let added = store
        .add_run_items(
            &p,
            lm.0,
            &RunItemsAdd {
                case_numbers: Some(vec![m.number]),
                ..Default::default()
            },
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(added.added, 1);
    let items = store
        .run_items(
            p.id,
            lm.0,
            &RunItemFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let item = &items.items[0];
    let full = store.run_item(p.id, item.id).await.unwrap();
    let to = |status| RunItemPatch {
        status: Some(status),
        ..Default::default()
    };
    let ch = store
        .patch_run_item(
            &p,
            item.id,
            full.revision,
            &to(RunStatus::Passed),
            "tester",
            &limits,
        )
        .await
        .unwrap();
    let (status, source, launch_id, result_id) = last(&db, m.id).await;
    assert_eq!(
        (status.as_deref(), source.as_deref(), launch_id),
        (Some("passed"), Some("manual"), Some(lm.0))
    );
    assert!(result_id.is_some());
    store
        .patch_run_item(
            &p,
            item.id,
            ch.revision,
            &to(RunStatus::Untested),
            "tester",
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(last(&db, m.id).await, (None, None, None, None));
}

/// The lateral the case list used before the columns (`afa7911`).
const OLD_LATERAL: &str = "select c.number, lr.status, lr.at, lr.manual, lr.launch_id, lr.id from cases c \
    left join lateral (select x.status, x.at, x.manual, x.launch_id, x.id from test_cases t \
      cross join lateral (select r.status, coalesce(r.stopped_at, r.updated_at) as at, r.run_item_id is not null as manual, \
        r.launch_id, r.id, r.launch_created_at from results r where r.test_case_id = t.id \
        order by r.launch_created_at desc, r.updated_at desc limit 1) x \
      where t.case_id = c.id order by x.launch_created_at desc limit 1) lr on true \
    where c.project_id = $1 order by c.number";

#[tokio::test]
async fn case_list_reads_the_columns_like_the_old_lateral() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "list").await;
    let mut cases = Vec::new();
    for i in 0..6 {
        let c = case_in(&store, &p, &format!("c{i}")).await;
        let mut ids = Vec::new();
        for k in 0..(i % 3) {
            ids.push(identity(&db, &p, &c, &format!("x.T{i}_{k}")).await);
        }
        cases.push((c, ids));
    }
    for (n, minutes) in [(0, 300), (1, 200), (2, 100)] {
        let l = launch(&db, &p, minutes, n % 2 == 0).await;
        for (i, (_, ids)) in cases.iter().enumerate() {
            for (k, tc) in ids.iter().enumerate() {
                // One identity of a case per launch: the old lateral picked any of them on a tie.
                if (i + k + n) % 4 == 3 || n % ids.len() != k {
                    continue;
                }
                let status = ["passed", "failed", "broken", "skipped"][(i + k + n) % 4];
                result(&db, &p, l, *tc, status, minutes - 10 - k as i32).await;
            }
        }
        store.sync_launch(l.0, &TmLimits::default()).await.unwrap();
    }
    type OldRow = (
        i64,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<bool>,
        Option<Uuid>,
        Option<Uuid>,
    );
    let old: Vec<OldRow> = sqlx::query_as(OLD_LATERAL)
        .bind(p.id)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let list = store
        .list_cases(
            p.id,
            &tm::cases::CaseQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(list.items.len(), old.len());
    let mut with_result = 0;
    for (item, (number, status, at, manual, launch_id, id)) in list.items.iter().zip(old) {
        assert_eq!(item.number, number);
        match (&item.last_result, status) {
            (None, None) => {}
            (Some(lr), Some(s)) => {
                with_result += 1;
                assert_eq!(lr.status, s, "TC-{number}");
                assert_eq!(Some(lr.at), at, "TC-{number}");
                assert_eq!(Some(lr.launch_id), launch_id, "TC-{number}");
                assert_eq!(Some(lr.result_id), id, "TC-{number}");
                assert_eq!(
                    lr.source == tm::ResultSource::Manual,
                    manual.unwrap(),
                    "TC-{number}"
                );
            }
            (a, b) => panic!("TC-{number}: columns {a:?}, lateral {b:?}"),
        }
    }
    assert!(with_result >= 3, "{with_result}");
    // A recompute of the whole project changes nothing.
    assert_eq!(store.recompute_last_status(p.id).await.unwrap(), 0);
}

#[tokio::test]
async fn runnable_version_and_skips() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "run").await;
    let limits = TmLimits::default();
    let s = ProjectSettings {
        case_review: CaseReviewSettings {
            mode: ReviewMode::Manual,
            ..Default::default()
        },
        case_workflow: Some(CaseWorkflowSettings::default()),
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    let a = case_in(&store, &p, "head pending, v1 approved").await;
    let b = case_in(&store, &p, "never approved").await;
    let c = case_in(&store, &p, "ready for review").await;
    let d = case_in(&store, &p, "plain").await;
    let e = case_in(&store, &p, "automated, never approved").await;
    let f = case_in(&store, &p, "automated, approved head").await;
    sqlx::query("update cases set version = 2, review_state = 'pending', approved_version = 1 where id = $1")
        .bind(a.id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update cases set review_state = 'pending' where id = $1")
        .bind(b.id)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update cases set state = 'ready_for_review', status = 'draft' where id = $1")
        .bind(c.id)
        .execute(&db.pool)
        .await
        .unwrap();
    for (case, name) in [(&e, "auto.E"), (&f, "auto.F")] {
        identity(&db, &p, case, name).await;
    }
    sqlx::query("update cases set automated = true, review_state = 'pending' where id = any($1)")
        .bind(vec![e.id, f.id])
        .execute(&db.pool)
        .await
        .unwrap();

    let open_launch = || async {
        let l = launch(&db, &p, 1, true).await;
        sqlx::query("update launches set status = 'running' where id = $1")
            .bind(l.0)
            .execute(&db.pool)
            .await
            .unwrap();
        l.0
    };
    let numbers = vec![a.number, b.number, c.number, d.number];
    let l = open_launch().await;
    let added = store
        .add_run_items(
            &p,
            l,
            &RunItemsAdd {
                case_numbers: Some(numbers.clone()),
                ..Default::default()
            },
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(added.added, 2);
    let mut skips: Vec<(i64, RunItemSkipCode)> = added
        .skips
        .iter()
        .map(|s| (s.case_number, s.code))
        .collect();
    skips.sort_by_key(|s| s.0);
    assert_eq!(
        skips,
        vec![
            (b.number, RunItemSkipCode::NotApproved),
            (c.number, RunItemSkipCode::BlockedByState)
        ]
    );
    let versions: Vec<(i64, i32)> = sqlx::query_as(
        "select c.number, ri.case_version from run_items ri join cases c on c.id = ri.case_id \
         where ri.launch_id = $1 order by c.number",
    )
    .bind(l)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(versions, vec![(a.number, 1), (d.number, 1)]);

    let l = open_launch().await;
    let added = store
        .add_run_items(
            &p,
            l,
            &RunItemsAdd {
                case_numbers: Some(numbers),
                include_unapproved: true,
                ..Default::default()
            },
            &limits,
        )
        .await
        .unwrap();
    assert_eq!((added.added, added.skips.len()), (4, 0));
    let a_version: i32 = sqlx::query_scalar(
        "select case_version from run_items where launch_id = $1 and case_id = $2",
    )
    .bind(l)
    .bind(a.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(a_version, 2);

    // Plans: manual mode leaves automated cases out of scope (they run at the head), and a
    // CI start without manual items creates no run items.
    let plan = store
        .create_managed_plan(
            p.id,
            &TestPlanWrite {
                name: "all".into(),
                case_numbers: Some(vec![
                    a.number, b.number, c.number, d.number, e.number, f.number,
                ]),
                ..Default::default()
            },
            "u",
            &limits,
        )
        .await
        .unwrap();
    let tp = store.plan_testplan(p.id, plan.id).await.unwrap().unwrap();
    let mut selectors: Vec<String> = tp.tests.iter().map(|t| t.selector.clone()).collect();
    selectors.sort();
    assert_eq!(selectors, vec!["auto.E".to_string(), "auto.F".to_string()]);
    let mut all = s.clone();
    all.case_review.mode = ReviewMode::All;
    store.put_project_settings(p.id, &all).await.unwrap();
    sqlx::query("update cases set approved_version = 1 where id = $1")
        .bind(f.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let tp = store.plan_testplan(p.id, plan.id).await.unwrap().unwrap();
    let selectors: Vec<String> = tp.tests.iter().map(|t| t.selector.clone()).collect();
    assert_eq!(
        selectors,
        vec!["auto.F".to_string()],
        "E has no approved version"
    );

    let start = |include_manual| StartPlanLaunch {
        name: "from plan".into(),
        external_id: None,
        environment: None,
        attributes: None,
        assign: tm::PlanAssign::None,
        include_automated: true,
        include_manual,
        include_unapproved: false,
        ci: None,
    };
    let launch_start = |ext: &str| LaunchStart {
        external_id: Some(ext.into()),
        name: "from plan".into(),
        ..Default::default()
    };
    let started = store
        .start_plan_launch(
            &p,
            plan.id,
            &start(Some(false)),
            &launch_start("ci-1"),
            "u",
            true,
            &limits,
        )
        .await
        .unwrap();
    assert_eq!((started.run_items, started.skips.len()), (0, 0));
    assert!(started.launch.manual.is_none());
    let started = store
        .start_plan_launch(
            &p,
            plan.id,
            &start(None),
            &launch_start("ci-2"),
            "u",
            true,
            &limits,
        )
        .await
        .unwrap();
    assert_eq!(started.run_items, 2);
    let mut codes: Vec<RunItemSkipCode> = started.skips.iter().map(|s| s.code).collect();
    codes.sort_by_key(|c| c.as_str());
    assert_eq!(
        codes,
        vec![
            RunItemSkipCode::BlockedByState,
            RunItemSkipCode::NotApproved
        ]
    );
}

#[tokio::test]
async fn state_and_review_columns_reach_lists_and_views() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "cols").await;
    let c = case_in(&store, &p, "c").await;
    sqlx::query("update cases set state = 'draft', review_state = 'changes_requested', approved_version = 1 where id = $1")
        .bind(c.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let v = store.case_view(p.id, c.number).await.unwrap();
    assert_eq!(v.state.as_deref(), Some("draft"));
    assert_eq!(v.review_state, Some(tm::ReviewState::ChangesRequested));
    assert_eq!(v.approved_version, Some(1));
    let l = store
        .list_cases(
            p.id,
            &tm::cases::CaseQuery {
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(l.items[0].state.as_deref(), Some("draft"));
    assert_eq!(
        l.items[0].review_state,
        Some(tm::ReviewState::ChangesRequested)
    );
    let bad = sqlx::query("update cases set review_state = 'maybe' where id = $1")
        .bind(c.id)
        .execute(&db.pool)
        .await;
    assert!(bad.is_err());
}

#[test]
fn stored_formats_stay_byte_identical_without_the_new_keys() {
    let body = CaseBody {
        description: "d".into(),
        preconditions: "p".into(),
        steps: vec![CaseStep {
            id: "s_1".into(),
            action: "a".into(),
            expected: "e".into(),
            data: String::new(),
            attachments: Vec::new(),
            steps: Vec::new(),
            shared: None,
        }],
        links: Vec::new(),
        parameters: None,
    };
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"description":"d","preconditions":"p","steps":[{"id":"s_1","action":"a","expected":"e","data":"","attachments":[],"steps":[]}],"links":[]}"#
    );
    let shared: CaseStep = serde_json::from_value(serde_json::json!(
        {"id": "", "shared": {"id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b", "number": 3}}
    ))
    .unwrap();
    assert_eq!(shared.shared.as_ref().map(|s| s.number), Some(3));
    assert!(shared.action.is_empty());
    let v = serde_json::to_value(ProjectSettings::default()).unwrap();
    assert!(v.get("caseWorkflow").is_none() && v.get("caseReview").is_none());
    assert!(v["caseSync"].get("requirementLinkTypes").is_none());
    let back = ProjectSettings::from_value(v);
    assert_eq!(
        back.case_sync.requirement_link_types,
        vec!["requirement".to_string()]
    );
    let mut on = ProjectSettings::default();
    on.case_review.mode = ReviewMode::All;
    on.case_sync.requirement_link_types = vec!["req".into()];
    on.case_workflow = Some(CaseWorkflowSettings::default());
    on.validate().unwrap();
    let v = serde_json::to_value(&on).unwrap();
    assert_eq!(v["caseReview"]["mode"], "all");
    assert_eq!(
        v["caseSync"]["requirementLinkTypes"],
        serde_json::json!(["req"])
    );
    assert_eq!(v["caseWorkflow"]["statuses"][1]["blocksRuns"], true);
    assert_eq!(ProjectSettings::from_value(v), on);
    // A broken area falls back to its default alone.
    let broken = ProjectSettings::from_value(serde_json::json!({
        "caseReview": {"mode": "sometimes"}, "rollup": {"afterDays": 7}}));
    assert_eq!(broken.case_review, CaseReviewSettings::default());
    assert_eq!(broken.rollup.after_days, 7);
    let mut bad = ProjectSettings::default();
    bad.case_review.approvals = 6;
    assert!(bad.validate().is_err());
    let mut bad = ProjectSettings::default();
    let mut w = CaseWorkflowSettings::default();
    w.statuses.retain(|s| s.key != "outdated");
    bad.case_workflow = Some(w);
    assert!(bad.validate().unwrap_err().contains("category"));
    let mut bad = ProjectSettings::default();
    bad.case_sync.requirement_link_types = vec!["1bad".into()];
    assert!(bad.validate().is_err());
}
