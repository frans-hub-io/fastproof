//! Shared steps on a real PostgreSQL: references and their validation, versions, usages,
//! purge, pins in runs and their expansion with paths, refresh and rerun, extract, inline
//! (also as a job) and the review-aware runnable version. They need
//! `FASTPROOF_TEST_DATABASE_URL`; without it they pass without doing anything.

use std::time::Instant;

use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::expand;
use fastproof_store::tm::runs::step_paths;
use fastproof_store::tm::shared_steps::{INLINE_SYNC_MAX, InlineOutcome, SharedStepQuery};
use fastproof_store::tm::{
    CaseBody, CaseReviewSettings, CaseRow, CaseStep, CaseWrite, ExtractSharedStep, ReviewDecision,
    ReviewKind, ReviewMode, ReviewState, ReviewVoteWrite, RunItemsAdd, RunStepWrite,
    SharedStepOrigin, SharedStepRef, SharedStepRow, SharedStepWrite, StepStatus, TmError,
    TmGateMode, TmJobKind, TmJobStatus, TmLimits, VersionSource,
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

fn open() -> TmLimits {
    TmLimits {
        gate_mode: TmGateMode::Open,
        ..Default::default()
    }
}

fn ctx<'a>(p: &'a Project, by: &'a str, l: &'a TmLimits) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by,
        limits: l,
        can_review: false,
    }
}

fn step(id: &str, action: &str) -> CaseStep {
    CaseStep {
        id: id.into(),
        action: action.into(),
        ..Default::default()
    }
}

fn write(name: &str, steps: Vec<CaseStep>) -> SharedStepWrite {
    SharedStepWrite {
        name: name.into(),
        description: None,
        steps,
    }
}

fn reference(row: &SharedStepRow) -> CaseStep {
    CaseStep {
        id: String::new(),
        shared: Some(SharedStepRef {
            id: row.id,
            number: row.number,
            version: None,
        }),
        ..Default::default()
    }
}

async fn new_shared(store: &Store, p: &Project, l: &TmLimits, name: &str) -> SharedStepRow {
    store
        .create_shared_step(
            &ctx(p, "ann", l),
            &write(
                name,
                vec![
                    step("", "open the login page"),
                    CaseStep {
                        steps: vec![step("", "type the user"), step("", "type the password")],
                        ..step("", "sign in")
                    },
                ],
            ),
            SharedStepOrigin::Ui,
        )
        .await
        .unwrap()
}

