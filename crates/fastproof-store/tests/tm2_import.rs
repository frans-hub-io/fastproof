//! Shared step and requirement records of an import on a real PostgreSQL: matching by keys,
//! number and name, what a resumed job remembers, parents, depth and count limits, and the
//! dry run that creates nothing. They need `FASTPROOF_TEST_DATABASE_URL`; without it they pass
//! without doing anything.

use fastproof_store::projects::CreateProject;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::import::{PlanOptions, Planner};
use fastproof_store::tm::{ImportMode, TmError, TmGateMode, TmLimits, UnknownFieldValues};
use fastproof_store::{Project, Store};
use fastproof_tm_import::{ImportedKey, ImportedRequirement, ImportedSharedStep, ImportedStep};
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
    async fn migrated() -> Option<(Self, Store)> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(6)
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

fn limits() -> TmLimits {
    TmLimits {
        gate_mode: TmGateMode::Open,
        ..Default::default()
    }
}

fn ctx<'a>(p: &'a Project, l: &'a TmLimits) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: "importer",
        limits: l,
        can_review: false,
    }
}

async fn planner(store: &Store, p: &Project, mode: ImportMode) -> Planner {
    store
        .import_planner(
            p.id,
            PlanOptions {
                mode,
                unknown_values: UnknownFieldValues::Skip,
                target_folder: None,
                numbered_source: true,
                identities: false,
                status: None,
            },
        )
        .await
        .unwrap()
}

