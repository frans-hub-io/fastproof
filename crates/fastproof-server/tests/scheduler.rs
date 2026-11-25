//! The scheduler of a real server: every standard job runs through it, and paused jobs (what
//! tests use to do a job's work themselves) run only when made due.

mod common;

use std::time::Duration;

use common::{Server, TestDb};

type Row = (
    String,
    i64,
    Option<String>,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
);

async fn jobs(db: &TestDb) -> Vec<Row> {
    sqlx::query_as(
        "select name, run_count, last_status, last_error, next_run_at from scheduled_jobs order by name",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

/// Waits until every job outside `paused` finished its first run; returns all rows.
async fn first_runs(db: &TestDb, paused: &[&str]) -> Vec<Row> {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let rows = jobs(db).await;
        if rows
            .iter()
            .all(|r| paused.contains(&r.0.as_str()) || r.2.is_some())
        {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "first runs did not finish: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn every_standard_job_runs_through_the_scheduler() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let _s = Server::start(&db).await;
    let names = fastproof_server::scheduler::standard_jobs()
        .unwrap()
        .names();
    let rows = first_runs(&db, &[]).await;
    assert_eq!(rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>(), {
        let mut n = names.clone();
        n.sort();
        n
    });
    for (name, runs, status, error, _) in &rows {
        assert!(*runs >= 1, "{name}");
        assert_eq!(status.as_deref(), Some("ok"), "{name}: {error:?}");
    }
}

#[tokio::test]
async fn paused_jobs_run_only_when_made_due() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let paused = ["outbox.prune", "jobs.reap"];
    let mut s = Server::start_paused(&db, &paused).await;
    let far = chrono::Utc::now() + chrono::Duration::days(3650);
    let rows = first_runs(&db, &paused).await;
    for (name, runs, _, _, next) in rows.iter().filter(|r| paused.contains(&r.0.as_str())) {
        assert_eq!(*runs, 0, "{name} is paused");
        assert!(*next > far, "{name} is due {next}");
    }

    db.run_scheduled("outbox.prune").await;
    let rows = jobs(&db).await;
    let row = |n: &str| rows.iter().find(|r| r.0 == n).unwrap().clone();
    assert_eq!(row("outbox.prune").1, 1, "ran once when made due");
    assert!(row("outbox.prune").4 > far, "and is paused again");
    assert_eq!(row("jobs.reap").1, 0);

    // A restart that pauses a job keeps it paused although it was due.
    s.stop().await;
    sqlx::query("update scheduled_jobs set next_run_at = now() where name = 'jobs.reap'")
        .execute(&db.pool)
        .await
        .unwrap();
    let _s = Server::start_paused(&db, &["jobs.reap"]).await;
    let (next,): (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("select next_run_at from scheduled_jobs where name = 'jobs.reap'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(next > far, "{next}");

    let mut sched = fastproof_server::scheduler::standard_jobs().unwrap();
    assert!(
        sched.pause(&["no.such.job".to_string()]).is_err(),
        "a misspelt job is refused, not silently left running"
    );
}