async fn new_case(
    store: &Store,
    p: &Project,
    l: &TmLimits,
    name: &str,
    steps: Vec<CaseStep>,
) -> Result<CaseRow, TmError> {
    store
        .create_case(
            &ctx(p, "ann", l),
            &CaseWrite {
                name: name.into(),
                body: Some(CaseBody {
                    steps,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
}

fn replace_body(row: &CaseRow, steps: Vec<CaseStep>) -> CaseWrite {
    CaseWrite {
        name: row.name.clone(),
        body: Some(CaseBody {
            steps,
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn usage_cases(db: &TestDb, shared: &SharedStepRow) -> Vec<i64> {
    sqlx::query_scalar(
        "select c.number from shared_step_usages u join cases c on c.id = u.case_id \
         where u.shared_step_id = $1 order by c.number",
    )
    .bind(shared.id)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

async fn open_launch(db: &TestDb, p: &Project) -> Uuid {
    let id = Uuid::now_v7();
    let number: i64 = sqlx::query_scalar(
        "update projects set launch_seq = launch_seq + 1 where id = $1 returning launch_seq",
    )
    .bind(p.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status, started_at, created_by_can_sync) \
         values ($1, $2, $3, 'l', 'running', now(), true)",
    )
    .bind(id)
    .bind(p.id)
    .bind(number)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn start_item(
    store: &Store,
    db: &TestDb,
    p: &Project,
    l: &TmLimits,
    launch: Uuid,
    case: &CaseRow,
) -> Uuid {
    let added = store
        .add_run_items(
            p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![case.number]),
                ..Default::default()
            },
            l,
        )
        .await
        .unwrap();
    assert_eq!(added.added, 1);
    sqlx::query_scalar("select id from run_items where launch_id = $1 and case_id = $2")
        .bind(launch)
        .bind(case.id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn invalid_case(r: &Result<CaseRow, TmError>) -> bool {
    matches!(
        r,
        Err(TmError::Invalid {
            code: "invalid_case",
            ..
        })
    )
}

#[tokio::test]
async fn shared_step_versions_archive_and_restore() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ss").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    assert_eq!((ss.number, ss.version, ss.revision), (1, 1, 1));
    assert_eq!(ss.key(), "SS-1");

    let body = write(
        "Log in",
        vec![step("", "open the login page"), step("", "press enter")],
    );
    let (before, after) = store
        .update_shared_step(&c, 1, ss.revision, &body)
        .await
        .unwrap();
    assert_eq!((before.version, after.version, after.revision), (1, 2, 2));
    let same = store
        .update_shared_step(&c, 1, after.revision, &write("Log in", after.steps.clone()))
        .await
        .unwrap();
    assert_eq!(
        (same.1.version, same.1.revision),
        (2, 2),
        "an unchanged save is no version"
    );
    assert!(matches!(
        store.update_shared_step(&c, 1, 1, &body).await,
        Err(TmError::Stale {
            current_revision: 2,
            ..
        })
    ));

    let versions = store.shared_step_versions(p.id, 1, None, 10).await.unwrap();
    assert_eq!(
        versions.items.iter().map(|v| v.version).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(versions.items[0].changed, vec!["steps".to_string()]);
    assert_eq!(versions.items[1].changed.len(), 3);
    let v1 = store.shared_step_version(p.id, 1, 1).await.unwrap();
    assert_eq!(v1.snapshot.steps.len(), 2);
    assert_eq!(v1.snapshot.steps[1].steps.len(), 2);
    assert!(matches!(
        store.shared_step_version(p.id, 1, 9).await,
        Err(TmError::NotFound(_))
    ));
    let page = store.shared_step_versions(p.id, 1, None, 1).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next.as_deref(), Some("2"));

    let (_, restored) = store
        .restore_shared_step_version(&c, 1, 1, after.revision)
        .await
        .unwrap();
    assert_eq!(restored.version, 3);
    assert_eq!(restored.steps[1].steps.len(), 2);
    let versions = store.shared_step_versions(p.id, 1, None, 10).await.unwrap();
    assert_eq!(versions.items[0].source, VersionSource::Restore);

    let archived = store.set_shared_step_archived(&c, 1, true).await.unwrap();
    assert!(archived.archived_at.is_some());
    assert!(matches!(
        store
            .update_shared_step(&c, 1, archived.revision, &body)
            .await,
        Err(TmError::Conflict {
            code: "shared_step_archived",
            ..
        })
    ));
    let listed = |archived: bool, limit: i64, before: Option<String>| SharedStepQuery {
        archived,
        limit,
        before,
        q: None,
    };
    let shown = store
        .list_shared_steps(p.id, &listed(false, 10, None))
        .await
        .unwrap();
    assert!(shown.items.is_empty());
    let trash = store
        .list_shared_steps(p.id, &listed(true, 10, None))
        .await
        .unwrap();
    assert_eq!(trash.items.len(), 1);
    let back = store.set_shared_step_archived(&c, 1, false).await.unwrap();
    assert!(back.archived_at.is_none());

    for i in 0..3 {
        new_shared(&store, &p, &l, &format!("Checkout flow {i}")).await;
    }
    for (q, want) in [
        ("SS-3", vec![3]),
        ("checkout", vec![2, 3, 4]),
        ("log in", vec![1]),
    ] {
        let got: Vec<i64> = store
            .list_shared_steps(
                p.id,
                &SharedStepQuery {
                    q: Some(q.into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .items
            .iter()
            .map(|i| i.number)
            .collect();
        assert_eq!(got, want, "{q}");
    }
    let first = store
        .list_shared_steps(p.id, &listed(false, 2, None))
        .await
        .unwrap();
    assert_eq!(first.next.as_deref(), Some("2"));
    let rest = store
        .list_shared_steps(p.id, &listed(false, 5, first.next.clone()))
        .await
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|i| i.number).collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(first.items[0].step_count, 4);
}

#[tokio::test]
async fn references_are_validated_and_usages_follow_the_case() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ref").await;
    let other = project(&store, "oth").await;
    let l = open();
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let foreign = new_shared(&store, &other, &l, "Elsewhere").await;

    let a = new_case(
        &store,
        &p,
        &l,
        "A",
        vec![step("", "first"), reference(&ss), step("", "last")],
    )
    .await
    .unwrap();
    assert_eq!(a.body.steps[1].shared.as_ref().unwrap().id, ss.id);
    assert_eq!(usage_cases(&db, &ss).await, vec![a.number]);
    let view = store.case_view(p.id, a.number).await.unwrap();
    assert_eq!(view.shared_steps.len(), 1);
    assert_eq!(view.shared_steps[0].number, ss.number);
    assert_eq!(view.shared_steps[0].version, 1);
    assert_eq!(view.shared_steps[0].steps.len(), 2);

    assert!(
        matches!(
            new_case(&store, &p, &l, "bad", vec![reference(&foreign)]).await,
            Err(TmError::NotFound(_))
        ),
        "a shared step of another project is 404"
    );
    let mut wrong_number = reference(&ss);
    wrong_number.shared.as_mut().unwrap().number = 99;
    assert!(invalid_case(
        &new_case(&store, &p, &l, "bad", vec![wrong_number]).await
    ));
    let mut with_content = reference(&ss);
    with_content.action = "mine".into();
    assert!(invalid_case(
        &new_case(&store, &p, &l, "bad", vec![with_content]).await
    ));
    let mut versioned = reference(&ss);
    versioned.shared.as_mut().unwrap().version = Some(1);
    assert!(invalid_case(
        &new_case(&store, &p, &l, "bad", vec![versioned]).await
    ));
    let nested = CaseStep {
        steps: vec![reference(&ss)],
        ..step("", "outer")
    };
    assert!(invalid_case(
        &new_case(&store, &p, &l, "bad", vec![nested]).await
    ));

    let small = TmLimits {
        max_shared_refs: 2,
        ..open()
    };
    assert!(matches!(
        new_case(&store, &p, &small, "many", vec![reference(&ss); 3]).await,
        Err(TmError::Conflict {
            code: "limit_reached",
            ..
        })
    ));
    new_case(&store, &p, &small, "twice", vec![reference(&ss); 2])
        .await
        .unwrap();

    let closed = TmLimits {
        gate_mode: TmGateMode::Closed,
        ..Default::default()
    };
    assert!(
        matches!(
            new_case(&store, &p, &closed, "gated", vec![reference(&ss)]).await,
            Err(TmError::Conflict {
                code: "upgrade_in_progress",
                ..
            })
        ),
        "a body that gains a shared step is a gated write"
    );
    new_case(&store, &p, &closed, "plain", vec![step("", "x")])
        .await
        .expect("bodies without shared steps are not gated");

    store
        .set_shared_step_archived(&ctx(&p, "ann", &l), ss.number, true)
        .await
        .unwrap();
    assert!(matches!(
        new_case(&store, &p, &l, "new ref", vec![reference(&ss)]).await,
        Err(TmError::Conflict {
            code: "shared_step_archived",
            ..
        })
    ));
    let mut edited = a.body.steps.clone();
    edited.push(step("", "one more"));
    let (_, a2) = store
        .update_case(
            &ctx(&p, "ann", &l),
            a.number,
            a.revision,
            &replace_body(&a, edited),
            true,
        )
        .await
        .expect("an existing reference to an archived shared step keeps working");
    assert_eq!(usage_cases(&db, &ss).await.len(), 2);

    store
        .update_case(
            &ctx(&p, "ann", &l),
            a2.number,
            a2.revision,
            &replace_body(&a2, vec![step("", "plain now")]),
            true,
        )
        .await
        .unwrap();
    assert_eq!(usage_cases(&db, &ss).await.len(), 1);

    let draft = store
        .create_shared_step(
            &ctx(&p, "agent", &l),
            &write("Draft", vec![step("", "x")]),
            SharedStepOrigin::Ai,
        )
        .await
        .unwrap();
    assert!(draft.ai_unreviewed);
    assert!(invalid_case(
        &new_case(&store, &p, &l, "uses draft", vec![reference(&draft)]).await
    ));
    let (_, reviewed) = store
        .update_shared_step(
            &ctx(&p, "bob", &l),
            draft.number,
            draft.revision,
            &write("Draft", vec![step("", "x")]),
        )
        .await
        .unwrap();
    assert!(
        !reviewed.ai_unreviewed,
        "a person other than the creator reviewed it"
    );
    new_case(&store, &p, &l, "uses draft", vec![reference(&draft)])
        .await
        .unwrap();
}

#[tokio::test]
async fn purge_is_refused_while_used_and_numbers_are_per_project() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pg").await;
    let other = project(&store, "ot").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let case = new_case(&store, &p, &l, "A", vec![reference(&ss)])
        .await
        .unwrap();
    let launch = open_launch(&db, &p).await;
    start_item(&store, &db, &p, &l, launch, &case).await;

    match store.purge_shared_step(&c, ss.number).await {
        Err(TmError::Conflict {
            code: "shared_step_in_use",
            details: Some(d),
            ..
        }) => assert_eq!(d, serde_json::json!({"cases": 1, "openRuns": 1})),
        other => panic!("{other:?}"),
    }
    let usage = store.shared_step_view(p.id, ss.number).await.unwrap().usage;
    assert_eq!((usage.cases, usage.open_runs), (1, 1));
    let list = store
        .shared_step_usages(p.id, ss.number, None, 10)
        .await
        .unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].case_number, case.number);
    assert_eq!(list.open_runs, 1);

    store
        .update_case(
            &c,
            case.number,
            case.revision,
            &replace_body(&case, vec![step("", "plain")]),
            true,
        )
        .await
        .unwrap();
    assert!(
        matches!(
            store.purge_shared_step(&c, ss.number).await,
            Err(TmError::Conflict {
                code: "shared_step_in_use",
                ..
            })
        ),
        "an open run still pins it"
    );
    sqlx::query("update launches set manual_state = 'closed' where id = $1")
        .bind(launch)
        .execute(&db.pool)
        .await
        .unwrap();
    store.purge_shared_step(&c, ss.number).await.unwrap();
    assert!(matches!(
        store.shared_step_view(p.id, ss.number).await,
        Err(TmError::NotFound(_))
    ));

    let theirs = new_shared(&store, &other, &l, "Theirs").await;
    let mine = new_shared(&store, &p, &l, "Mine").await;
    assert_eq!(mine.number, 2, "numbers are never reused");
    assert_eq!(theirs.number, 1, "and are per project");
    assert_eq!(
        store.shared_step_view(other.id, 1).await.unwrap().name,
        "Theirs"
    );
    assert!(matches!(
        store.shared_step_view(p.id, 1).await,
        Err(TmError::NotFound(_))
    ));
    assert!(matches!(
        store.shared_step_usages(other.id, 2, None, 10).await,
        Err(TmError::NotFound(_))
    ));
}

#[tokio::test]
async fn runs_pin_expand_refresh_and_rerun() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "run").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let case = new_case(
        &store,
        &p,
        &l,
        "Checkout",
        vec![step("", "first"), reference(&ss), step("", "last")],
    )
    .await
    .unwrap();

    let l1 = open_launch(&db, &p).await;
    let item1 = start_item(&store, &db, &p, &l, l1, &case).await;
    let pins: serde_json::Value =
        sqlx::query_scalar("select shared_pins from run_items where id = $1")
            .bind(item1)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pins, serde_json::json!({ss.id.to_string(): 1}));

    let run = store.run_item(p.id, item1).await.unwrap();
    assert_eq!(run.steps.len(), 3);
    let shared = &run.steps[1];
    assert_eq!(shared.shared.as_ref().unwrap().version, Some(1));
    assert_eq!(shared.steps.len(), 2);
    assert_eq!(shared.steps[1].steps.len(), 2, "own steps nest to depth 2");
    let paths: Vec<String> = step_paths(&run.steps).into_iter().map(|s| s.path).collect();
    assert_eq!(paths, vec!["1", "2", "2.1", "2.2", "2.2.1", "2.2.2", "3"]);
    assert_eq!(run.shared_pins.len(), 1);
    assert_eq!(
        (run.shared_pins[0].version, run.shared_pins[0].current),
        (1, 1)
    );
    let stored = store
        .case_version_snapshot(case.id, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored.body.steps[1].steps.is_empty(),
        "versions keep refs unexpanded"
    );

    store
        .update_shared_step(
            &c,
            ss.number,
            ss.revision,
            &write(
                "Log in",
                vec![step("", "open the new login page"), step("", "confirm")],
            ),
        )
        .await
        .unwrap();
    let run = store.run_item(p.id, item1).await.unwrap();
    assert_eq!(
        run.steps[1].steps.len(),
        2,
        "a run started before the edit keeps its pinned steps"
    );
    assert_eq!(run.steps[1].steps[0].action, "open the login page");
    assert_eq!(run.steps[1].steps[1].steps.len(), 2);
    assert_eq!(
        (run.shared_pins[0].version, run.shared_pins[0].current),
        (1, 2)
    );

    let l2 = open_launch(&db, &p).await;
    let item2 = start_item(&store, &db, &p, &l, l2, &case).await;
    let run2 = store.run_item(p.id, item2).await.unwrap();
    assert_eq!(run2.steps[1].steps[0].action, "open the new login page");
    assert_eq!(run2.shared_pins[0].version, 2);

    let (_, rev) = store.refresh_run_item(p.id, &p.key, item1).await.unwrap();
    assert!(rev > run.revision);
    let refreshed = store.run_item(p.id, item1).await.unwrap();
    assert_eq!(refreshed.shared_pins[0].version, 2);
    assert_eq!(
        step_paths(&refreshed.steps)
            .into_iter()
            .map(|s| s.path)
            .collect::<Vec<_>>(),
        vec!["1", "2", "2.1", "2.2", "3"]
    );

    store
        .put_run_step(
            &p,
            item1,
            "1",
            &RunStepWrite {
                status: StepStatus::Passed,
                actual: None,
                comment: None,
                attachment_ids: None,
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    assert!(matches!(
        store.refresh_run_item(p.id, &p.key, item1).await,
        Err(TmError::Conflict {
            code: "run_item_started",
            ..
        })
    ));
    assert!(matches!(
        store.refresh_run_item(Uuid::now_v7(), &p.key, item1).await,
        Err(TmError::NotFound(_))
    ));

    store
        .update_shared_step(
            &c,
            ss.number,
            2,
            &write("Log in", vec![step("", "third wording")]),
        )
        .await
        .unwrap();
    let before = store.run_item(p.id, item1).await.unwrap();
    store
        .rerun_run_item(&p, item1, before.revision, "tess")
        .await
        .unwrap();
    let after = store.run_item(p.id, item1).await.unwrap();
    assert_eq!(after.shared_pins[0].version, 3, "a rerun re-pins");
    assert_eq!(after.steps[1].steps[0].action, "third wording");
    let earlier: serde_json::Value = sqlx::query_scalar(
        "select shared_pins from run_item_executions where run_item_id = $1 and execution_no = 1",
    )
    .bind(item1)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(earlier, serde_json::json!({ss.id.to_string(): 2}));

    let mut conn = db.pool.acquire().await.unwrap();
    let review = CaseReviewSettings::default();
    let pins = expand::pins_for_items(&mut conn, p.id, &[(case.id, 1)], &review)
        .await
        .unwrap();
    assert_eq!(pins[&case.id][&ss.id], 3);
    let empty = expand::pins_for_items(&mut conn, p.id, &[], &review)
        .await
        .unwrap();
    assert!(empty.is_empty());
    let mut steps = vec![step("a", "own")];
    expand::expand_steps(&mut conn, p.id, &mut steps, &Default::default())
        .await
        .unwrap();
    assert_eq!(steps.len(), 1);
    drop(conn);

    let case2 = store
        .case_by_number(p.id, case.number)
        .await
        .unwrap()
        .unwrap();
    let (_, v2) = store
        .update_case(
            &c,
            case2.number,
            case2.revision,
            &replace_body(&case2, vec![step("", "no shared any more")]),
            true,
        )
        .await
        .unwrap();
    let l3 = open_launch(&db, &p).await;
    let item3 = start_item(&store, &db, &p, &l, l3, &v2).await;
    let none: Option<serde_json::Value> =
        sqlx::query_scalar("select shared_pins from run_items where id = $1")
            .bind(item3)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(none.is_none());
    let old = store.run_item(p.id, item1).await.unwrap();
    assert_eq!(old.case_version, 1, "the older case version still expands");
    let mut conn = db.pool.acquire().await.unwrap();
    let pins = expand::pins_for_items(&mut conn, p.id, &[(case.id, 1)], &review)
        .await
        .unwrap();
    assert_eq!(
        pins[&case.id][&ss.id], 3,
        "pins of an older case version come from its snapshot"
    );
}

#[tokio::test]
async fn review_decides_the_runnable_version() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "rv").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ann = CaseCtx {
        can_review: true,
        ..ctx(&p, "ann", &l)
    };
    let bob = CaseCtx {
        can_review: true,
        ..ctx(&p, "bob", &l)
    };
    let approve = |version: i32| ReviewVoteWrite {
        version,
        decision: ReviewDecision::Approve,
        comment: None,
    };
    let s = ProjectSettings {
        case_review: CaseReviewSettings {
            mode: ReviewMode::Manual,
            ..Default::default()
        },
        ..Default::default()
    };
    store.put_project_settings(p.id, &s).await.unwrap();
    let ss = new_shared(&store, &p, &l, "Log in").await;
    assert_eq!(
        (ss.review_state, ss.approved_version),
        (Some(ReviewState::Pending), None),
        "a new shared step waits for review"
    );
    let (t, ss_revision) = store
        .vote(
            &bob,
            ReviewKind::SharedStep,
            ss.number,
            ss.revision,
            &approve(1),
        )
        .await
        .unwrap();
    assert_eq!(
        (t.review_state, t.approved_version),
        (Some(ReviewState::Approved), Some(1))
    );
    let case = new_case(&store, &p, &l, "A", vec![reference(&ss)])
        .await
        .unwrap();
    assert_eq!(
        (case.review_state, case.approved_version),
        (Some(ReviewState::Pending), None),
        "a new manual case waits for review and is not runnable"
    );
    let launch = open_launch(&db, &p).await;
    let skipped = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![case.number]),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    assert_eq!(skipped.added, 0, "a case never approved does not run");
    let refused = store
        .vote(
            &ann,
            ReviewKind::Case,
            case.number,
            case.revision,
            &approve(1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            TmError::Forbidden {
                code: "self_approval",
                ..
            }
        ),
        "the author does not approve her own case: {refused:?}"
    );
    store
        .vote(
            &bob,
            ReviewKind::Case,
            case.number,
            case.revision,
            &approve(1),
        )
        .await
        .unwrap();

    let (_, v2) = store
        .update_shared_step(
            &c,
            ss.number,
            ss_revision,
            &write("Log in", vec![step("", "second version")]),
        )
        .await
        .unwrap();
    assert_eq!(
        (v2.version, v2.review_state, v2.approved_version),
        (2, Some(ReviewState::Pending), Some(1)),
        "an edit waits for review and v1 stays runnable"
    );
    let view = store.case_view(p.id, case.number).await.unwrap();
    assert_eq!(
        (
            view.shared_steps[0].version,
            view.shared_steps[0].steps.len()
        ),
        (1, 2),
        "the case lists the shared step at its approved version"
    );
    let item = start_item(&store, &db, &p, &l, launch, &case).await;
    let run = store.run_item(p.id, item).await.unwrap();
    assert_eq!(run.shared_pins[0].version, 1);
    assert_eq!(run.shared_pins[0].current, 1);
    assert_eq!(run.steps[0].steps.len(), 2);
    let refused = store
        .vote(
            &ann,
            ReviewKind::SharedStep,
            ss.number,
            v2.revision,
            &approve(2),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            TmError::Forbidden {
                code: "self_approval",
                ..
            }
        ),
        "the author of the pending change does not approve it: {refused:?}"
    );
    let (t, _) = store
        .vote(
            &bob,
            ReviewKind::SharedStep,
            ss.number,
            v2.revision,
            &approve(2),
        )
        .await
        .unwrap();
    assert_eq!(
        (t.review_state, t.approved_version),
        (Some(ReviewState::Approved), Some(2))
    );
    let run = store.run_item(p.id, item).await.unwrap();
    assert_eq!(
        (run.shared_pins[0].version, run.shared_pins[0].current),
        (1, 2),
        "a started run keeps its pin and offers the newly approved version"
    );
}

#[tokio::test]
async fn extract_moves_adjacent_steps_into_a_shared_step() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ex").await;
    let other = project(&store, "ox").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let case = new_case(
        &store,
        &p,
        &l,
        "Checkout",
        vec![
            step("s_a", "open"),
            CaseStep {
                steps: vec![step("s_b1", "child")],
                ..step("s_b", "log in")
            },
            step("s_c", "pay"),
            step("s_d", "done"),
        ],
    )
    .await
    .unwrap();
    let ex = |ids: &[&str]| ExtractSharedStep {
        step_ids: ids.iter().map(|s| s.to_string()).collect(),
        name: "Log in and pay".into(),
        description: Some("both".into()),
    };
    for (ids, why) in [
        (vec!["s_a", "s_c"], "not adjacent"),
        (vec!["s_b1"], "not top level"),
        (vec!["nope"], "unknown"),
        (vec![], "empty"),
    ] {
        let r = store
            .extract_shared_step(&c, case.number, case.revision, &ex(&ids))
            .await;
        assert!(
            matches!(
                r,
                Err(TmError::Invalid {
                    code: "invalid_case",
                    ..
                })
            ),
            "{why}: {r:?}"
        );
    }
    assert!(matches!(
        store
            .extract_shared_step(&c, case.number, case.revision + 5, &ex(&["s_b"]))
            .await,
        Err(TmError::Stale { .. })
    ));
    assert!(matches!(
        store
            .extract_shared_step(
                &ctx(&other, "ann", &l),
                case.number,
                case.revision,
                &ex(&["s_b"])
            )
            .await,
        Err(TmError::NotFound(_))
    ));
    let none: i64 = sqlx::query_scalar("select count(*) from shared_steps")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(none, 0, "a refused extract creates nothing");

    let (shared, after) = store
        .extract_shared_step(&c, case.number, case.revision, &ex(&["s_c", "s_b"]))
        .await
        .unwrap();
    assert_eq!(shared.number, 1);
    assert_eq!(shared.steps.len(), 2);
    assert_eq!(shared.steps[0].id, "s_b");
    assert_eq!(shared.steps[0].steps[0].id, "s_b1");
    assert_eq!(after.body.steps.len(), 3);
    assert_eq!(after.body.steps[1].shared.as_ref().unwrap().id, shared.id);
    assert_eq!(after.version, 2);
    assert_eq!(usage_cases(&db, &shared).await, vec![case.number]);
    let view = store.case_view(p.id, case.number).await.unwrap();
    assert_eq!(view.shared_steps[0].steps.len(), 2);

    let gated = TmLimits {
        gate_mode: TmGateMode::Closed,
        ..Default::default()
    };
    let again = new_case(
        &store,
        &p,
        &open(),
        "Second",
        vec![step("s_x", "a"), step("s_y", "b")],
    )
    .await
    .unwrap();
    let r = store
        .extract_shared_step(
            &ctx(&p, "ann", &gated),
            again.number,
            again.revision,
            &ex(&["s_x"]),
        )
        .await;
    assert!(
        matches!(
            r,
            Err(TmError::Conflict {
                code: "upgrade_in_progress",
                ..
            })
        ),
        "{r:?}"
    );
    let n: i64 = sqlx::query_scalar("select count(*) from shared_steps where project_id = $1")
        .bind(p.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "a gated extract rolls back with its shared step");
}

