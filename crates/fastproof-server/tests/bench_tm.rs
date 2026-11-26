//! `fastproof bench tm` with tiny numbers against a test server: every phase runs without a
//! failed request, the counts add up and the projects are deleted at the end.

mod common;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_server::cli::bench_tm::{TmBenchArgs, run};
use reqwest::StatusCode;

#[tokio::test]
async fn bench_tm_runs_every_phase() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start_with(
        &db,
        &[
            ("FASTPROOF_TM_GATE", "open"),
            ("FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE", "100000"),
        ],
    )
    .await;
    let tok = s.dir.path().join("token");
    std::fs::write(&tok, ADMIN_TOKEN).unwrap();
    let a = TmBenchArgs {
        url: s.base.clone(),
        token_file: Some(tok),
        project: Some("tmb".into()),
        keep: false,
        seed: 7,
        cases: 60,
        folders: 12,
        folder_depth: 3,
        fields: 7,
        steps: 4,
        versions: 3,
        version_sample: 5,
        save_steps: 30,
        save_sample: 3,
        automated: 10,
        plan_cases: 30,
        run_cases: 12,
        run_steps: 5,
        testers: 3,
        think_ms: 1,
        upload_results: 20,
        import_cases: 25,
        samples: 5,
        parallel: 4,
        read_parallel: 2,
        wait_timeout: 120,
        shared_steps: 12,
        shared_hot_cases: 8,
        parameterized: 6,
        parameter_rows: 4,
        requirements: 30,
        requirement_links: 60,
        fpql_queries: 8,
        n2_plan_cases: 20,
        review_votes: 10,
        no_export: false,
    };
    let r = run(a).await.unwrap();
    eprintln!("{}", r.render());
    assert_eq!(r.errors, 0, "{:?}", r.first_errors);
    let count = |op: &str| r.ops.iter().find(|o| o.op == op).map_or(0, |o| o.count);
    assert_eq!(count("field.create"), 7);
    assert_eq!(count("folder.create"), 12);
    assert_eq!(count("case.create"), 60);
    assert_eq!(count("case.update"), 5 * 2);
    assert_eq!(count("case.save.big"), 3);
    assert_eq!(count("cases.list"), 5);
    assert_eq!(count("cases.tree"), 5);
    assert_eq!(count("case.get"), 5);
    assert_eq!(count("plan.launch"), 1);
    assert_eq!(count("run.item.get"), 12);
    assert_eq!(count("run.step"), 12 * 5);
    assert_eq!(count("run.final"), 12);
    assert_eq!(count("export.ndjson"), 1);
    assert_eq!(count("import.total"), 1);
    assert_eq!(r.cases_created, 60);
    assert_eq!(
        r.automated_cases, 20,
        "10 linked by ALLURE_ID, 10 created from code"
    );
    assert_eq!(r.plan_cases, 30);
    assert_eq!((r.run_items, r.step_results), (12, 60));
    let m = &r.manual;
    assert_eq!(m["state"], "closed", "{m}");
    let done = m["passed"].as_i64().unwrap()
        + m["failed"].as_i64().unwrap()
        + m["blocked"].as_i64().unwrap();
    assert_eq!(done, 12, "{m}");
    assert_eq!(m["untested"], 0, "{m}");
    assert!(
        r.launch_results >= 12 + 18,
        "manual and automated results in the plan launch: {}",
        r.launch_results
    );
    assert_eq!(
        r.exported_cases,
        70 + 12,
        "cases and the 12 shared step records of the v2 export"
    );
    assert_eq!(r.shared_steps, 12);
    assert_eq!(r.parameterized_cases, 6);
    assert_eq!((r.requirements_created, r.requirement_links_made), (30, 60));
    assert_eq!(count("shared.create"), 12);
    assert_eq!(count("requirement.create"), 30);
    assert_eq!(count("cases.list.fpql"), 8);
    assert_eq!(count("query.validate"), 8);
    assert_eq!(count("shared.save.hot"), 10);
    assert_eq!(count("plan.launch.n2"), 1);
    assert!(r.n2_run_items >= 7, "{}", r.n2_run_items);
    assert_eq!(r.n2_iterations, 6 * 4 + 20);
    assert_eq!(count("run.item.get.n2"), 5);
    assert_eq!(count("iteration.step"), 20 * 3);
    assert_eq!(count("review.bulk"), 1);
    assert_eq!(r.review_applied, 10);
    assert_eq!(r.imported_cases, 25);
    for key in ["tmb", "tmb-imp"] {
        let (st, _) = s.get(&format!("/api/v1/projects/{key}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{key} is deleted");
    }
}
