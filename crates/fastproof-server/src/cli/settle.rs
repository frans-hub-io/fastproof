//! Waiting for the writer after an upload: with Kafka the upload is `done` once its results are
//! on the topic, and the launch counts grow when the writers have stored them. The server does
//! not report that progress, so the CLI watches the launch counts.

use std::time::Duration;

use fastproof_store::Launch;
use tokio::time::Instant;
use uuid::Uuid;

use super::client::Client;

/// How long the counts may stay unchanged below the expected total before the wait ends.
pub const QUIET: Duration = Duration::from_secs(15);

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
    expected: i64,
    last: i64,
    changed_at: Instant,
    quiet: Duration,
}

impl Tracker {
    pub fn new(baseline: i64, parsed: i64, quiet: Duration, now: Instant) -> Self {
        Self {
            expected: baseline.max(0).saturating_add(parsed.max(0)),
            last: baseline,
            changed_at: now,
            quiet,
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
        if now.duration_since(self.changed_at) >= self.quiet {
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
    fn quiet_below_the_expected_total_ends_the_wait() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 10, QUIET, t0);
        assert_eq!(t.observe(4, t0 + Duration::from_secs(5)), None);
        assert_eq!(t.observe(4, t0 + Duration::from_secs(19)), None);
        assert_eq!(
            t.observe(4, t0 + Duration::from_secs(20)),
            Some(Settled::Quiet)
        );
    }

    #[test]
    fn growth_restarts_the_quiet_window() {
        let t0 = Instant::now();
        let mut t = Tracker::new(0, 10, QUIET, t0);
        assert_eq!(t.observe(3, t0 + Duration::from_secs(10)), None);
        assert_eq!(t.observe(5, t0 + Duration::from_secs(20)), None);
        assert_eq!(t.observe(5, t0 + Duration::from_secs(34)), None);
        assert_eq!(
            t.observe(5, t0 + Duration::from_secs(35)),
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