#[tokio::test]
async fn inline_replaces_references_with_copies() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "in").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let a = new_case(&store, &p, &l, "A", vec![step("", "first"), reference(&ss)])
        .await
        .unwrap();
    let b = new_case(&store, &p, &l, "B", vec![reference(&ss), reference(&ss)])
        .await
        .unwrap();
    match store.inline_shared_step(&c, ss.number).await.unwrap() {
        InlineOutcome::Done { updated, skipped } => assert_eq!((updated, skipped), (2, 0)),
        other => panic!("{other:?}"),
    }
    let a2 = store.case_by_number(p.id, a.number).await.unwrap().unwrap();
    assert_eq!((a2.version, a2.body.steps.len()), (2, 3));
    assert!(a2.body.steps.iter().all(|s| s.shared.is_none()));
    assert_eq!(a2.body.steps[2].steps.len(), 2);
    let b2 = store.case_by_number(p.id, b.number).await.unwrap().unwrap();
    assert_eq!(b2.body.steps.len(), 4);
    let ids: std::collections::HashSet<&String> = b2.body.steps.iter().map(|s| &s.id).collect();
    assert_eq!(ids.len(), 4, "inlined copies get fresh step ids");
    let source: String =
        sqlx::query_scalar("select source from case_versions where case_id = $1 and version = 2")
            .bind(a.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(source, "bulk");
    assert!(usage_cases(&db, &ss).await.is_empty());
    let usage = store.shared_step_view(p.id, ss.number).await.unwrap().usage;
    assert_eq!(usage.cases, 0);
    let unused = store.purge_shared_step(&c, ss.number).await;
    assert!(unused.is_ok(), "{unused:?}");
}

