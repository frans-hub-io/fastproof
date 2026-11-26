//! Synthetic flaky families with a known cause (contract §7.8, §13 "H"): series windows as the
//! store builds them, generated from a seed, for the recall tests of the classifiers in
//! [`crate::hints`]. Each family plants one cause in background noise (a few random failures,
//! random hosts and workers, unrelated tests running before); `stable` and `noise` plant none.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, TimeZone, Utc};
use uuid::Uuid;

use crate::hints::{Context, EnvShare, OtherSeries, Outcome, Run, classify};
use crate::model::FlakyHintKind;
use crate::synth::Rng;

/// A family of flaky series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Times out near a fixed limit that passing runs come close to.
    Timing,
    /// Fails on one host (or in one environment of the test) far more often.
    Environment,
    /// Shares state with another test: fails together with it, or after it on the same worker.
    Order,
    /// Fails with changing errors and passes on a retry of the same run.
    Retry,
    /// Fails when the grid or the cluster has trouble.
    Infra,
    /// Fails at night (UTC); needs the long ClickHouse history.
    Time,
    /// Rare unrelated failures: no hint expected.
    Stable,
    /// Frequent failures without any planted cause: no hint expected.
    Noise,
}

impl Family {
    pub const ALL: [Family; 8] = [
        Family::Timing,
        Family::Environment,
        Family::Order,
        Family::Retry,
        Family::Infra,
        Family::Time,
        Family::Stable,
        Family::Noise,
    ];

    /// The hint the family plants.
    pub fn expected(self) -> Option<FlakyHintKind> {
        match self {
            Family::Timing => Some(FlakyHintKind::Timing),
            Family::Environment => Some(FlakyHintKind::Environment),
            Family::Order => Some(FlakyHintKind::Order),
            Family::Retry => Some(FlakyHintKind::Retry),
            Family::Infra => Some(FlakyHintKind::Infra),
            Family::Time => Some(FlakyHintKind::Time),
            Family::Stable | Family::Noise => None,
        }
    }

    /// Runs in the window: the PostgreSQL maximum, or a longer ClickHouse history for `time`.
    pub fn runs(self) -> usize {
        match self {
            Family::Time => 120,
            _ => crate::hints::PG_MAX_RUNS,
        }
    }
}

/// One generated series.
#[derive(Clone, Debug)]
pub struct FlakyCase {
    pub family: Family,
    pub runs: Vec<Run>,
    pub ctx: Context,
}

const HOSTS: [&str; 3] = ["ci-1", "ci-2", "ci-3"];
const INFRA_SIGNALS: [&str; 5] = [
    "grid:session_crashed",
    "grid:browser_oom",
    "k8s:pod_evicted",
    "k8s:oom_killed",
    "ci:runner_oom",
];

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap()
}

fn launch(seed: u64, i: usize) -> Uuid {
    Uuid::from_u64_pair(0xf1a4_0000_0000_0000 | (seed & 0xffff_ffff), i as u64)
}

/// A run with the background of every family: a host, a worker, a few unrelated tests that ran
/// before it on that worker, a duration.
fn base(rng: &mut Rng, seed: u64, i: usize, n: usize, span_hours: i64) -> Run {
    let step = span_hours * 60 / n.max(1) as i64;
    let at = start() + Duration::minutes(step * i as i64 + rng.below(step.max(1) as u64) as i64);
    let host = HOSTS[rng.below(3) as usize];
    let earlier: BTreeSet<String> = (0..3).map(|_| format!("t{}", rng.below(40))).collect();
    Run {
        launch_id: launch(seed, i),
        result_id: Some(Uuid::from_u64_pair(seed, 0x1000 + i as u64)),
        at,
        outcome: Outcome::Passed,
        flaky_in_run: false,
        duration_ms: Some(800 + rng.below(1_500) as i64),
        dims: vec![("host", host.to_string())],
        fingerprints: Vec::new(),
        signals: Vec::new(),
        burst: false,
        worker: Some(format!("{host}/{}", rng.below(4))),
        earlier: earlier.into_iter().collect(),
    }
}

fn fail(r: &mut Run, rng: &mut Rng) {
    r.outcome = Outcome::Failed;
    r.fingerprints = vec![format!("err{:02}", rng.below(30))];
}

