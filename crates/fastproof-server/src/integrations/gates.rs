//! Quality gates: evaluated when a launch is finished (and its uploads are processed) and on
//! demand by `GET .../launches/{id}/gate`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use fastproof_store::integrations::GateCounts;
use fastproof_store::{Launch, Project};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::settings::{Gate, GateRule, Metric, matches_any};
use crate::state::AppState;

/// Names of new failures kept for summaries.
pub const NEW_FAILURE_SAMPLE: i64 = 10;

/// What a gate rule is computed from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Facts {
    pub counts: GateCounts,
    pub duration_s: Option<f64>,
}

impl Facts {
    pub fn of(counts: GateCounts, launch: &Launch) -> Self {
        Self {
            counts,
            duration_s: launch.duration_ms.map(|d| d as f64 / 1000.0),
        }
    }

    /// Percent of passed among executed results, `None` without executed results.
    pub fn pass_rate(&self) -> Option<f64> {
        let executed = self.counts.total - self.counts.skipped;
        (executed > 0).then(|| self.counts.passed as f64 * 100.0 / executed as f64)
    }

    fn value(&self, m: Metric) -> Option<f64> {
        match m {
            Metric::Failed => Some(self.counts.failed as f64),
            Metric::Broken => Some(self.counts.broken as f64),
            Metric::Flaky => Some(self.counts.flaky as f64),
            Metric::PassRate => self.pass_rate(),
            Metric::Duration => self.duration_s,
            Metric::NewFailures => None,
        }
    }
}

/// A comparison launch and what the evaluated launch looks like against it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Baseline {
    /// The baseline launch, `None` when the branch has no finished launch.
    pub launch_id: Option<Uuid>,
    pub facts: Option<Facts>,
    /// New failures of the evaluated launch against this baseline.
    pub new_failures: i64,
}

