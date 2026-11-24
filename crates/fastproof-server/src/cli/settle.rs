//! Waiting for the writer after an upload: with Kafka the upload is `done` once its results are
//! on the topic, and the launch counts grow when the writers have stored them. The server does
//! not report that progress, so the CLI watches the launch counts.

use std::time::Duration;

use fastproof_store::Launch;
use tokio::time::Instant;
use uuid::Uuid;

use super::client::Client;

/// How long the counts may stay at the baseline (nothing stored yet) before the wait ends.
pub const QUIET: Duration = Duration::from_secs(15);
/// How long the counts may stay unchanged once some results are stored. Attempts of one test are
/// one result, so the total often stays below the parsed count: a short stable window ends the
/// wait instead of the long one.
pub const SETTLED: Duration = Duration::from_secs(3);

/// How a wait for the stored results ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The launch holds at least the results the uploads parsed.
    Complete,
    /// The counts stopped changing below that number (attempts of one result are counted once).
    Quiet,
    /// The time ran out while the counts were still changing or far behind.
    TimedOut,
}

/// Decides from successive observations of the launch's result total.
#[derive(Debug)]
pub struct Tracker {
    baseline: i64,
    expected: i64,
    last: i64,
    changed_at: Instant,
    quiet: Duration,
    settled: Duration,
}

impl Tracker {
    pub fn new(baseline: i64, parsed: i64, quiet: Duration, now: Instant) -> Self {
        Self {
            baseline,
            expected: baseline.max(0).saturating_add(parsed.max(0)),
            last: baseline,
            changed_at: now,
            quiet,
            settled: SETTLED.min(quiet),
        }
    }

    pub fn observe(&mut self, total: i64, now: Instant) -> Option<Settled> {
        if total >= self.expected {
            return Some(Settled::Complete);
        }
        if total != self.last {
            self.last = total;
            self.changed_at = now;
        }
        let window = if total > self.baseline {
            self.settled
        } else {
            self.quiet
        };
        if now.duration_since(self.changed_at) >= window {
            return Some(Settled::Quiet);
        }
        None
    }
}

/// Polls the launch until its total reaches `baseline + parsed`, stops changing, or `timeout`
/// passes. Returns the launch as last seen.
pub async fn wait_stored(
    client: &Client,
    project: &str,
    launch: Uuid,
    baseline: i64,
    parsed: i64,
    timeout: Duration,
) -> anyhow::Result<(Launch, Settled)> {
    let start = Instant::now();
    let deadline = start + timeout;
    let mut tracker = Tracker::new(baseline, parsed, QUIET, start);
    let mut delay = Duration::from_millis(200);
    loop {
        let l = client.launch(project, launch).await?;
        let now = Instant::now();
        if let Some(s) = tracker.observe(l.counts.total, now) {
            return Ok((l, s));
        }
        if now >= deadline {
            return Ok((l, Settled::TimedOut));
        }
        tokio::time::sleep(delay.min(deadline - now)).await;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_when_the_total_reaches_the_parsed_results() {
        let t0 = Instant::now();
        let mut t = Tracker::new(2, 10, QUIET, t0);
        assert_eq!(t.observe(2, t0), None);
        assert_eq!(t.observe(9, t0 + Duration::from_secs(1)), None);
        assert_eq!(
            t.observe(12, t0 + Duration::from_secs(2)),
            Some(Settled::Complete)
        );
    }

    #[test]
    fn zero_results_early_is_not_an_answer() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 5, QUIET, t0);
        for s in 0..14 {
            assert_eq!(t.observe(0, t0 + Duration::from_secs(s)), None);
        }
    }

    #[test]
    fn nothing_stored_waits_the_long_window() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 10, QUIET, t0);
        assert_eq!(t.observe(0, t0 + Duration::from_secs(14)), None);
        assert_eq!(
            t.observe(0, t0 + Duration::from_secs(15)),
            Some(Settled::Quiet)
        );
    }

    #[test]
    fn stored_results_below_the_parsed_count_settle_quickly() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 16, QUIET, t0);
        assert_eq!(t.observe(13, t0 + Duration::from_millis(200)), None);
        assert_eq!(t.observe(13, t0 + Duration::from_secs(3)), None);
        assert_eq!(
            t.observe(13, t0 + Duration::from_millis(3200)),
            Some(Settled::Quiet)
        );
    }

    #[test]
    fn growth_restarts_the_settle_window() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 10, QUIET, t0);
        assert_eq!(t.observe(3, t0 + Duration::from_secs(10)), None);
        assert_eq!(t.observe(5, t0 + Duration::from_secs(12)), None);
        assert_eq!(t.observe(5, t0 + Duration::from_secs(14)), None);
        assert_eq!(
            t.observe(5, t0 + Duration::from_secs(15)),
            Some(Settled::Quiet)
        );
    }

    #[test]
    fn nothing_parsed_is_complete_at_once() {
        let t0 = Instant::now();
        let mut t = Tracker::new(3, 0, QUIET, t0);
        assert_eq!(t.observe(3, t0), Some(Settled::Complete));
    }
}
