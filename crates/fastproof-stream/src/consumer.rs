//! Partition workers: a Kafka partition consumed from its committed offset, and the PostgreSQL
//! outbox (`events`) read in id order without skipping rows of transactions still in flight.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fastproof_store::Store;
use fastproof_store::events::OutboxEvent;
use rskafka::record::RecordAndOffset;
use tokio_util::sync::CancellationToken;

use crate::groups::{PartitionCtx, PartitionRunner};
use crate::kafka::{FetchWindow, Fetched, Kafka, Start, record_bytes};

/// Handles one batch of a partition and commits `next_offset`. A batch holds at most
/// [`crate::KafkaConfig::batch_max_records`] records and about
/// [`crate::KafkaConfig::batch_max_bytes`] bytes (a larger record is a batch of its own).
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

/// Consumes the partitions a group member holds. Fetches stay within the process budget
/// ([`Kafka::fetch_bounded`]); the records are handed over in bounded batches, each committed
/// before its bytes go back to the budget.
pub struct KafkaConsumer<H> {
    pub kafka: Arc<Kafka>,
    pub handler: Arc<H>,
    /// Where to start without a committed offset.
    pub start: Start,
    pub max_wait_ms: i32,
}

/// Takes the next batch from the front of `records`: at most `max_records` records and
/// `max_bytes` bytes, and at least one record.
pub fn next_batch(
    records: &mut VecDeque<RecordAndOffset>,
    max_records: usize,
    max_bytes: usize,
) -> (Vec<RecordAndOffset>, usize) {
    let mut out = Vec::new();
    let mut bytes = 0;
    while let Some(r) = records.front() {
        let b = record_bytes(r);
        if !out.is_empty() && (out.len() >= max_records || bytes + b > max_bytes) {
            break;
        }
        bytes += b;
        out.extend(records.pop_front());
    }
    (out, bytes)
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
        let cfg = self.kafka.config();
        let mut window = FetchWindow::new(cfg.fetch_max_bytes);
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            let fetched = tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                r = self.kafka.fetch_bounded(&ctx.topic, ctx.partition, offset, &mut window, self.max_wait_ms) => r?,
            };
            let Fetched {
                records,
                high_watermark: hwm,
                from,
                mut reservation,
            } = fetched;
            offset = from;
            if records.is_empty() {
                ctx.record_lag(hwm, offset);
                continue;
            }
            let mut records = VecDeque::from(records);
            while !records.is_empty() {
                let (batch, bytes) =
                    next_batch(&mut records, cfg.batch_max_records, cfg.batch_max_bytes);
                let Some(next) = batch.last().map(|r| r.offset + 1) else {
                    break;
                };
                let n = batch.len();
                let started = Instant::now();
                self.handler.handle(&ctx, batch, next, hwm).await?;
                reservation.release(bytes);
                metrics::counter!("fastproof_kafka_consumed_total", "group" => ctx.group.clone(), "topic" => ctx.topic.clone())
                    .increment(n as u64);
                metrics::histogram!("fastproof_kafka_batch_seconds", "group" => ctx.group.clone())
                    .record(started.elapsed().as_secs_f64());
                offset = next;
                ctx.record_lag(hwm, offset);
                if stop.is_cancelled() {
                    return Ok(());
                }
            }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rskafka::record::Record;

    use super::*;

    fn rec(offset: i64, size: usize) -> RecordAndOffset {
        RecordAndOffset {
            record: Record {
                key: None,
                value: Some(vec![b'x'; size]),
                headers: BTreeMap::new(),
                timestamp: chrono::Utc::now(),
            },
            offset,
        }
    }

    #[test]
    fn batches_are_bounded_in_records_and_bytes() {
        let mut q: VecDeque<RecordAndOffset> = (0..10).map(|i| rec(i, 1000)).collect();
        q.push_back(rec(10, 50_000));
        q.push_back(rec(11, 10));
        let (b, bytes) = next_batch(&mut q, 4, 1 << 20);
        assert_eq!(
            (b.len(), b[0].offset, bytes),
            (4, 0, 4 * record_bytes(&b[0]))
        );
        let (b, _) = next_batch(&mut q, 100, 3000);
        assert_eq!(b.len(), 2, "bytes cap the batch");
        let (b, _) = next_batch(&mut q, 100, 3000);
        assert_eq!(b.iter().map(|r| r.offset).collect::<Vec<_>>(), vec![6, 7]);
        let (b, _) = next_batch(&mut q, 100, 3000);
        assert_eq!(b.len(), 2);
        let (b, _) = next_batch(&mut q, 100, 3000);
        assert_eq!(
            (b.len(), b[0].offset),
            (1, 10),
            "a record larger than the cap is a batch of its own"
        );
        let (b, _) = next_batch(&mut q, 100, 3000);
        assert_eq!(b[0].offset, 11);
        assert!(q.is_empty());
        assert!(next_batch(&mut q, 100, 3000).0.is_empty());
    }
}