fn shared(
    row: u64,
    number: Option<i64>,
    key: Option<(&str, &str)>,
    name: &str,
) -> ImportedSharedStep {
    ImportedSharedStep {
        row,
        number,
        keys: key
            .map(|(k, v)| {
                vec![ImportedKey {
                    kind: k.into(),
                    value: v.into(),
                }]
            })
            .unwrap_or_default(),
        name: name.into(),
        steps: vec![ImportedStep {
            action: format!("do {name}"),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn req(row: u64, number: Option<i64>, title: &str, parent: Option<i64>) -> ImportedRequirement {
    ImportedRequirement {
        row,
        number,
        title: title.into(),
        parent,
        ..Default::default()
    }
}

async fn n(pool: &PgPool, sql: &'static str, p: &Project) -> i64 {
    sqlx::query_scalar(sql)
        .bind(p.id)
        .fetch_one(pool)
        .await
        .unwrap()
}

const SHARED: &str = "select count(*) from shared_steps where project_id = $1";
const REQS: &str = "select count(*) from requirements where project_id = $1";

#[tokio::test]
async fn a_resumed_import_does_not_create_its_records_again() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "res").await;
    let l = limits();
    let c = ctx(&p, &l);
    let mut first = planner(&store, &p, ImportMode::CreateOnly).await;
    store
        .import_shared_step(&mut first, &c, &shared(2, None, None, "keyless"), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut first, &c, &req(3, Some(1), "Root", None), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut first, &c, &req(4, Some(2), "Child", Some(1)), true)
        .await
        .unwrap();
    assert!(first.report.errors.is_empty(), "{:?}", first.report.errors);
    assert!(first.rec.dirty);
    assert_eq!(
        (n(&db.pool, SHARED, &p).await, n(&db.pool, REQS, &p).await),
        (1, 2)
    );
    let saved = serde_json::to_value(&first.rec.maps).unwrap();

    let mut second = planner(&store, &p, ImportMode::CreateOnly).await;
    second.rec.load(Some(&saved));
    store
        .import_shared_step(&mut second, &c, &shared(2, None, None, "keyless"), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut second, &c, &req(3, Some(1), "Root", None), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut second, &c, &req(4, Some(2), "Child", Some(1)), true)
        .await
        .unwrap();
    assert_eq!(
        (n(&db.pool, SHARED, &p).await, n(&db.pool, REQS, &p).await),
        (1, 2),
        "the rows the first run created are not created again"
    );
    assert!(!second.rec.dirty, "nothing new to remember");
    assert_eq!(second.rec.maps, first.rec.maps);
    let depth: i16 =
        sqlx::query_scalar("select depth from requirements where project_id = $1 and number = 2")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(depth, 2);
}

#[tokio::test]
async fn records_are_matched_by_key_then_by_number_and_name() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "mat").await;
    let l = limits();
    let c = ctx(&p, &l);
    let mut a = planner(&store, &p, ImportMode::Upsert).await;
    store
        .import_shared_step(
            &mut a,
            &c,
            &shared(2, Some(7), Some(("qase", "h1")), "Log in"),
            true,
        )
        .await
        .unwrap();
    let mut b = planner(&store, &p, ImportMode::Upsert).await;
    store
        .import_shared_step(
            &mut b,
            &c,
            &shared(2, Some(7), Some(("qase", "h1")), "Renamed"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        n(&db.pool, SHARED, &p).await,
        1,
        "same key: the same shared step"
    );
    let name: String = sqlx::query_scalar("select name from shared_steps where project_id = $1")
        .bind(p.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(name, "Renamed", "upsert updates it");
    let mut d = planner(&store, &p, ImportMode::CreateOnly).await;
    store
        .import_shared_step(&mut d, &c, &shared(2, Some(1), None, "Renamed"), true)
        .await
        .unwrap();
    assert_eq!(
        n(&db.pool, SHARED, &p).await,
        1,
        "no key: the same number and name"
    );
    store
        .import_shared_step(&mut d, &c, &shared(3, Some(9), None, "Other"), true)
        .await
        .unwrap();
    assert_eq!(n(&db.pool, SHARED, &p).await, 2);
    let key: i64 =
        sqlx::query_scalar("select count(*) from shared_step_keys where project_id = $1")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(key, 1);
    assert!(
        d.rec
            .shared
            .contains_key(&("number".to_string(), "9".to_string()))
    );
}

#[tokio::test]
async fn requirement_parents_depth_and_count() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "rq").await;
    let l = TmLimits {
        max_requirement_depth: 2,
        max_requirements: 4,
        ..limits()
    };
    let c = ctx(&p, &l);
    let mut pl = planner(&store, &p, ImportMode::CreateOnly).await;
    let all = [
        req(2, Some(1), "A", None),
        req(3, Some(2), "B", Some(1)),
        req(4, Some(3), "C too deep", Some(2)),
        req(5, Some(4), "Orphan", Some(99)),
        req(6, Some(5), "D", None),
        req(7, Some(6), "E over the count", None),
    ];
    for r in &all {
        store
            .import_requirement(&mut pl, &c, r, true)
            .await
            .unwrap();
    }
    let errors: Vec<(i64, &str)> = pl
        .report
        .errors
        .iter()
        .map(|e| (e.row, e.code.as_str()))
        .collect();
    assert_eq!(
        errors,
        vec![(4, "limit_reached"), (7, "limit_reached")],
        "{errors:?}"
    );
    assert_eq!(pl.report.warnings.len(), 1);
    assert_eq!(pl.report.warnings[0].code, "parent_not_found");
    assert_eq!(n(&db.pool, REQS, &p).await, 4);
    let orphan: (i16, Option<Uuid>) = sqlx::query_as(
        "select depth, parent_id from requirements where project_id = $1 and title = 'Orphan'",
    )
    .bind(p.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(orphan, (1, None));
    let paths: Vec<(String, i16, i32)> = sqlx::query_as(
        "select title, depth, cardinality(path_ids) from requirements where project_id = $1 order by number",
    )
    .bind(p.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(paths[1], ("B".to_string(), 2, 2), "{paths:?}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select requirement_seq from projects where id = $1")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        5,
        "the sequence follows the kept numbers"
    );
}

#[tokio::test]
async fn a_dry_run_creates_nothing_and_checks_the_records() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "dry").await;
    let l = limits();
    let c = ctx(&p, &l);
    let mut pl = planner(&store, &p, ImportMode::CreateOnly).await;
    store
        .import_shared_step(
            &mut pl,
            &c,
            &shared(2, Some(1), Some(("testrail", "5")), "ok"),
            false,
        )
        .await
        .unwrap();
    let mut empty = shared(3, None, None, "no steps");
    empty.steps.clear();
    store
        .import_shared_step(&mut pl, &c, &empty, false)
        .await
        .unwrap();
    store
        .import_requirement(&mut pl, &c, &req(4, Some(1), "Root", None), false)
        .await
        .unwrap();
    let mut bad = req(5, None, "bad status", None);
    bad.status = Some("finished".into());
    store
        .import_requirement(&mut pl, &c, &bad, false)
        .await
        .unwrap();
    let mut url = req(6, None, "bad url", None);
    url.url = Some("javascript:alert(1)".into());
    store
        .import_requirement(&mut pl, &c, &url, false)
        .await
        .unwrap();
    assert_eq!(n(&db.pool, SHARED, &p).await, 0);
    assert_eq!(n(&db.pool, REQS, &p).await, 0);
    let rows: Vec<i64> = pl.report.errors.iter().map(|e| e.row).collect();
    assert_eq!(rows, vec![3, 5, 6], "{:?}", pl.report.errors);
    assert!(
        pl.rec
            .virtual_keys
            .contains(&("testrail".to_string(), "5".to_string()))
    );
}

