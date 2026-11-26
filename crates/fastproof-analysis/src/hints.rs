//! Flaky-cause classifiers (contract §7.8, design `75` §7): from the recent runs of one series
//! (a test in one environment) and a little context about the launches they ran in, the likely
//! causes of its flakiness. Six kinds, each with the minimum data below which it never speaks:
//!
//! | Kind | Evidence | Minimum |
//! |---|---|---|
//! | `timing` | failures are timeouts, or stop at a steady duration ceiling (± 5 %) above where passing runs usually end, with their p95 at ≥ 70 % of it | 5 failures |
//! | `environment` | one host, CI runner or environment fails ≥ 3× as often as the others, Fisher p < 0.05 | 5 runs per value |
//! | `order` | fails together with another test (Jaccard ≥ 0.6) or only when a test ran earlier on the same worker | 10 runs |
//! | `retry` | fails, then passes on a retry of the same run, with different errors | 3 such runs |
//! | `infra` | failures coincide with a burst of infrastructure errors in the launch or with grid, CI, Kubernetes or mock signals | 2 matches |
//! | `time` | fails more often at some hours or weekdays (UTC) | ClickHouse only, 30 runs |
//!
//! A hint suggests actions (a mute with an end, an issue, a rule draft); it never changes
//! anything by itself. This is a transparent rule-based search over the evidence shown with
//! each hint, not a learned model.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Timelike, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::fisher;
use crate::model::{FlakyHintKind, HintConfidence};

/// Runs of a series in the PostgreSQL window, at most.
pub const PG_MAX_RUNS: usize = 50;
/// `timing`: failures needed.
pub const TIMING_MIN_FAILURES: usize = 5;
/// `timing`: a failure is "at the ceiling" within this share of it.
pub const CEILING_TOLERANCE: f64 = 0.05;
/// `timing`: share of the failures at the ceiling (or with a timeout message) that names it.
pub const TIMING_MIN_SHARE: f64 = 0.6;
/// `timing`: the p95 of passing runs reaches this share of the ceiling.
pub const PASS_P95_OF_CEILING: f64 = 0.7;
/// `environment` and `time`: runs per value (and among the other values).
pub const ENV_MIN_RUNS: usize = 5;
/// `environment` and `time`: the value fails at least this many times as often as the others.
pub const ENV_MIN_RATIO: f64 = 3.0;
/// `environment` and `time`: Fisher's two-sided p-value below this.
pub const ENV_MAX_P: f64 = 0.05;
/// `order`: runs needed.
pub const ORDER_MIN_RUNS: usize = 10;
/// `order`: Jaccard similarity of the sets of failing launches.
pub const ORDER_MIN_JACCARD: f64 = 0.6;
/// `order`: failures shared with the other test (or with the earlier test), at least.
pub const ORDER_MIN_SHARED: usize = 3;
/// `order` through a worker: share of the failures with the earlier test, at least.
pub const ORDER_WORKER_FAIL_SHARE: f64 = 0.8;
/// `order` through a worker: share of the passes with the earlier test, at most.
pub const ORDER_WORKER_PASS_SHARE: f64 = 0.2;
/// `retry`: runs that failed and then passed on a retry.
pub const RETRY_MIN_RUNS: usize = 3;
/// `infra`: failures with infrastructure evidence.
pub const INFRA_MIN_MATCHES: usize = 2;
/// `infra`: share of a launch's results that failed with infrastructure errors within
/// [`BURST_SECONDS`] of each other that makes a burst.
pub const BURST_SHARE: f64 = 0.2;
/// `infra`: width of a burst.
pub const BURST_SECONDS: i64 = 60;
/// `time`: runs needed (ClickHouse window only).
pub const TIME_MIN_RUNS: usize = 30;
/// Example runs kept in each hint's evidence.
pub const MAX_EXAMPLES: usize = 5;

/// Final outcome of a run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// `failed` or `broken`.
    Failed,
    /// Skipped or unknown: not counted.
    #[default]
    Other,
}

impl Outcome {
    pub fn of(status: &str) -> Self {
        match status {
            "passed" => Self::Passed,
            "failed" | "broken" => Self::Failed,
            _ => Self::Other,
        }
    }
}

/// One run of the series.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub launch_id: Uuid,
    /// The result (PostgreSQL window; runs only ClickHouse still has carry none).
    pub result_id: Option<Uuid>,
    pub at: DateTime<Utc>,
    pub outcome: Outcome,
    /// Failed, then passed on a retry within the run.
    pub flaky_in_run: bool,
    pub duration_ms: Option<i64>,
    /// Environment dimensions of the run (`host`, `ciRunner`, `env`), with their value.
    pub dims: Vec<(&'static str, String)>,
    /// Fingerprints of the failed attempts.
    pub fingerprints: Vec<String>,
    /// Signals matched to the run's attempts, as `source:kind`.
    pub signals: Vec<String>,
    /// The failure fell into a burst of infrastructure errors of its launch.
    pub burst: bool,
    /// The worker (host and thread) when known; `earlier` is only meaningful then.
    pub worker: Option<String>,
    /// Keys of the tests that ran earlier on the same worker in the same launch.
    pub earlier: Vec<String>,
}

