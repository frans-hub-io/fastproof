//! The in-flight byte budget of one process (`FASTPROOF_KAFKA_MAX_INFLIGHT_BYTES`): every
//! Kafka fetch reserves its share before it is sent and keeps the bytes of the records it
//! returned until they are handled and committed. A fetch waits while the budget is spent, so
//! the records a process holds are bounded whatever the lag and however many partitions it
//! consumes.

use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Bytes a decoded record is counted for beyond its key, value and headers.
pub const RECORD_OVERHEAD: usize = 128;

/// A byte budget shared by the consumers of a process.
#[derive(Debug)]
pub struct ByteBudget {
    limit: usize,
    used: Mutex<usize>,
    freed: Notify,
}

impl ByteBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            used: Mutex::new(0),
            freed: Notify::new(),
        })
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Bytes reserved now.
    pub fn used(&self) -> usize {
        *self.used.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn add(&self, n: usize) {
        let mut u = self.used.lock().unwrap_or_else(|e| e.into_inner());
        *u += n;
        metrics::gauge!("fastproof_kafka_inflight_bytes").set(*u as f64);
    }

    fn sub(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut u = self.used.lock().unwrap_or_else(|e| e.into_inner());
        *u = u.saturating_sub(n);
        metrics::gauge!("fastproof_kafka_inflight_bytes").set(*u as f64);
        drop(u);
        self.freed.notify_waiters();
    }

    fn try_take(&self, n: usize) -> bool {
        let mut u = self.used.lock().unwrap_or_else(|e| e.into_inner());
        if *u == 0 || *u + n <= self.limit {
            *u += n;
            metrics::gauge!("fastproof_kafka_inflight_bytes").set(*u as f64);
            true
        } else {
            false
        }
    }

    /// Waits until `n` bytes fit (or nothing else is reserved, so a request larger than the
    /// whole budget still proceeds alone) and reserves them.
    pub async fn reserve(self: &Arc<Self>, n: usize) -> Reservation {
        let mut waited = false;
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if self.try_take(n) {
                if waited {
                    metrics::counter!("fastproof_kafka_budget_waits_total").increment(1);
                }
                return Reservation {
                    budget: self.clone(),
                    bytes: n,
                };
            }
            waited = true;
            freed.await;
        }
    }
}

/// Bytes held from a [`ByteBudget`]; released when dropped.
#[derive(Debug)]
pub struct Reservation {
    budget: Arc<ByteBudget>,
    bytes: usize,
}

impl Reservation {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Sets the reserved bytes to `n`: more is taken without waiting (the memory is already
    /// in use), less is given back.
    pub fn resize(&mut self, n: usize) {
        if n > self.bytes {
            self.budget.add(n - self.bytes);
        } else {
            self.budget.sub(self.bytes - n);
        }
        self.bytes = n;
    }

    /// Gives back `n` bytes (at most what is held).
    pub fn release(&mut self, n: usize) {
        let n = n.min(self.bytes);
        self.budget.sub(n);
        self.bytes -= n;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.sub(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn reservations_wait_for_room_and_release_on_drop() {
        let b = ByteBudget::new(100);
        let mut a = b.reserve(60).await;
        let c = b.reserve(40).await;
        assert_eq!(b.used(), 100);
        let waiting = tokio::spawn({
            let b = b.clone();
            async move { b.reserve(30).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the budget is spent");
        a.release(30);
        let d = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(b.used(), 100);
        drop((a, c, d));
        assert_eq!(b.used(), 0);
        let mut big = b.reserve(500).await;
        assert_eq!(b.used(), 500, "a request larger than the budget runs alone");
        big.resize(10);
        assert_eq!(b.used(), 10);
        big.resize(70);
        assert_eq!(b.used(), 70);
        drop(big);
        assert_eq!(b.used(), 0);
    }
}
