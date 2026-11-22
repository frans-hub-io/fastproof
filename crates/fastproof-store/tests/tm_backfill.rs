//! Upgrade from stage 1 with backfill: a database with stage-1 data is migrated, the backfill
//! gives cases to the identities seen on trusted branches (numbered by `first_seen_at`,
//! explicit `ALLURE_ID` as a key) and leaves the others unlinked; a second run changes
//! nothing. Needs `FASTPROOF_TEST_DATABASE_URL`; without it the test passes without doing
//! anything.

use std::time::Duration;

use fastproof_store::Store;
use fastproof_store::tm::TmLimits;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("FASTPROOF_TEST_DATABASE_URL") else {
            eprintln!("FASTPROOF_TEST_DATABASE_URL is not set: skipped");
            return None;
        };
        let admin: PgConnectOptions = url.parse().expect("FASTPROOF_TEST_DATABASE_URL");
        let name = format!("fastproof_test_{:016x}", rand::random::<u64>());
        let mut conn = admin
            .connect()
            .await
            .expect("connecting to the test server");
        sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Ok(mut c) = admin.connect().await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "drop database if exists {name} with (force)"
                    )))
                    .execute(&mut c)
                    .await;
                }
            });
        })
        .join()
        .unwrap();
    }
}

struct Stage1 {
    pool: PgPool,
    project: Uuid,
}

impl Stage1 {
    async fn launch(
        &self,
        number: i64,
        branch: &str,
        debug: bool,
    ) -> (Uuid, chrono::DateTime<chrono::Utc>) {
        let id = Uuid::now_v7();
        let at = sqlx::query_scalar(
            "insert into launches (id, project_id, number, name, branch, debug, status, finished_at) \
             values ($1, $2, $3, 'nightly', $4, $5, 'finished', now()) returning created_at",
        )
        .bind(id)
        .bind(self.project)
        .bind(number)
        .bind(branch)
        .bind(debug)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (id, at)
    }

    async fn identity(
        &self,
        full: &str,
        display: &str,
        days_ago: i32,
        explicit: Option<&str>,
    ) -> (Uuid, Uuid) {
        let tc = Uuid::now_v7();
        sqlx::query(
            "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, explicit_id, \
               explicit_id_source, first_seen_at, last_seen_at) \
             values ($1, $2, md5($3), $3, $4, $5, case when $5 is not null then 'allure_id' end, \
               now() - make_interval(days => $6), now())",
        )
        .bind(tc)
        .bind(self.project)
        .bind(full)
        .bind(display)
        .bind(explicit)
        .bind(days_ago)
        .execute(&self.pool)
        .await
        .unwrap();
        let se = Uuid::now_v7();
        sqlx::query(
            "insert into series (id, project_id, test_case_id, series_key, parameters_hash) \
             values ($1, $2, $3, md5($4 || ':s'), 'p')",
        )
        .bind(se)
        .bind(self.project)
        .bind(tc)
        .bind(full)
        .execute(&self.pool)
        .await
        .unwrap();
        (tc, se)
    }

