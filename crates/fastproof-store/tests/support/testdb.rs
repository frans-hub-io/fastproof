//! Scratch databases of the integration tests on the server named by
//! `FASTPROOF_TEST_DATABASE_URL` (shared by the tests of `fastproof-store` and
//! `fastproof-server`).
//!
//! Every test gets a database `fastproof_test_t<unix seconds>_<random>`. Dropping one is handed
//! to a background thread with its own runtime and admin connection: `DROP DATABASE` waits until
//! every backend of the server has accepted a signal barrier, and a backend still in a password
//! login waits for its client, so the drop must never block the runtime that owns such a login
//! (it would wait for PostgreSQL's `authentication_timeout`, a minute). The test waits a few
//! seconds for the drop and otherwise lets it finish in the background. The first database of a
//! process also removes `fastproof_test_*` databases older than an hour (left behind by killed
//! runs).

#![allow(dead_code)]

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, Connection, PgConnection};

/// Prefix of every test database.
pub const PREFIX: &str = "fastproof_test_";

/// Age after which a test database counts as left behind.
pub const STALE_AFTER: Duration = Duration::from_secs(3600);

/// How long a dropped test waits for its database to be gone before it moves on.
const DROP_WAIT: Duration = Duration::from_secs(3);

/// The admin connection options, or `None` (the test is skipped) without
/// `FASTPROOF_TEST_DATABASE_URL`.
pub fn admin() -> Option<PgConnectOptions> {
    let Ok(url) = std::env::var("FASTPROOF_TEST_DATABASE_URL") else {
        eprintln!("FASTPROOF_TEST_DATABASE_URL is not set: skipped");
        return None;
    };
    Some(url.parse().expect("FASTPROOF_TEST_DATABASE_URL"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A new database name with its creation time.
pub fn new_name() -> String {
    format!("{PREFIX}t{}_{:016x}", unix_now(), rand::random::<u64>())
}

/// The creation time (Unix seconds) a name carries; `None` for names of older harnesses.
pub fn created_at(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(PREFIX)?.strip_prefix('t')?;
    let (secs, random) = rest.split_once('_')?;
    (random.len() == 16 && random.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| secs.parse().ok())
        .flatten()
}

/// Creates a fresh database and returns its name. The first call of a process also queues the
/// removal of stale test databases.
pub async fn create(admin: &PgConnectOptions) -> String {
    static CLEANUP: OnceLock<()> = OnceLock::new();
    CLEANUP.get_or_init(|| {
        let _ = reaper().send(Job::Cleanup(admin.clone()));
    });
    let name = new_name();
    let mut conn = admin
        .connect()
        .await
        .expect("connecting to the test server");
    sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}")))
        .execute(&mut conn)
        .await
        .unwrap();
    let _ = conn.close().await;
    name
}

/// Drops a test database: queued to the background thread, waited for at most a few seconds.
/// Close (or drop) the pools of the database first, so they open no new connections.
pub fn drop_database(admin: &PgConnectOptions, name: &str) {
    let (done, wait) = std::sync::mpsc::channel();
    let job = Job::Drop {
        admin: admin.clone(),
        name: name.to_string(),
        done,
    };
    if reaper().send(job).is_ok() {
        let _ = wait.recv_timeout(DROP_WAIT);
    }
}

enum Job {
    Drop {
        admin: PgConnectOptions,
        name: String,
        done: std::sync::mpsc::Sender<()>,
    },
    Cleanup(PgConnectOptions),
}

fn reaper() -> &'static tokio::sync::mpsc::UnboundedSender<Job> {
    static REAPER: OnceLock<tokio::sync::mpsc::UnboundedSender<Job>> = OnceLock::new();
    REAPER.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("testdb-reaper".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let mut conn: Option<PgConnection> = None;
                    while let Some(job) = rx.recv().await {
                        match job {
                            Job::Drop { admin, name, done } => {
                                drop_with_retries(&mut conn, &admin, &name).await;
                                let _ = done.send(());
                            }
                            Job::Cleanup(admin) => {
                                remove_stale(&admin).await;
                            }
                        }
                    }
                });
            })
            .expect("starting the test database reaper");
        tx
    })
}

async fn execute(
    conn: &mut Option<PgConnection>,
    admin: &PgConnectOptions,
    sql: String,
) -> Result<(), sqlx::Error> {
    if conn.is_none() {
        *conn = Some(admin.connect().await?);
    }
    let c = conn.as_mut().expect("connected");
    let r = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(c).await;
    if r.is_err() {
        *conn = None;
    }
    r.map(|_| ())
}

async fn drop_with_retries(conn: &mut Option<PgConnection>, admin: &PgConnectOptions, name: &str) {
    let sql = format!("drop database if exists {name} with (force)");
    let mut pause = Duration::from_millis(100);
    for attempt in 1..=20 {
        match execute(conn, admin, sql.clone()).await {
            Ok(()) => return,
            Err(e) if attempt == 20 => {
                eprintln!("test database {name} was not dropped: {e}");
            }
            Err(_) => {
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(3));
            }
        }
    }
}

/// Drops `fastproof_test_*` databases older than [`STALE_AFTER`]: by the time in their name,
/// or (names of older harnesses) by the time of their `PG_VERSION` file when the test user may
/// read it. Returns the names it dropped.
pub async fn remove_stale(admin: &PgConnectOptions) -> Vec<String> {
    let Ok(mut c) = admin.connect().await else {
        return Vec::new();
    };
    let pattern = format!("{}%", PREFIX.replace('_', "\\_"));
    let rows: Vec<(String, Option<f64>)> = match sqlx::query_as(
        "select datname::text, extract(epoch from now() - \
           (pg_stat_file('base/' || oid || '/PG_VERSION', true)).modification)::float8 \
         from pg_database where datname like $1",
    )
    .bind(&pattern)
    .fetch_all(&mut c)
    .await
    {
        Ok(rows) => rows,
        Err(_) => sqlx::query_scalar::<_, String>(
            "select datname::text from pg_database where datname like $1",
        )
        .bind(&pattern)
        .fetch_all(&mut c)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|n| (n, None))
        .collect(),
    };
    let now = unix_now();
    let stale: Vec<String> = rows
        .into_iter()
        .filter(|(name, file_age)| match created_at(name) {
            Some(t) => now.saturating_sub(t) > STALE_AFTER.as_secs(),
            None => file_age.is_some_and(|a| a > STALE_AFTER.as_secs_f64()),
        })
        .map(|(name, _)| name)
        .filter(|name| {
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .collect();
    if !stale.is_empty() {
        eprintln!("removing {} stale test databases", stale.len());
    }
    let mut dropped = Vec::new();
    for name in stale {
        let sql = format!("drop database if exists {name} with (force)");
        if sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut c)
            .await
            .is_ok()
        {
            dropped.push(name);
        }
    }
    let _ = c.close().await;
    dropped
}
