//! Consumer groups managed in PostgreSQL (rskafka has no group protocol; `32-stack-streaming.md`
//! 1.3). Members heartbeat in `kafka_consumers`; a member is dead after [`MEMBER_TTL`]. Every
//! member computes the same range assignment over the sorted live members, claims its
//! partitions in `kafka_partition_leases` (only free or expired leases), renews them with the
//! heartbeat and releases the ones it no longer owns after their worker stopped. Offsets live
//! in `kafka_offsets`; a commit fails unless the member still holds the lease, so a member that
//! lost a partition can never move its offset.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Heartbeat period.
pub const HEARTBEAT: Duration = Duration::from_secs(3);
/// A member without a heartbeat for this long is dead; leases last as long.
pub const MEMBER_TTL: Duration = Duration::from_secs(10);

/// Raised when a commit finds that the lease is gone.
#[derive(Debug)]
pub struct LeaseLost;

impl std::fmt::Display for LeaseLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the partition lease was lost")
    }
}

impl std::error::Error for LeaseLost {}

/// One partition held by this member.
#[derive(Clone, Debug)]
pub struct PartitionCtx {
    pub pool: PgPool,
    pub group: String,
    pub member: String,
    pub topic: String,
    pub partition: i32,
}

impl PartitionCtx {
    /// The stored next offset, if any.
    pub async fn stored_offset(&self) -> anyhow::Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "select next_offset from kafka_offsets where group_name = $1 and topic = $2 and partition = $3",
        )
        .bind(&self.group)
        .bind(&self.topic)
        .bind(self.partition)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Commits `next_offset` inside the caller's transaction after checking (and renewing) the
    /// lease; fails with [`LeaseLost`] when another member owns the partition now.
    pub async fn commit_in(
        &self,
        conn: &mut PgConnection,
        next_offset: i64,
        high_watermark: Option<i64>,
    ) -> anyhow::Result<()> {
        let held = sqlx::query(
            "update kafka_partition_leases set lease_until = greatest(lease_until, now() + make_interval(secs => $5)) \
             where group_name = $1 and topic = $2 and partition = $3 and owner = $4",
        )
        .bind(&self.group)
        .bind(&self.topic)
        .bind(self.partition)
        .bind(&self.member)
        .bind(MEMBER_TTL.as_secs_f64())
        .execute(&mut *conn)
        .await?;
        if held.rows_affected() != 1 {
            return Err(LeaseLost.into());
        }
        sqlx::query(
            "insert into kafka_offsets (group_name, topic, partition, next_offset, high_watermark, updated_at) \
             values ($1, $2, $3, $4, $5, now()) on conflict (group_name, topic, partition) do update \
             set next_offset = excluded.next_offset, high_watermark = coalesce(excluded.high_watermark, kafka_offsets.high_watermark), \
             updated_at = now()",
        )
        .bind(&self.group)
        .bind(&self.topic)
        .bind(self.partition)
        .bind(next_offset)
        .bind(high_watermark)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Commits `next_offset` in its own transaction.
    pub async fn commit(
        &self,
        next_offset: i64,
        high_watermark: Option<i64>,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        self.commit_in(&mut tx, next_offset, high_watermark).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Exports the lag of this partition.
    pub fn record_lag(&self, high_watermark: i64, next_offset: i64) {
        metrics::gauge!("fastproof_kafka_lag", "group" => self.group.clone(), "topic" => self.topic.clone(),
            "partition" => self.partition.to_string())
        .set((high_watermark - next_offset).max(0) as f64);
    }
}

/// Runs the work of one held partition until `stop`.
#[async_trait]
pub trait PartitionRunner: Send + Sync + 'static {
    async fn run(self: Arc<Self>, ctx: PartitionCtx, stop: CancellationToken)
    -> anyhow::Result<()>;
}

/// The range assignment: member `i` of `m` sorted live members gets partitions
/// `[i*n/m, (i+1)*n/m)` of every topic.
pub fn assign(members: &[String], me: &str, topics: &[(String, i32)]) -> BTreeSet<(String, i32)> {
    let mut out = BTreeSet::new();
    let Some(i) = members.iter().position(|m| m == me) else {
        return out;
    };
    let m = members.len() as i64;
    for (t, n) in topics {
        let n = *n as i64;
        for p in (i as i64 * n / m)..((i as i64 + 1) * n / m) {
            out.insert((t.clone(), p as i32));
        }
    }
    out
}

/// A member of one group.
pub struct Group {
    pub pool: PgPool,
    pub group: String,
    pub member: String,
    pub topics: Vec<(String, i32)>,
}