#[tokio::test]
async fn inline_runs_as_a_job_above_the_sync_limit() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "jb").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let a = new_case(&store, &p, &l, "A", vec![reference(&ss)])
        .await
        .unwrap();
    let b = new_case(&store, &p, &l, "B", vec![step("", "x"), reference(&ss)])
        .await
        .unwrap();
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by) \
         select gen_random_uuid(), $1, 1000 + g, 'filler ' || g, 'active', 'ui', '{\"steps\": []}'::jsonb, 'test', 'test' \
         from generate_series(1, $2) g",
    )
    .bind(p.id)
    .bind(INLINE_SYNC_MAX as i32)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into shared_step_usages (shared_step_id, case_id, project_id) \
         select $1, id, project_id from cases where project_id = $2 and number > 1000 on conflict do nothing",
    )
    .bind(ss.id)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let job = match store.inline_shared_step(&c, ss.number).await.unwrap() {
        InlineOutcome::Job(j) => j,
        other => panic!("{other:?}"),
    };
    assert_eq!(job.kind, TmJobKind::SharedInline);
    assert!(matches!(
        store.inline_shared_step(&c, ss.number).await,
        Err(TmError::Conflict { .. })
    ));
    let claimed = store
        .claim_tm_job("worker-1", TmJobKind::SharedInline)
        .await
        .unwrap()
        .expect("the job is claimable");
    assert_eq!(claimed.id, job.id);
    store
        .run_shared_inline_job(job.id, "worker-1", &l)
        .await
        .unwrap();
    let done = store.tm_job(p.id, job.id).await.unwrap().unwrap();
    assert_eq!(done.status, TmJobStatus::Done);
    assert_eq!(done.report.unwrap().update, 2);
    for n in [a.number, b.number] {
        let row = store.case_by_number(p.id, n).await.unwrap().unwrap();
        assert!(row.body.steps.iter().all(|s| s.shared.is_none()), "TC-{n}");
    }
    let left: i64 =
        sqlx::query_scalar("select count(*) from shared_step_usages where shared_step_id = $1")
            .bind(ss.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(left, 0, "the job also drops the usage rows that were stale");
}

