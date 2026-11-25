//! Gate history and simulation reads (contract §7.5).
//!
//! The history is read from the stored gate reports (`launch_gates` rows in state `done`). The
//! simulation reads the newest finished launches and their result counts; the evaluation
//! itself is server code (`integrations/gates.rs`).

use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::types::{
    AnalysisResult, GateHistory, GateHistoryBranch, GateHistoryGate, GateHistoryItem,
    GateHistoryQuery, GateRuleOutcome,
};
use crate::integrations::GateCounts;
use crate::launches::{LAUNCH_SELECT, launch_row};
use crate::tm::TmError;
use crate::{Launch, Store, no_nul};

/// Evaluations the aggregates of one history window are computed from (the newest ones).
pub const HISTORY_AGGREGATE_CAP: i64 = 20_000;

/// Most branches listed in the history.
pub const HISTORY_BRANCHES: usize = 100;

/// A launch the simulation evaluates a draft gate on.
#[derive(Clone, Debug)]
pub struct SimulationLaunch {
    pub launch: Launch,
    /// Result counts of the launch; `None` when its results are gone (rolled up and deleted by
    /// retention): such a launch cannot be evaluated.
    pub counts: Option<GateCounts>,
}

type ItemRow = (Uuid, i64, Option<String>, DateTime<Utc>, String, Value);
type EvalRow = (
    Uuid,
    Option<String>,
    DateTime<Utc>,
    String,
    bool,
    Vec<Option<String>>,
);
type CountRow = (Uuid, i64, i64, i64, i64, i64, i64, i64);

/// One gate of one evaluation, as the aggregates read it.
#[derive(Clone, Debug, PartialEq)]
pub struct GateEval {
    pub launch_id: Uuid,
    pub branch: Option<String>,
    pub evaluated_at: DateTime<Utc>,
    pub gate: String,
    pub failed: bool,
    /// Metrics of the failed rules.
    pub metrics: Vec<String>,
}

/// Per-gate and per-branch aggregates of evaluations given newest first or in any order. A
/// branch counts each launch once: it failed when any of its gates did.
///
/// `flips` counts passed/failed alternations between consecutive evaluations of the same gate
/// on the same branch (evaluations without a branch form one sequence). `mean_seconds_to_green`
/// is the mean time from the first failed evaluation of a streak to the next passed one on the
/// same branch; a streak that is still red does not count.
pub fn aggregate(evals: &[GateEval]) -> (Vec<GateHistoryGate>, Vec<GateHistoryBranch>) {
    let mut sorted: Vec<&GateEval> = evals.iter().collect();
    sorted.sort_by(|a, b| {
        a.evaluated_at
            .cmp(&b.evaluated_at)
            .then_with(|| a.gate.cmp(&b.gate))
    });

    #[derive(Default)]
    struct G {
        evaluations: i64,
        failed: i64,
        flips: i64,
        green_secs: Vec<f64>,
        metrics: BTreeMap<String, i64>,
        // branch -> (last failed?, start of the current red streak)
        last: BTreeMap<Option<String>, (bool, Option<DateTime<Utc>>)>,
    }
    let mut gates: BTreeMap<&str, G> = BTreeMap::new();
    let mut launches: BTreeMap<Uuid, (Option<&str>, bool)> = BTreeMap::new();
    for e in sorted {
        let g = gates.entry(e.gate.as_str()).or_default();
        g.evaluations += 1;
        if e.failed {
            g.failed += 1;
            for m in &e.metrics {
                *g.metrics.entry(m.clone()).or_default() += 1;
            }
        }
        match g.last.get_mut(&e.branch) {
            None => {
                g.last.insert(
                    e.branch.clone(),
                    (e.failed, e.failed.then_some(e.evaluated_at)),
                );
            }
            Some((was_failed, since)) => {
                if *was_failed != e.failed {
                    g.flips += 1;
                }
                if e.failed && !*was_failed {
                    *since = Some(e.evaluated_at);
                }
                if !e.failed
                    && *was_failed
                    && let Some(t) = since.take()
                {
                    g.green_secs
                        .push((e.evaluated_at - t).num_milliseconds().max(0) as f64 / 1000.0);
                }
                *was_failed = e.failed;
            }
        }
        let l = launches
            .entry(e.launch_id)
            .or_insert((e.branch.as_deref(), false));
        l.1 |= e.failed;
    }
    let mut branches: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for (branch, failed) in launches.into_values() {
        if let Some(b) = branch {
            let c = branches.entry(b).or_default();
            c.0 += 1;
            c.1 += i64::from(failed);
        }
    }
    let gates = gates
        .into_iter()
        .map(|(name, g)| {
            let top = g
                .metrics
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map(|(m, _)| m.clone());
            GateHistoryGate {
                gate: name.to_string(),
                evaluations: g.evaluations,
                failed: g.failed,
                flips: g.flips,
                mean_seconds_to_green: (!g.green_secs.is_empty()).then(|| {
                    let m = g.green_secs.iter().sum::<f64>() / g.green_secs.len() as f64;
                    (m * 1000.0).round() / 1000.0
                }),
                top_failing_metric: top,
            }
        })
        .collect();
    let mut branches: Vec<GateHistoryBranch> = branches
        .into_iter()
        .map(|(b, (evaluations, failed))| GateHistoryBranch {
            branch: b.to_string(),
            evaluations,
            failed,
        })
        .collect();
    branches.sort_by(|a, b| {
        b.evaluations
            .cmp(&a.evaluations)
            .then_with(|| a.branch.cmp(&b.branch))
    });
    branches.truncate(HISTORY_BRANCHES);
    (gates, branches)
}