#[tokio::test]
async fn a_closed_gate_refuses_the_records() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "gate").await;
    let l = TmLimits {
        gate_mode: TmGateMode::Closed,
        ..TmLimits::default()
    };
    let c = ctx(&p, &l);
    let mut pl = planner(&store, &p, ImportMode::CreateOnly).await;
    let e = store
        .import_shared_step(&mut pl, &c, &shared(2, None, None, "x"), true)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &e,
            TmError::Conflict {
                code: "upgrade_in_progress",
                ..
            }
        ),
        "{e:?}"
    );
    let e = store
        .import_requirement(&mut pl, &c, &req(3, None, "x", None), false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &e,
            TmError::Conflict {
                code: "upgrade_in_progress",
                ..
            }
        ),
        "{e:?}"
    );
    assert_eq!(
        n(&db.pool, SHARED, &p).await + n(&db.pool, REQS, &p).await,
        0
    );
}

/// Review H3: shared step and requirement records ran without a heartbeat, and their maps
/// were saved only with the first batch of cases. A file with many records outlived the
/// 120 s lease (a second worker then created them all again), and a restart before the first
/// batch duplicated every keyless record. The worker now extends the lease and saves the maps
/// as it goes; a resumed run reuses them and keeps the first run's numbering decision.
#[tokio::test]
async fn a_long_run_of_records_keeps_the_lease_and_saves_its_maps() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "lease").await;
    let l = limits();
    let c = ctx(&p, &l);
    let job = Uuid::now_v7();
    sqlx::query(
        "insert into tm_jobs (id, project_id, kind, params, status, lease_owner, lease_until) \
         values ($1, $2, 'import', '{\"format\": \"fastproof-ndjson\"}', 'running', 'w1', now() + interval '5 seconds')",
    )
    .bind(job)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut first = planner(&store, &p, ImportMode::CreateOnly).await;
    store
        .import_shared_step(&mut first, &c, &shared(2, None, None, "keyless"), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut first, &c, &req(3, Some(5), "Kept", None), true)
        .await
        .unwrap();
    assert!(
        store
            .save_import_records(&mut first, job, "w1")
            .await
            .unwrap()
    );
    assert!(!first.rec.dirty);
    let (params, lease): (serde_json::Value, bool) = sqlx::query_as(
        "select params, lease_until > now() + interval '60 seconds' from tm_jobs where id = $1",
    )
    .bind(job)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(lease, "the lease is extended");
    assert_eq!(
        params["format"], "fastproof-ndjson",
        "other parameters stay"
    );
    assert_eq!(params["importMaps"]["keepRequirementNumbers"], true);
    assert!(
        !store
            .save_import_records(&mut first, job, "someone-else")
            .await
            .unwrap(),
        "another worker's lease is not touched"
    );

    let mut second = planner(&store, &p, ImportMode::CreateOnly).await;
    second.rec.load(params.get("importMaps"));
    store
        .import_shared_step(&mut second, &c, &shared(2, None, None, "keyless"), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut second, &c, &req(3, Some(5), "Kept", None), true)
        .await
        .unwrap();
    store
        .import_requirement(&mut second, &c, &req(4, Some(9), "Next", None), true)
        .await
        .unwrap();
    assert!(
        second.report.errors.is_empty(),
        "{:?}",
        second.report.errors
    );
    assert_eq!(
        (n(&db.pool, SHARED, &p).await, n(&db.pool, REQS, &p).await),
        (1, 2),
        "the records of the first run are not created again"
    );
    let numbers: Vec<i64> =
        sqlx::query_scalar("select number from requirements where project_id = $1 order by number")
            .bind(p.id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        numbers,
        vec![5, 9],
        "the resumed run keeps the file's numbers as the first did"
    );
    let payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "select payload from events where type = 'requirement.updated' and project = $1 order by id",
    )
    .bind(&p.key)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(payloads.len(), 2);
    let keys: Vec<&String> = payloads[0].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec!["number", "revision"],
        "the event shape of the API"
    );
}

