//! Retries inside one launch (Allure 3 `retrySubstore.ts` semantics).

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::result::ParsedResult;
use crate::status::Status;

/// What the grouping needs to know about one result.
#[derive(Debug, Clone, Copy)]
pub struct AttemptKey<'a> {
    pub series_key: &'a str,
    pub start: Option<i64>,
    pub status: Status,
}

/// Results of one series within a launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryGroup {
    pub series_key: String,
    /// Index of the latest attempt (the one shown as the result).
    pub latest: usize,
    /// All attempts, oldest first; the last element equals `latest`.
    pub attempts: Vec<usize>,
    /// Attempts that are not the latest, oldest first.
    pub earlier: Vec<usize>,
    /// More than one attempt and their passed/failed/broken statuses differ.
    pub flaky_in_run: bool,
}

/// Allure 3 order: a result with a start is newer than one without; then higher start;
/// then later ingest order. Returns `Greater` when `a` is newer.
fn compare_newer(a: (usize, &AttemptKey<'_>), b: (usize, &AttemptKey<'_>)) -> Ordering {
    match (a.1.start, b.1.start) {
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (Some(x), Some(y)) if x != y => x.cmp(&y),
        _ => a.0.cmp(&b.0),
    }
}

/// Groups results by series key; the slice order is the ingest order.
pub fn group_attempts(items: &[AttemptKey<'_>]) -> Vec<RetryGroup> {
    let mut order: Vec<&str> = Vec::new();
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        by_key
            .entry(it.series_key)
            .or_insert_with(|| {
                order.push(it.series_key);
                Vec::new()
            })
            .push(i);
    }
    order
        .into_iter()
        .map(|key| {
            let mut idx = by_key.remove(key).unwrap_or_default();
            idx.sort_by(|&a, &b| compare_newer((a, &items[a]), (b, &items[b])));
            let latest = *idx.last().unwrap_or(&0);
            let mut significant = idx
                .iter()
                .map(|&i| items[i].status)
                .filter(|s| s.is_significant());
            let first = significant.next();
            let flaky_in_run =
                idx.len() > 1 && first.is_some() && significant.any(|s| Some(s) != first);
            let earlier = idx[..idx.len().saturating_sub(1)].to_vec();
            RetryGroup {
                series_key: key.to_string(),
                latest,
                attempts: idx,
                earlier,
                flaky_in_run,
            }
        })
        .collect()
}

/// Groups parsed results of one launch by `identity.series_key`.
pub fn group_retries(results: &[ParsedResult]) -> Vec<RetryGroup> {
    let keys: Vec<AttemptKey<'_>> = results
        .iter()
        .map(|r| AttemptKey {
            series_key: &r.identity.series_key,
            start: r.start,
            status: r.status,
        })
        .collect();
    group_attempts(&keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(series: &str, start: Option<i64>, status: Status) -> AttemptKey<'_> {
        AttemptKey {
            series_key: series,
            start,
            status,
        }
    }

    #[test]
    fn latest_by_start_then_ingest_order() {
        let items = [
            k("a", Some(200), Status::Passed),
            k("a", Some(100), Status::Failed),
            k("b", None, Status::Failed),
            k("b", None, Status::Failed),
            k("c", None, Status::Passed),
            k("c", Some(1), Status::Failed),
            k("d", Some(5), Status::Passed),
        ];
        let g = group_attempts(&items);
        assert_eq!(g.len(), 4);
        assert_eq!(g[0].series_key, "a");
        assert_eq!(g[0].latest, 0);
        assert_eq!(g[0].attempts, vec![1, 0]);
        assert_eq!(g[0].earlier, vec![1]);
        assert!(g[0].flaky_in_run);
        assert_eq!(g[1].latest, 3);
        assert!(!g[1].flaky_in_run);
        assert_eq!(g[2].latest, 5, "a result with start wins over one without");
        assert!(g[2].flaky_in_run);
        assert_eq!(g[3].attempts, vec![6]);
        assert!(!g[3].flaky_in_run);
    }

    #[test]
    fn same_start_uses_ingest_order() {
        let items = [
            k("a", Some(1), Status::Failed),
            k("a", Some(1), Status::Passed),
        ];
        let g = group_attempts(&items);
        assert_eq!(g[0].latest, 1);
    }

    #[test]
    fn non_significant_statuses_do_not_make_flaky() {
        let items = [k("a", None, Status::Skipped), k("a", None, Status::Passed)];
        assert!(!group_attempts(&items)[0].flaky_in_run);
        let items = [k("a", None, Status::Failed), k("a", None, Status::Broken)];
        assert!(group_attempts(&items)[0].flaky_in_run);
    }
}
