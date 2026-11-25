//! Levels of the running replicas (`replica_levels`) and the gate for new data formats (§3).
//!
//! Replicas of 0.2.0-dev2 reject unknown keys in cases, settings and widgets, so new formats
//! are written only when every live replica knows them: no replica seen within
//! [`SEEN_WINDOW_SECONDS`] reports a level below [`TM_LEVEL`], and migration
//! [`GATE_MIGRATION`] was installed at least the settle time ago (binaries before it write no
//! row at all) or in the same run as the first migration (a fresh install).

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::Store;
use crate::tm::ReplicaLevel;

/// The test-management level of this build: 2 knows every DTO of TM next (both nights).
pub const TM_LEVEL: i32 = 2;
/// The migration that brought `replica_levels`.
pub const GATE_MIGRATION: i64 = 911;
/// The analysis level of this build: 1 knows every format of FastProof Analysis (contract
/// `76`, both nights): log rules, signal conditions, new gate metrics, the `errorRegressed`
/// event, widget pattern modes and the new task kinds.
pub const ANALYSIS_LEVEL: i32 = 1;
/// The migration that brought `replica_levels.analysis_level`.
pub const ANALYSIS_GATE_MIGRATION: i64 = 1011;
/// Replicas seen within this many seconds count as running.
pub const SEEN_WINDOW_SECONDS: i64 = 120;
/// Rows not seen for this many seconds are removed.
pub const FORGET_SECONDS: i64 = 86_400;
/// A gate migration installed within this many seconds of the first migration was a fresh
/// install.
pub const FRESH_INSTALL_SECONDS: i64 = 300;

/// What the gate rule found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmGate {
    /// New formats may be written.
    pub open: bool,
    /// Replicas seen within the window that report a lower level.
    pub behind: Vec<String>,
    /// When the settle time after the gate migration ends (`None`: a fresh install, or the
    /// migration is missing).
    pub settle_until: Option<DateTime<Utc>>,
}

/// The gate rule on a connection (no cache; see `tm::gate` for the cached check).
pub async fn tm_gate_on(conn: &mut PgConnection, settle: Duration) -> anyhow::Result<TmGate> {
    level_gate_on(conn, "tm_level", TM_LEVEL, GATE_MIGRATION, settle).await
}

/// The analysis gate rule on a connection (no cache; see `analysis::gate` for the cached
/// check): as [`tm_gate_on`] on `analysis_level` and migration [`ANALYSIS_GATE_MIGRATION`].
pub async fn analysis_gate_on(conn: &mut PgConnection, settle: Duration) -> anyhow::Result<TmGate> {
    level_gate_on(
        conn,
        "analysis_level",
        ANALYSIS_LEVEL,
        ANALYSIS_GATE_MIGRATION,
        settle,
    )
    .await
}

/// The rule of a replica level: no replica seen within [`SEEN_WINDOW_SECONDS`] reports
/// `column < level`, and `migration` was installed at least `settle` ago or with the first
/// migration (a fresh install). Rows written before the column existed read its default (0).
/// `column` is one of the two level columns, never user input.
async fn level_gate_on(
    conn: &mut PgConnection,
    column: &'static str,
    level: i32,
    migration: i64,
    settle: Duration,
) -> anyhow::Result<TmGate> {
    let behind: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select instance from replica_levels where seen_at > now() - make_interval(secs => $1) \
         and {column} < $2 order by instance limit 100"
    )))
    .bind(SEEN_WINDOW_SECONDS as f64)
    .bind(level)
    .fetch_all(&mut *conn)
    .await?;
    let (at, first, now): (Option<DateTime<Utc>>, Option<DateTime<Utc>>, DateTime<Utc>) =
        sqlx::query_as(
            "select (select installed_on from fastproof_schema_migrations where version = $1 and success), \
               (select installed_on from fastproof_schema_migrations where success order by version limit 1), \
               now()",
        )
        .bind(migration)
        .fetch_one(&mut *conn)
        .await?;
    let settle = chrono::Duration::from_std(settle).unwrap_or(chrono::Duration::MAX);
    let (settled, settle_until) = match (at, first) {
        (None, _) => (false, None),
        (Some(at), Some(first))
            if (at - first) < chrono::Duration::seconds(FRESH_INSTALL_SECONDS) =>
        {
            (true, None)
        }
        (Some(at), _) => {
            let until = at.checked_add_signed(settle);
            (until.is_some_and(|u| u <= now), until)
        }
    };
    Ok(TmGate {
        open: settled && behind.is_empty(),
        behind,
        settle_until,
    })
}

impl Store {
    /// Records that this replica runs at [`TM_LEVEL`] and [`ANALYSIS_LEVEL`] (upsert by
    /// instance) and forgets rows not seen for a day.
    pub async fn replica_heartbeat(&self, instance: &str, version: &str) -> anyhow::Result<()> {
        sqlx::query(
            "insert into replica_levels (instance, tm_level, version, analysis_level) values ($1, $2, $3, $4) \
             on conflict (instance) do update set tm_level = excluded.tm_level, version = excluded.version, \
               analysis_level = excluded.analysis_level, \
               seen_at = now(), started_at = case when replica_levels.version = excluded.version \
                 then replica_levels.started_at else now() end",
        )
        .bind(crate::no_nul(instance))
        .bind(TM_LEVEL)
        .bind(crate::no_nul(version))
        .bind(ANALYSIS_LEVEL)
        .execute(&self.pool)
        .await?;
        sqlx::query("delete from replica_levels where seen_at < now() - make_interval(secs => $1)")
            .bind(FORGET_SECONDS as f64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The gate rule (no cache).
    pub async fn tm_gate(&self, settle: Duration) -> anyhow::Result<TmGate> {
        let mut conn = self.pool.acquire().await?;
        tm_gate_on(&mut conn, settle).await
    }

    /// The analysis gate rule (no cache).
    pub async fn analysis_gate(&self, settle: Duration) -> anyhow::Result<TmGate> {
        let mut conn = self.pool.acquire().await?;
        analysis_gate_on(&mut conn, settle).await
    }

    /// Replicas seen within a day, newest first.
    pub async fn replica_levels(&self) -> anyhow::Result<Vec<ReplicaLevel>> {
        type Row = (String, i32, String, DateTime<Utc>, DateTime<Utc>, i32);
        let rows: Vec<Row> = sqlx::query_as(
            "select instance, tm_level, version, started_at, seen_at, analysis_level from replica_levels \
             order by seen_at desc, instance limit 1000",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(instance, tm_level, version, started_at, seen_at, analysis_level)| ReplicaLevel {
                    instance,
                    tm_level,
                    analysis_level,
                    version,
                    started_at,
                    seen_at,
                },
            )
            .collect())
    }
}