/// Generates one series of the family.
pub fn generate(family: Family, seed: u64) -> FlakyCase {
    let mut rng = Rng::new(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ family as u64);
    let n = family.runs();
    let span = if family == Family::Time {
        24 * 60
    } else {
        24 * 20
    };
    let mut ctx = Context {
        long_history: family == Family::Time,
        ..Context::default()
    };
    let mut runs: Vec<Run> = Vec::with_capacity(n);
    let variant = rng.below(2);
    let limit = 20_000 + rng.below(20) as i64 * 1_000;
    let bad_host = HOSTS[rng.below(3) as usize];
    let mut partner: BTreeSet<Uuid> = BTreeSet::new();
    for i in 0..n {
        let mut r = base(&mut rng, seed, i, n, span);
        match family {
            Family::Stable => {
                if rng.chance(0.03) {
                    fail(&mut r, &mut rng);
                }
            }
            Family::Noise => {
                if rng.chance(0.2) {
                    fail(&mut r, &mut rng);
                }
            }
            Family::Timing => {
                r.duration_ms = Some((limit as f64 * (0.55 + 0.42 * rng.unit())) as i64);
                if rng.chance(0.25) {
                    r.outcome = Outcome::Failed;
                    r.duration_ms = Some(limit + rng.below(limit as u64 / 100) as i64);
                    r.fingerprints = vec![if rng.chance(0.7) {
                        "timeout".to_string()
                    } else {
                        format!("err{:02}", rng.below(30))
                    }];
                } else if rng.chance(0.02) {
                    fail(&mut r, &mut rng);
                }
            }
            Family::Environment => {
                let host = r.dims[0].1.clone();
                let p = if variant == 1 || host != bad_host {
                    0.03
                } else {
                    0.5
                };
                if rng.chance(p) {
                    fail(&mut r, &mut rng);
                }
            }
            Family::Order => {
                if variant == 0 {
                    if rng.chance(0.25) {
                        fail(&mut r, &mut rng);
                        if rng.chance(0.9) {
                            partner.insert(r.launch_id);
                        }
                    } else if rng.chance(0.03) {
                        partner.insert(r.launch_id);
                    }
                } else {
                    let after = rng.chance(0.3);
                    if after {
                        r.earlier.push("polluter".into());
                    }
                    if rng.chance(if after { 0.85 } else { 0.02 }) {
                        fail(&mut r, &mut rng);
                    }
                }
            }
            Family::Retry => {
                if rng.chance(0.3) {
                    r.flaky_in_run = true;
                    r.fingerprints = vec![format!("race{}", rng.below(6))];
                } else if rng.chance(0.03) {
                    fail(&mut r, &mut rng);
                }
            }
            Family::Infra => {
                if rng.chance(0.3) {
                    fail(&mut r, &mut rng);
                    if rng.chance(0.7) {
                        if rng.chance(0.5) {
                            r.signals = vec![rng.pick(&INFRA_SIGNALS).to_string()];
                        } else {
                            r.burst = true;
                        }
                    }
                }
            }
            Family::Time => {
                use chrono::Timelike;
                let p = if r.at.hour() < 6 { 0.5 } else { 0.03 };
                if rng.chance(p) {
                    fail(&mut r, &mut rng);
                }
            }
        }
        runs.push(r);
    }
    match family {
        Family::Timing => {
            ctx.timeout_fingerprints.insert("timeout".into());
        }
        Family::Order if variant == 0 => ctx.co_failing.push(OtherSeries {
            key: "partner".into(),
            name: "test partner".into(),
            failed_launches: partner,
        }),
        Family::Order => {
            ctx.names
                .insert("polluter".into(), "test that leaves state behind".into());
        }
        Family::Environment if variant == 1 => {
            // The series runs in one environment; the same test elsewhere is healthy.
            let failures = (n as f64 * 0.45) as u32;
            for r in runs.iter_mut() {
                r.outcome = Outcome::Passed;
                r.fingerprints.clear();
            }
            for r in runs.iter_mut().take(failures as usize) {
                fail(r, &mut rng);
            }
            ctx.own_env = Some("staging".into());
            ctx.env_siblings = vec![
                EnvShare {
                    value: "staging".into(),
                    runs: n as u32,
                    failures,
                },
                EnvShare {
                    value: "prod".into(),
                    runs: 60 + rng.below(40) as u32,
                    failures: rng.below(3) as u32,
                },
                EnvShare {
                    value: "qa".into(),
                    runs: 30,
                    failures: rng.below(2) as u32,
                },
            ];
        }
        _ => {}
    }
    FlakyCase { family, runs, ctx }
}

