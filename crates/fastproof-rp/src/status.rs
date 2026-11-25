//! Statuses, log levels and item types of ReportPortal in native terms.

use fastproof_model::Status;
use serde_json::Value;

/// Message of results that did not finish (stopped, interrupted, cancelled, or never
/// finished before the launch did).
pub const INTERRUPTED: &str = "interrupted";

/// A ReportPortal item status in native terms: the status and whether the item was
/// interrupted (`STOPPED`, `INTERRUPTED`, `CANCELLED` are broken with message `interrupted`).
/// `None` for a missing or unknown status.
pub fn item_status(s: Option<&str>) -> Option<(Status, bool)> {
    let s = s?.trim().to_ascii_uppercase();
    Some(match s.as_str() {
        "PASSED" | "INFO" | "WARN" => (Status::Passed, false),
        "FAILED" => (Status::Failed, false),
        "SKIPPED" => (Status::Skipped, false),
        "STOPPED" | "INTERRUPTED" | "CANCELLED" => (Status::Broken, true),
        _ => return None,
    })
}

/// Whether a launch finish status means the launch was interrupted.
pub fn launch_interrupted(s: Option<&str>) -> bool {
    matches!(
        s.map(|v| v.trim().to_ascii_uppercase()).as_deref(),
        Some("STOPPED" | "INTERRUPTED" | "CANCELLED")
    )
}

/// The worst of the statuses (failed, broken, skipped, passed; unknown is ignored).
pub fn worst(statuses: impl IntoIterator<Item = Status>) -> Option<Status> {
    let rank = |s: Status| match s {
        Status::Failed => 4,
        Status::Broken => 3,
        Status::Skipped => 1,
        Status::Passed => 0,
        Status::Unknown => -1,
    };
    statuses
        .into_iter()
        .filter(|s| *s != Status::Unknown)
        .max_by_key(|s| rank(*s))
}

/// The native level (`error`, `warn`, `info`, `debug`, `trace`) of a ReportPortal log level:
/// a name (`fatal` and `error` are errors, `unknown` is info) or a number of the
/// ReportPortal scale (50000 fatal, 40000 error, 30000 warn, 20000 info, 10000 debug, 5000
/// trace).
pub fn log_level(v: &Value) -> &'static str {
    match v {
        Value::Number(n) => level_number(n.as_f64().unwrap_or(20000.0)),
        Value::String(s) => {
            let t = s.trim();
            if let Ok(n) = t.parse::<f64>() {
                return level_number(n);
            }
            match t.to_ascii_lowercase().as_str() {
                "fatal" | "error" | "critical" | "severe" => "error",
                "warn" | "warning" => "warn",
                "debug" => "debug",
                "trace" | "finest" | "finer" => "trace",
                _ => "info",
            }
        }
        _ => "info",
    }
}

fn level_number(n: f64) -> &'static str {
    if n >= 60000.0 {
        "info"
    } else if n >= 40000.0 {
        "error"
    } else if n >= 30000.0 {
        "warn"
    } else if n >= 20000.0 {
        "info"
    } else if n >= 10000.0 {
        "debug"
    } else {
        "trace"
    }
}

/// An item type in upper case (`STEP` when missing).
pub fn item_type(t: Option<&str>) -> String {
    let t = t.map(str::trim).filter(|t| !t.is_empty()).unwrap_or("STEP");
    crate::cut(&t.to_ascii_uppercase(), 64)
}

/// `BEFORE_*` and `AFTER_*` types are fixtures.
pub fn is_fixture_type(t: &str) -> bool {
    t.starts_with("BEFORE_") || t.starts_with("AFTER_")
}

/// Types that are a test unless they turn out to have tests under them.
pub fn is_test_type(t: &str) -> bool {
    matches!(t, "STEP" | "SCENARIO" | "TEST")
}

/// Types whose start opens a live `running` result at once (a `TEST` opens one only when it
/// finishes: it is often a container in TestNG).
pub fn opens_running_result(t: &str) -> bool {
    matches!(t, "STEP" | "SCENARIO")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn statuses() {
        assert_eq!(item_status(Some("passed")), Some((Status::Passed, false)));
        assert_eq!(item_status(Some("WARN")), Some((Status::Passed, false)));
        assert_eq!(item_status(Some("Failed")), Some((Status::Failed, false)));
        assert_eq!(item_status(Some("STOPPED")), Some((Status::Broken, true)));
        assert_eq!(item_status(Some("cancelled")), Some((Status::Broken, true)));
        assert_eq!(item_status(Some("whatever")), None);
        assert_eq!(item_status(None), None);
        assert!(launch_interrupted(Some("interrupted")));
        assert!(!launch_interrupted(Some("PASSED")));
        assert_eq!(
            worst([Status::Passed, Status::Unknown, Status::Skipped]),
            Some(Status::Skipped)
        );
        assert_eq!(
            worst([Status::Broken, Status::Failed, Status::Passed]),
            Some(Status::Failed)
        );
        assert_eq!(worst([Status::Unknown]), None);
    }

    #[test]
    fn levels() {
        assert_eq!(log_level(&json!("FATAL")), "error");
        assert_eq!(log_level(&json!("warn")), "warn");
        assert_eq!(log_level(&json!("unknown")), "info");
        assert_eq!(log_level(&json!(40000)), "error");
        assert_eq!(log_level(&json!("30000")), "warn");
        assert_eq!(log_level(&json!(10000)), "debug");
        assert_eq!(log_level(&json!(5000)), "trace");
        assert_eq!(log_level(&json!(50000)), "error");
        assert_eq!(log_level(&json!(null)), "info");
    }

    #[test]
    fn types() {
        assert_eq!(item_type(Some(" test ")), "TEST");
        assert_eq!(item_type(None), "STEP");
        assert!(is_fixture_type("BEFORE_CLASS"));
        assert!(is_fixture_type("AFTER_METHOD"));
        assert!(!is_fixture_type("STEP"));
        assert!(is_test_type("SCENARIO"));
        assert!(!is_test_type("SUITE"));
    }
}
