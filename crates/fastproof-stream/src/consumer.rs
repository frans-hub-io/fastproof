//! Partition workers: a Kafka partition consumed from its committed offset, and the PostgreSQL
//! outbox (`events`) read in id order without skipping rows of transactions still in flight.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fastproof_store::Store;
use fastproof_store::events::OutboxEvent;
use rskafka::record::RecordAndOffset;
use tokio_util::sync::CancellationToken;

use crate::groups::{PartitionCtx, PartitionRunner};
use crate::kafka::{Kafka, Start};

/// Handles one fetched batch of a partition and commits `next_offset`.
#[async_trait]
pub trait BatchHandler: Send + Sync + 'static {
    async fn handle(
        &self,
        ctx: &PartitionCtx,
        records: Vec<RecordAndOffset>,
        next_offset: i64,
        high_watermark: i64,
    ) -> anyhow::Result<()>;
}

/// Consumes the partitions a group member holds.
pub struct KafkaConsumer<H> {
    pub kafka: Arc<Kafka>,
    pub handler: Arc<H>,
    /// Where to start without a committed offset.
    pub start: Start,
    pub max_bytes: i32,
    pub max_wait_ms: i32,
}

#[async_trait]
impl<H: BatchHandler> PartitionRunner for KafkaConsumer<H> {
    async fn run(
        self: Arc<Self>,
        ctx: PartitionCtx,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut offset = match ctx.stored_offset().await? {
            Some(o) => o,
            None => {
                self.kafka
                    .offset(&ctx.topic, ctx.partition, self.start)
                    .await?
            }
        };
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            let fetched = tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                r = self.kafka.fetch(&ctx.topic, ctx.partition, offset, self.max_bytes, self.max_wait_ms) => r?,
            };
            let (records, hwm, from) = fetched;
            offset = from;
            let records: Vec<RecordAndOffset> =
                records.into_iter().filter(|r| r.offset >= offset).collect();
            let Some(last) = records.last().map(|r| r.offset) else {
                ctx.record_lag(hwm, offset);
                continue;
            };
            let next = last + 1;
            let n = records.len();
            let started = Instant::now();
            self.handler.handle(&ctx, records, next, hwm).await?;
            metrics::counter!("fastproof_kafka_consumed_total", "group" => ctx.group.clone(), "topic" => ctx.topic.clone())
                .increment(n as u64);
            metrics::histogram!("fastproof_kafka_batch_seconds", "group" => ctx.group.clone())
                .record(started.elapsed().as_secs_f64());
            offset = next;
            ctx.record_lag(hwm, offset);
        }
    }
}

/// Topic name under which outbox positions are stored in `kafka_offsets`.
pub const OUTBOX_TOPIC: &str = "outbox";

/// A missing outbox id seen while later ids were visible.
struct Gap {
    upto: i64,
    xmax: String,
    since: Instant,
}

/// Reads the outbox in id order. Identity values are taken before commit, so a gap may be a
/// transaction still running: it is skipped only once every transaction that was running when
/// the gap was seen has ended (or after [`GAP_LIMIT`]).
pub struct OutboxReader {
    store: Store,
    last: i64,
    gap: Option<Gap>,
}

/// A gap is skipped after this long whatever the transaction horizon says.
pub const GAP_LIMIT: Duration = Duration::from_secs(300);

impl OutboxReader {
    /// Starts after the outbox id `last`.
    pub fn new(store: Store, last: i64) -> Self {
        Self {
            store,
            last,
            gap: None,
        }
    }

    pub fn last(&self) -> i64 {
        self.last
    }

    /// Marks events up to `id` as handled.
    pub fn advance(&mut self, id: i64) {
        self.last = self.last.max(id);
    }

    async fn settled(&self, g: &Gap) -> anyhow::Result<bool> {
        if g.since.elapsed() > GAP_LIMIT {
            return Ok(true);
        }
        Ok(sqlx::query_scalar::<_, bool>(
            "select pg_snapshot_xmin(pg_current_snapshot()) >= $1::text::xid8",
        )
        .bind(&g.xmax)
        .fetch_one(self.store.pool())
        .await?)
    }

    /// The next events after the last handled one, without crossing an unsettled gap.
    pub async fn next(&mut self, limit: i64) -> anyhow::Result<Vec<OutboxEvent>> {
        let rows = self.store.outbox_after(self.last, limit).await?;
        let mut out = Vec::with_capacity(rows.len());
        let mut expected = self.last + 1;
        let max_seen = rows.last().map(|r| r.id).unwrap_or(self.last);
        let mut settled: Option<bool> = None;
        for r in rows {
            if r.id != expected {
                let missing_upto = r.id - 1;
                let covered = self.gap.as_ref().is_some_and(|g| missing_upto <= g.upto);
                let ok = if covered {
                    if settled.is_none() {
                        let g = self.gap.as_ref().expect("covered");
                        settled = Some(self.settled(g).await?);
                    }
                    settled == Some(true)
                } else {
                    false
                };
                if !ok {
                    if !covered {
                        let xmax: String = sqlx::query_scalar(
                            "select pg_snapshot_xmax(pg_current_snapshot())::text",
                        )
                        .fetch_one(self.store.pool())
                        .await?;
                        self.gap = Some(Gap {
                            upto: max_seen,
                            xmax,
                            since: Instant::now(),
                        });
                    }
                    break;
                }
                metrics::counter!("fastproof_outbox_gaps_skipped_total").increment(1);
            }
            expected = r.id + 1;
            out.push(r);
        }
        if self.gap.as_ref().is_some_and(|g| expected > g.upto) {
            self.gap = None;
        }
        Ok(out)
    }
}
