//! Past runs read from the history files of Allure reports, before they become launches.

use serde::{Deserialize, Serialize};

use crate::status::Status;

/// Status counts of one past report (Allure 2 `history-trend.json`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryTotals {
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub unknown: i64,
    pub total: i64,
}

/// What a history file says about a whole past report (trend files, metrics).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryTrend {
    pub totals: Option<HistoryTotals>,
    pub duration_ms: Option<i64>,
    pub retries: Option<i64>,
    pub runs: Option<i64>,
    pub categories: Vec<(String, i64)>,
    pub metrics: Vec<(String, f64)>,
}

/// One test in a past report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryResult {
    /// `testCaseHash.parametersHash[.environmentHash]`, the FastProof series key.
    pub series_key: String,
    pub full_name: Option<String>,
    pub name: Option<String>,
    pub status: Status,
    pub start_ms: Option<i64>,
    pub stop_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub message: Option<String>,
    /// Statuses of the earlier attempts, oldest first.
    pub retries: Vec<Status>,
    pub environment: Option<String>,
}

/// One past report: a launch to create.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    /// Stable within the source, so a second import finds the launch it made.
    pub key: String,
    pub name: String,
    pub timestamp_ms: i64,
    pub url: Option<String>,
    pub trend: HistoryTrend,
    pub results: Vec<HistoryResult>,
}

/// The three dot-separated md5 parts of a series key.
pub fn split_series_key(key: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = key.split('.');
    let (a, b, c) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() {
        return None;
    }
    let hex = |s: &str| s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (hex(a) && hex(b) && c.is_none_or(hex)).then_some((a, b, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_keys() {
        let h = "d41d8cd98f00b204e9800998ecf8427e";
        assert_eq!(split_series_key(&format!("{h}.{h}")), Some((h, h, None)));
        assert_eq!(
            split_series_key(&format!("{h}.{h}.{h}")),
            Some((h, h, Some(h)))
        );
        assert_eq!(split_series_key(&format!("{h}.{h}.{h}.{h}")), None);
        assert_eq!(split_series_key(h), None);
        assert_eq!(split_series_key(&format!("{h}.ZZ")), None);
        assert_eq!(split_series_key(&format!("{}.{h}", h.to_uppercase())), None);
    }
}
