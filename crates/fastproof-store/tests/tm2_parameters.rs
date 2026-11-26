//! Parameters on a real PostgreSQL: validation and the gate on case writes, iterations of run
//! items (one per enabled row of the pinned version), iteration step writes and status
//! changes, the derived item status, the projection of each iteration onto its own series
//! (the Allure parameters hash of the row), `allIterations`, rerun, closing the run, the
//! `maxRunIterations` limit and the launch counters. They need `FASTPROOF_TEST_DATABASE_URL`;
//! without it they pass without doing anything.

use std::collections::BTreeMap;

use fastproof_model::Parameter;
use fastproof_model::identity::parameters_hash;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::cases::CaseCtx;
use fastproof_store::tm::{
    CaseBody, CaseParameters, CaseRow, CaseStep, CaseWrite, ManualUntestedTo, ParameterRow,
    RunItemPatch, RunItemsAdd, RunIterationPatch, RunStatus, RunStepWrite, StepStatus, TmError,
    TmGateMode, TmLimits, UntestedStepsTo,
};
use fastproof_store::{Project, Store};
use serde_json::Value;
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

fn limits(gate: TmGateMode) -> TmLimits {
    TmLimits {
        gate_mode: gate,
        ..Default::default()
    }
}

fn ctx<'a>(p: &'a Project, l: &'a TmLimits) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: "ann",
        limits: l,
        can_review: false,
    }
}

fn step(action: &str, expected: &str, children: Vec<CaseStep>) -> CaseStep {
    CaseStep {
        action: action.into(),
        expected: expected.into(),
        steps: children,
        ..Default::default()
    }
}

fn row(id: &str, browser: &str, currency: &str, enabled: bool) -> ParameterRow {
    ParameterRow {
        id: id.into(),
        values: [
            ("browser".to_string(), browser.to_string()),
            ("currency".to_string(), currency.to_string()),
        ]
        .into(),
        enabled,
    }
}

fn params(rows: Vec<ParameterRow>) -> CaseParameters {
    CaseParameters {
        columns: vec!["browser".into(), "currency".into()],
        rows,
        generator: None,
    }
}

fn checkout_body(rows: Vec<ParameterRow>) -> CaseBody {
    CaseBody {
        preconditions: "Open the shop in ${browser}".into(),
        steps: vec![
            step("Add a book", "The cart has one book", vec![]),
            step(
                "Pay in ${currency}",
                "",
                vec![
                    step("Choose ${currency}", "", vec![]),
                    step("Confirm", "Paid in ${currency}", vec![]),
                ],
            ),
        ],
        parameters: Some(params(rows)),
        ..Default::default()
    }
}

