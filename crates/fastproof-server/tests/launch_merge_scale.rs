//! Scale of a launch merge: two launches with `FASTPROOF_TEST_MERGE_SCALE` attempts in all
//! (half of their tests shared, client ids colliding across them), merged into a new launch.
//! Prints the plan of the attempt copy (`EXPLAIN ANALYZE`, rolled back) and the time of the
//! merge. Skipped without `FASTPROOF_TEST_DATABASE_URL` and `FASTPROOF_TEST_MERGE_SCALE`.

mod common;

use std::time::Instant;

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_store::Store;
use fastproof_store::merges::{COPY_ATTEMPTS_SQL, LaunchMergeRequest, MergeCaller, MergeOptions};
use serde_json::json;
use uuid::Uuid;

async fn launch(s: &Server, name: &str) -> (Uuid, chrono::DateTime<chrono::Utc>) {
    let (st, b) = s
        .post(
            "/api/v1/projects/scale/launches",
            ADMIN_TOKEN,
            json!({"name": name, "vcs": {"branch": "main"}}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    let id: Uuid = b["id"].as_str().unwrap().parse().unwrap();
    let (st, _) = s
        .post(
            &format!("/api/v1/projects/scale/launches/{id}/finish"),
            ADMIN_TOKEN,
            json!({}),
        )
        .await;
    assert!(st.is_success());
    (id, chrono::Utc::now())
}

/// Results and one attempt each for tests `from..from+count` of the project in a launch.
async fn seed(db: &TestDb, project: Uuid, launch: Uuid, from: i64, count: i64) {
    let at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select created_at from launches where id = $1")
            .bind(launch)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
           full_name, status) \
         select gen_random_uuid(), $1, $2, $3, s.test_case_id, s.id, 't' || i, 'c.T#t' || i, \
           case when i % 7 = 0 then 'failed' else 'passed' end \
         from generate_series($4::bigint, $4 + $5 - 1) as i join series s on s.project_id = $2 and s.series_key = 'h' || i || '.p'",
    )
    .bind(at)
    .bind(project)
    .bind(launch)
    .bind(from)
    .bind(count)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, client_key, seq, \
           status, started_at, detail, steps_z, message) \
         select gen_random_uuid(), r.launch_created_at, r.project_id, r.launch_id, r.id, r.series_id, \
           'r:' || substr(r.name, 2), row_number() over (order by r.name), r.status, \
           r.launch_created_at + make_interval(secs => substr(r.name, 2)::double precision), '{\"format\": \"fastproof\"}', \
           decode(repeat('ab', 512), 'hex'), case when r.status = 'failed' then 'expected 200 but was 503' end \
         from results r where r.launch_id = $1 and r.launch_created_at = $2",
    )
    .bind(launch)
    .bind(at)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("update launches set attempt_seq = $2, count_total = $2 where id = $1")
        .bind(launch)
        .bind(count)
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_large_merge_is_linear() {
    let Some(n) = std::env::var("FASTPROOF_TEST_MERGE_SCALE")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
    else {
        eprintln!("FASTPROOF_TEST_MERGE_SCALE is not set: skipped");
        return;
    };
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = Server::start(&db).await;
    db.hold_jobs(&["launches.merges"]).await;
    s.project("scale").await;
    let store = Store::new(db.pool.clone());
    let project = store.project("scale").await.unwrap().unwrap();
    let half = n / 2;
    let started = Instant::now();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name) \
         select gen_random_uuid(), $1, 'h' || i, 'c.T#t' || i, 't' || i from generate_series(1, $2::bigint) as i",
    )
    .bind(project.id)
    .bind(half + half / 2)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash) \
         select gen_random_uuid(), $1, t.id, t.test_case_hash || '.p', 'p' from test_cases t where t.project_id = $1",
    )
    .bind(project.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let (a, _) = launch(&s, "shard 1").await;
    let (b, _) = launch(&s, "shard 2").await;
    let (x, _) = launch(&s, "empty").await;
    seed(&db, project.id, a, 1, half).await;
    seed(&db, project.id, b, half / 2 + 1, half).await;
    sqlx::query("analyze").execute(&db.pool).await.unwrap();
    eprintln!(
        "seeded {n} attempts in {:.1}s",
        started.elapsed().as_secs_f64()
    );

    let pool = db.pool.clone();
    let at = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
                "select created_at from launches where id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (ta, tb, tx_at) = (at(a).await, at(b).await, at(x).await);
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, status) \
         select distinct on (r.series_id) gen_random_uuid(), $1, r.project_id, $2, r.test_case_id, r.series_id, r.name, \
           r.full_name, r.status from results r where r.launch_id = any($3) order by r.series_id",
    )
    .bind(tx_at)
    .bind(x)
    .bind(vec![a, b])
    .execute(&mut *tx)
    .await
    .unwrap();
    let ids: Vec<Uuid> = (0..n).map(|_| Uuid::now_v7()).collect();
    let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "explain (analyze, buffers, costs off) {COPY_ATTEMPTS_SQL}"
    )))
    .bind(tx_at)
    .bind(project.id)
    .bind(x)
    .bind(vec![a, b])
    .bind(vec![ta, tb])
    .bind(vec![0i32, 1])
    .bind(&ids)
    .bind(0i64)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    eprintln!(
        "plan of the attempt copy ({n} attempts):\n{}",
        plan.join("\n")
    );
    assert!(
        !plan.iter().any(|l| l.contains("SubPlan")),
        "the copy must not run a subplan per row"
    );

    let started = Instant::now();
    let (m, created) = store
        .merge_launches(
            &project,
            &LaunchMergeRequest {
                launch_ids: vec![a, b],
                ..Default::default()
            },
            &MergeCaller {
                name: "admin".into(),
                key: "static:admin".into(),
                can_sync: true,
                can_triage: true,
            },
            MergeOptions {
                max_attempts: n + 1,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let took = started.elapsed();
    eprintln!(
        "merge request ({n} attempts, first phase and up to 15 s of analysis) took {:.1}s: {:?}",
        took.as_secs_f64(),
        m.counts
    );
    assert!(created);
    let started = Instant::now();
    let rest = store
        .drain_merge(
            project.id,
            m.id.unwrap(),
            std::time::Duration::from_secs(3600),
        )
        .await
        .unwrap();
    eprintln!(
        "the rest of the analysis ({rest} series) took {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let left: i64 = sqlx::query_scalar("select count(*) from launch_merge_work")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert_eq!(m.counts.attempts, n);
    assert_eq!(m.counts.results, half + half / 2);
    assert_eq!(m.counts.shared_series, half / 2);
    assert_eq!(
        m.counts.rekeyed,
        half / 2,
        "client ids of the second shard name other tests where they do not overlap"
    );
}
