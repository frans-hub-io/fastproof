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
    let s = Server::start(&db).await;
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
    assert_eq!(r.exported_cases, 70);
    assert_eq!(r.imported_cases, 25);
    for key in ["tmb", "tmb-imp"] {
        let (st, _) = s.get(&format!("/api/v1/projects/{key}"), ADMIN_TOKEN).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{key} is deleted");
    }
}