#[tokio::test]
async fn saving_a_shared_step_used_by_thousands_of_cases_stays_fast() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pf").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let body = serde_json::json!({"steps": [{"id": "s_1", "action": "", "expected": "", "data": "",
        "attachments": [], "steps": [], "shared": {"id": ss.id, "number": ss.number}}]});
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, body, created_by, updated_by) \
         select gen_random_uuid(), $1, g, 'case ' || g, 'active', 'ui', $3, 'test', 'test' \
         from generate_series(1, $2) g",
    )
    .bind(p.id)
    .bind(5000_i32)
    .bind(&body)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into shared_step_usages (shared_step_id, case_id, project_id) \
         select $1, id, project_id from cases where project_id = $2",
    )
    .bind(ss.id)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut rev = ss.revision;
    let mut best = u128::MAX;
    for i in 0..5 {
        let started = Instant::now();
        let (_, after) = store
            .update_shared_step(
                &c,
                ss.number,
                rev,
                &write("Log in", vec![step("", &format!("wording {i}"))]),
            )
            .await
            .unwrap();
        best = best.min(started.elapsed().as_millis());
        rev = after.revision;
    }
    println!("save of a shared step used by 5000 cases: best of 5 = {best} ms");
    assert!(best < 1000, "{best} ms");
    let touched: i64 =
        sqlx::query_scalar("select count(*) from cases where project_id = $1 and version > 1")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        touched, 0,
        "saving a shared step never touches case versions"
    );
}