struct Held {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl Group {
    async fn heartbeat(&self) -> anyhow::Result<Vec<String>> {
        let ttl = MEMBER_TTL.as_secs_f64();
        sqlx::query(
            "insert into kafka_consumers (group_name, member_id) values ($1, $2) \
             on conflict (group_name, member_id) do update set heartbeat_at = now()",
        )
        .bind(&self.group)
        .bind(&self.member)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "delete from kafka_consumers where group_name = $1 and heartbeat_at < now() - make_interval(secs => $2)",
        )
        .bind(&self.group)
        .bind(ttl)
        .execute(&self.pool)
        .await?;
        Ok(sqlx::query_scalar(
            "select member_id from kafka_consumers where group_name = $1 order by member_id",
        )
        .bind(&self.group)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn acquire(&self, topic: &str, partition: i32) -> anyhow::Result<bool> {
        let r = sqlx::query(
            "insert into kafka_partition_leases (group_name, topic, partition, owner, lease_until) \
             values ($1, $2, $3, $4, now() + make_interval(secs => $5)) \
             on conflict (group_name, topic, partition) do update set owner = excluded.owner, \
               lease_until = excluded.lease_until \
             where kafka_partition_leases.owner is null or kafka_partition_leases.owner = excluded.owner \
               or kafka_partition_leases.lease_until < now()",
        )
        .bind(&self.group)
        .bind(topic)
        .bind(partition)
        .bind(&self.member)
        .bind(MEMBER_TTL.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    async fn renew(&self) -> anyhow::Result<BTreeSet<(String, i32)>> {
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "update kafka_partition_leases set lease_until = now() + make_interval(secs => $3) \
             where group_name = $1 and owner = $2 and lease_until >= now() returning topic, partition",
        )
        .bind(&self.group)
        .bind(&self.member)
        .bind(MEMBER_TTL.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    async fn release(&self, topic: &str, partition: i32) -> anyhow::Result<()> {
        sqlx::query(
            "update kafka_partition_leases set owner = null, lease_until = null \
             where group_name = $1 and topic = $2 and partition = $3 and owner = $4",
        )
        .bind(&self.group)
        .bind(topic)
        .bind(partition)
        .bind(&self.member)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn leave(&self) {
        let _ = sqlx::query(
            "update kafka_partition_leases set owner = null, lease_until = null where group_name = $1 and owner = $2",
        )
        .bind(&self.group)
        .bind(&self.member)
        .execute(&self.pool)
        .await;
        let _ = sqlx::query("delete from kafka_consumers where group_name = $1 and member_id = $2")
            .bind(&self.group)
            .bind(&self.member)
            .execute(&self.pool)
            .await;
    }

    fn spawn<R: PartitionRunner>(&self, runner: &Arc<R>, topic: &str, partition: i32) -> Held {
        let stop = CancellationToken::new();
        let ctx = PartitionCtx {
            pool: self.pool.clone(),
            group: self.group.clone(),
            member: self.member.clone(),
            topic: topic.to_string(),
            partition,
        };
        let runner = runner.clone();
        let s = stop.clone();
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_millis(500);
            loop {
                if s.is_cancelled() {
                    return;
                }
                match runner.clone().run(ctx.clone(), s.clone()).await {
                    Ok(()) => return,
                    Err(e) => {
                        if e.downcast_ref::<LeaseLost>().is_some() {
                            tracing::warn!(group = %ctx.group, topic = %ctx.topic, partition = ctx.partition, "partition lease lost");
                            return;
                        }
                        metrics::counter!("fastproof_stream_errors_total", "group" => ctx.group.clone()).increment(1);
                        tracing::warn!(group = %ctx.group, topic = %ctx.topic, partition = ctx.partition,
                            error = %format!("{e:#}"), "partition worker failed; restarting");
                    }
                }
                tokio::select! {
                    _ = s.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        Held { stop, task }
    }

    /// Joins the group and runs `runner` on every partition this member holds until `stop`;
    /// then stops the workers, releases the leases and leaves.
    pub async fn run<R: PartitionRunner>(self, runner: Arc<R>, stop: CancellationToken) {
        let mut held: BTreeMap<(String, i32), Held> = BTreeMap::new();
        loop {
            if let Err(e) = self.tick(&runner, &mut held).await {
                tracing::warn!(group = %self.group, error = %format!("{e:#}"), "consumer group heartbeat failed");
            }
            metrics::gauge!("fastproof_stream_partitions_held", "group" => self.group.clone())
                .set(held.len() as f64);
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = tokio::time::sleep(HEARTBEAT) => {}
            }
        }
        let held = std::mem::take(&mut held);
        for h in held.values() {
            h.stop.cancel();
        }
        let tasks = futures::future::join_all(held.into_values().map(|h| h.task));
        let _ = tokio::time::timeout(Duration::from_secs(20), tasks).await;
        self.leave().await;
    }

    async fn tick<R: PartitionRunner>(
        &self,
        runner: &Arc<R>,
        held: &mut BTreeMap<(String, i32), Held>,
    ) -> anyhow::Result<()> {
        let members = self.heartbeat().await.context("heartbeat")?;
        let desired = assign(&members, &self.member, &self.topics);
        let renewed = self.renew().await.context("renewing leases")?;
        held.retain(|k, h| {
            let alive = !h.task.is_finished();
            if !renewed.contains(k) || !alive {
                h.stop.cancel();
                false
            } else {
                true
            }
        });
        let release: Vec<(String, i32)> = held
            .keys()
            .filter(|k| !desired.contains(*k))
            .cloned()
            .collect();
        for k in release {
            if let Some(h) = held.remove(&k) {
                h.stop.cancel();
                let _ = tokio::time::timeout(Duration::from_secs(30), h.task).await;
                self.release(&k.0, k.1).await?;
                tracing::info!(group = %self.group, topic = %k.0, partition = k.1, "released partition");
            }
        }
        for k in desired {
            if held.contains_key(&k) {
                continue;
            }
            if self.acquire(&k.0, k.1).await? {
                tracing::info!(group = %self.group, member = %self.member, topic = %k.0, partition = k.1, "acquired partition");
                held.insert(k.clone(), self.spawn(runner, &k.0, k.1));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_assignment_covers_every_partition_once() {
        let topics = vec![("a".to_string(), 12), ("b".to_string(), 5)];
        for m in 1..=7 {
            let members: Vec<String> = (0..m).map(|i| format!("m{i}")).collect();
            let mut all = BTreeSet::new();
            let mut total = 0;
            for me in &members {
                let mine = assign(&members, me, &topics);
                total += mine.len();
                all.extend(mine);
            }
            assert_eq!(total, 17);
            assert_eq!(all.len(), 17);
        }
        assert!(assign(&["x".into()], "y", &topics).is_empty());
    }
}
