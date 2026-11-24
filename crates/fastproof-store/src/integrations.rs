//! Integrations: encrypted project secrets, quality gate results, the outbox consumer that
//! queues gate evaluations for finished launches, and outbound deliveries (notifications, VCS
//! comments and statuses) with retries.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::launches::{LAUNCH_SELECT, launch_row};
use crate::{Launch, Page, Store, json_no_nul, no_nul};

/// An encrypted secret as stored, with what it may be used for: `purpose` (`vcs`, `fastmock`,
/// `slack`, `telegram`, `webhook`, `smtp`; `None` for secrets stored before purposes existed,
/// which cannot be used) and the destination `origin` it may be sent to.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSecret {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub purpose: Option<String>,
    pub origin: Option<String>,
    /// What else the secret is bound to (the `ci` purpose: provider, repository and workflow).
    pub scope: Option<String>,
}

impl std::fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedSecret")
            .field("key_id", &self.key_id)
            .field("purpose", &self.purpose)
            .field("origin", &self.origin)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// A secret as listed: the name only, never the value.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretMeta {
    pub name: String,
    /// What the secret may be used for: `vcs`, `fastmock`, `slack`, `telegram`, `webhook` or
    /// `smtp` (absent for a secret stored before purposes existed; it must be stored again).
    pub purpose: Option<String>,
    /// The only origin (`https://host[:port]`) the secret may be sent to, when bound to one.
    pub origin: Option<String>,
    /// For `ci` secrets: the provider, repository and workflow the token may be used for.
    pub scope: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<String>,
}

/// Result counts of a launch for gates (muted results are not counted as failures).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateCounts {
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub flaky: i64,
}

/// A delivery to queue.
#[derive(Clone, Debug)]
pub struct NewDelivery {
    pub channel: String,
    pub target_id: String,
    pub events: Vec<String>,
    pub payload: serde_json::Value,
    pub max_attempts: i32,
}