    async fn result(
        &self,
        launch: (Uuid, chrono::DateTime<chrono::Utc>),
        ident: (Uuid, Uuid),
        name: &str,
        suite: &str,
    ) {
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, suite_path, status, labels) \
             values ($1, $2, $3, $4, $5, $6, $7, $7, array[$8], 'passed', '[{\"name\": \"tag\", \"value\": \"old\"}]')",
        )
        .bind(Uuid::now_v7())
        .bind(launch.1)
        .bind(self.project)
        .bind(launch.0)
        .bind(ident.0)
        .bind(ident.1)
        .bind(name)
        .bind(suite)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn upgrade_then_backfill_creates_cases_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrator()
        .run_to(402, &db.pool)
        .await
        .unwrap();
    let project = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name) values ($1, 'old', 'Old')")
        .bind(project)
        .execute(&db.pool)
        .await
        .unwrap();
    let st = Stage1 {
        pool: db.pool.clone(),
        project,
    };
    let opted_out = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name) values ($1, 'optout', 'Opted out')")
        .bind(opted_out)
        .execute(&db.pool)
        .await
        .unwrap();
    let other = Stage1 {
        pool: db.pool.clone(),
        project: opted_out,
    };
    let other_launch = other.launch(1, "main", false).await;
    let other_ident = other.identity("shop.B#one", "one", 3, None).await;
    other.result(other_launch, other_ident, "one", "shop").await;
    let main = st.launch(1, "main", false).await;
    let feature = st.launch(2, "feature/x", false).await;
    let debug = st.launch(3, "main", true).await;
    let a = st.identity("shop.A#one", "one", 3, Some("77")).await;
    let b = st.identity("shop.A#branch", "branch", 2, None).await;
    let c = st.identity("shop.A#debug", "debug", 1, None).await;
    let d = st.identity("shop.A#rolled", "rolled", 4, None).await;
    let e = st.identity("shop.A#late", "late", 0, None).await;
    st.result(main, a, "one", "shop").await;
    st.result(feature, b, "branch", "shop").await;
    st.result(debug, c, "debug", "shop").await;
    st.result(main, e, "late", "shop").await;
    sqlx::query(
        "insert into series_daily (project_id, series_id, day, branch, runs, passed) \
         values ($1, $2, current_date - 30, 'main', 3, 3), ($1, $3, current_date - 30, 'feature/y', 1, 1)",
    )
    .bind(project)
    .bind(d.1)
    .bind(b.1)
    .execute(&db.pool)
    .await
    .unwrap();

    let applied = fastproof_store::migrate(&db.pool).await.unwrap();
    assert!(
        applied.contains(&500) && applied.contains(&504),
        "{applied:?}"
    );
    let store = Store::new(db.pool.clone());
    assert!(store.tm_upgrade_at().await.unwrap().is_some());
    let jobs = || async {
        sqlx::query_as::<_, (String, String, serde_json::Value)>(
            "select p.key, j.status, j.params from tm_jobs j join projects p on p.id = j.project_id \
             where j.kind = 'backfill' order by p.key",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap()
    };
    let hour = Duration::from_secs(3600);
    assert_eq!(
        store.enqueue_upgrade_backfills(hour).await.unwrap(),
        0,
        "the delay after the upgrade has not passed"
    );
    assert!(jobs().await.is_empty());
    sqlx::query(
        "update projects set settings = jsonb_set(coalesce(settings, '{}'), '{caseSync}', '{\"enabled\": false}') \
         where id = $1",
    )
    .bind(opted_out)
    .execute(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .enqueue_upgrade_backfills(Duration::ZERO)
            .await
            .unwrap(),
        1
    );
    let decided = jobs().await;
    assert_eq!(decided.len(), 2, "{decided:?}");
    assert_eq!(
        (decided[0].0.as_str(), decided[0].1.as_str()),
        ("old", "queued")
    );
    assert_eq!(
        (decided[1].0.as_str(), decided[1].1.as_str()),
        ("optout", "cancelled"),
        "caseSync off when the delay passed: skipped"
    );
    assert_eq!(decided[1].2["skipped"], true);
    sqlx::query("update projects set settings = settings #- '{caseSync}' where id = $1")
        .bind(opted_out)
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .enqueue_upgrade_backfills(Duration::ZERO)
            .await
            .unwrap(),
        0,
        "once per project, also after turning caseSync back on"
    );
    let run = store
        .run_case_backfill("t", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(run.done, "{run:?}");
    assert_eq!(run.stats.created, 3, "{run:?}");

    let cases: Vec<(i64, String, String, bool, Option<Uuid>)> = sqlx::query_as(
        "select number, name, origin, automated, folder_id from cases where project_id = $1 order by number",
    )
    .bind(project)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let names: Vec<(i64, &str)> = cases.iter().map(|c| (c.0, c.1.as_str())).collect();
    assert_eq!(
        names,
        vec![(1, "rolled"), (2, "one"), (3, "late")],
        "numbered by first_seen_at"
    );
    assert!(cases.iter().all(|c| c.2 == "code" && c.3));
    let path = store
        .case_folder_path(project, cases[1].4.unwrap())
        .await
        .unwrap();
    let path: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(path, vec!["Automated", "shop"]);
    let keys: Vec<String> = sqlx::query_scalar(
        "select k.value from case_keys k join cases c on c.id = k.case_id where c.number = 2 and k.kind = 'allure_id' \
         order by k.value",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(keys, vec!["2", "77"]);
    let tags: Vec<String> = sqlx::query_scalar("select tags from cases where number = 2")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(tags, vec!["old"]);
    let unlinked: Vec<String> = sqlx::query_scalar(
        "select full_name from test_cases where project_id = $1 and case_id is null and kind = 'code' order by full_name",
    )
    .bind(project)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(unlinked, vec!["shop.A#branch", "shop.A#debug"]);
    let version_launch: Option<Uuid> = sqlx::query_scalar(
        "select v.launch_id from case_versions v join cases c on c.id = v.case_id where c.number = 2",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(version_launch, Some(main.0));
    let _ = (a, d, e);

    let snapshot = || async {
        let v: (i64, i64, i64, i64) = sqlx::query_as(
            "select (select count(*) from cases), (select count(*) from case_versions), \
             (select coalesce(sum(revision), 0)::bigint from cases), (select count(*) from case_folders)",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        v
    };
    let before = snapshot().await;
    let job = store
        .start_case_backfill(project, Some("admin"))
        .await
        .unwrap();
    let run = store
        .run_case_backfill("t", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(run.job, Some(job.id));
    assert!(run.done);
    assert_eq!(
        run.stats.created + run.stats.linked + run.stats.updated,
        0,
        "{run:?}"
    );
    assert_eq!(
        snapshot().await,
        before,
        "a second backfill changes nothing"
    );
    let none = store
        .run_case_backfill("t", &TmLimits::default(), Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(none.job, None);
    let other_cases: i64 = sqlx::query_scalar("select count(*) from cases where project_id = $1")
        .bind(opted_out)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(other_cases, 0, "the opted-out project got no cases");
}

/// Backfill throughput (run by hand: `cargo test -p fastproof-store --test tm_backfill --
/// --ignored`; `FASTPROOF_BACKFILL_N` identities, default 20000).
#[tokio::test]
#[ignore]
async fn backfill_timing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let n: i64 = std::env::var("FASTPROOF_BACKFILL_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    fastproof_store::migrator()
        .run_to(402, &db.pool)
        .await
        .unwrap();
    let project = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name) values ($1, 'perf', 'Perf')")
        .bind(project)
        .execute(&db.pool)
        .await
        .unwrap();
    let st = Stage1 {
        pool: db.pool.clone(),
        project,
    };
    let (lid, at) = st.launch(1, "main", false).await;
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, first_seen_at, last_seen_at) \
         select gen_random_uuid(), $1, md5('t' || i), 'pkg.Suite' || (i % 50) || '#test' || i, 'test ' || i, \
           now() - make_interval(secs => i), now() from generate_series(1, $2) i",
    )
    .bind(project)
    .bind(n)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash) \
         select gen_random_uuid(), $1, id, test_case_hash, 'p' from test_cases where project_id = $1",
    )
    .bind(project)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, \
           suite_path, status, labels) \
         select gen_random_uuid(), $3, $1, $2, t.id, s.id, t.display_name, t.full_name, \
           array[split_part(t.full_name, '#', 1)], 'passed', \
           '[{\"name\": \"severity\", \"value\": \"normal\"}, {\"name\": \"tag\", \"value\": \"smoke\"}]' \
         from test_cases t join series s on s.test_case_id = t.id where t.project_id = $1",
    )
    .bind(project)
    .bind(lid)
    .bind(at)
    .execute(&db.pool)
    .await
    .unwrap();
    fastproof_store::migrate(&db.pool).await.unwrap();
    let store = Store::new(db.pool.clone());
    store
        .enqueue_upgrade_backfills(Duration::ZERO)
        .await
        .unwrap();
    let t = std::time::Instant::now();
    let mut created = 0;
    loop {
        let run = store
            .run_case_backfill("t", &TmLimits::default(), Duration::from_secs(600))
            .await
            .unwrap();
        created += run.stats.created;
        if run.done || run.job.is_none() {
            break;
        }
    }
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "backfill: {created} cases from {n} identities in {secs:.1} s ({:.0} identities/s)",
        n as f64 / secs
    );
    assert_eq!(created as i64, n);
}
