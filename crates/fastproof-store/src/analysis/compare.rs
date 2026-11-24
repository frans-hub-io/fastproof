//! Comparison of two launches by series: new failures, fixed, still failing, new and missing
//! tests, and the largest duration changes.

use std::collections::HashMap;

use fastproof_model::Status;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::results::{MAX_TREE_RESULTS, SUMMARY_SELECT, summary_row};
use crate::{Launch, ResultSummary, Store};

/// Results listed per category, at most.
pub const MAX_COMPARE_ITEMS: usize = 1000;
/// Duration changes listed, at most.
pub const MAX_DURATION_CHANGES: usize = 100;
/// Smallest absolute duration change reported.
pub const MIN_DELTA_MS: i64 = 500;
/// Smallest relative duration change reported (either way).
pub const MIN_RATIO: f64 = 1.2;

/// A test that became slower or faster.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DurationChange {
    pub result_id_a: Uuid,
    pub result_id_b: Uuid,
    pub name: String,
    /// Duration in B minus duration in A.
    pub delta_ms: i64,
    /// Duration in B over duration in A.
    pub ratio: f64,
}

/// Launch B compared with launch A.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    /// Failed or broken in B, passed or skipped in A (results of B).
    pub new_failures: Vec<ResultSummary>,
    /// Failed or broken in A, passed in B (results of B).
    pub fixed: Vec<ResultSummary>,
    /// Failed or broken in both (results of B).
    pub still_failing: Vec<ResultSummary>,
    /// In B only.
    pub new_tests: Vec<ResultSummary>,
    /// In A only (results of A).
    pub missing: Vec<ResultSummary>,
    pub duration_changes: Vec<DurationChange>,
    /// A list was cut at its limit.
    pub truncated: bool,
}

type Row = (Uuid, Uuid, String, Option<i64>, String);

/// Compared outcome of one series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    NewFailure,
    Fixed,
    StillFailing,
    New,
    Missing,
    Same,
}

/// How a series changed between two statuses (`None` = absent).
pub fn classify(a: Option<Status>, b: Option<Status>) -> Change {
    match (a, b) {
        (None, Some(_)) => Change::New,
        (Some(_), None) => Change::Missing,
        (Some(a), Some(b)) if b.is_failure() && a.is_failure() => Change::StillFailing,
        (Some(a), Some(b)) if b.is_failure() && !a.is_failure() && a != Status::Unknown => {
            Change::NewFailure
        }
        (Some(a), Some(Status::Passed)) if a.is_failure() => Change::Fixed,
        _ => Change::Same,
    }
}

impl Store {
    async fn compare_rows(&self, l: &Launch) -> anyhow::Result<Vec<Row>> {
        Ok(sqlx::query_as(
            "select id, series_id, status, duration_ms, name from results \
             where launch_id = $1 and launch_created_at = $2 order by left(full_name, 600), full_name, id limit $3",
        )
        .bind(l.id)
        .bind(l.created_at)
        .bind(MAX_TREE_RESULTS)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn summaries(&self, l: &Launch, ids: &[Uuid]) -> anyhow::Result<Vec<ResultSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{SUMMARY_SELECT} where r.launch_id = $1 and r.launch_created_at = $2 and r.id = any($3) \
             order by left(r.full_name, 600), r.full_name, r.id"
        )))
        .bind(l.id)
        .bind(l.created_at)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(summary_row).collect::<Result<_, _>>()?)
    }

    pub async fn compare(&self, a: &Launch, b: &Launch) -> anyhow::Result<Comparison> {
        let ra = self.compare_rows(a).await?;
        let rb = self.compare_rows(b).await?;
        let by_a: HashMap<Uuid, &Row> = ra.iter().map(|r| (r.1, r)).collect();
        let by_b: HashMap<Uuid, &Row> = rb.iter().map(|r| (r.1, r)).collect();
        let mut lists: [Vec<Uuid>; 5] = Default::default();
        let mut truncated = false;
        let mut push = |i: usize, id: Uuid, lists: &mut [Vec<Uuid>; 5]| {
            if lists[i].len() < MAX_COMPARE_ITEMS {
                lists[i].push(id);
            } else {
                truncated = true;
            }
        };
        let mut durations = Vec::new();
        for r in &rb {
            let old = by_a.get(&r.1);
            let change = classify(
                old.map(|o| Status::parse_lenient(&o.2)),
                Some(Status::parse_lenient(&r.2)),
            );
            match change {
                Change::NewFailure => push(0, r.0, &mut lists),
                Change::Fixed => push(1, r.0, &mut lists),
                Change::StillFailing => push(2, r.0, &mut lists),
                Change::New => push(3, r.0, &mut lists),
                _ => {}
            }
            if let Some(o) = old
                && let (Some(da), Some(db)) = (o.3, r.3)
                && da > 0
            {
                let delta = db - da;
                let ratio = db as f64 / da as f64;
                if delta.abs() >= MIN_DELTA_MS && (ratio >= MIN_RATIO || ratio <= 1.0 / MIN_RATIO) {
                    durations.push(DurationChange {
                        result_id_a: o.0,
                        result_id_b: r.0,
                        name: r.4.clone(),
                        delta_ms: delta,
                        ratio: (ratio * 1000.0).round() / 1000.0,
                    });
                }
            }
        }
        for r in &ra {
            if !by_b.contains_key(&r.1) {
                push(4, r.0, &mut lists);
            }
        }
        durations.sort_by_key(|x| std::cmp::Reverse(x.delta_ms.abs()));
        if durations.len() > MAX_DURATION_CHANGES {
            durations.truncate(MAX_DURATION_CHANGES);
            truncated = true;
        }
        Ok(Comparison {
            new_failures: self.summaries(b, &lists[0]).await?,
            fixed: self.summaries(b, &lists[1]).await?,
            still_failing: self.summaries(b, &lists[2]).await?,
            new_tests: self.summaries(b, &lists[3]).await?,
            missing: self.summaries(a, &lists[4]).await?,
            duration_changes: durations,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        use Status::*;
        assert_eq!(classify(Some(Passed), Some(Failed)), Change::NewFailure);
        assert_eq!(classify(Some(Skipped), Some(Broken)), Change::NewFailure);
        assert_eq!(classify(Some(Failed), Some(Passed)), Change::Fixed);
        assert_eq!(classify(Some(Broken), Some(Failed)), Change::StillFailing);
        assert_eq!(classify(None, Some(Passed)), Change::New);
        assert_eq!(classify(Some(Passed), None), Change::Missing);
        assert_eq!(classify(Some(Passed), Some(Passed)), Change::Same);
        assert_eq!(classify(Some(Failed), Some(Skipped)), Change::Same);
    }
}