/// Security review: restoring a case version is a new reference like any other (archived and
/// purged shared steps, the gate), and a reference to a shared step that is gone never gets
/// stored with the case.
#[tokio::test]
async fn restored_versions_and_new_cases_never_keep_dangling_references() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "dang").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let archived = new_shared(&store, &p, &l, "Archived later").await;
    let purged = new_shared(&store, &p, &l, "Purged later").await;
    let case = new_case(
        &store,
        &p,
        &l,
        "A",
        vec![reference(&purged), reference(&archived)],
    )
    .await
    .unwrap();
    let v2 = store
        .update_case(
            &c,
            case.number,
            case.revision,
            &replace_body(&case, vec![step("", "own step")]),
            true,
        )
        .await
        .unwrap()
        .1;
    store
        .set_shared_step_archived(&c, archived.number, true)
        .await
        .unwrap();
    store.purge_shared_step(&c, purged.number).await.unwrap();
    let r = store
        .restore_case_version(&c, case.number, 1, v2.revision)
        .await;
    assert!(
        matches!(r, Err(TmError::NotFound("shared step"))),
        "a purged shared step is not referenced again: {r:?}"
    );
    let v3 = store
        .update_case(
            &c,
            case.number,
            v2.revision,
            &replace_body(&v2, vec![reference(&archived)]),
            true,
        )
        .await;
    assert!(
        matches!(
            v3,
            Err(TmError::Conflict {
                code: "shared_step_archived",
                ..
            })
        ),
        "{v3:?}"
    );
    let only_archived = new_case(&store, &p, &l, "B", vec![step("", "x")])
        .await
        .unwrap();
    let mut conn = db.pool.acquire().await.unwrap();
    sqlx::query("update shared_steps set archived_at = null where id = $1")
        .bind(archived.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let b2 = store
        .update_case(
            &c,
            only_archived.number,
            only_archived.revision,
            &replace_body(&only_archived, vec![reference(&archived)]),
            true,
        )
        .await
        .unwrap()
        .1;
    let b3 = store
        .update_case(
            &c,
            only_archived.number,
            b2.revision,
            &replace_body(&b2, vec![step("", "y")]),
            true,
        )
        .await
        .unwrap()
        .1;
    store
        .set_shared_step_archived(&c, archived.number, true)
        .await
        .unwrap();
    let r = store
        .restore_case_version(&c, only_archived.number, 2, b3.revision)
        .await;
    assert!(
        matches!(
            r,
            Err(TmError::Conflict {
                code: "shared_step_archived",
                ..
            })
        ),
        "restoring a version is a new reference to an archived shared step: {r:?}"
    );
    let closed = TmLimits {
        gate_mode: TmGateMode::Closed,
        ..Default::default()
    };
    store
        .set_shared_step_archived(&c, archived.number, false)
        .await
        .unwrap();
    let r = store
        .restore_case_version(
            &ctx(&p, "ann", &closed),
            only_archived.number,
            2,
            b3.revision,
        )
        .await;
    assert!(
        matches!(
            r,
            Err(TmError::Conflict {
                code: "upgrade_in_progress",
                ..
            })
        ),
        "restoring the first reference is a gated write: {r:?}"
    );
    store
        .restore_case_version(&c, only_archived.number, 2, b3.revision)
        .await
        .unwrap();
    assert_eq!(
        usage_cases(&db, &archived).await,
        vec![only_archived.number]
    );

    let gone = CaseBody {
        steps: vec![CaseStep {
            shared: Some(SharedStepRef {
                id: purged.id,
                number: purged.number,
                version: None,
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let r = fastproof_store::tm::shared_steps::sync_usages(&mut conn, p.id, case.id, &gone).await;
    assert!(
        matches!(r, Err(TmError::NotFound("shared step"))),
        "a case created while its shared step was purged rolls back: {r:?}"
    );
}

async fn big_shared(store: &Store, p: &Project, l: &TmLimits) -> SharedStepRow {
    let text = "x".repeat(15_000);
    store
        .create_shared_step(
            &ctx(p, "ann", l),
            &write("Big", (0..4).map(|_| step("", &text)).collect()),
            SharedStepOrigin::Ui,
        )
        .await
        .unwrap()
}

/// Security review: `maxSharedRefs` references to one large shared step cost a few bytes in
/// the case but are charged to one expansion budget per run item, and a manual report page
/// ends early instead of holding hundreds of expanded snapshots.
#[tokio::test]
async fn expansion_is_charged_to_a_byte_budget() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "amp").await;
    let l = open();
    let ss = big_shared(&store, &p, &l).await;
    let size = serde_json::to_vec(&ss.steps).unwrap().len();
    let refs = l.max_shared_refs;
    assert!(size * refs > expand::MAX_EXPANDED_SHARED_BYTES);
    let launch = open_launch(&db, &p).await;
    let mut items = Vec::new();
    for name in ["A", "B"] {
        let case = new_case(
            &store,
            &p,
            &l,
            name,
            (0..refs).map(|_| reference(&ss)).collect(),
        )
        .await
        .unwrap();
        items.push(start_item(&store, &db, &p, &l, launch, &case).await);
    }
    let run = store.run_item(p.id, items[0]).await.unwrap();
    assert_eq!(run.steps.len(), refs);
    let expanded = run.steps.iter().filter(|s| !s.steps.is_empty()).count();
    assert_eq!(expanded, expand::MAX_EXPANDED_SHARED_BYTES / size);
    assert!(
        run.steps[expanded..]
            .iter()
            .all(|s| s.steps.is_empty() && s.action.contains("is not shown"))
    );
    let total: usize = run
        .steps
        .iter()
        .map(|s| serde_json::to_vec(&s.steps).unwrap().len())
        .sum();
    assert!(total <= expand::MAX_EXPANDED_SHARED_BYTES + refs * 2);
    let path = format!("{}.1", expanded + 1);
    let r = store
        .put_run_step(
            &p,
            items[0],
            &path,
            &RunStepWrite {
                status: StepStatus::Passed,
                actual: None,
                comment: None,
                attachment_ids: None,
            },
            "ann",
            &l,
        )
        .await;
    assert!(
        matches!(r, Err(TmError::NotFound("step"))),
        "a step that is not shown takes no result: {r:?}"
    );

    let (page, more) = store
        .manual_report_page(p.id, launch, 0, 500, 1)
        .await
        .unwrap();
    assert_eq!(
        (page.len(), more),
        (1, true),
        "the page ends after one large item"
    );
    let (rest, more) = store
        .manual_report_page(p.id, launch, page[0].case_number, 500, 1)
        .await
        .unwrap();
    assert_eq!(
        (rest.len(), more),
        (1, false),
        "the last item ends the report"
    );
    let (all, more) = store
        .manual_report_page(p.id, launch, 0, 500, usize::MAX)
        .await
        .unwrap();
    assert_eq!((all.len(), more), (2, false));
}

/// Security review: shared step versions follow the project's case version retention; the
/// head, the approved version and versions runs pinned are kept.
#[tokio::test]
async fn shared_step_versions_follow_retention() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ret").await;
    let keep_all = project(&store, "kept").await;
    let l = open();
    let c = ctx(&p, "ann", &l);
    let ss = new_shared(&store, &p, &l, "Log in").await;
    let other = new_shared(&store, &keep_all, &l, "Log in").await;
    let mut row = ss.clone();
    let mut other_row = other.clone();
    for v in 2..=5 {
        if v == 3 {
            let case = new_case(&store, &p, &l, "A", vec![reference(&row)])
                .await
                .unwrap();
            let launch = open_launch(&db, &p).await;
            start_item(&store, &db, &p, &l, launch, &case).await;
        }
        row = store
            .update_shared_step(
                &c,
                row.number,
                row.revision,
                &write("Log in", vec![step("", &format!("version {v}"))]),
            )
            .await
            .unwrap()
            .1;
        other_row = store
            .update_shared_step(
                &ctx(&keep_all, "ann", &l),
                other_row.number,
                other_row.revision,
                &write("Log in", vec![step("", &format!("version {v}"))]),
            )
            .await
            .unwrap()
            .1;
    }
    sqlx::query(
        "update shared_steps set approved_version = 4, review_state = 'pending' where id = $1",
    )
    .bind(ss.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update shared_step_versions set created_at = created_at - interval '2 hours'")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut s = ProjectSettings::default();
    s.retention.case_versions_keep = 1;
    store.put_project_settings(p.id, &s).await.unwrap();
    store
        .prune_case_versions(std::time::Duration::from_secs(60), 1, 0)
        .await
        .unwrap();
    let versions = |id: Uuid| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i32>(
                "select version from shared_step_versions where shared_step_id = $1 order by version desc",
            )
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        versions(ss.id).await,
        vec![5, 4, 2],
        "the head, the approved version and the version a run pinned"
    );
    assert_eq!(versions(other.id).await.len(), 5, "no retention set");
}