/// The failed rules of a stored gate report, of one gate when `gate` is given.
pub fn failed_outcomes(report: &Value, gate: Option<&str>) -> Vec<GateRuleOutcome> {
    report
        .get("results")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter(|r| r.get("ok").and_then(Value::as_bool) != Some(true))
                .filter(|r| gate.is_none_or(|g| r.get("gate").and_then(Value::as_str) == Some(g)))
                .filter_map(|r| {
                    Some(GateRuleOutcome {
                        gate: r.get("gate")?.as_str()?.to_string(),
                        metric: r.get("rule")?.get("metric")?.as_str()?.to_string(),
                        actual: r.get("actual").and_then(Value::as_f64),
                        expected: r.get("expected").and_then(Value::as_f64).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn cursor(at: DateTime<Utc>, id: Uuid) -> String {
    format!("{}_{}", at.timestamp_micros(), id.simple())
}

fn parse_cursor(s: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let (t, id) = s.split_once('_')?;
    let micros: i64 = t.parse().ok()?;
    Some((
        Utc.timestamp_micros(micros).single()?,
        Uuid::parse_str(id).ok()?,
    ))
}

const FILTER: &str = "g.project_id = $1 and g.state = 'done' and g.status in ('passed', 'failed') \
    and g.evaluated_at >= $2 and g.evaluated_at <= $3 \
    and ($4::text is null or l.branch = $4) \
    and jsonb_typeof(g.report->'results') = 'array' and jsonb_array_length(g.report->'results') > 0 \
    and ($5::text is null or exists (select 1 from jsonb_array_elements(g.report->'results') e \
      where e->>'gate' = $5))";

impl Store {
    /// The gate evaluations of a project in a window (default: the last 30 days up to now,
    /// callers cap the window at 365 days), newest first, with the aggregates of the whole
    /// window on the first page (later pages, `before` set, carry none). Evaluations of
    /// launches without branch count in the gate totals but not in `branches`.
    pub async fn gate_history(
        &self,
        project_id: Uuid,
        q: &GateHistoryQuery,
    ) -> AnalysisResult<GateHistory> {
        let to = q.to.unwrap_or_else(Utc::now);
        let from = q.from.unwrap_or(to - chrono::Duration::days(30));
        let limit = q.limit.clamp(1, 500);
        let after = match q.before.as_deref() {
            Some(b) => Some(
                parse_cursor(b).ok_or_else(|| TmError::bad_request("before is not a cursor"))?,
            ),
            None => None,
        };
        let branch = q.branch.as_deref().map(no_nul);
        let gate = q.gate.as_deref().map(no_nul);

        let rows: Vec<ItemRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select g.launch_id, l.number, l.branch, g.evaluated_at, g.status, g.report \
                 from launch_gates g join launches l on l.id = g.launch_id \
                 where {FILTER} \
                   and ($6::timestamptz is null or (g.evaluated_at, g.launch_id) < ($6::timestamptz, $7::uuid)) \
                 order by g.evaluated_at desc, g.launch_id desc limit $8"
            )))
            .bind(project_id)
            .bind(from)
            .bind(to)
            .bind(&branch)
            .bind(&gate)
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1).unwrap_or(Uuid::nil()))
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await?;
        let next = (rows.len() as i64 > limit)
            .then(|| rows.get(limit as usize - 1).map(|r| cursor(r.3, r.0)))
            .flatten();
        let items = rows
            .into_iter()
            .take(limit as usize)
            .map(|(launch_id, number, branch, at, status, report)| {
                let failed = failed_outcomes(&report, gate.as_deref());
                let status = if gate.is_some() {
                    if failed.is_empty() {
                        "passed"
                    } else {
                        "failed"
                    }
                    .to_string()
                } else {
                    status
                };
                GateHistoryItem {
                    launch_id,
                    launch_number: number,
                    branch,
                    evaluated_at: at,
                    status,
                    failed,
                }
            })
            .collect();

        let (gates, branches) = if after.is_some() {
            (Vec::new(), Vec::new())
        } else {
            let rows: Vec<EvalRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "with ev as (select g.launch_id, l.branch, g.evaluated_at, g.report \
                       from launch_gates g join launches l on l.id = g.launch_id \
                       where {FILTER} order by g.evaluated_at desc, g.launch_id desc limit $6) \
                     select ev.launch_id, ev.branch, ev.evaluated_at, r->>'gate', \
                       not bool_and(coalesce((r->>'ok')::boolean, false)), \
                       coalesce(array_agg(r->'rule'->>'metric') \
                         filter (where not coalesce((r->>'ok')::boolean, false)), '{{}}') \
                     from ev cross join lateral jsonb_array_elements(ev.report->'results') r \
                     where ($5::text is null or r->>'gate' = $5) and r->>'gate' is not null \
                     group by ev.launch_id, ev.branch, ev.evaluated_at, r->>'gate'"
            )))
            .bind(project_id)
            .bind(from)
            .bind(to)
            .bind(&branch)
            .bind(&gate)
            .bind(HISTORY_AGGREGATE_CAP)
            .fetch_all(&self.pool)
            .await?;
            let evals: Vec<GateEval> = rows
                .into_iter()
                .map(
                    |(launch_id, branch, evaluated_at, gate, failed, metrics)| GateEval {
                        launch_id,
                        branch,
                        evaluated_at,
                        gate,
                        failed,
                        metrics: metrics.into_iter().flatten().collect(),
                    },
                )
                .collect();
            aggregate(&evals)
        };
        Ok(GateHistory {
            items,
            next,
            gates,
            branches,
        })
    }

    /// The newest `n` finished, non-debug launches of a project that are not merged into
    /// another one (of one `branch` when given), newest first, with their result counts.
    pub async fn simulation_inputs(
        &self,
        project_id: Uuid,
        branch: Option<&str>,
        n: i64,
    ) -> anyhow::Result<Vec<SimulationLaunch>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{LAUNCH_SELECT} where l.project_id = $1 and l.status <> 'running' and not l.debug \
             and l.merged_into is null and ($2::text is null or l.branch = $2) \
             order by l.created_at desc, l.number desc limit $3"
        )))
        .bind(project_id)
        .bind(branch.map(no_nul))
        .bind(n.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?;
        let launches: Vec<Launch> = rows
            .iter()
            .map(launch_row)
            .collect::<Result<_, sqlx::Error>>()?;
        let ids: Vec<Uuid> = launches.iter().map(|l| l.id).collect();
        let created: Vec<DateTime<Utc>> = launches.iter().map(|l| l.created_at).collect();
        let counts: Vec<CountRow> = sqlx::query_as(
            "select r.launch_id, count(*), count(*) filter (where r.status = 'passed'), \
               count(*) filter (where r.status = 'failed' and not r.muted), \
               count(*) filter (where r.status = 'broken' and not r.muted), \
               count(*) filter (where r.status = 'skipped'), count(*) filter (where r.flaky_in_run), \
               count(*) filter (where r.status in ('failed', 'broken') and not r.muted \
                 and coalesce(r.defect->>'group', 'toInvestigate') = 'toInvestigate') \
             from results r join unnest($1::uuid[], $2::timestamptz[]) as t(id, created) \
               on r.launch_id = t.id and r.launch_created_at = t.created \
             group by r.launch_id",
        )
        .bind(&ids)
        .bind(&created)
        .fetch_all(&self.pool)
        .await?;
        let counts: BTreeMap<Uuid, GateCounts> = counts
            .into_iter()
            .map(|r| {
                (
                    r.0,
                    GateCounts {
                        total: r.1,
                        passed: r.2,
                        failed: r.3,
                        broken: r.4,
                        skipped: r.5,
                        flaky: r.6,
                        to_investigate: r.7,
                    },
                )
            })
            .collect();
        Ok(launches
            .into_iter()
            .map(|l| {
                let c = counts.get(&l.id).copied();
                let counts = match c {
                    Some(c) => Some(c),
                    None if l.counts.total == 0 => Some(GateCounts::default()),
                    None => None,
                };
                SimulationLaunch { launch: l, counts }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(min: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + min * 60, 0).unwrap()
    }

    fn ev(branch: &str, min: i64, gate: &str, failed: bool, metrics: &[&str]) -> GateEval {
        GateEval {
            launch_id: Uuid::from_u128(((min as u128) << 8) | u128::from(branch.as_bytes()[0])),
            branch: Some(branch.to_string()),
            evaluated_at: at(min),
            gate: gate.to_string(),
            failed,
            metrics: metrics.iter().map(|m| m.to_string()).collect(),
        }
    }

    #[test]
    fn flips_time_to_green_and_top_metric_on_a_fixed_sequence() {
        // main: pass, fail (t=10), fail, pass (t=40), fail (t=50), pass (t=55), fail (t=70, open)
        // dev: fail (t=5), pass (t=7)
        let evals = vec![
            ev("main", 0, "g", false, &[]),
            ev("main", 10, "g", true, &["failed"]),
            ev("main", 20, "g", true, &["failed", "newErrors"]),
            ev("main", 40, "g", false, &[]),
            ev("main", 50, "g", true, &["newErrors"]),
            ev("main", 55, "g", false, &[]),
            ev("main", 70, "g", true, &["failed"]),
            ev("dev", 5, "g", true, &["passRate"]),
            ev("dev", 7, "g", false, &[]),
            ev("main", 1, "other", false, &[]),
        ];
        let mut shuffled = evals.clone();
        shuffled.reverse();
        let (gates, branches) = aggregate(&shuffled);
        assert_eq!(gates.len(), 2);
        let g = &gates[0];
        assert_eq!(g.gate, "g");
        assert_eq!((g.evaluations, g.failed), (9, 5));
        // main: pass>fail, fail>pass, pass>fail, fail>pass, pass>fail = 5; dev: 1
        assert_eq!(g.flips, 6);
        // main: 10 -> 40 = 1800 s, 50 -> 55 = 300 s; dev: 5 -> 7 = 120 s; mean 740 s
        assert_eq!(g.mean_seconds_to_green, Some(740.0));
        assert_eq!(g.top_failing_metric.as_deref(), Some("failed"));
        assert_eq!(gates[1].gate, "other");
        assert_eq!(gates[1].mean_seconds_to_green, None);
        assert_eq!(gates[1].top_failing_metric, None);
        assert_eq!(
            branches,
            vec![
                GateHistoryBranch {
                    branch: "main".into(),
                    evaluations: 8,
                    failed: 4
                },
                GateHistoryBranch {
                    branch: "dev".into(),
                    evaluations: 2,
                    failed: 1
                },
            ]
        );
    }

    #[test]
    fn a_tie_of_failing_metrics_picks_the_first_name() {
        let evals = vec![
            ev("main", 0, "g", true, &["total"]),
            ev("main", 1, "g", true, &["failed"]),
        ];
        let (gates, _) = aggregate(&evals);
        assert_eq!(gates[0].top_failing_metric.as_deref(), Some("failed"));
    }

    #[test]
    fn failed_outcomes_read_a_stored_report() {
        let report = json!({"status": "failed", "results": [
            {"gate": "a", "rule": {"metric": "failed", "op": "lte", "value": 0}, "actual": 3.0, "expected": 0.0, "ok": false},
            {"gate": "a", "rule": {"metric": "total", "op": "gte", "value": 1}, "actual": 9.0, "expected": 1.0, "ok": true},
            {"gate": "b", "rule": {"metric": "newErrors", "op": "lte", "value": 0}, "actual": null, "expected": 0.0, "ok": false},
        ]});
        assert_eq!(failed_outcomes(&report, None).len(), 2);
        let only_b = failed_outcomes(&report, Some("b"));
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].metric, "newErrors");
        assert_eq!(only_b[0].actual, None);
        assert!(failed_outcomes(&json!({}), None).is_empty());
    }

    #[test]
    fn cursors_round_trip() {
        let t = at(3);
        let id = Uuid::now_v7();
        assert_eq!(parse_cursor(&cursor(t, id)), Some((t, id)));
        assert_eq!(parse_cursor("x"), None);
        assert_eq!(parse_cursor("1_zz"), None);
    }
}