/// Outcome of one rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuleResult {
    pub gate: String,
    pub rule: GateRule,
    /// The measured value (a difference to the baseline for metrics other than
    /// `newFailures` when the rule has a baseline); `null` when it cannot be measured.
    pub actual: Option<f64>,
    /// The rule's value.
    pub expected: f64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_launch_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `passed`, `failed` or `pending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum GateStatus {
    Passed,
    Failed,
    Pending,
}

impl GateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GateStatus::Passed => "passed",
            GateStatus::Failed => "failed",
            GateStatus::Pending => "pending",
        }
    }
}

/// The gate result of a launch: `pending` while the launch runs or uploads are processed,
/// otherwise `failed` when a rule of an enabled gate that applies to the branch does not hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateReport {
    pub status: GateStatus,
    pub results: Vec<RuleResult>,
    pub launch_id: Uuid,
    pub evaluated_at: DateTime<Utc>,
}

/// The gates that apply to a branch.
pub fn applicable<'a>(gates: &'a [Gate], branch: Option<&str>) -> Vec<&'a Gate> {
    gates
        .iter()
        .filter(|g| g.enabled && matches_any(&g.branches, branch))
        .collect()
}

/// Evaluates gates. `previous` is the default comparison for `newFailures` (the newest
/// earlier launch of the same branch); `baselines` holds the named baseline branches.
pub fn evaluate(
    gates: &[&Gate],
    pending: bool,
    current: &Facts,
    previous: &Baseline,
    baselines: &BTreeMap<String, Baseline>,
) -> (GateStatus, Vec<RuleResult>) {
    let mut results = Vec::new();
    for g in gates {
        for r in &g.rules {
            let base = match &r.baseline {
                Some(b) => baselines.get(b).cloned().unwrap_or_default(),
                None => previous.clone(),
            };
            let mut note = None;
            let actual = match (r.metric, &r.baseline) {
                (Metric::NewFailures, _) => {
                    if base.launch_id.is_none() {
                        note = Some("no baseline launch: every failure is new".to_string());
                    }
                    Some(base.new_failures as f64)
                }
                (m, None) => current.value(m),
                (m, Some(_)) => match (&base.facts, current.value(m)) {
                    (Some(bf), Some(c)) => bf.value(m).map(|b| c - b),
                    _ => None,
                },
            };
            let ok = match actual {
                Some(a) => r.op.holds(a, r.value),
                None if r.baseline.is_some() && r.metric != Metric::NewFailures => {
                    note = Some("no baseline launch to compare with".to_string());
                    true
                }
                None => {
                    note = Some(match r.metric {
                        Metric::PassRate => "no executed results".to_string(),
                        _ => "not measured".to_string(),
                    });
                    false
                }
            };
            results.push(RuleResult {
                gate: g.name.clone(),
                rule: r.clone(),
                actual: actual.map(|a| (a * 1000.0).round() / 1000.0),
                expected: r.value,
                ok,
                baseline_launch_id: base.launch_id,
                note,
            });
        }
    }
    let status = if pending {
        GateStatus::Pending
    } else if results.iter().all(|r| r.ok) {
        GateStatus::Passed
    } else {
        GateStatus::Failed
    };
    (status, results)
}

/// A launch is pending while it runs or has uploads that are not processed.
pub fn is_pending(l: &Launch) -> bool {
    l.status == "running" || l.uploads.queued + l.uploads.processing > 0
}

pub fn branch_of(l: &Launch) -> Option<&str> {
    l.vcs.as_ref().and_then(|v| v.branch.as_deref())
}

pub fn env_of(l: &Launch) -> Option<&str> {
    l.environment.as_ref().and_then(|e| e.key.as_deref())
}

/// Everything integrations need to know about a finished launch.
#[derive(Clone, Debug)]
pub struct Evaluation {
    pub report: GateReport,
    pub facts: Facts,
    /// New failures against the previous launch of the same branch and environment.
    pub new_failures: i64,
    /// Up to [`NEW_FAILURE_SAMPLE`] new failures: result id and full name.
    pub new_failure_sample: Vec<(Uuid, String)>,
    pub previous_launch_id: Option<Uuid>,
}

async fn baseline_for(
    s: &AppState,
    project: &Project,
    launch: &Launch,
    branch: &str,
    sample: i64,
) -> anyhow::Result<(Baseline, Vec<(Uuid, String)>)> {
    let b = s
        .store
        .baseline_launch(
            project.id,
            branch,
            env_of(launch),
            launch.id,
            launch.created_at,
        )
        .await?;
    let (n, names) = s.store.new_failures(launch, b.as_ref(), sample).await?;
    let facts = match &b {
        Some(bl) => Some(Facts::of(s.store.gate_counts(bl).await?, bl)),
        None => None,
    };
    Ok((
        Baseline {
            launch_id: b.map(|l| l.id),
            facts,
            new_failures: n,
        },
        names,
    ))
}

/// Evaluates the gates of a project for a launch.
pub async fn evaluate_launch(
    s: &AppState,
    project: &Project,
    launch: &Launch,
    gates: &[Gate],
) -> anyhow::Result<Evaluation> {
    let branch = branch_of(launch);
    let apply = applicable(gates, branch);
    let counts = s.store.gate_counts(launch).await?;
    let facts = Facts::of(counts, launch);
    let (previous, names) = match branch {
        Some(b) => baseline_for(s, project, launch, b, NEW_FAILURE_SAMPLE).await?,
        None => {
            let (n, names) = s
                .store
                .new_failures(launch, None, NEW_FAILURE_SAMPLE)
                .await?;
            (
                Baseline {
                    launch_id: None,
                    facts: None,
                    new_failures: n,
                },
                names,
            )
        }
    };
    let mut baselines = BTreeMap::new();
    for g in &apply {
        for r in &g.rules {
            if let Some(b) = &r.baseline
                && !baselines.contains_key(b)
            {
                let (bl, _) = baseline_for(s, project, launch, b, 0).await?;
                baselines.insert(b.clone(), bl);
            }
        }
    }
    let (status, results) = evaluate(&apply, is_pending(launch), &facts, &previous, &baselines);
    Ok(Evaluation {
        report: GateReport {
            status,
            results,
            launch_id: launch.id,
            evaluated_at: Utc::now(),
        },
        facts,
        new_failures: previous.new_failures,
        new_failure_sample: names,
        previous_launch_id: previous.launch_id,
    })
}

#[cfg(test)]
mod tests {
    use super::super::settings::Op;
    use super::*;

    fn counts(total: i64, passed: i64, failed: i64, broken: i64, skipped: i64) -> GateCounts {
        GateCounts {
            total,
            passed,
            failed,
            broken,
            skipped,
            flaky: 1,
        }
    }

    fn rule(metric: Metric, op: Op, value: f64, baseline: Option<&str>) -> GateRule {
        GateRule {
            metric,
            op,
            value,
            baseline: baseline.map(str::to_string),
        }
    }

    fn gate(rules: Vec<GateRule>) -> Gate {
        Gate {
            name: "g".into(),
            enabled: true,
            branches: None,
            rules,
        }
    }

    #[test]
    fn math() {
        let cur = Facts {
            counts: counts(100, 90, 4, 1, 5),
            duration_s: Some(120.0),
        };
        assert!((cur.pass_rate().unwrap() - 94.736).abs() < 0.01);
        let prev = Baseline {
            launch_id: Some(Uuid::nil()),
            facts: Some(Facts {
                counts: counts(100, 95, 0, 0, 5),
                duration_s: Some(100.0),
            }),
            new_failures: 3,
        };
        let mut bl = BTreeMap::new();
        bl.insert("main".to_string(), prev.clone());
        let g = gate(vec![
            rule(Metric::Failed, Op::Lte, 4.0, None),
            rule(Metric::Broken, Op::Lt, 1.0, None),
            rule(Metric::PassRate, Op::Gte, 94.0, None),
            rule(Metric::NewFailures, Op::Lte, 3.0, None),
            rule(Metric::Duration, Op::Lte, 30.0, Some("main")),
            rule(Metric::PassRate, Op::Gte, -6.0, Some("main")),
            rule(Metric::Flaky, Op::Gt, 0.0, None),
        ]);
        let (st, res) = evaluate(&[&g], false, &cur, &prev, &bl);
        let oks: Vec<bool> = res.iter().map(|r| r.ok).collect();
        assert_eq!(oks, vec![true, false, true, true, true, true, true]);
        assert_eq!(st, GateStatus::Failed);
        assert_eq!(res[4].actual, Some(20.0));
        assert_eq!(res[5].actual, Some(-5.263));
        assert_eq!(res[4].baseline_launch_id, Some(Uuid::nil()));

        let (st, _) = evaluate(&[&g], true, &cur, &prev, &bl);
        assert_eq!(st, GateStatus::Pending);
        let (st, res) = evaluate(&[], false, &cur, &prev, &bl);
        assert_eq!((st, res.len()), (GateStatus::Passed, 0));
    }

    #[test]
    fn missing_data() {
        let empty = Facts {
            counts: counts(3, 0, 0, 0, 3),
            duration_s: None,
        };
        let none = Baseline::default();
        let g = gate(vec![
            rule(Metric::PassRate, Op::Gte, 90.0, None),
            rule(Metric::Failed, Op::Lte, 0.0, Some("main")),
            rule(Metric::NewFailures, Op::Lte, 0.0, Some("main")),
        ]);
        let (st, res) = evaluate(&[&g], false, &empty, &none, &BTreeMap::new());
        assert_eq!(st, GateStatus::Failed);
        assert!(!res[0].ok && res[0].actual.is_none());
        assert!(res[1].ok && res[1].note.is_some());
        assert!(res[2].ok && res[2].note.as_deref().unwrap().contains("every failure"));
    }

    #[test]
    fn branch_filter() {
        let mut g1 = gate(vec![rule(Metric::Failed, Op::Lte, 0.0, None)]);
        g1.branches = Some(vec!["release/*".into()]);
        let mut g2 = g1.clone();
        g2.branches = None;
        g2.enabled = false;
        let gs = vec![g1, g2];
        assert_eq!(applicable(&gs, Some("release/2")).len(), 1);
        assert_eq!(applicable(&gs, Some("main")).len(), 0);
        assert_eq!(applicable(&gs, None).len(), 0);
    }
}
