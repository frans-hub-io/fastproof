//! PostgreSQL schema and repository of FastProof.
//!
//! Migrations are embedded and tracked in their own table, [`MIGRATIONS_TABLE`], apart from the
//! platform's `platform_schema_migrations`. They are additive; indexes on existing tables are
//! built with `CREATE INDEX CONCURRENTLY` in separate no-transaction migrations. Results and
//! attempts are partitioned by month on the creation time of their launch, so every row of a
//! launch lives in one partition and old months can be dropped as a whole.
//!
//! Every query lists its columns explicitly.

pub mod attachments;
pub mod codec;
pub mod events;
pub mod history;
pub mod ingest;
pub mod launches;
pub mod maintenance;
pub mod projects;
pub mod results;
pub mod scheduler;
pub mod search;
pub mod types;
pub mod uploads;

use std::sync::LazyLock;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::migrate::Migrator;

pub use history::{HistoryEntry, HistoryFilter, HistoryStore, PgHistoryStore, SeriesHistoryEntry};
pub use types::*;

/// The table that records which FastProof migrations ran.
pub const MIGRATIONS_TABLE: &str = "fastproof_schema_migrations";

/// Channel of live-update notifications (`pg_notify`), JSON payload [`events::LiveEvent`].
pub const LIVE_CHANNEL: &str = "fastproof_live";
/// Channel that wakes ingest workers when a job is queued.
pub const JOBS_CHANNEL: &str = "fastproof_jobs";

static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| {
    let mut m = sqlx::migrate!("./migrations");
    m.dangerous_set_table_name(MIGRATIONS_TABLE);
    m.set_ignore_missing(true);
    m.set_locking(false);
    m
});

/// Advisory lock key of the FastProof migrations (distinct from the platform's and sqlx's).
const LOCK_KEY: i64 = 0x6661_7374_7072_6f66;

/// The embedded migrations.
pub fn migrator() -> &'static Migrator {
    &MIGRATOR
}

/// Version of the newest embedded migration.
pub fn expected_version() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0)
}

async fn applied_versions(conn: &mut sqlx::PgConnection) -> anyhow::Result<Vec<i64>> {
    let present: bool =
        sqlx::query_scalar("select to_regclass('fastproof_schema_migrations') is not null")
            .fetch_one(&mut *conn)
            .await?;
    if !present {
        return Ok(Vec::new());
    }
    Ok(
        sqlx::query_scalar("select version from fastproof_schema_migrations where success")
            .fetch_all(&mut *conn)
            .await?,
    )
}

/// Applies pending migrations under an advisory lock that is polled, not waited for (so that a
/// replica waiting for the lock never blocks a concurrent index build), and returns the
/// versions this call applied.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<Vec<i64>> {
    let mut conn = pool.acquire().await?;
    while !sqlx::query_scalar::<_, bool>("select pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?
    {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let result = async {
        let before = applied_versions(&mut conn).await?;
        MIGRATOR.run_direct(None, &mut *conn, false).await?;
        let after = applied_versions(&mut conn).await?;
        Ok::<_, anyhow::Error>(
            after
                .into_iter()
                .filter(|v| !before.contains(v))
                .collect::<Vec<_>>(),
        )
    }
    .await;
    let unlocked = sqlx::query("select pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    let applied = result?;
    unlocked?;
    for v in &applied {
        tracing::info!(version = v, "applied FastProof migration");
    }
    Ok(applied)
}

/// Checks that every embedded migration ran unchanged.
pub async fn check_schema(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let applied = applied_versions(&mut conn).await?;
    let missing: Vec<String> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .map(|m| format!("{} ({})", m.version, m.description))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "FastProof migration(s) {} not applied: run `fastproof migrate` first",
            missing.join(", ")
        );
    }
    let rows: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("select version, checksum from fastproof_schema_migrations")
            .fetch_all(&mut *conn)
            .await?;
    for (v, sum) in rows {
        if let Some(m) = MIGRATOR.iter().find(|m| m.version == v)
            && sum.as_slice() != &*m.checksum
        {
            anyhow::bail!(
                "FastProof migration {v} ({}) differs from the one built into this version",
                m.description
            );
        }
    }
    Ok(())
}

/// The repository: one pool, methods grouped by module.
#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// `select 1`.
    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("select 1").execute(&self.pool).await?;
        Ok(())
    }
}

/// Text without NUL characters (PostgreSQL text cannot hold them).
pub(crate) fn no_nul(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "\u{FFFD}")
    } else {
        s.to_string()
    }
}

pub(crate) fn opt_no_nul(s: Option<&str>) -> Option<String> {
    s.map(no_nul)
}

/// A JSON value without NUL characters in strings (jsonb cannot hold `\u0000`).
pub(crate) fn json_no_nul(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) if s.contains('\0') => Value::String(no_nul(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(json_no_nul).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (no_nul(&k), json_no_nul(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Milliseconds since the epoch to a timestamp, `None` outside years 1970..=9999.
pub fn ts_ms(ms: Option<i64>) -> Option<chrono::DateTime<chrono::Utc>> {
    let ms = ms?;
    if !(0..=253_402_300_799_000).contains(&ms) {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(ms)
}
