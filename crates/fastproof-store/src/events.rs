//! The outbox (`events`, requirement X8) and live-update notifications.
//!
//! Outbox rows are written in the same transaction as the change they describe, so a consumer
//! (an indexer, a later ClickHouse sink) never sees an event for a change that rolled back. Live
//! notifications go through `pg_notify`, which PostgreSQL delivers on commit.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{LIVE_CHANNEL, Store, json_no_nul};

/// A live-update notification: `type` is `launch.updated` or `upload.updated`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveEvent {
    pub project: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: serde_json::Value,
}

/// Writes one outbox row.
pub async fn emit(
    conn: &mut PgConnection,
    kind: &str,
    project: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query("insert into events (type, project, payload) values ($1, $2, $3)")
        .bind(kind)
        .bind(project)
        .bind(json_no_nul(payload))
        .execute(conn)
        .await?;
    Ok(())
}

/// Writes many outbox rows of one type with one statement.
pub async fn emit_many(
    conn: &mut PgConnection,
    kind: &str,
    project: Option<&str>,
    payloads: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    if payloads.is_empty() {
        return Ok(());
    }
    let payloads: Vec<serde_json::Value> = payloads.into_iter().map(json_no_nul).collect();
    sqlx::query(
        "insert into events (type, project, payload) select $1, $2, p from unnest($3::jsonb[]) as p",
    )
    .bind(kind)
    .bind(project)
    .bind(payloads)
    .execute(conn)
    .await?;
    Ok(())
}

/// Sends a live notification (delivered when the transaction commits).
pub async fn notify(conn: &mut PgConnection, ev: &LiveEvent) -> anyhow::Result<()> {
    sqlx::query("select pg_notify($1, $2)")
        .bind(LIVE_CHANNEL)
        .bind(serde_json::to_string(ev)?)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn notify_launch(conn: &mut PgConnection, project: &str, id: Uuid) -> anyhow::Result<()> {
    notify(
        conn,
        &LiveEvent {
            project: project.to_string(),
            kind: "launch.updated".into(),
            data: serde_json::json!({"id": id}),
        },
    )
    .await
}

pub async fn notify_upload(
    conn: &mut PgConnection,
    project: &str,
    launch_id: Uuid,
    id: Uuid,
) -> anyhow::Result<()> {
    notify(
        conn,
        &LiveEvent {
            project: project.to_string(),
            kind: "upload.updated".into(),
            data: serde_json::json!({"launchId": launch_id, "id": id}),
        },
    )
    .await
}

/// An outbox row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboxEvent {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub project: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

type OutboxRow = (
    i64,
    String,
    Option<String>,
    serde_json::Value,
    chrono::DateTime<chrono::Utc>,
);

impl Store {
    /// Outbox rows after `after_id`, oldest first (for consumers and tests).
    pub async fn outbox_after(
        &self,
        after_id: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<OutboxEvent>> {
        let rows: Vec<OutboxRow> =
            sqlx::query_as(
                "select id, type, project, payload, created_at from events where id > $1 order by id limit $2",
            )
            .bind(after_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, kind, project, payload, created_at)| OutboxEvent {
                id,
                kind,
                project,
                payload,
                created_at,
            })
            .collect())
    }

    /// Deletes outbox rows older than `days`, at most `batch` at a time; returns how many.
    pub async fn prune_outbox(&self, days: i64, batch: i64) -> anyhow::Result<u64> {
        let r = sqlx::query(
            "delete from events where id in (select id from events \
             where created_at < now() - make_interval(days => $1::int) order by id limit $2)",
        )
        .bind(days)
        .bind(batch)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }
}