/// Review L1 and L4: requirement keys follow the requirements API (an imported row the API
/// would refuse could not be edited later), and a shared step that matches by key but is
/// archived or an unreviewed AI draft is not reused (the cases keep the steps inline instead
/// of failing one by one).
#[tokio::test]
async fn records_follow_the_api_rules_and_skip_unusable_shared_steps() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "rules").await;
    let l = limits();
    let c = ctx(&p, &l);
    let mut pl = planner(&store, &p, ImportMode::Upsert).await;
    let mut spaced = req(2, None, "spaced key", None);
    spaced.issue_key = Some("PAY 1".into());
    let mut tracker = req(3, None, "tracker without key", None);
    tracker.tracker_id = Some("jira".into());
    let mut bad_tracker = req(4, None, "bad tracker", None);
    bad_tracker.tracker_id = Some("my tracker".into());
    bad_tracker.issue_key = Some("PAY-1".into());
    let mut ok = req(5, None, "ok", None);
    ok.tracker_id = Some("jira".into());
    ok.issue_key = Some("PAY-1".into());
    for r in [&spaced, &tracker, &bad_tracker, &ok] {
        store
            .import_requirement(&mut pl, &c, r, true)
            .await
            .unwrap();
    }
    let rows: Vec<i64> = pl.report.errors.iter().map(|e| e.row).collect();
    assert_eq!(rows, vec![2, 3, 4], "{:?}", pl.report.errors);
    assert_eq!(n(&db.pool, REQS, &p).await, 1);

    for (row, key, how) in [
        (6, "h-arch", "archived_at = now()"),
        (7, "h-ai", "ai_unreviewed = true"),
    ] {
        let mut a = planner(&store, &p, ImportMode::Upsert).await;
        store
            .import_shared_step(
                &mut a,
                &c,
                &shared(row, None, Some(("qase", key)), key),
                true,
            )
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update shared_steps set {how} where project_id = $1 and name = $2"
        )))
        .bind(p.id)
        .bind(key)
        .execute(&db.pool)
        .await
        .unwrap();
        let before = n(&db.pool, SHARED, &p).await;
        let mut b = planner(&store, &p, ImportMode::Upsert).await;
        let mut changed = shared(row, None, Some(("qase", key)), key);
        changed.steps[0].action = "changed".into();
        store
            .import_shared_step(&mut b, &c, &changed, true)
            .await
            .unwrap();
        assert_eq!(
            n(&db.pool, SHARED, &p).await,
            before,
            "{key}: nothing created"
        );
        assert_eq!(b.report.warnings.len(), 1, "{key}: {:?}", b.report.warnings);
        assert!(
            !b.rec
                .shared
                .contains_key(&("qase".to_string(), key.to_string())),
            "{key}: cases keep inline steps"
        );
        let action: String = sqlx::query_scalar(
            "select steps -> 0 ->> 'action' from shared_steps where project_id = $1 and name = $2",
        )
        .bind(p.id)
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(action, format!("do {key}"), "{key}: not updated");
    }
}

/// Review M3: the shared steps of an export were all read into memory before the 64 MiB
/// check. They are read row by row now and the export stops at the limit.
#[tokio::test]
async fn exported_shared_steps_stop_at_the_byte_limit() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "exp").await;
    let l = limits();
    let c = ctx(&p, &l);
    let mut pl = planner(&store, &p, ImportMode::CreateOnly).await;
    for i in 0..3u64 {
        let mut x = shared(i + 2, None, None, &format!("S{i}"));
        x.steps[0].action = "x".repeat(1000);
        store
            .import_shared_step(&mut pl, &c, &x, true)
            .await
            .unwrap();
    }
    let src = store
        .export_source(p.id, &Default::default(), 100)
        .await
        .unwrap();
    assert!(src.all);
    let all = store
        .export_shared_steps(p.id, &src, 1 << 20)
        .await
        .unwrap();
    assert_eq!(all.len(), 3);
    let e = store
        .export_shared_steps(p.id, &src, 2500)
        .await
        .unwrap_err();
    assert!(
        matches!(
            &e,
            TmError::Conflict {
                code: "limit_reached",
                ..
            }
        ),
        "{e:?}"
    );
}
