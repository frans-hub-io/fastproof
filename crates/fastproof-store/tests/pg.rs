//! Store tests on a real PostgreSQL. They need `FASTPROOF_TEST_DATABASE_URL` (a role that may
//! create databases); without it they pass without doing anything. Each test creates its own
//! `fastproof_test_*` database and drops it afterwards, also when the test fails.

use std::time::Duration;

use fastproof_model::{
    AttachmentId, AttachmentRef, Container, IdentityInput, ParsedResult, SourceFormat, Status,
    Step, TestIdentity,
};
use fastproof_store::ingest::{LaunchRef, NewAttempt};
use fastproof_store::launches::{LaunchFilter, LaunchStart};
use fastproof_store::projects::CreateProject;
use fastproof_store::results::{GroupBy, ResultFilter};
use fastproof_store::{HistoryFilter, HistoryStore, PgHistoryStore, Store};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

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
        Some(Self { admin, name, pool })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

fn result(full_name: &str, status: Status, start: i64, uuid: &str) -> ParsedResult {
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        full_name,
        display_name: full_name.rsplit('#').next().unwrap(),
        ..Default::default()
    });
    ParsedResult {
        source_uuid: Some(uuid.to_string()),
        name: identity.display_name.clone(),
        full_name: full_name.to_string(),
        identity,
        status,
        start: Some(start),
        stop: Some(start + 10),
        steps: vec![Step {
            name: "step one".into(),
            status,
            ..Default::default()
        }],
        format: SourceFormat::AllureResults,
        ..Default::default()
    }
}

#[tokio::test]
async fn migrations_run_twice_concurrently() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (a, b) = tokio::join!(
        fastproof_store::migrate(&db.pool),
        fastproof_store::migrate(&db.pool)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        a.len() + b.len(),
        fastproof_store::migrator().iter().count()
    );
    assert!(fastproof_store::migrate(&db.pool).await.unwrap().is_empty());
    fastproof_store::check_schema(&db.pool).await.unwrap();
    let store = Store::new(db.pool.clone());
    let parts = store.partitions("results").await.unwrap();
    assert!(parts.contains(&"results_default".to_string()));
    assert!(parts.len() >= 5, "{parts:?}");
    assert_eq!(store.ensure_partitions(1, 3).await.unwrap(), 0);
    assert!(store.ensure_partitions(1, 4).await.unwrap() == 2);
    assert_eq!(
        fastproof_store::search::ensure_trigram(&db.pool).await,
        fastproof_store::search::Trigram::Enabled
    );
}