impl Run {
    /// Counted in the window (it passed or failed).
    pub fn counted(&self) -> bool {
        self.outcome != Outcome::Other
    }

    /// A failure event: the run failed, or one of its attempts failed before a retry passed.
    pub fn failing(&self) -> bool {
        self.outcome == Outcome::Failed || (self.flaky_in_run && self.counted())
    }

    /// A clean pass: passed without a failed attempt.
    pub fn clean_pass(&self) -> bool {
        self.outcome == Outcome::Passed && !self.flaky_in_run
    }
}

/// Another test and the launches of the window where it failed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherSeries {
    pub key: String,
    pub name: String,
    pub failed_launches: BTreeSet<Uuid>,
}

/// Runs and failure events of the test case in one environment (its sibling series).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvShare {
    pub value: String,
    pub runs: u32,
    pub failures: u32,
}

/// What the classifiers know besides the runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// Fingerprints whose message is a timeout.
    pub timeout_fingerprints: BTreeSet<String>,
    /// Fingerprints whose message is an infrastructure error (connection refused, DNS, 502/503,
    /// out of memory, browser crashed).
    pub infra_fingerprints: BTreeSet<String>,
    /// Tests failing in the same launches as this one.
    pub co_failing: Vec<OtherSeries>,
    /// Names of the tests in [`Run::earlier`], by key.
    pub names: BTreeMap<String, String>,
    /// The environment of this series and the same test in the other environments.
    pub own_env: Option<String>,
    pub env_siblings: Vec<EnvShare>,
    /// The window comes from ClickHouse (the whole history): `time` may speak.
    pub long_history: bool,
}

/// What a hint suggests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Mute,
    Issue,
    Rule,
}

/// A likely cause.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub kind: FlakyHintKind,
    pub confidence: HintConfidence,
    pub summary: String,
    pub evidence: Map<String, Value>,
    /// The amount of data the kind's minimum applies to.
    pub sample: u32,
    pub actions: Vec<Action>,
}

/// The hints of a series with the counts shown next to them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub window_runs: u32,
    pub failures: u32,
    pub infra_failures: u32,
    pub hints: Vec<Hint>,
}

impl Summary {
    /// Share of the failures with infrastructure evidence; `None` without failures.
    pub fn infra_share(&self) -> Option<f64> {
        (self.failures > 0).then(|| round3(self.infra_failures as f64 / self.failures as f64))
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round_p(p: f64) -> f64 {
    if p == 0.0 {
        return 0.0;
    }
    let digits = 3 - p.log10().floor() as i32 - 1;
    let f = 10f64.powi(digits.clamp(0, 300));
    (p * f).round() / f
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as i64)
}

fn secs(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{} ms", ms.round() as i64)
    }
}

/// Infrastructure evidence of a failure event.
fn infra_evidence(r: &Run, ctx: &Context) -> bool {
    r.failing()
        && (r.burst
            || !r.signals.is_empty()
            || r.fingerprints
                .iter()
                .any(|f| ctx.infra_fingerprints.contains(f)))
}

fn examples<'a>(runs: impl Iterator<Item = &'a Run>) -> Value {
    let mut v: Vec<&Run> = runs.collect();
    v.sort_by_key(|r| std::cmp::Reverse(r.at));
    Value::Array(
        v.into_iter()
            .take(MAX_EXAMPLES)
            .map(|r| {
                let mut m = Map::new();
                m.insert("launchId".into(), json!(r.launch_id));
                if let Some(id) = r.result_id {
                    m.insert("resultId".into(), json!(id));
                }
                m.insert("at".into(), json!(r.at));
                m.insert(
                    "status".into(),
                    json!(match (r.outcome, r.flaky_in_run) {
                        (Outcome::Failed, _) => "failed",
                        (Outcome::Passed, true) => "passedOnRetry",
                        (Outcome::Passed, false) => "passed",
                        (Outcome::Other, _) => "other",
                    }),
                );
                Value::Object(m)
            })
            .collect(),
    )
}

/// Percentile (nearest rank) of sorted values.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