async fn new_case(
    store: &Store,
    p: &Project,
    l: &TmLimits,
    name: &str,
    body: CaseBody,
) -> Result<CaseRow, TmError> {
    store
        .create_case(
            &ctx(p, l),
            &CaseWrite {
                name: name.into(),
                body: Some(body),
                ..Default::default()
            },
        )
        .await
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

async fn item_of(db: &TestDb, launch: Uuid, case: &CaseRow) -> Uuid {
    sqlx::query_scalar("select id from run_items where launch_id = $1 and case_id = $2")
        .bind(launch)
        .bind(case.id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn code(e: &TmError) -> &'static str {
    match e {
        TmError::Invalid { code, .. } | TmError::Conflict { code, .. } => code,
        TmError::NotFound(_) => "not_found",
        TmError::Stale { .. } => "stale",
        _ => "other",
    }
}

/// The launch counters as stored, then as a full recount gives them: they must agree.
async fn counters_agree(db: &TestDb, launch: Uuid) {
    let q = "select count_total, count_passed, count_failed, count_broken, count_skipped, count_unknown, \
               count_untested, count_in_progress from launches where id = $1";
    let before: (i32, i32, i32, i32, i32, i32, i32, i32) = sqlx::query_as(q)
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let mut conn = db.pool.acquire().await.unwrap();
    Store::recount_launch(&mut conn, launch).await.unwrap();
    let after: (i32, i32, i32, i32, i32, i32, i32, i32) = sqlx::query_as(q)
        .bind(launch)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(before, after, "incremental counters match a recount");
}

#[tokio::test]
async fn validation_and_the_gate() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pv").await;
    let closed = limits(TmGateMode::Closed);
    let open = limits(TmGateMode::Open);
    let e = new_case(
        &store,
        &p,
        &closed,
        "Checkout",
        checkout_body(vec![row("r_1", "firefox", "EUR", true)]),
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "upgrade_in_progress", "{e:?}");
    let plain = new_case(
        &store,
        &p,
        &closed,
        "Plain ${x}",
        CaseBody {
            steps: vec![step("echo ${HOME}", "", vec![])],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        plain.body.parameters.is_none(),
        "${{…}} without parameters is plain text"
    );

    let c = new_case(
        &store,
        &p,
        &open,
        "Checkout",
        checkout_body(vec![row("r_1", "firefox", "EUR", true)]),
    )
    .await
    .unwrap();
    assert_eq!(c.body.parameters.as_ref().unwrap().rows.len(), 1);
    let (_, after) = store
        .update_case(
            &ctx(&p, &closed),
            c.number,
            c.revision,
            &CaseWrite {
                name: "Checkout".into(),
                body: Some(checkout_body(vec![
                    row("r_1", "firefox", "EUR", true),
                    row("r_2", "chromium", "USD", true),
                ])),
                ..Default::default()
            },
            true,
        )
        .await
        .expect("a case that has parameters stays writable while the gate is closed");
    assert_eq!(after.version, 2);

    let mut bad = checkout_body(vec![row("r_1", "firefox", "EUR", true)]);
    bad.steps[1].steps[0].expected = "Shows ${price}".into();
    let e = new_case(&store, &p, &open, "Bad", bad).await.unwrap_err();
    match &e {
        TmError::Invalid {
            code: "invalid_case",
            details: Some(Value::Array(d)),
            ..
        } => {
            assert_eq!(d[0]["path"], "body.steps[1].steps[0].expected", "{d:?}");
            assert!(d[0]["message"].as_str().unwrap().contains("${price}"));
        }
        e => panic!("{e:?}"),
    }
    let many: Vec<ParameterRow> = (0..3)
        .map(|i| row(&format!("r_{i}"), &i.to_string(), "EUR", true))
        .collect();
    let small = TmLimits {
        max_parameter_rows: 2,
        ..open
    };
    let e = new_case(&store, &p, &small, "Many", checkout_body(many))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "limit_reached");
    drop(db);
}

#[tokio::test]
async fn iterations_steps_status_and_projection() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pr").await;
    let l = limits(TmGateMode::Open);
    let c = new_case(
        &store,
        &p,
        &l,
        "Checkout",
        checkout_body(vec![
            row("r_ff", "firefox", "EUR", true),
            row("r_off", "safari", "GBP", false),
            row("r_cr", "chromium", "USD", true),
        ]),
    )
    .await
    .unwrap();
    let plain = new_case(
        &store,
        &p,
        &l,
        "Plain",
        CaseBody {
            steps: vec![step("Do it", "", vec![])],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let launch = open_launch(&db, &p).await;
    let added = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![c.number, plain.number]),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    assert_eq!(added.added, 2);
    let item = item_of(&db, launch, &c).await;
    let plain_item = item_of(&db, launch, &plain).await;

    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.summary.iteration_count, 2);
    assert_eq!(view.parameter_columns, ["browser", "currency"]);
    let rows: Vec<&str> = view.iterations.iter().map(|i| i.row_id.as_str()).collect();
    assert_eq!(rows, ["r_ff", "r_cr"], "enabled rows in row order");
    assert_eq!(view.iterations[1].values["currency"], "USD");
    assert_eq!(view.iterations[1].position, 2);
    assert!(
        view.steps[1].action.contains("${currency}"),
        "the item keeps the placeholders"
    );
    let plain_view = store.run_item(p.id, plain_item).await.unwrap();
    assert_eq!(plain_view.summary.iteration_count, 0);
    assert!(plain_view.iterations.is_empty() && plain_view.parameter_columns.is_empty());

    let e = store
        .put_run_step(
            &p,
            item,
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
        .unwrap_err();
    assert_eq!(code(&e), "parameterized_item");
    let w = |status| RunStepWrite {
        status,
        actual: Some("ok".into()),
        comment: None,
        attachment_ids: None,
    };
    let ack = store
        .put_iteration_step(&p, item, "r_ff", "1", &w(StepStatus::Passed), "tess", &l)
        .await
        .unwrap();
    assert_eq!(
        (ack.iteration_status, ack.item_status, ack.suggested_status),
        (
            RunStatus::InProgress,
            RunStatus::InProgress,
            RunStatus::InProgress
        )
    );
    for path in ["2.1", "2.2"] {
        store
            .put_iteration_step(&p, item, "r_ff", path, &w(StepStatus::Passed), "tess", &l)
            .await
            .unwrap();
    }
    let ack = store
        .put_iteration_step(&p, item, "r_ff", "2", &w(StepStatus::Passed), "tess", &l)
        .await
        .unwrap();
    assert_eq!(ack.suggested_status, RunStatus::Passed);
    for (row, path) in [("r_ff", "9"), ("r_nope", "1"), ("r_off", "1"), ("x", "1")] {
        let e = store
            .put_iteration_step(&p, item, row, path, &w(StepStatus::Passed), "tess", &l)
            .await
            .unwrap_err();
        assert_eq!(code(&e), "not_found", "{row} {path}");
    }
    let e = store
        .put_iteration_step(
            &p,
            plain_item,
            "r_ff",
            "1",
            &w(StepStatus::Passed),
            "tess",
            &l,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found", "a plain item has no iterations");

    let rev = store.run_item(p.id, item).await.unwrap().revision;
    let e = store
        .patch_run_iteration(
            &p,
            item,
            "r_ff",
            rev - 1,
            &RunIterationPatch {
                status: Some(RunStatus::Passed),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "stale");
    let ch = store
        .patch_run_iteration(
            &p,
            item,
            "r_ff",
            rev,
            &RunIterationPatch {
                status: Some(RunStatus::Passed),
                comment: Some("fine".into()),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    assert_eq!(
        (ch.old_status, ch.new_status, ch.item_status),
        (
            RunStatus::InProgress,
            RunStatus::Passed,
            RunStatus::InProgress
        )
    );
    counters_agree(&db, launch).await;

    let results: Vec<(Uuid, String, Uuid, String, Value, Option<Uuid>)> = sqlx::query_as(
        "select r.id, r.status, r.series_id, s.parameters_hash, s.parameters, r.run_item_id \
         from results r join series s on s.id = r.series_id where r.launch_id = $1",
    )
    .bind(launch)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(results.len(), 1, "only the final iteration is projected");
    let (rid, status, series, ph, sp, run_item) = &results[0];
    assert_eq!(status, "passed");
    assert_eq!(*run_item, Some(item));
    let from_code = parameters_hash(&[
        Parameter::new("currency", "EUR"),
        Parameter::new("browser", "firefox"),
    ]);
    assert_eq!(
        *ph, from_code,
        "the same hash as @Parameter browser=firefox currency=EUR"
    );
    assert_eq!(sp[0]["name"], "browser");
    let it_series: Uuid = sqlx::query_scalar(
        "select series_id from run_iterations where run_item_id = $1 and row_id = 'r_ff'",
    )
    .bind(item)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(*series, it_series);
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.iterations[0].result_id, Some(*rid));
    assert_eq!(view.iterations[0].comment, "fine");
    assert_eq!(view.iterations[0].step_results.len(), 4);
    assert_eq!(view.result_id, None, "the item itself is not projected");
    let detail: Value = sqlx::query_scalar(
        "select detail from attempts where result_id = $1 order by seq desc limit 1",
    )
    .bind(rid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(detail["iteration"], "r_ff");
    assert_eq!(detail["parameters"][1]["value"], "EUR");
    let z: Vec<u8> = sqlx::query_scalar(
        "select steps_z from attempts where result_id = $1 order by seq desc limit 1",
    )
    .bind(rid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let steps: Vec<fastproof_model::Step> = fastproof_store::codec::decode(&z).unwrap();
    let names = serde_json::to_string(&steps).unwrap();
    assert!(
        names.contains("Pay in EUR") && !names.contains("${currency}"),
        "{names}"
    );

    let rev = view.revision;
    let ch = store
        .patch_run_iteration(
            &p,
            item,
            "r_cr",
            rev,
            &RunIterationPatch {
                status: Some(RunStatus::Failed),
                untested_steps_to: Some(UntestedStepsTo::Skipped),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    assert_eq!(ch.item_status, RunStatus::Failed);
    let skipped: i64 = sqlx::query_scalar(
        "select count(*) from run_iteration_step_results where run_item_id = $1 and row_id = 'r_cr' and status = 'skipped'",
    )
    .bind(item)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(skipped, 3, "the untested leaf steps 1, 2.1 and 2.2");
    let n: i64 = sqlx::query_scalar("select count(*) from results where launch_id = $1")
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
    counters_agree(&db, launch).await;

    let rev = store.run_item(p.id, item).await.unwrap().revision;
    store
        .patch_run_iteration(
            &p,
            item,
            "r_cr",
            rev,
            &RunIterationPatch {
                status: Some(RunStatus::Untested),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("select count(*) from results where launch_id = $1")
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 1, "back to untested removes the iteration's result");
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.summary.status, RunStatus::InProgress);
    counters_agree(&db, launch).await;

    let e = store
        .patch_run_item(
            &p,
            item,
            view.revision,
            &RunItemPatch {
                status: Some(RunStatus::Passed),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "parameterized_item");
    let ch = store
        .patch_run_item(
            &p,
            item,
            view.revision,
            &RunItemPatch {
                status: Some(RunStatus::Blocked),
                all_iterations: Some(true),
                comment: Some("env down".into()),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    assert_eq!(ch.new_status, RunStatus::Blocked);
    let view = store.run_item(p.id, item).await.unwrap();
    assert!(
        view.iterations
            .iter()
            .all(|i| i.status == RunStatus::Blocked)
    );
    assert_eq!(view.comment, "env down");
    let broken: i64 = sqlx::query_scalar(
        "select count(*) from results where launch_id = $1 and status = 'broken'",
    )
    .bind(launch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(broken, 2);
    counters_agree(&db, launch).await;

    let ch = store
        .rerun_run_item(&p, item, view.revision, "tess")
        .await
        .unwrap();
    assert_eq!(ch.new_status, RunStatus::Untested);
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.summary.execution_no, 2);
    assert_eq!(view.iterations.len(), 2);
    assert!(
        view.iterations
            .iter()
            .all(|i| i.status == RunStatus::Untested && i.step_results.is_empty())
    );
    let n: i64 = sqlx::query_scalar("select count(*) from results where launch_id = $1")
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        n, 2,
        "earlier results stay until the new execution is final"
    );
    store
        .put_iteration_step(&p, item, "r_cr", "1", &w(StepStatus::Passed), "tess", &l)
        .await
        .unwrap();
    counters_agree(&db, launch).await;

    let out = store
        .close_manual_run(&p, launch, ManualUntestedTo::Skipped)
        .await
        .unwrap();
    assert_eq!(out.skipped, 2);
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.summary.status, RunStatus::Skipped);
    assert!(
        view.iterations
            .iter()
            .all(|i| i.status == RunStatus::Skipped)
    );
    let statuses: Vec<String> = sqlx::query_scalar(
        "select r.status from results r where r.launch_id = $1 and r.run_item_id = $2 order by r.status",
    )
    .bind(launch)
    .bind(item)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(statuses, ["skipped", "skipped"]);
    counters_agree(&db, launch).await;
}

#[tokio::test]
async fn iteration_limit_and_pinned_version() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pl").await;
    let l = limits(TmGateMode::Open);
    let rows = |n: usize| -> Vec<ParameterRow> {
        (0..n)
            .map(|i| row(&format!("r_{i}"), &format!("b{i}"), "EUR", true))
            .collect()
    };
    let a = new_case(&store, &p, &l, "A", checkout_body(rows(3)))
        .await
        .unwrap();
    let b = new_case(&store, &p, &l, "B", checkout_body(rows(4)))
        .await
        .unwrap();
    let small = TmLimits {
        max_run_iterations: 6,
        ..l
    };
    let launch = open_launch(&db, &p).await;
    let e = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![a.number, b.number]),
                ..Default::default()
            },
            &small,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "limit_reached");
    let n: i64 = sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
        .bind(launch)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "nothing was added");
    let added = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![a.number]),
                ..Default::default()
            },
            &small,
        )
        .await
        .unwrap();
    assert_eq!(added.added, 1);
    let e = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![b.number]),
                ..Default::default()
            },
            &small,
        )
        .await
        .unwrap_err();
    assert_eq!(
        code(&e),
        "limit_reached",
        "3 + 4 iterations over a limit of 6 per launch"
    );

    // The item runs the version it was pinned to: an edit after the start changes nothing.
    let item = item_of(&db, launch, &a).await;
    let mut changed = checkout_body(rows(1));
    if let Some(ps) = changed.parameters.as_mut() {
        ps.rows[0].values.insert("browser".into(), "edge".into());
    }
    store
        .update_case(
            &ctx(&p, &l),
            a.number,
            a.revision,
            &CaseWrite {
                name: "A".into(),
                body: Some(changed),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.iterations.len(), 3);
    let values: Vec<BTreeMap<String, String>> =
        view.iterations.iter().map(|i| i.values.clone()).collect();
    assert_eq!(values[2]["browser"], "b2");
}

