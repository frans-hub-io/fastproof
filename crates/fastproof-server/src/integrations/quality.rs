//! Test quality metrics on the ops listener (X5) with bounded label cardinality: per project,
//! the most active branches and environments get their own label value, the rest are `other`.
//!
//! - `fastproof_project_pass_rate{project,branch,environment}`: passed / (total - skipped) of
//!   the newest finished launch of each branch and environment in the last 30 days (0 to 1;
//!   for `other`, of the newest launches of the branches in it together);
//! - `fastproof_project_flaky_tests{project}`: series flaky within a run in the last 20
//!   launches;
//! - `fastproof_launch_duration_seconds{project}`: histogram of finished launch durations.

use std::collections::{BTreeMap, HashMap};

use fastproof_store::integrations::QualityRow;

use crate::state::AppState;

/// Projects exported, at most.
pub const MAX_PROJECTS: usize = 1000;
/// Days of launches considered.
pub const WINDOW_DAYS: i64 = 30;
/// Label value of branches and environments outside the top ones.
pub const OTHER: &str = "other";
/// Label value of launches without branch or environment.
pub const NONE: &str = "none";

fn top(counts: &HashMap<String, i64>, n: usize) -> Vec<String> {
    let mut v: Vec<(&String, &i64)> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
}

/// Groups rows into label buckets: `((branch, environment), (passed, executed))`.
pub fn buckets(
    rows: &[(QualityRow, i64)],
    top_branches: usize,
    top_envs: usize,
) -> BTreeMap<(String, String), (i64, i64)> {
    let mut per_branch: HashMap<String, i64> = HashMap::new();
    let mut per_env: HashMap<String, i64> = HashMap::new();
    for (r, n) in rows {
        *per_branch.entry(r.branch.clone()).or_default() += n;
        *per_env.entry(r.environment.clone()).or_default() += n;
    }
    let tb = top(&per_branch, top_branches);
    let te = top(&per_env, top_envs);
    let name = |v: &str, keep: &[String]| {
        if !keep.iter().any(|k| k == v) {
            OTHER.to_string()
        } else if v.is_empty() {
            NONE.to_string()
        } else {
            v.chars().take(128).collect()
        }
    };
    let mut out: BTreeMap<(String, String), (i64, i64)> = BTreeMap::new();
    for (r, _) in rows {
        let k = (name(&r.branch, &tb), name(&r.environment, &te));
        let e = out.entry(k).or_default();
        e.0 += r.passed;
        e.1 += r.total - r.skipped;
    }
    out
}

/// Recomputes the gauges.
pub async fn refresh(s: &AppState) -> anyhow::Result<()> {
    let cfg = &s.cfg.integrations;
    for p in s.store.projects().await?.into_iter().take(MAX_PROJECTS) {
        let rows = s.store.quality_rows(p.id, WINDOW_DAYS).await?;
        for ((branch, env), (passed, executed)) in buckets(
            &rows,
            cfg.metrics_top_branches,
            cfg.metrics_top_environments,
        ) {
            if executed > 0 {
                metrics::gauge!("fastproof_project_pass_rate", "project" => p.key.clone(),
                    "branch" => branch, "environment" => env)
                .set(passed as f64 / executed as f64);
            }
        }
        let flaky = s.store.recent_flaky_series(p.id, 20).await?;
        metrics::gauge!("fastproof_project_flaky_tests", "project" => p.key.clone())
            .set(flaky as f64);
    }
    Ok(())
}

/// Histograms whose buckets differ from the latency default of `_seconds` metrics: launches run
/// for minutes to hours.
pub fn histogram_buckets() -> Vec<(String, Vec<f64>)> {
    vec![(
        "fastproof_launch_duration_seconds".into(),
        franshub_observe::LONG_DURATION_BUCKETS.to_vec(),
    )]
}

/// Records the duration of a finished launch.
pub fn launch_finished(project: &str, duration_ms: Option<i64>) {
    if let Some(ms) = duration_ms {
        metrics::histogram!("fastproof_launch_duration_seconds", "project" => project.to_string())
            .record(ms.max(0) as f64 / 1000.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(b: &str, e: &str, passed: i64, total: i64, n: i64) -> (QualityRow, i64) {
        (
            QualityRow {
                branch: b.into(),
                environment: e.into(),
                passed,
                total,
                skipped: 0,
            },
            n,
        )
    }

    #[test]
    fn cardinality_is_bounded() {
        let mut rows = vec![row("main", "", 9, 10, 50), row("release", "ff", 1, 2, 20)];
        for i in 0..100 {
            rows.push(row(&format!("feature-{i}"), "", 1, 1, 1));
        }
        let b = buckets(&rows, 2, 1);
        assert_eq!(b.len(), 3, "{b:?}");
        assert_eq!(b[&("main".to_string(), NONE.to_string())], (9, 10));
        assert_eq!(b[&(OTHER.to_string(), NONE.to_string())], (100, 100));
        assert_eq!(b[&("release".to_string(), OTHER.to_string())], (1, 2));
    }
}