#[tokio::test]
async fn results_attempts_retries_and_fixtures() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    let store = Store::new(db.pool.clone());
    let CreateProject::Created(project) =
        store.create_project("team-a", "Team A", "").await.unwrap()
    else {
        panic!("exists")
    };
    assert!(matches!(
        store.create_project("team-a", "x", "").await.unwrap(),
        CreateProject::Exists
    ));
    let start = LaunchStart {
        external_id: Some("ci-1".into()),
        name: "nightly".into(),
        ..Default::default()
    };
    let (launch, created) = store
        .start_launch(&project, &start, "tester")
        .await
        .unwrap();
    assert!(created);
    assert_eq!(launch.number, 1);
    let (again, created) = store
        .start_launch(&project, &start, "tester")
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(again.id, launch.id);
    let lref = LaunchRef {
        id: launch.id,
        project_id: project.id,
        project_key: project.key.clone(),
        created_at: launch.created_at,
    };

    let mut r1 = result("a.B#ok", Status::Passed, 1_000, "u1");
    r1.attachments.push(AttachmentRef {
        id: Some(AttachmentId(uuid::Uuid::now_v7().to_string())),
        ..AttachmentRef::new("shot", "shot.png", Some("image/png".into()))
    });
    let r2 = result("a.B#flaky", Status::Failed, 1_000, "u2");
    let o = store
        .persist_attempts(
            &lref,
            None,
            vec![
                NewAttempt {
                    client_key: "k1".into(),
                    result: r1,
                },
                NewAttempt {
                    client_key: "k2".into(),
                    result: r2,
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((o.accepted, o.duplicates, o.results), (2, 0, 2));

    store
        .upsert_containers(
            &lref,
            &[Container {
                uuid: "c1".into(),
                children: vec!["u3".into(), "u2".into()],
                befores: vec![Step {
                    name: "setUp".into(),
                    status: Status::Passed,
                    start: Some(1),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        )
        .await
        .unwrap();

    let r3 = result("a.B#flaky", Status::Passed, 2_000, "u3");
    let dup = result("a.B#ok", Status::Passed, 1_000, "u1");
    let o = store
        .persist_attempts(
            &lref,
            None,
            vec![
                NewAttempt {
                    client_key: "k3".into(),
                    result: r3,
                },
                NewAttempt {
                    client_key: "k1".into(),
                    result: dup,
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!((o.accepted, o.duplicates), (1, 1));

    let early = result("a.B#flaky", Status::Broken, 500, "u4");
    store
        .persist_attempts(
            &lref,
            None,
            vec![NewAttempt {
                client_key: "k4".into(),
                result: early,
            }],
        )
        .await
        .unwrap();

    let l = store.launch(project.id, launch.id).await.unwrap().unwrap();
    assert_eq!(l.counts.total, 2);
    assert_eq!(l.counts.passed, 2);
    assert_eq!(l.counts.flaky, 1);

    let page = store
        .results(
            &l,
            &ResultFilter {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let next = store
        .results(
            &l,
            &ResultFilter {
                limit: 1,
                before: page.next.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(next.items.len(), 1);
    assert_ne!(next.items[0].id, page.items[0].id);
    assert!(next.next.is_none());
    let flaky = store
        .results(
            &l,
            &ResultFilter {
                limit: 50,
                flaky: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(flaky.items.len(), 1);
    let fr = &flaky.items[0];
    assert_eq!(fr.status, "passed");
    assert_eq!(fr.attempt_count, 3);
    let hist = PgHistoryStore::new(db.pool.clone());
    let d = store
        .result_detail(project.id, fr.id, &hist)
        .await
        .unwrap()
        .unwrap();
    let statuses: Vec<&str> = d.attempts.iter().map(|a| a.status.as_str()).collect();
    assert_eq!(statuses, vec!["broken", "failed", "passed"]);
    let names: Vec<&str> = d.attempts[1]
        .steps
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["setUp", "step one"],
        "fixture attached from another call"
    );
    assert_eq!(d.attempts[1].steps[0].kind, "fixture");
    assert_eq!(d.history.len(), 1);

    let tree = store.tree(&l, GroupBy::Package).await.unwrap();
    assert_eq!(tree.counts.total, 2);
    assert_eq!(tree.children[0].name, "a");

    let h = hist
        .test_case_history(
            project.id,
            fr.test_case_id,
            &HistoryFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(h.len(), 1);
    let trend = hist
        .launch_trend(
            project.id,
            &HistoryFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(trend[0].total, 2);

    let events = store.outbox_after(0, 1000).await.unwrap();
    assert!(events.iter().filter(|e| e.kind == "result.updated").count() >= 4);
    let list = store
        .launches(
            project.id,
            &LaunchFilter {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(list.items.len(), 1);

    assert!(
        store
            .delete_launch(project.id, &project.key, launch.id)
            .await
            .unwrap()
    );
    assert!(store.launch(project.id, launch.id).await.unwrap().is_none());
}

#[tokio::test]
async fn scheduler_claims_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    let store = Store::new(db.pool.clone());
    store
        .register_scheduled_job("t.job", "1m", chrono::Utc::now())
        .await
        .unwrap();
    let names = vec!["t.job".to_string()];
    let lease = Duration::from_secs(30);
    let (a, b) = tokio::join!(
        store.claim_scheduled_jobs(&names, "a", lease),
        store.claim_scheduled_jobs(&names, "b", lease)
    );
    assert_eq!(a.unwrap().len() + b.unwrap().len(), 1);
    assert!(
        store
            .claim_scheduled_jobs(&names, "c", lease)
            .await
            .unwrap()
            .is_empty()
    );
    let jobs = store.scheduled_jobs().await.unwrap();
    let owner = jobs[0].lease_owner.clone().unwrap();
    store
        .finish_scheduled_job(
            "t.job",
            &owner,
            None,
            Duration::from_millis(5),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .claim_scheduled_jobs(&names, "c", lease)
            .await
            .unwrap()
            .len(),
        1
    );
}