fn timing(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    let failing: Vec<&Run> = runs.iter().copied().filter(|r| r.failing()).collect();
    if failing.len() < TIMING_MIN_FAILURES {
        return None;
    }
    let timeouts = failing
        .iter()
        .filter(|r| {
            r.fingerprints
                .iter()
                .any(|f| ctx.timeout_fingerprints.contains(f))
        })
        .count();
    let mut fail_ms: Vec<f64> = runs
        .iter()
        .filter(|r| r.outcome == Outcome::Failed)
        .filter_map(|r| r.duration_ms)
        .filter(|d| *d > 0)
        .map(|d| d as f64)
        .collect();
    fail_ms.sort_by(f64::total_cmp);
    let mut pass_ms: Vec<f64> = runs
        .iter()
        .filter(|r| r.clean_pass())
        .filter_map(|r| r.duration_ms)
        .filter(|d| *d > 0)
        .map(|d| d as f64)
        .collect();
    pass_ms.sort_by(f64::total_cmp);
    let ceiling = median(&fail_ms);
    let near = fail_ms
        .iter()
        .filter(|d| (**d - ceiling).abs() <= CEILING_TOLERANCE * ceiling)
        .count();
    let at_ceiling = fail_ms.len() >= TIMING_MIN_FAILURES
        && ceiling > 0.0
        && near as f64 >= TIMING_MIN_SHARE * fail_ms.len() as f64;
    let timeout_share = timeouts as f64 / failing.len() as f64;
    let by_message = timeout_share >= TIMING_MIN_SHARE;
    let pass_p95 = percentile(&pass_ms, 0.95);
    // A limit lies above where passing runs usually end, and the slow ones come close to it
    // (runs that all take the same time are no evidence).
    let reaches = !pass_ms.is_empty()
        && ceiling > 0.0
        && pass_p95 >= PASS_P95_OF_CEILING * ceiling
        && median(&pass_ms) < ceiling * (1.0 - CEILING_TOLERANCE);
    if !(by_message || (at_ceiling && reaches)) {
        return None;
    }
    let points = by_message as u8 + at_ceiling as u8 + reaches as u8;
    let confidence = match points {
        3 => HintConfidence::High,
        2 if failing.len() >= 10 => HintConfidence::High,
        2 => HintConfidence::Medium,
        _ => HintConfidence::Low,
    };
    let mut parts = Vec::new();
    if by_message {
        parts.push(format!(
            "{timeouts} of {} failures are timeouts",
            failing.len()
        ));
    }
    if at_ceiling {
        parts.push(format!(
            "{near} of {} failures stop at about {}",
            fail_ms.len(),
            secs(ceiling)
        ));
    }
    if reaches {
        parts.push(format!(
            "passing runs reach {} of it (p95 {})",
            pct(pass_p95 / ceiling),
            secs(pass_p95)
        ));
    }
    let mut ev = Map::new();
    ev.insert("failures".into(), json!(failing.len()));
    ev.insert("timeoutFailures".into(), json!(timeouts));
    if ceiling > 0.0 {
        ev.insert("ceilingMs".into(), json!(ceiling.round() as i64));
        ev.insert("atCeiling".into(), json!(near));
    }
    if !pass_ms.is_empty() {
        ev.insert("passP95Ms".into(), json!(pass_p95.round() as i64));
        if ceiling > 0.0 {
            ev.insert("passP95OfCeiling".into(), json!(round3(pass_p95 / ceiling)));
        }
    }
    ev.insert("examples".into(), examples(failing.iter().copied()));
    Some(Hint {
        kind: FlakyHintKind::Timing,
        confidence,
        summary: format!("Runs into a time limit: {}", parts.join("; ")),
        evidence: ev,
        sample: failing.len() as u32,
        actions: vec![Action::Mute, Action::Issue, Action::Rule],
    })
}

/// One value of a dimension against the others.
#[derive(Clone, Debug)]
struct Split {
    dim: &'static str,
    value: String,
    runs: u64,
    failures: u64,
    other_runs: u64,
    other_failures: u64,
    p: f64,
    ratio: f64,
}

fn rate(f: u64, n: u64) -> f64 {
    if n == 0 { 0.0 } else { f as f64 / n as f64 }
}

/// The value fails at least [`ENV_MIN_RATIO`] times as often as the others with Fisher p below
/// [`ENV_MAX_P`], both sides with at least [`ENV_MIN_RUNS`] runs.
fn split(dim: &'static str, value: &str, n: u64, f: u64, on: u64, of: u64) -> Option<Split> {
    if n < ENV_MIN_RUNS as u64 || on < ENV_MIN_RUNS as u64 || f == 0 {
        return None;
    }
    let (rv, ro) = (rate(f, n), rate(of, on));
    if rv < ENV_MIN_RATIO * ro {
        return None;
    }
    let p = fisher::two_sided(f, n - f, of, on - of);
    if p >= ENV_MAX_P {
        return None;
    }
    let ratio = if ro == 0.0 { f64::INFINITY } else { rv / ro };
    Some(Split {
        dim,
        value: value.to_string(),
        runs: n,
        failures: f,
        other_runs: on,
        other_failures: of,
        p,
        ratio,
    })
}

