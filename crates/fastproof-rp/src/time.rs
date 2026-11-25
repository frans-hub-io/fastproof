//! Times of ReportPortal requests: epoch milliseconds (a number or a string of digits) or
//! ISO-8601 with any fraction (microseconds from Java agents with server version 5.13.2+)
//! and an offset (none means UTC).

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

/// How far in the future a time may lie (clock skew of the agent).
pub const MAX_FUTURE_MS: i64 = 5 * 60 * 1000;

/// Milliseconds since the epoch of a time value, if it is one.
pub fn parse(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        Value::String(s) => parse_str(s),
        _ => None,
    }
}

fn parse_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_millis());
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(t) = DateTime::parse_from_str(s, f) {
            return Some(t.timestamp_millis());
        }
    }
    for f in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, f) {
            return Some(t.and_utc().timestamp_millis());
        }
    }
    None
}

/// The time of `v` within `0..=now + 5 min`, or `now` when it is missing or unreadable.
pub fn bounded_or_now(v: &Value, now: i64) -> i64 {
    match parse(v) {
        Some(t) => clamp(t, now),
        None => now,
    }
}

/// `t` within `0..=now + 5 min`.
pub fn clamp(t: i64, now: i64) -> i64 {
    t.clamp(0, now.saturating_add(MAX_FUTURE_MS))
}

/// Now, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// ISO-8601 with milliseconds in UTC, as ReportPortal answers.
pub fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn epoch_and_iso_forms() {
        assert_eq!(parse(&json!(1_700_000_000_123i64)), Some(1_700_000_000_123));
        assert_eq!(parse(&json!("1700000000123")), Some(1_700_000_000_123));
        assert_eq!(parse(&json!(1.7e12)), Some(1_700_000_000_000));
        assert_eq!(
            parse(&json!("2023-11-14T22:13:20.123456Z")),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse(&json!("2023-11-15T01:13:20.123+03:00")),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse(&json!("2023-11-15T01:13:20.123+0300")),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse(&json!("2023-11-14T22:13:20.123")),
            Some(1_700_000_000_123)
        );
        assert_eq!(
            parse(&json!("2023-11-14T22:13:20Z")),
            Some(1_700_000_000_000)
        );
        assert_eq!(parse(&json!("yesterday")), None);
        assert_eq!(parse(&json!(null)), None);
        assert_eq!(parse(&json!({"t": 1})), None);
    }

    #[test]
    fn times_are_bounded() {
        let now = 1_700_000_000_000;
        assert_eq!(bounded_or_now(&json!(-5), now), 0);
        assert_eq!(
            bounded_or_now(&json!(now + 3_600_000), now),
            now + MAX_FUTURE_MS
        );
        assert_eq!(bounded_or_now(&json!(null), now), now);
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
    }
}
