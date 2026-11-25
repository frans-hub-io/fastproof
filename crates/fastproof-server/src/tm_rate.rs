//! Per-caller rate of test-management writes that multiply work (plan launches, run items
//! added from a plan, plan item changes and syncs, managed plans with a filter, bulk case
//! changes, archiving a folder with its cases): a token bucket per caller and replica that
//! holds `FASTPROOF_TM_MULTIPLYING_WRITES_PER_MINUTE` requests and refills at that rate.
//! A refused request answers 429 `rate_limited` with `Retry-After`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiResult};

use crate::limits::caller_key;
use crate::state::AppState;

/// Callers remembered per replica; beyond it, callers whose bucket refilled are forgotten,
/// then the tenth that was used longest ago.
const MAX_CALLERS: usize = 10_000;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// The buckets of one replica.
#[derive(Debug, Default)]
pub struct WriteRate {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl WriteRate {
    /// Takes one request of `key` at `now` with `per_minute` allowed; `Err` holds the wait.
    pub fn take(&self, key: &str, per_minute: u32, now: Instant) -> Result<(), Duration> {
        self.take_n(key, per_minute, 1, now)
    }

    /// Takes `n` requests of `key` at once, or none of them; `Err` holds the wait.
    pub fn take_n(&self, key: &str, per_minute: u32, n: u32, now: Instant) -> Result<(), Duration> {
        let need = f64::from(n.max(1));
        let cap = f64::from(per_minute.max(1));
        let per_sec = cap / 60.0;
        let mut m = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= MAX_CALLERS && !m.contains_key(key) {
            m.retain(|_, b| b.tokens + now.duration_since(b.at).as_secs_f64() * per_sec < cap);
            if m.len() >= MAX_CALLERS {
                let mut ats: Vec<Instant> = m.values().map(|b| b.at).collect();
                ats.sort_unstable();
                let cut = ats[ats.len() / 10];
                m.retain(|_, b| b.at > cut);
            }
        }
        let b = m.entry(key.to_string()).or_insert(Bucket {
            tokens: cap,
            at: now,
        });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * per_sec).min(cap);
        b.at = now;
        if b.tokens >= need {
            b.tokens -= need;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((need - b.tokens) / per_sec))
        }
    }
}

/// Admits one multiplying write of the caller, or answers 429 `rate_limited`.
pub fn admit(s: &AppState, auth: &Auth) -> ApiResult<()> {
    admit_n(s, auth, 1)
}

/// Admits `n` multiplying writes of the caller at once (a request that fans out), or none.
pub fn admit_n(s: &AppState, auth: &Auth, n: u32) -> ApiResult<()> {
    let per_minute = s.cfg.tm.multiplying_writes_per_minute as u32;
    s.guards
        .tm_writes
        .take_n(&caller_key(auth), per_minute, n, Instant::now())
        .map_err(|wait| {
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "tm_writes")
                .increment(1);
            let secs = wait.as_secs().max(1).to_string();
            let mut e = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!(
                    "at most {per_minute} plan launches, plan changes and bulk case changes a minute per caller"
                ),
            );
            if let Ok(v) = HeaderValue::from_str(&secs) {
                e = e.with_header(RETRY_AFTER, v);
            }
            e
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refuses_beyond_the_rate_and_refills() {
        let r = WriteRate::default();
        let t = Instant::now();
        for _ in 0..3 {
            assert!(r.take("a", 3, t).is_ok());
        }
        let wait = r.take("a", 3, t).unwrap_err();
        assert!(wait <= Duration::from_secs(20), "{wait:?}");
        assert!(r.take("b", 3, t).is_ok(), "callers are separate");
        assert!(r.take("a", 3, t + Duration::from_secs(21)).is_ok());
        assert!(r.take("a", 3, t + Duration::from_secs(21)).is_err());
    }

    #[test]
    fn several_requests_are_taken_together_or_not_at_all() {
        let r = WriteRate::default();
        let t = Instant::now();
        assert!(r.take_n("a", 5, 4, t).is_ok());
        assert!(r.take_n("a", 5, 2, t).is_err(), "one token is left");
        assert!(r.take("a", 5, t).is_ok(), "the refused pair took nothing");
        assert!(
            r.take_n("b", 5, 6, t).is_err(),
            "more than the bucket holds"
        );
    }

    #[test]
    fn caller_map_is_bounded() {
        let r = WriteRate::default();
        let t = Instant::now();
        for i in 0..(MAX_CALLERS + 10) {
            let _ = r.take(&format!("c{i}"), 5, t);
        }
        assert!(r.buckets.lock().unwrap().len() <= MAX_CALLERS);
    }

    #[test]
    fn a_full_map_keeps_recent_exhausted_buckets() {
        let r = WriteRate::default();
        let t = Instant::now();
        for i in 0..MAX_CALLERS {
            let at = t + Duration::from_millis(i as u64);
            let _ = r.take(&format!("c{i}"), 1, at);
        }
        let late = t + Duration::from_millis(MAX_CALLERS as u64);
        assert!(r.take("new", 1, late).is_ok());
        assert!(
            r.take(&format!("c{}", MAX_CALLERS - 1), 1, late).is_err(),
            "the newest exhausted bucket is not forgiven"
        );
        assert!(r.buckets.lock().unwrap().len() < MAX_CALLERS);
    }
}