/// Every value of every dimension of the runs that splits.
fn splits(runs: &[&Run], key: impl Fn(&Run) -> Vec<(&'static str, String)>) -> Vec<Split> {
    let mut by: BTreeMap<(&'static str, String), (u64, u64)> = BTreeMap::new();
    let mut total: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    for r in runs {
        for (d, v) in key(r) {
            let e = by.entry((d, v)).or_default();
            e.0 += 1;
            e.1 += r.failing() as u64;
            let t = total.entry(d).or_default();
            t.0 += 1;
            t.1 += r.failing() as u64;
        }
    }
    let mut out = Vec::new();
    for ((d, v), (n, f)) in &by {
        let (tn, tf) = total[d];
        if let Some(s) = split(d, v, *n, *f, tn - n, tf - f) {
            out.push(s);
        }
    }
    out
}

fn best(mut v: Vec<Split>) -> Option<Split> {
    v.sort_by(|a, b| {
        a.p.total_cmp(&b.p)
            .then(b.ratio.total_cmp(&a.ratio))
            .then(a.dim.cmp(b.dim))
            .then(a.value.cmp(&b.value))
    });
    v.into_iter().next()
}

fn split_confidence(s: &Split) -> HintConfidence {
    if s.p < 0.001 {
        HintConfidence::High
    } else if s.p < 0.01 {
        HintConfidence::Medium
    } else {
        HintConfidence::Low
    }
}

fn split_evidence(s: &Split) -> Map<String, Value> {
    let mut ev = Map::new();
    ev.insert("dimension".into(), json!(s.dim));
    ev.insert("value".into(), json!(s.value));
    ev.insert("runs".into(), json!(s.runs));
    ev.insert("failures".into(), json!(s.failures));
    ev.insert("failRate".into(), json!(round3(rate(s.failures, s.runs))));
    ev.insert("otherRuns".into(), json!(s.other_runs));
    ev.insert("otherFailures".into(), json!(s.other_failures));
    ev.insert(
        "otherFailRate".into(),
        json!(round3(rate(s.other_failures, s.other_runs))),
    );
    if s.ratio.is_finite() {
        ev.insert("ratio".into(), json!(round3(s.ratio)));
    }
    ev.insert("p".into(), json!(round_p(s.p)));
    ev
}

fn dim_label(d: &str) -> &str {
    match d {
        "host" => "host",
        "ciRunner" => "CI runner",
        "env" => "environment",
        "hour" => "hours (UTC)",
        "weekday" => "weekday",
        other => other,
    }
}

fn environment(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    let mut found = splits(runs, |r| {
        r.dims
            .iter()
            .filter(|(d, _)| *d != "env")
            .map(|(d, v)| (*d, v.clone()))
            .collect()
    });
    // The series is one environment: compare it with the same test in the others.
    if let Some(own) = &ctx.own_env
        && let Some(mine) = ctx.env_siblings.iter().find(|e| &e.value == own)
    {
        let (on, of) = ctx
            .env_siblings
            .iter()
            .filter(|e| &e.value != own)
            .fold((0u64, 0u64), |a, e| {
                (a.0 + e.runs as u64, a.1 + e.failures as u64)
            });
        if let Some(s) = split("env", own, mine.runs as u64, mine.failures as u64, on, of) {
            found.push(s);
        }
    }
    let s = best(found)?;
    let mut ev = split_evidence(&s);
    let matching: Vec<&Run> = runs
        .iter()
        .copied()
        .filter(|r| r.failing() && r.dims.iter().any(|(d, v)| *d == s.dim && *v == s.value))
        .collect();
    ev.insert("examples".into(), examples(matching.into_iter()));
    Some(Hint {
        kind: FlakyHintKind::Environment,
        confidence: split_confidence(&s),
        summary: format!(
            "Fails on {} {}: {} of {} runs there against {} of {} elsewhere (p = {})",
            dim_label(s.dim),
            s.value,
            s.failures,
            s.runs,
            s.other_failures,
            s.other_runs,
            round_p(s.p)
        ),
        evidence: ev,
        sample: s.runs.min(s.other_runs) as u32,
        actions: vec![Action::Mute, Action::Issue],
    })
}

fn order(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    if runs.len() < ORDER_MIN_RUNS {
        return None;
    }
    let mine: BTreeSet<Uuid> = runs
        .iter()
        .filter(|r| r.failing() && !r.burst)
        .map(|r| r.launch_id)
        .collect();
    let mut co: Option<(f64, usize, &OtherSeries)> = None;
    if mine.len() >= ORDER_MIN_SHARED {
        for o in &ctx.co_failing {
            let shared = mine.intersection(&o.failed_launches).count();
            let union = mine.union(&o.failed_launches).count();
            if shared < ORDER_MIN_SHARED || union == 0 {
                continue;
            }
            let j = shared as f64 / union as f64;
            if j < ORDER_MIN_JACCARD {
                continue;
            }
            let better = match co {
                None => true,
                Some((bj, bs, b)) => {
                    j > bj || (j == bj && (shared > bs || (shared == bs && o.key < b.key)))
                }
            };
            if better {
                co = Some((j, shared, o));
            }
        }
    }
    if let Some((j, shared, o)) = co {
        let confidence = if j >= 0.8 && shared >= 5 {
            HintConfidence::High
        } else if j >= 0.7 || shared >= 5 {
            HintConfidence::Medium
        } else {
            HintConfidence::Low
        };
        let mut ev = Map::new();
        ev.insert("via".into(), json!("coFailure"));
        ev.insert("test".into(), json!(o.key));
        ev.insert("name".into(), json!(o.name));
        ev.insert("jaccard".into(), json!(round3(j)));
        ev.insert("shared".into(), json!(shared));
        ev.insert("failures".into(), json!(mine.len()));
        ev.insert("otherFailures".into(), json!(o.failed_launches.len()));
        ev.insert(
            "examples".into(),
            examples(
                runs.iter()
                    .copied()
                    .filter(|r| r.failing() && o.failed_launches.contains(&r.launch_id)),
            ),
        );
        return Some(Hint {
            kind: FlakyHintKind::Order,
            confidence,
            summary: format!(
                "Fails together with {}: {shared} shared failing launches, Jaccard {:.2}",
                o.name, j
            ),
            evidence: ev,
            sample: runs.len() as u32,
            actions: vec![Action::Mute, Action::Issue],
        });
    }
    worker_order(runs, ctx)
}

fn worker_order(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    let known: Vec<&Run> = runs
        .iter()
        .copied()
        .filter(|r| r.worker.is_some())
        .collect();
    if known.len() < ORDER_MIN_RUNS {
        return None;
    }
    let fails: Vec<&Run> = known.iter().copied().filter(|r| r.failing()).collect();
    let passes: Vec<&Run> = known.iter().copied().filter(|r| r.clean_pass()).collect();
    if fails.len() < ORDER_MIN_SHARED || passes.is_empty() {
        return None;
    }
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    for r in &fails {
        keys.extend(r.earlier.iter().map(String::as_str));
    }
    let mut best: Option<(f64, usize, usize, &str)> = None;
    for k in keys {
        let fw = fails
            .iter()
            .filter(|r| r.earlier.iter().any(|e| e == k))
            .count();
        let pw = passes
            .iter()
            .filter(|r| r.earlier.iter().any(|e| e == k))
            .count();
        if fw < ORDER_MIN_SHARED
            || (fw as f64) < ORDER_WORKER_FAIL_SHARE * fails.len() as f64
            || (pw as f64) > ORDER_WORKER_PASS_SHARE * passes.len() as f64
        {
            continue;
        }
        let p = fisher::two_sided(
            fw as u64,
            pw as u64,
            (fails.len() - fw) as u64,
            (passes.len() - pw) as u64,
        );
        if p >= ENV_MAX_P {
            continue;
        }
        let better = match best {
            None => true,
            Some((bp, bfw, _, bk)) => p < bp || (p == bp && (fw > bfw || (fw == bfw && k < bk))),
        };
        if better {
            best = Some((p, fw, pw, k));
        }
    }
    let (p, fw, pw, k) = best?;
    let name = ctx.names.get(k).cloned().unwrap_or_else(|| k.to_string());
    let mut ev = Map::new();
    ev.insert("via".into(), json!("worker"));
    ev.insert("test".into(), json!(k));
    ev.insert("name".into(), json!(name));
    ev.insert("failuresWith".into(), json!(fw));
    ev.insert("failures".into(), json!(fails.len()));
    ev.insert("passesWith".into(), json!(pw));
    ev.insert("passes".into(), json!(passes.len()));
    ev.insert("p".into(), json!(round_p(p)));
    ev.insert(
        "examples".into(),
        examples(
            fails
                .iter()
                .copied()
                .filter(|r| r.earlier.iter().any(|e| e == k)),
        ),
    );
    Some(Hint {
        kind: FlakyHintKind::Order,
        confidence: if p < 0.001 {
            HintConfidence::High
        } else if p < 0.01 {
            HintConfidence::Medium
        } else {
            HintConfidence::Low
        },
        summary: format!(
            "Fails after {name} ran earlier on the same worker: {fw} of {} failures against {pw} of {} passes",
            fails.len(),
            passes.len()
        ),
        evidence: ev,
        sample: known.len() as u32,
        actions: vec![Action::Mute, Action::Issue],
    })
}

fn retry(runs: &[&Run]) -> Option<Hint> {
    let retried: Vec<&Run> = runs
        .iter()
        .copied()
        .filter(|r| r.flaky_in_run && r.outcome == Outcome::Passed)
        .collect();
    if retried.len() < RETRY_MIN_RUNS {
        return None;
    }
    let mut fps: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &retried {
        for f in &r.fingerprints {
            *fps.entry(f.as_str()).or_default() += 1;
        }
    }
    // One stable error that goes away on retry is a known failure (timing, infra), not chance.
    if fps.len() == 1 {
        return None;
    }
    let failing = runs.iter().filter(|r| r.failing()).count().max(1);
    let share = retried.len() as f64 / failing as f64;
    let confidence = if fps.is_empty() {
        HintConfidence::Low
    } else if retried.len() >= 5 && share >= 0.6 {
        HintConfidence::High
    } else if retried.len() >= 4 || share >= 0.5 {
        HintConfidence::Medium
    } else {
        HintConfidence::Low
    };
    let mut top: Vec<(&str, usize)> = fps.iter().map(|(k, v)| (*k, *v)).collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut ev = Map::new();
    ev.insert("retriedRuns".into(), json!(retried.len()));
    ev.insert("failures".into(), json!(failing));
    ev.insert("share".into(), json!(round3(share)));
    ev.insert("distinctErrors".into(), json!(fps.len()));
    ev.insert(
        "fingerprints".into(),
        json!(top.iter().take(3).map(|(k, _)| *k).collect::<Vec<_>>()),
    );
    ev.insert("examples".into(), examples(retried.iter().copied()));
    let errors = if fps.is_empty() {
        String::new()
    } else {
        format!(" with {} different errors", fps.len())
    };
    Some(Hint {
        kind: FlakyHintKind::Retry,
        confidence,
        summary: format!(
            "Passes on a retry of the same run in {} of {failing} failing runs{errors}",
            retried.len()
        ),
        evidence: ev,
        sample: retried.len() as u32,
        actions: vec![Action::Mute, Action::Issue],
    })
}

fn infra(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    let failing = runs.iter().filter(|r| r.failing()).count();
    let matched: Vec<&Run> = runs
        .iter()
        .copied()
        .filter(|r| infra_evidence(r, ctx))
        .collect();
    if matched.len() < INFRA_MIN_MATCHES {
        return None;
    }
    let share = matched.len() as f64 / failing.max(1) as f64;
    let bursts = matched.iter().filter(|r| r.burst).count();
    let mut signals: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &matched {
        for s in &r.signals {
            *signals.entry(s.as_str()).or_default() += 1;
        }
    }
    let errors = matched
        .iter()
        .filter(|r| {
            r.fingerprints
                .iter()
                .any(|f| ctx.infra_fingerprints.contains(f))
        })
        .count();
    let confidence = if matched.len() >= 4 && share >= 0.5 {
        HintConfidence::High
    } else if matched.len() >= 3 || share >= 0.5 {
        HintConfidence::Medium
    } else {
        HintConfidence::Low
    };
    let mut why = Vec::new();
    if !signals.is_empty() {
        let mut top: Vec<(&str, usize)> = signals.iter().map(|(k, v)| (*k, *v)).collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        why.push(format!(
            "signals {}",
            top.iter()
                .take(3)
                .map(|(k, n)| format!("{} ×{n}", k.replacen(':', " ", 1)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if bursts > 0 {
        why.push(format!(
            "{bursts} in a burst of infrastructure errors in the launch"
        ));
    }
    if errors > 0 {
        why.push(format!("{errors} with an infrastructure error"));
    }
    let mut ev = Map::new();
    ev.insert("matches".into(), json!(matched.len()));
    ev.insert("failures".into(), json!(failing));
    ev.insert("share".into(), json!(round3(share)));
    ev.insert("bursts".into(), json!(bursts));
    ev.insert("infraErrors".into(), json!(errors));
    ev.insert("signals".into(), json!(signals));
    ev.insert("examples".into(), examples(matched.iter().copied()));
    Some(Hint {
        kind: FlakyHintKind::Infra,
        confidence,
        summary: format!(
            "{} of {failing} failures coincide with infrastructure trouble ({})",
            matched.len(),
            why.join("; ")
        ),
        evidence: ev,
        sample: matched.len() as u32,
        actions: vec![Action::Issue, Action::Rule],
    })
}

/// Six-hour block of the day (UTC) and weekday of a run.
fn time_dims(r: &Run) -> Vec<(&'static str, String)> {
    let h = r.at.hour() / 6 * 6;
    vec![
        ("hour", format!("{:02}–{:02}", h, h + 6)),
        ("weekday", r.at.weekday().to_string()),
    ]
}

fn time(runs: &[&Run], ctx: &Context) -> Option<Hint> {
    if !ctx.long_history || runs.len() < TIME_MIN_RUNS {
        return None;
    }
    let s = best(splits(runs, time_dims))?;
    let mut ev = split_evidence(&s);
    ev.insert(
        "examples".into(),
        examples(runs.iter().copied().filter(|r| {
            r.failing()
                && time_dims(r)
                    .iter()
                    .any(|(d, v)| *d == s.dim && *v == s.value)
        })),
    );
    Some(Hint {
        kind: FlakyHintKind::Time,
        confidence: split_confidence(&s),
        summary: format!(
            "Fails more often on {} {}: {} of {} runs against {} of {} at other times (p = {})",
            dim_label(s.dim),
            s.value,
            s.failures,
            s.runs,
            s.other_failures,
            s.other_runs,
            round_p(s.p)
        ),
        evidence: ev,
        sample: runs.len() as u32,
        actions: vec![Action::Mute, Action::Issue],
    })
}

/// Order of the kinds when confidences tie: specific causes before the generic retry.
fn rank(k: FlakyHintKind) -> u8 {
    match k {
        FlakyHintKind::Infra => 0,
        FlakyHintKind::Environment => 1,
        FlakyHintKind::Order => 2,
        FlakyHintKind::Timing => 3,
        FlakyHintKind::Time => 4,
        FlakyHintKind::Retry => 5,
    }
}

/// The likely causes of a series' flakiness from its runs (any order) and context. Hints are
/// sorted by confidence, then from the specific causes to the generic `retry`.
pub fn classify(runs: &[Run], ctx: &Context) -> Summary {
    let counted: Vec<&Run> = runs.iter().filter(|r| r.counted()).collect();
    let failures = counted.iter().filter(|r| r.failing()).count();
    let infra_failures = counted.iter().filter(|r| infra_evidence(r, ctx)).count();
    let mut hints: Vec<Hint> = [
        timing(&counted, ctx),
        environment(&counted, ctx),
        order(&counted, ctx),
        retry(&counted),
        infra(&counted, ctx),
        time(&counted, ctx),
    ]
    .into_iter()
    .flatten()
    .collect();
    hints.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then(rank(a.kind).cmp(&rank(b.kind)))
    });
    Summary {
        window_runs: counted.len() as u32,
        failures: failures as u32,
        infra_failures: infra_failures as u32,
        hints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn run(i: u32, outcome: Outcome) -> Run {
        Run {
            launch_id: Uuid::from_u128(i as u128 + 1),
            result_id: Some(Uuid::from_u128(0x1000 + i as u128)),
            at: Utc.with_ymd_and_hms(2026, 11, 2, 10, 0, 0).unwrap()
                + chrono::Duration::hours(i as i64 * 7),
            outcome,
            ..Run::default()
        }
    }

    fn kinds(s: &Summary) -> Vec<FlakyHintKind> {
        s.hints.iter().map(|h| h.kind).collect()
    }

    #[test]
    fn timing_needs_five_failures() {
        let ctx = Context {
            timeout_fingerprints: ["t".to_string()].into(),
            ..Context::default()
        };
        let mut runs: Vec<Run> = (0..20).map(|i| run(i, Outcome::Passed)).collect();
        for r in runs.iter_mut().take(4) {
            r.outcome = Outcome::Failed;
            r.fingerprints = vec!["t".into()];
        }
        assert!(classify(&runs, &ctx).hints.is_empty());
        runs[4].outcome = Outcome::Failed;
        runs[4].fingerprints = vec!["t".into()];
        let s = classify(&runs, &ctx);
        assert_eq!(kinds(&s), vec![FlakyHintKind::Timing]);
        assert_eq!(s.hints[0].sample, 5);
        assert_eq!(s.hints[0].evidence["timeoutFailures"], 5);
    }

    #[test]
    fn timing_by_a_duration_ceiling() {
        let mut runs: Vec<Run> = (0..30).map(|i| run(i, Outcome::Passed)).collect();
        for (i, r) in runs.iter_mut().enumerate() {
            if i % 4 == 0 {
                r.outcome = Outcome::Failed;
                r.duration_ms = Some(30_000 + (i as i64 % 3) * 200);
                r.fingerprints = vec![format!("e{i}")];
            } else {
                r.duration_ms = Some(18_000 + (i as i64 * 397) % 9_000);
            }
        }
        let s = classify(&runs, &Context::default());
        assert_eq!(kinds(&s), vec![FlakyHintKind::Timing], "{s:?}");
        let ev = &s.hints[0].evidence;
        assert_eq!(ev["ceilingMs"], 30_200);
        assert!(ev["passP95OfCeiling"].as_f64().unwrap() >= 0.7);
        // Passing runs far below the ceiling: no hint from durations alone.
        for r in runs.iter_mut().filter(|r| r.outcome == Outcome::Passed) {
            r.duration_ms = Some(2_000);
        }
        assert!(classify(&runs, &Context::default()).hints.is_empty());
        // Every run takes the same time: no ceiling either.
        for r in runs.iter_mut() {
            r.duration_ms = Some(900);
        }
        assert!(classify(&runs, &Context::default()).hints.is_empty());
    }

    #[test]
    fn environment_by_host_and_by_sibling_environments() {
        let mut runs: Vec<Run> = (0..30).map(|i| run(i, Outcome::Passed)).collect();
        for (i, r) in runs.iter_mut().enumerate() {
            let host = if i % 3 == 0 { "ci-3" } else { "ci-1" };
            r.dims = vec![("host", host.to_string())];
            if (host == "ci-3" && i % 2 == 0) || i == 7 {
                r.outcome = Outcome::Failed;
            }
        }
        let s = classify(&runs, &Context::default());
        assert_eq!(kinds(&s), vec![FlakyHintKind::Environment], "{s:?}");
        assert_eq!(s.hints[0].evidence["value"], "ci-3");
        assert_eq!(s.hints[0].evidence["dimension"], "host");
        // Four runs on the host are below the minimum.
        let few: Vec<Run> = runs
            .iter()
            .filter(|r| r.dims[0].1 == "ci-1" || r.launch_id.as_u128() <= 12)
            .cloned()
            .collect();
        assert!(
            few.iter().filter(|r| r.dims[0].1 == "ci-3").count() < ENV_MIN_RUNS
                && !kinds(&classify(&few, &Context::default()))
                    .contains(&FlakyHintKind::Environment)
        );
        let ctx = Context {
            own_env: Some("staging".into()),
            env_siblings: vec![
                EnvShare {
                    value: "staging".into(),
                    runs: 20,
                    failures: 9,
                },
                EnvShare {
                    value: "prod".into(),
                    runs: 40,
                    failures: 1,
                },
                EnvShare {
                    value: "qa".into(),
                    runs: 10,
                    failures: 0,
                },
            ],
            ..Context::default()
        };
        let quiet: Vec<Run> = (0..20).map(|i| run(i, Outcome::Passed)).collect();
        let s = classify(&quiet, &ctx);
        assert_eq!(kinds(&s), vec![FlakyHintKind::Environment]);
        assert_eq!(s.hints[0].evidence["dimension"], "env");
        assert_eq!(s.hints[0].evidence["otherRuns"], 50);
    }

    #[test]
    fn order_by_co_failure_and_by_worker() {
        let mut runs: Vec<Run> = (0..12).map(|i| run(i, Outcome::Passed)).collect();
        for r in runs.iter_mut().take(4) {
            r.outcome = Outcome::Failed;
        }
        let failed: BTreeSet<Uuid> = runs.iter().take(4).map(|r| r.launch_id).collect();
        let mut other = failed.clone();
        other.insert(Uuid::from_u128(99));
        let ctx = Context {
            co_failing: vec![OtherSeries {
                key: "b".into(),
                name: "test b".into(),
                failed_launches: other,
            }],
            ..Context::default()
        };
        let s = classify(&runs, &ctx);
        assert_eq!(kinds(&s), vec![FlakyHintKind::Order], "{s:?}");
        assert_eq!(s.hints[0].evidence["jaccard"], 0.8);
        // Nine runs are below the minimum.
        assert!(classify(&runs[..9], &ctx).hints.is_empty());

        let mut runs: Vec<Run> = (0..20).map(|i| run(i, Outcome::Passed)).collect();
        for (i, r) in runs.iter_mut().enumerate() {
            r.worker = Some(format!("h1/{}", i % 4));
            r.earlier = vec![format!("x{i}")];
            if i % 4 == 1 {
                r.outcome = Outcome::Failed;
                r.earlier.push("db-reset".into());
            }
        }
        let ctx = Context {
            names: [("db-reset".to_string(), "resets the database".to_string())].into(),
            ..Context::default()
        };
        let s = classify(&runs, &ctx);
        assert_eq!(kinds(&s), vec![FlakyHintKind::Order], "{s:?}");
        assert_eq!(s.hints[0].evidence["via"], "worker");
        assert!(s.hints[0].summary.contains("resets the database"));
    }

    #[test]
    fn retry_needs_three_runs_with_different_errors() {
        let mut runs: Vec<Run> = (0..10).map(|i| run(i, Outcome::Passed)).collect();
        for (i, r) in runs.iter_mut().enumerate().take(2) {
            r.flaky_in_run = true;
            r.fingerprints = vec![format!("f{i}")];
        }
        assert!(classify(&runs, &Context::default()).hints.is_empty());
        runs[2].flaky_in_run = true;
        runs[2].fingerprints = vec!["f2".into()];
        let s = classify(&runs, &Context::default());
        assert_eq!(kinds(&s), vec![FlakyHintKind::Retry]);
        assert_eq!(s.failures, 3);
        // One stable error is not chance.
        for r in runs.iter_mut().take(3) {
            r.fingerprints = vec!["same".into()];
        }
        assert!(classify(&runs, &Context::default()).hints.is_empty());
    }

    #[test]
    fn infra_needs_two_matches_and_feeds_the_share() {
        let mut runs: Vec<Run> = (0..10).map(|i| run(i, Outcome::Passed)).collect();
        runs[0].outcome = Outcome::Failed;
        runs[0].signals = vec!["grid:session_crashed".into()];
        runs[1].outcome = Outcome::Failed;
        runs[2].outcome = Outcome::Failed;
        runs[3].outcome = Outcome::Failed;
        let s = classify(&runs, &Context::default());
        assert!(s.hints.is_empty());
        assert_eq!(
            (s.failures, s.infra_failures, s.infra_share()),
            (4, 1, Some(0.25))
        );
        runs[1].burst = true;
        let s = classify(&runs, &Context::default());
        assert_eq!(kinds(&s), vec![FlakyHintKind::Infra]);
        assert_eq!(s.infra_share(), Some(0.5));
        assert_eq!(s.hints[0].evidence["signals"]["grid:session_crashed"], 1);
        // A signal on a passing run is not a match.
        runs[1].burst = false;
        runs[4].signals = vec!["grid:node_lost".into()];
        assert!(classify(&runs, &Context::default()).hints.is_empty());
    }

    #[test]
    fn time_only_with_long_history_and_thirty_runs() {
        let start = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let mut runs: Vec<Run> = (0..40)
            .map(|i| {
                let mut r = run(i, Outcome::Passed);
                r.at = start + chrono::Duration::hours(i as i64 * 6);
                if r.at.hour() < 6 && i % 8 != 4 {
                    r.outcome = Outcome::Failed;
                }
                r
            })
            .collect();
        let ctx = Context {
            long_history: true,
            ..Context::default()
        };
        let s = classify(&runs, &ctx);
        assert!(kinds(&s).contains(&FlakyHintKind::Time), "{s:?}");
        assert!(!kinds(&classify(&runs, &Context::default())).contains(&FlakyHintKind::Time));
        runs.truncate(29);
        assert!(!kinds(&classify(&runs, &ctx)).contains(&FlakyHintKind::Time));
    }

    #[test]
    fn skipped_runs_are_not_counted_and_nothing_is_said_without_data() {
        let runs: Vec<Run> = (0..5).map(|i| run(i, Outcome::Other)).collect();
        let s = classify(&runs, &Context::default());
        assert_eq!((s.window_runs, s.failures, s.infra_share()), (0, 0, None));
        assert!(s.hints.is_empty());
        assert!(classify(&[], &Context::default()).hints.is_empty());
    }

    #[test]
    fn p_values_are_rounded_to_three_significant_digits() {
        assert_eq!(round_p(0.000_123_456), 0.000_123);
        assert_eq!(round_p(0.04567), 0.0457);
        assert_eq!(round_p(0.0), 0.0);
    }
}