/// A claimed delivery.
#[derive(Clone, Debug)]
pub struct DeliveryJob {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Option<Uuid>,
    pub channel: String,
    pub target_id: String,
    pub events: Vec<String>,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// A delivery as the API shows it (no target, no payload).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryView {
    pub id: Uuid,
    /// The notification id, or `vcs`.
    pub notification_id: String,
    /// `webhook`, `slack`, `teams`, `email`, `telegram`, `gitlab` or `github`.
    pub kind: String,
    pub launch_id: Option<Uuid>,
    pub events: Vec<String>,
    /// `pending`, `sending`, `delivered` or `failed`.
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: Option<String>,
    pub response_status: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Timing and trace id of one attempt of a result.
#[derive(Clone, Debug)]
pub struct AttemptWindow {
    pub no: i32,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub trace_id: Option<String>,
    pub labels: serde_json::Value,
    pub launch_attributes: serde_json::Value,
}

/// The newest finished launch of a project on one branch and environment.
#[derive(Clone, Debug)]
pub struct QualityRow {
    pub branch: String,
    pub environment: String,
    pub passed: i64,
    pub total: i64,
    pub skipped: i64,
}

type SecretMetaRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<String>,
);
type SealedRow = (
    Uuid,
    String,
    String,
    Vec<u8>,
    Vec<u8>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type AttemptRow = (
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<String>,
    serde_json::Value,
    serde_json::Value,
);

/// Launches whose gate waits in the consumer's `pending` list (their rows were held), at most.
pub const MAX_PENDING_GATES: usize = 10_000;

const DELIVERY_COLUMNS: &str = "id, target_id, channel, launch_id, events, status, attempts, max_attempts, \
    last_error, response_status, created_at, next_attempt_at, delivered_at";

fn delivery_view(r: &sqlx::postgres::PgRow) -> Result<DeliveryView, sqlx::Error> {
    let status: String = r.try_get("status")?;
    let next: DateTime<Utc> = r.try_get("next_attempt_at")?;
    Ok(DeliveryView {
        id: r.try_get("id")?,
        notification_id: r.try_get("target_id")?,
        kind: r.try_get("channel")?,
        launch_id: r.try_get("launch_id")?,
        events: r.try_get("events")?,
        next_attempt_at: matches!(status.as_str(), "pending" | "sending").then_some(next),
        status,
        attempts: r.try_get("attempts")?,
        max_attempts: r.try_get("max_attempts")?,
        last_error: r.try_get("last_error")?,
        response_status: r.try_get("response_status")?,
        created_at: r.try_get("created_at")?,
        delivered_at: r.try_get("delivered_at")?,
    })
}

impl Store {
    /// Creates or replaces a secret; `true` when it was created.
    pub async fn put_secret(
        &self,
        project_id: Uuid,
        name: &str,
        sealed: &SealedSecret,
        by: &str,
    ) -> anyhow::Result<bool> {
        let created: bool = sqlx::query_scalar(
            "insert into project_secrets (project_id, name, key_id, nonce, ciphertext, updated_by, purpose, origin, scope) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             on conflict (project_id, name) do update set key_id = excluded.key_id, nonce = excluded.nonce, \
             ciphertext = excluded.ciphertext, updated_by = excluded.updated_by, purpose = excluded.purpose, \
             origin = excluded.origin, scope = excluded.scope, updated_at = now() \
             returning (xmax = 0)",
        )
        .bind(project_id)
        .bind(no_nul(name))
        .bind(&sealed.key_id)
        .bind(&sealed.nonce)
        .bind(&sealed.ciphertext)
        .bind(no_nul(by))
        .bind(sealed.purpose.as_deref().map(no_nul))
        .bind(sealed.origin.as_deref().map(no_nul))
        .bind(sealed.scope.as_deref().map(no_nul))
        .fetch_one(&self.pool)
        .await?;
        Ok(created)
    }

    pub async fn sealed_secret(
        &self,
        project_id: Uuid,
        name: &str,
    ) -> anyhow::Result<Option<SealedSecret>> {
        type Row = (
            String,
            Vec<u8>,
            Vec<u8>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let r: Option<Row> = sqlx::query_as(
            "select key_id, nonce, ciphertext, purpose, origin, scope from project_secrets \
             where project_id = $1 and name = $2",
        )
        .bind(project_id)
        .bind(no_nul(name))
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(
            |(key_id, nonce, ciphertext, purpose, origin, scope)| SealedSecret {
                key_id,
                nonce,
                ciphertext,
                purpose,
                origin,
                scope,
            },
        ))
    }

    pub async fn secret_names(&self, project_id: Uuid) -> anyhow::Result<Vec<SecretMeta>> {
        let rows: Vec<SecretMetaRow> = sqlx::query_as(
            "select name, purpose, origin, scope, created_at, updated_at, updated_by from project_secrets \
             where project_id = $1 order by name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(name, purpose, origin, scope, created_at, updated_at, updated_by)| SecretMeta {
                    name,
                    purpose,
                    origin,
                    scope,
                    created_at,
                    updated_at,
                    updated_by,
                },
            )
            .collect())
    }

    pub async fn delete_secret(&self, project_id: Uuid, name: &str) -> anyhow::Result<bool> {
        let r = sqlx::query("delete from project_secrets where project_id = $1 and name = $2")
            .bind(project_id)
            .bind(no_nul(name))
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Secrets sealed with another key than `key_id` (for re-encryption after a key rotation).
    pub async fn secrets_not_under_key(
        &self,
        key_id: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, String, SealedSecret)>> {
        let rows: Vec<SealedRow> = sqlx::query_as(
            "select project_id, name, key_id, nonce, ciphertext, purpose, origin, scope from project_secrets \
             where key_id <> $1 limit $2",
        )
        .bind(key_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(p, n, key_id, nonce, ciphertext, purpose, origin, scope)| {
                    (
                        p,
                        n,
                        SealedSecret {
                            key_id,
                            nonce,
                            ciphertext,
                            purpose,
                            origin,
                            scope,
                        },
                    )
                },
            )
            .collect())
    }

    /// Replaces the ciphertext of a secret if it is still sealed with `old_key_id`.
    pub async fn reseal_secret(
        &self,
        project_id: Uuid,
        name: &str,
        old_key_id: &str,
        sealed: &SealedSecret,
    ) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "update project_secrets set key_id = $4, nonce = $5, ciphertext = $6 \
             where project_id = $1 and name = $2 and key_id = $3",
        )
        .bind(project_id)
        .bind(name)
        .bind(old_key_id)
        .bind(&sealed.key_id)
        .bind(&sealed.nonce)
        .bind(&sealed.ciphertext)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// The settings document of a project (`{}` when unset).
    pub async fn project_settings_json(
        &self,
        project_id: Uuid,
    ) -> anyhow::Result<serde_json::Value> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select settings from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.unwrap_or_else(|| serde_json::json!({})))
    }

    /// Merges top-level keys into the settings document of a project.
    pub async fn merge_project_settings(
        &self,
        project_id: Uuid,
        patch: serde_json::Value,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update projects set settings = settings || $2, updated_at = now() where id = $1",
        )
        .bind(project_id)
        .bind(json_no_nul(patch))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The key of a project by id.
    pub async fn project_key(&self, project_id: Uuid) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("select key from projects where id = $1 and deleted_at is null")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Result counts of a launch; muted results count as neither failed nor broken.
    pub async fn gate_counts(&self, launch: &Launch) -> anyhow::Result<GateCounts> {
        let r: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "select count(*), count(*) filter (where status = 'passed'), \
             count(*) filter (where status = 'failed' and not muted), \
             count(*) filter (where status = 'broken' and not muted), \
             count(*) filter (where status = 'skipped'), count(*) filter (where flaky_in_run) \
             from results where launch_id = $1 and launch_created_at = $2",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(GateCounts {
            total: r.0,
            passed: r.1,
            failed: r.2,
            broken: r.3,
            skipped: r.4,
            flaky: r.5,
        })
    }

    /// The newest finished, non-debug launch on `branch` (and the same environment) created
    /// before `before`, other than `exclude`.
    pub async fn baseline_launch(
        &self,
        project_id: Uuid,
        branch: &str,
        env_key: Option<&str>,
        exclude: Uuid,
        before: DateTime<Utc>,
    ) -> anyhow::Result<Option<Launch>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{LAUNCH_SELECT} where l.project_id = $1 and l.branch = $2 and l.env_key is not distinct from $3 \
             and l.id <> $4 and l.created_at <= $5 and l.status <> 'running' and not l.debug \
             order by l.created_at desc, l.number desc limit 1"
        )))
        .bind(project_id)
        .bind(no_nul(branch))
        .bind(env_key.map(no_nul))
        .bind(exclude)
        .bind(before)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(launch_row).transpose()?)
    }

    /// Failed or broken (not muted) results of `launch` whose series was not failed or broken
    /// in `baseline` (all of them without a baseline): their count and up to `sample` result ids
    /// with their full names.
    pub async fn new_failures(
        &self,
        launch: &Launch,
        baseline: Option<&Launch>,
        sample: i64,
    ) -> anyhow::Result<(i64, Vec<(Uuid, String)>)> {
        let (bid, bcreated) = match baseline {
            Some(b) => (Some(b.id), Some(b.created_at)),
            None => (None, None),
        };
        let rows: Vec<(Uuid, String, i64)> = sqlx::query_as(
            "with nf as (select r.id, r.full_name from results r where r.launch_id = $1 and r.launch_created_at = $2 \
             and r.status in ('failed', 'broken') and not r.muted and not exists (select 1 from results b \
             where b.launch_id = $3 and b.launch_created_at = $4 and b.series_id = r.series_id \
             and b.status in ('failed', 'broken'))) \
             select id, full_name, (select count(*) from nf) from nf order by full_name, id limit $5",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(bid)
        .bind(bcreated)
        .bind(sample.max(1))
        .fetch_all(&self.pool)
        .await?;
        let count = rows.first().map(|r| r.2).unwrap_or(0);
        Ok((
            count,
            rows.into_iter()
                .take(sample.max(0) as usize)
                .map(|r| (r.0, r.1))
                .collect(),
        ))
    }

    /// Reads `launch.finished` outbox events after the consumer's cursor (older than
    /// `settle`, so that transactions that started earlier have committed) and queues a gate
    /// evaluation for each launch. Only one replica consumes at a time; returns how many
    /// launches were queued, or `None` when another replica holds the cursor.
    ///
    /// A gate row references its launch, so queueing it share-locks the launch row. The rows
    /// are locked with `skip locked`: a launch whose row another transaction holds exclusively
    /// (a merge or a manual run of its project) is kept in the cursor's `pending` list and
    /// queued at a later round, instead of being waited for while the rows of other projects'
    /// launches stay locked and every other project's gates and deliveries wait.
    pub async fn consume_finished_launches(
        &self,
        consumer: &str,
        settle: Duration,
        batch: i64,
    ) -> anyhow::Result<Option<u64>> {
        self.consume_finished_launches_with(consumer, settle, batch, MAX_PENDING_GATES)
            .await
    }

    /// [`Self::consume_finished_launches`] with a given size of the `pending` list.
    pub async fn consume_finished_launches_with(
        &self,
        consumer: &str,
        settle: Duration,
        batch: i64,
        max_pending: usize,
    ) -> anyhow::Result<Option<u64>> {
        sqlx::query(
            "insert into integration_cursors (name, last_event_id) \
             select $1, coalesce(max(id), 0) from events on conflict (name) do nothing",
        )
        .bind(consumer)
        .execute(&self.pool)
        .await?;
        let mut tx = self.pool.begin().await?;
        let cursor: Option<(i64, Vec<Uuid>)> = sqlx::query_as(
            "select last_event_id, pending from integration_cursors where name = $1 for update skip locked",
        )
        .bind(consumer)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((cursor, pending)) = cursor else {
            tx.rollback().await?;
            return Ok(None);
        };
        let rows: Vec<(i64, String, serde_json::Value)> = sqlx::query_as(
            "select id, type, payload from events where id > $1 \
             and created_at < now() - make_interval(secs => $2) order by id limit $3",
        )
        .bind(cursor)
        .bind(settle.as_secs_f64())
        .bind(batch)
        .fetch_all(&mut *tx)
        .await?;
        let last = rows.last().map(|r| r.0);
        if last.is_none() && pending.is_empty() {
            tx.rollback().await?;
            return Ok(Some(0));
        }
        // The first event of each launch, so a launch that cannot wait in `pending` is read
        // again from its event.
        let mut event_of: HashMap<Uuid, i64> = HashMap::new();
        for (event, _, payload) in rows.iter().filter(|r| r.1 == "launch.finished") {
            if let Some(id) = payload
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
            {
                event_of.entry(id).or_insert(*event);
            }
        }
        let was_pending: HashSet<Uuid> = pending.iter().copied().collect();
        let mut ids: Vec<Uuid> = event_of.keys().copied().chain(pending).collect();
        ids.sort();
        ids.dedup();
        let mut queued = 0;
        let mut held: Vec<Uuid> = Vec::new();
        if !ids.is_empty() {
            let free: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "select id, project_id from launches where id = any($1) order by id for key share skip locked",
            )
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await?;
            let (launches, projects): (Vec<Uuid>, Vec<Uuid>) = free.iter().copied().unzip();
            queued = sqlx::query(
                "insert into launch_gates (launch_id, project_id) \
                 select l, p from unnest($1::uuid[], $2::uuid[]) as t(l, p) \
                 on conflict (launch_id) do nothing",
            )
            .bind(&launches)
            .bind(&projects)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let free_set: HashSet<Uuid> = launches.iter().copied().collect();
            let skipped: Vec<Uuid> = ids
                .iter()
                .filter(|id| !free_set.contains(id))
                .copied()
                .collect();
            if !skipped.is_empty() {
                // Still there (not deleted meanwhile): held by another transaction.
                held = sqlx::query_scalar("select id from launches where id = any($1)")
                    .bind(&skipped)
                    .fetch_all(&mut *tx)
                    .await?;
            }
        }
        // Launches that waited already keep their place; those of this round's events fill
        // what is left in event order, and when one does not fit, the cursor stays before its
        // event, so the outbox keeps it (nothing is dropped).
        let mut last = last;
        if held.len() > max_pending {
            let (mut keep, mut fresh): (Vec<Uuid>, Vec<Uuid>) =
                held.iter().partition(|id| was_pending.contains(id));
            fresh.sort_by_key(|id| event_of.get(id).copied().unwrap_or(i64::MAX));
            keep.truncate(max_pending);
            let room = max_pending - keep.len();
            if let Some(e) = fresh.get(room).and_then(|id| event_of.get(id)) {
                last = Some(e - 1);
            }
            keep.extend(fresh.into_iter().take(room));
            metrics::counter!("fastproof_gates_backlogged_total").increment(1);
            tracing::warn!(
                held = held.len(),
                "more launches held than gates can wait for; the rest stay in the outbox"
            );
            held = keep;
        }
        if !held.is_empty() {
            metrics::counter!("fastproof_gates_deferred_total").increment(held.len() as u64);
            tracing::debug!(
                launches = held.len(),
                "launches held by another transaction; their gates are queued later"
            );
        }
        sqlx::query(
            "update integration_cursors set last_event_id = $2, pending = $3, updated_at = now() where name = $1",
        )
        .bind(consumer)
        .bind(last.unwrap_or(cursor))
        .bind(&held)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(queued))
    }

    /// Launches waiting for their gate evaluation, oldest first, with whether the gate was
    /// evaluated before (a re-evaluation, e.g. after a manual run closed on a finished launch).
    pub async fn waiting_gates(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, Uuid, DateTime<Utc>, bool)>> {
        Ok(sqlx::query_as(
            "select project_id, launch_id, queued_at, evaluated_at is not null from launch_gates \
             where state = 'waiting' order by queued_at limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Stores the gate result of a waiting launch and queues its deliveries in one
    /// transaction; `false` when another replica did it first.
    pub async fn complete_gate(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        status: &str,
        report: serde_json::Value,
        deliveries: &[NewDelivery],
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query(
            "update launch_gates set state = 'done', status = $2, report = $3, evaluated_at = now() \
             where launch_id = $1 and state = 'waiting'",
        )
        .bind(launch_id)
        .bind(status)
        .bind(json_no_nul(report))
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        for d in deliveries {
            sqlx::query(
                "insert into integration_deliveries (id, project_id, launch_id, channel, target_id, events, payload, max_attempts) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(Uuid::now_v7())
            .bind(project_id)
            .bind(launch_id)
            .bind(&d.channel)
            .bind(no_nul(&d.target_id))
            .bind(&d.events)
            .bind(json_no_nul(d.payload.clone()))
            .bind(d.max_attempts.max(1))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Claims due deliveries (and ones whose sender died) for `lease`.
    pub async fn claim_deliveries(
        &self,
        limit: i64,
        lease: Duration,
    ) -> anyhow::Result<Vec<DeliveryJob>> {
        let rows = sqlx::query(
            "update integration_deliveries d set status = 'sending', attempts = attempts + 1, \
             lease_until = now() + make_interval(secs => $2), updated_at = now() \
             from projects p where p.id = d.project_id and d.id in (select id from integration_deliveries \
             where (status = 'pending' and next_attempt_at <= now()) or (status = 'sending' and lease_until < now()) \
             order by next_attempt_at limit $1 for update skip locked) \
             returning d.id, d.project_id, p.key, d.launch_id, d.channel, d.target_id, d.events, d.payload, \
             d.attempts, d.max_attempts",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(DeliveryJob {
                    id: r.try_get("id")?,
                    project_id: r.try_get("project_id")?,
                    project_key: r.try_get("key")?,
                    launch_id: r.try_get("launch_id")?,
                    channel: r.try_get("channel")?,
                    target_id: r.try_get("target_id")?,
                    events: r.try_get("events")?,
                    payload: r.try_get("payload")?,
                    attempts: r.try_get("attempts")?,
                    max_attempts: r.try_get("max_attempts")?,
                })
            })
            .collect()
    }

    /// Records the outcome of a send: delivered, retried at `retry_at`, or failed for good.
    pub async fn finish_delivery(
        &self,
        id: Uuid,
        delivered: bool,
        error: Option<&str>,
        response_status: Option<i32>,
        retry_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        let status = if delivered {
            "delivered"
        } else if retry_at.is_some() {
            "pending"
        } else {
            "failed"
        };
        sqlx::query(
            "update integration_deliveries set status = $2, last_error = $3, response_status = $4, \
             next_attempt_at = coalesce($5, next_attempt_at), lease_until = null, updated_at = now(), \
             delivered_at = case when $2 = 'delivered' then now() else delivered_at end where id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(error.map(|e| no_nul(&e.chars().take(1000).collect::<String>())))
        .bind(response_status)
        .bind(retry_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Deliveries of a project, newest first.
    pub async fn deliveries(
        &self,
        project_id: Uuid,
        status: Option<&str>,
        launch_id: Option<Uuid>,
        before: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Page<DeliveryView>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {DELIVERY_COLUMNS} from integration_deliveries where project_id = $1 \
             and ($2::text is null or status = $2) and ($3::uuid is null or launch_id = $3) \
             and ($4::uuid is null or id < $4) order by id desc limit $5"
        )))
        .bind(project_id)
        .bind(status)
        .bind(launch_id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items: Vec<DeliveryView> =
            rows.iter().map(delivery_view).collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items.last().map(|d| d.id.to_string())
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Deletes finished deliveries older than `days`; returns how many.
    pub async fn prune_deliveries(&self, days: i64) -> anyhow::Result<u64> {
        let r = sqlx::query(
            "delete from integration_deliveries where id in (select id from integration_deliveries \
             where status in ('delivered', 'failed') and created_at < now() - make_interval(days => $1::int) limit 10000)",
        )
        .bind(days)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// One attempt of a result (1-based `no`, the last one when `None`).
    pub async fn attempt_window(
        &self,
        project_id: Uuid,
        result_id: Uuid,
        no: Option<i32>,
    ) -> anyhow::Result<Option<AttemptWindow>> {
        let rows: Vec<AttemptRow> =
            sqlx::query_as(
                "select a.started_at, a.stopped_at, a.trace_id, r.labels, l.attributes from results r \
                 join launches l on l.id = r.launch_id \
                 join attempts a on a.result_id = r.id and a.launch_created_at = r.launch_created_at \
                 where r.project_id = $1 and r.id = $2 order by a.seq limit 1000",
            )
            .bind(project_id)
            .bind(result_id)
            .fetch_all(&self.pool)
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let idx = match no {
            None => rows.len() - 1,
            Some(n) if n >= 1 && (n as usize) <= rows.len() => n as usize - 1,
            Some(_) => return Ok(None),
        };
        let (started_at, stopped_at, trace_id, labels, launch_attributes) = rows[idx].clone();
        Ok(Some(AttemptWindow {
            no: idx as i32 + 1,
            started_at,
            stopped_at,
            trace_id,
            labels,
            launch_attributes,
        }))
    }

    /// The newest finished, non-debug launch per branch and environment of a project in the
    /// last `days`, with the number of launches per branch and environment.
    pub async fn quality_rows(
        &self,
        project_id: Uuid,
        days: i64,
    ) -> anyhow::Result<Vec<(QualityRow, i64)>> {
        let rows: Vec<(String, String, i32, i32, i32, i64)> = sqlx::query_as(
            "select distinct on (b, e) b, e, count_passed, count_total, count_skipped, n from \
             (select coalesce(branch, '') as b, coalesce(env_key, '') as e, count_passed, count_total, count_skipped, \
             created_at, count(*) over (partition by coalesce(branch, ''), coalesce(env_key, '')) as n \
             from launches where project_id = $1 and status = 'finished' and not debug \
             and created_at > now() - make_interval(days => $2::int)) x order by b, e, created_at desc",
        )
        .bind(project_id)
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(branch, environment, passed, total, skipped, n)| {
                (
                    QualityRow {
                        branch,
                        environment,
                        passed: passed as i64,
                        total: total as i64,
                        skipped: skipped as i64,
                    },
                    n,
                )
            })
            .collect())
    }

    /// Distinct series flaky within a run in the last `launches` finished launches.
    pub async fn recent_flaky_series(
        &self,
        project_id: Uuid,
        launches: i64,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select count(distinct r.series_id) from (select id, created_at from launches \
             where project_id = $1 and status <> 'running' and not debug order by number desc limit $2) l \
             join results r on r.launch_id = l.id and r.launch_created_at = l.created_at where r.flaky_in_run",
        )
        .bind(project_id)
        .bind(launches)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The stored gate result of a launch: `(state, status, report)`.
    pub async fn stored_gate(
        &self,
        launch_id: Uuid,
    ) -> anyhow::Result<Option<(String, Option<String>, Option<serde_json::Value>)>> {
        Ok(
            sqlx::query_as("select state, status, report from launch_gates where launch_id = $1")
                .bind(launch_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}