/// Share of `cases` series of the family whose hints include the planted kind (for `stable`
/// and `noise`: the share without any hint).
pub fn recall(family: Family, cases: u64, seed: u64) -> f64 {
    let mut hit = 0u64;
    for k in 0..cases {
        let c = generate(family, seed.wrapping_add(k));
        let s = classify(&c.runs, &c.ctx);
        let ok = match family.expected() {
            Some(kind) => s.hints.iter().any(|h| h.kind == kind),
            None => s.hints.is_empty(),
        };
        hit += ok as u64;
    }
    hit as f64 / cases.max(1) as f64
}

/// Share of `cases` series of a flaky family whose first hint (the one the list shows first)
/// is the planted kind.
pub fn top_precision(family: Family, cases: u64, seed: u64) -> f64 {
    let Some(kind) = family.expected() else {
        return 1.0;
    };
    let mut hit = 0u64;
    for k in 0..cases {
        let c = generate(family, seed.wrapping_add(k));
        let s = classify(&c.runs, &c.ctx);
        hit += (s.hints.first().map(|h| h.kind) == Some(kind)) as u64;
    }
    hit as f64 / cases.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_series() {
        for f in Family::ALL {
            let a = generate(f, 7);
            let b = generate(f, 7);
            assert_eq!(a.runs, b.runs);
            assert_eq!(a.ctx, b.ctx);
            assert_eq!(a.runs.len(), f.runs());
        }
    }

    #[test]
    fn recall_of_every_flaky_family_is_at_least_0_8() {
        for f in Family::ALL {
            let r = recall(f, 200, 1_000);
            eprintln!("{f:?}: {r:.3}");
            if f.expected().is_some() {
                assert!(r >= 0.8, "{f:?}: recall {r}");
            }
        }
    }

    #[test]
    fn the_planted_cause_comes_first() {
        for f in Family::ALL {
            let p = top_precision(f, 200, 3_000);
            eprintln!("{f:?}: first {p:.3}");
            assert!(p >= 0.75, "{f:?}: the planted hint came first in {p}");
        }
    }

    #[test]
    fn stable_series_get_no_hint() {
        assert!(recall(Family::Stable, 200, 5_000) >= 0.95);
    }

    #[test]
    fn noise_rarely_gets_a_hint() {
        let r = recall(Family::Noise, 200, 9_000);
        assert!(
            r >= 0.7,
            "series without a cause got a hint in {:.0}%",
            (1.0 - r) * 100.0
        );
    }

    #[test]
    fn below_the_minimum_no_family_gets_its_hint() {
        for f in Family::ALL {
            let Some(kind) = f.expected() else { continue };
            for seed in 0..50 {
                let mut c = generate(f, seed);
                match kind {
                    FlakyHintKind::Timing => {
                        let mut seen = 0;
                        for r in c.runs.iter_mut() {
                            if r.failing() {
                                seen += 1;
                                if seen >= crate::hints::TIMING_MIN_FAILURES {
                                    r.outcome = Outcome::Passed;
                                    r.flaky_in_run = false;
                                }
                            }
                        }
                    }
                    FlakyHintKind::Order => c.runs.truncate(crate::hints::ORDER_MIN_RUNS - 1),
                    FlakyHintKind::Time => c.runs.truncate(crate::hints::TIME_MIN_RUNS - 1),
                    FlakyHintKind::Retry => {
                        let mut seen = 0;
                        for r in c.runs.iter_mut().filter(|r| r.flaky_in_run) {
                            seen += 1;
                            if seen >= crate::hints::RETRY_MIN_RUNS {
                                r.flaky_in_run = false;
                            }
                        }
                    }
                    FlakyHintKind::Infra => {
                        let mut seen = 0;
                        for r in c.runs.iter_mut() {
                            if r.burst || !r.signals.is_empty() {
                                seen += 1;
                                if seen >= crate::hints::INFRA_MIN_MATCHES {
                                    r.burst = false;
                                    r.signals.clear();
                                }
                            }
                        }
                    }
                    FlakyHintKind::Environment => {
                        // At most four runs on every host and in every environment.
                        let mut per: std::collections::BTreeMap<String, usize> = Default::default();
                        c.runs.retain(|r| {
                            let n = per.entry(r.dims[0].1.clone()).or_default();
                            *n += 1;
                            *n < crate::hints::ENV_MIN_RUNS
                        });
                        for e in c.ctx.env_siblings.iter_mut() {
                            e.runs = e.runs.min(crate::hints::ENV_MIN_RUNS as u32 - 1);
                            e.failures = e.failures.min(e.runs);
                        }
                    }
                }
                let s = classify(&c.runs, &c.ctx);
                assert!(
                    !s.hints.iter().any(|h| h.kind == kind),
                    "{f:?} seed {seed}: {kind:?} below its minimum: {:?}",
                    s.hints
                );
            }
        }
    }
}