/// Security review fixes: projection in bounded rounds, the launch budget checked before the
/// tables are read, rows that spell the same Allure parameters, the gate on version restore,
/// closing a run with a failed item that still has open iterations, refresh of an item with
/// iteration results.
#[tokio::test]
async fn review_fixes() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ps").await;
    let l = limits(TmGateMode::Open);
    let pass = RunStepWrite {
        status: StepStatus::Passed,
        actual: None,
        comment: None,
        attachment_ids: None,
    };

    // More iterations than one projection round: every one gets its own result.
    let many = fastproof_store::tm::projection::ITERATION_CHUNK * 2 + 5;
    let wide = new_case(
        &store,
        &p,
        &l,
        "Wide",
        checkout_body(
            (0..many)
                .map(|i| row(&format!("r_{i}"), &format!("b{i}"), "EUR", true))
                .collect(),
        ),
    )
    .await
    .unwrap();
    // A failed iteration next to an open one, for the close below.
    let half = new_case(
        &store,
        &p,
        &l,
        "Half",
        checkout_body(vec![
            row("r_a", "firefox", "EUR", true),
            row("r_b", "chromium", "EUR", true),
        ]),
    )
    .await
    .unwrap();
    let launch = open_launch(&db, &p).await;
    store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![wide.number, half.number]),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    let item = item_of(&db, launch, &wide).await;
    let view = store.run_item(p.id, item).await.unwrap();
    assert_eq!(view.iterations.len(), many);
    store
        .patch_run_item(
            &p,
            item,
            view.revision,
            &RunItemPatch {
                status: Some(RunStatus::Passed),
                all_iterations: Some(true),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    let (results, series): (i64, i64) = sqlx::query_as(
        "select count(*), count(distinct series_id) from results where launch_id = $1 and run_item_id = $2 \
         and status = 'passed'",
    )
    .bind(launch)
    .bind(item)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((results, series), (many as i64, many as i64));
    let linked: i64 = sqlx::query_scalar(
        "select count(*) from run_iterations where run_item_id = $1 and result_id is not null",
    )
    .bind(item)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(linked, many as i64);
    counters_agree(&db, launch).await;

    // Refresh refuses an item whose iterations have step results, even when every iteration
    // went back to untested.
    let h = item_of(&db, launch, &half).await;
    store
        .put_iteration_step(&p, h, "r_b", "1", &pass, "tess", &l)
        .await
        .unwrap();
    let rev = store.run_item(p.id, h).await.unwrap().revision;
    store
        .patch_run_iteration(
            &p,
            h,
            "r_b",
            rev,
            &RunIterationPatch {
                status: Some(RunStatus::Untested),
                comment: None,
                untested_steps_to: None,
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    assert_eq!(
        store.run_item(p.id, h).await.unwrap().summary.status,
        RunStatus::Untested
    );
    let e = store.refresh_run_item(p.id, &p.key, h).await.unwrap_err();
    assert_eq!(code(&e), "run_item_started");

    // Closing with `skipped` also skips the open iteration of an item that is failed already.
    let rev = store.run_item(p.id, h).await.unwrap().revision;
    store
        .patch_run_iteration(
            &p,
            h,
            "r_a",
            rev,
            &RunIterationPatch {
                status: Some(RunStatus::Failed),
                comment: None,
                untested_steps_to: None,
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    store
        .close_manual_run(&p, launch, ManualUntestedTo::Skipped)
        .await
        .unwrap();
    let v = store.run_item(p.id, h).await.unwrap();
    assert_eq!(v.summary.status, RunStatus::Failed);
    let st: Vec<(String, RunStatus, bool)> = v
        .iterations
        .iter()
        .map(|i| (i.row_id.clone(), i.status, i.result_id.is_some()))
        .collect();
    assert_eq!(
        st,
        [
            ("r_a".to_string(), RunStatus::Failed, true),
            ("r_b".to_string(), RunStatus::Skipped, true)
        ]
    );
    counters_agree(&db, launch).await;

    // Rows that spell the same Allure parameters: refused on save; a stored body that has
    // them anyway (written before the check) runs the first of them only.
    let mut spelled = checkout_body(vec![
        row("r_1", "a,currency:b", "", true),
        row("r_2", "a", "b,currency:", true),
    ]);
    let e = new_case(&store, &p, &l, "Spelled", spelled.clone())
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_case", "{e:?}");
    if let Some(ps) = spelled.parameters.as_mut() {
        ps.rows[1].values.insert("browser".into(), "z".into());
    }
    let c = new_case(&store, &p, &l, "Spelled", spelled).await.unwrap();
    sqlx::query(
        "update cases set body = jsonb_set(body, '{parameters,rows,1,values,browser}', '\"a\"') where id = $1",
    )
    .bind(c.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let launch2 = open_launch(&db, &p).await;
    store
        .add_run_items(
            &p,
            launch2,
            &RunItemsAdd {
                case_numbers: Some(vec![c.number]),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    let it = item_of(&db, launch2, &c).await;
    let v = store.run_item(p.id, it).await.unwrap();
    assert_eq!(v.summary.iteration_count, 1);
    assert_eq!(v.iterations[0].row_id, "r_1");
    store
        .patch_run_item(
            &p,
            it,
            v.revision,
            &RunItemPatch {
                status: Some(RunStatus::Passed),
                all_iterations: Some(true),
                ..Default::default()
            },
            "tess",
            &l,
        )
        .await
        .unwrap();
    store
        .close_manual_run(&p, launch2, ManualUntestedTo::Skipped)
        .await
        .unwrap();

    // Restoring a version with parameters onto a case without them is gated.
    let closed = limits(TmGateMode::Closed);
    let (_, plain) = store
        .update_case(
            &ctx(&p, &l),
            half.number,
            half.revision,
            &CaseWrite {
                name: "Half".into(),
                body: Some(CaseBody {
                    steps: vec![step("Do it", "", vec![])],
                    ..Default::default()
                }),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert!(plain.body.parameters.is_none());
    let e = store
        .restore_case_version(&ctx(&p, &closed), half.number, half.version, plain.revision)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "upgrade_in_progress", "{e:?}");
    store
        .restore_case_version(&ctx(&p, &l), half.number, half.version, plain.revision)
        .await
        .unwrap();

    // The launch budget is checked before the tables are read: cases whose parameters add up
    // to more than `maxIterationBytes` are refused, and nothing is added.
    let big = "select string_agg(md5(g::text), '') from generate_series(1, 33000) g";
    let mut numbers = Vec::new();
    let n_big = fastproof_store::tm::iterations::MAX_ITERATION_BYTES / (1024 * 1024) + 2;
    for i in 0..n_big {
        let c = new_case(
            &store,
            &p,
            &l,
            &format!("Big {i}"),
            checkout_body(vec![row("r_1", "firefox", "EUR", true)]),
        )
        .await
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update cases set body = jsonb_set(body, '{{parameters,rows,0,values,browser}}', to_jsonb(({big}))) \
             where id = $1"
        )))
        .bind(c.id)
        .execute(&db.pool)
        .await
        .unwrap();
        numbers.push(c.number);
    }
    let launch3 = open_launch(&db, &p).await;
    let e = store
        .add_run_items(
            &p,
            launch3,
            &RunItemsAdd {
                case_numbers: Some(numbers),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap_err();
    match &e {
        TmError::Conflict {
            code: "limit_reached",
            details: Some(d),
            ..
        } => assert_eq!(d["limit"], "maxIterationBytes", "{d}"),
        e => panic!("{e:?}"),
    }
    let n: i64 = sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
        .bind(launch3)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() as f64) * 0.95).ceil() as usize - 1]
}

/// The targets of §14 for parameters (run with `--release -- --ignored`): adding 5000 cases, 2000
/// of them with 8 rows (16 000 iterations), to a launch; reading an item with 20 iterations;
/// writing an iteration step. Prints the numbers.
#[tokio::test]
#[ignore = "performance measurement; run with --release -- --ignored"]
async fn perf_targets() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pp").await;
    let l = limits(TmGateMode::Open);
    let body = |rows: usize| -> Value {
        let mut b = checkout_body(
            (0..rows)
                .map(|i| row(&format!("r_{i}"), &format!("browser{i}"), "EUR", true))
                .collect(),
        );
        b.steps
            .push(step("Check the total in ${currency}", "", vec![]));
        serde_json::to_value(b).unwrap()
    };
    let plain = serde_json::to_value(CaseBody {
        steps: vec![step("Open", "", vec![]), step("Check", "Fine", vec![])],
        ..Default::default()
    })
    .unwrap();
    sqlx::query(
        "insert into cases (id, project_id, number, name, origin, body) \
         select gen_random_uuid(), $1, g, 'Case ' || g, 'ui', \
           case when g <= 2000 then $2::jsonb when g = 5001 then $4::jsonb else $3::jsonb end \
         from generate_series(1, 5001) g",
    )
    .bind(p.id)
    .bind(body(8))
    .bind(&plain)
    .bind(body(20))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update projects set case_seq = 5001 where id = $1")
        .bind(p.id)
        .execute(&db.pool)
        .await
        .unwrap();
    let launch = open_launch(&db, &p).await;
    let t = std::time::Instant::now();
    let added = store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some((1..=5000).collect()),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    let launch_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(added.added, 5000);
    let its: i64 = sqlx::query_scalar(
        "select count(*) from run_iterations it join run_items ri on ri.id = it.run_item_id where ri.launch_id = $1",
    )
    .bind(launch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(its, 16_000);
    store
        .add_run_items(
            &p,
            launch,
            &RunItemsAdd {
                case_numbers: Some(vec![5001]),
                ..Default::default()
            },
            &l,
        )
        .await
        .unwrap();
    let item: Uuid = sqlx::query_scalar(
        "select ri.id from run_items ri join cases c on c.id = ri.case_id where ri.launch_id = $1 and c.number = 5001",
    )
    .bind(launch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let mut get = Vec::new();
    for _ in 0..40 {
        let t = std::time::Instant::now();
        let v = store.run_item(p.id, item).await.unwrap();
        get.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(v.iterations.len(), 20);
    }
    let mut put = Vec::new();
    for i in 0..60 {
        let row = format!("r_{}", i % 20);
        let path = ["1", "2.1", "2.2", "3"][i % 4];
        let t = std::time::Instant::now();
        store
            .put_iteration_step(
                &p,
                item,
                &row,
                path,
                &RunStepWrite {
                    status: StepStatus::Passed,
                    actual: Some("ok".into()),
                    comment: None,
                    attachment_ids: None,
                },
                "tess",
                &l,
            )
            .await
            .unwrap();
        put.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let mut finish = Vec::new();
    for i in 0..20 {
        let rev = store.run_item(p.id, item).await.unwrap().revision;
        let t = std::time::Instant::now();
        store
            .patch_run_iteration(
                &p,
                item,
                &format!("r_{i}"),
                rev,
                &RunIterationPatch {
                    status: Some(RunStatus::Passed),
                    ..Default::default()
                },
                "tess",
                &l,
            )
            .await
            .unwrap();
        finish.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!(
        "perf: launch 5000 cases / 16000 iterations {launch_ms:.0} ms (target < 3000); \
         GET run item with 20 iterations p95 {:.1} ms (target < 80); \
         iteration step write p95 {:.1} ms (target < 50); iteration finish p95 {:.1} ms",
        p95(get),
        p95(put),
        p95(finish)
    );
    counters_agree(&db, launch).await;
}
