//! Synthetic failure families and triage decisions for the backtest (contract §7.3).
//!
//! A deterministic generator (fixed seeds, no randomness from the system) of launches whose
//! failures come from seven families with a known cause, and of the decisions a person makes
//! on them with a configurable accuracy, mistakes included. It is only a source of
//! [`DecisionEvent`]s for the harness, like the export of a real project; nothing in the live
//! path knows about it.
//!
//! The person's model: the first time an error is triaged, the chosen group is the true one
//! with probability `triager_accuracy`, else another; later decisions on the same error follow
//! what was chosen before (people look at their earlier decision), with a small rate of
//! one-off slips and of corrections of a wrong first decision.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::DecisionEvent;

/// The built-in defect groups, in a fixed order.
pub const GROUPS: [&str; 4] = ["productBug", "automationBug", "systemIssue", "noDefect"];

/// The synthetic id of the built-in type of a group (stable across runs).
pub fn type_id(group: &str) -> Uuid {
    let n = GROUPS.iter().position(|g| *g == group).unwrap_or(0) as u128;
    Uuid::from_u128(0x5e5e_0000_0000_4000_8000_0000_0000_0001 + n)
}

/// The kinds of failures the generator makes, each with its true cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Family {
    /// A product bug with a stable fingerprint, failing its tests until fixed.
    ProductBug,
    /// DNS failures and 503 answers of a shared environment, in random tests.
    EnvironmentNoise,
    /// A test that times out now and then.
    TimingFlaky,
    /// Two tests that share state and fail together now and then.
    OrderDependent,
    /// One launch where a large share of the tests fail with a new infrastructure error.
    InfraBurst,
    /// A request the test's mock did not expect.
    MockUnmatched,
    /// One product bug whose message changed form, so it has two fingerprints.
    NormalizerDrift,
}

impl Family {
    pub const ALL: [Family; 7] = [
        Family::ProductBug,
        Family::EnvironmentNoise,
        Family::TimingFlaky,
        Family::OrderDependent,
        Family::InfraBurst,
        Family::MockUnmatched,
        Family::NormalizerDrift,
    ];

    /// The defect group of the family's cause.
    pub fn group(self) -> &'static str {
        match self {
            Family::ProductBug | Family::NormalizerDrift => "productBug",
            Family::TimingFlaky | Family::OrderDependent | Family::MockUnmatched => "automationBug",
            Family::EnvironmentNoise | Family::InfraBurst => "systemIssue",
        }
    }
}

/// What to generate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthProfile {
    pub name: String,
    pub seed: u64,
    pub launches: u32,
    pub tests: u32,
    /// Share of first decisions of an error that name its true group (default 0.9).
    pub triager_accuracy: f64,
    /// Chance that a later decision on a known error is a one-off slip.
    pub slip_rate: f64,
    /// Chance per later decision that a wrong first decision is corrected for good.
    pub correction_rate: f64,
    /// Chance that a failing cluster of a launch is triaged at all.
    pub triage_rate: f64,
    /// New product bugs per launch (expected).
    pub product_bugs_per_launch: f64,
    /// Share of product bugs whose message changes form during their life.
    pub drift_share: f64,
    /// Chance of an infrastructure burst per launch.
    pub burst_rate: f64,
}

impl SynthProfile {
    fn base(name: &str, launches: u32, tests: u32) -> Self {
        Self {
            name: name.into(),
            seed: 7,
            launches,
            tests,
            triager_accuracy: 0.9,
            slip_rate: 0.01,
            correction_rate: 0.03,
            triage_rate: 0.85,
            product_bugs_per_launch: 0.5,
            drift_share: 0.1,
            burst_rate: 0.05,
        }
    }

    /// The profiles of the CLI and the tests: `s` (120 launches × 200 tests), `m` (600 ×
    /// 1000), `drift` (150 × 200 with half of the product bugs changing form).
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "s" => Some(Self::base("s", 120, 200)),
            "m" => {
                let mut p = Self::base("m", 600, 1000);
                p.product_bugs_per_launch = 1.5;
                Some(p)
            }
            "drift" => {
                let mut p = Self::base("drift", 150, 200);
                p.drift_share = 0.5;
                Some(p)
            }
            _ => None,
        }
    }

    /// The same profile with another seed.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

/// An error of the synthetic data: what it looks like and its true cause.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthError {
    pub id: usize,
    pub family: Family,
    pub group: String,
    pub message: String,
    pub trace: String,
    pub fingerprint: String,
    pub message_norm: String,
    pub frames: Vec<String>,
    /// For a drifted message: the error it is the other form of.
    pub drift_of: Option<usize>,
}

/// A failed result of a synthetic launch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthResult {
    /// Index of the test (0-based).
    pub test: u32,
    pub test_case_id: Uuid,
    pub name: String,
    pub error: usize,
}

/// A synthetic launch: its time, branch and environment and its failed results (every other
/// test passed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthLaunch {
    pub index: u32,
    pub at: DateTime<Utc>,
    pub branch: String,
    pub env_key: String,
    pub failed: Vec<SynthResult>,
}

/// What the generator knows about one decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTruth {
    pub launch: u32,
    pub error: usize,
    pub family: Family,
    /// The true group.
    pub group: String,
    /// The person named the true group.
    pub correct: bool,
    /// The first decision of an error not seen before in any form.
    pub new_error: bool,
}

/// Launches, the errors with their truth, the decisions (as the export would give them) and
/// what the generator knows about each decision (same order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthData {
    pub launches: Vec<SynthLaunch>,
    pub errors: Vec<SynthError>,
    pub truth: Vec<DecisionTruth>,
    pub decisions: Vec<DecisionEvent>,
}

/// SplitMix64: small, fast and the same everywhere.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Syllables with a letter outside a–f each, so that no generated word looks like hex and
/// survives normalization unchanged.
const SYLLABLES: [&str; 48] = [
    "ka", "lo", "mi", "nu", "po", "ri", "su", "ti", "vo", "wy", "zo", "gru", "hel", "jor", "kip",
    "lum", "mor", "nix", "orp", "pyl", "qua", "ros", "sim", "tor", "ulm", "vin", "wor", "xan",
    "yul", "zim", "bro", "cly", "dru", "fli", "glo", "hun", "irk", "jas", "kel", "lix", "mun",
    "nol", "pli", "rux", "sto", "tri", "vux", "zep",
];

fn word(rng: &mut Rng) -> String {
    let n = 2 + rng.below(2);
    (0..n).map(|_| *rng.pick(&SYLLABLES)).collect()
}

fn camel(rng: &mut Rng) -> String {
    let mut w = word(rng);
    if let Some(f) = w.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    w
}

fn phrase(rng: &mut Rng, words: u64) -> String {
    (0..words).map(|_| word(rng)).collect::<Vec<_>>().join(" ")
}

fn trace_of(rng: &mut Rng, exception: &str, message: &str) -> String {
    let pkg = word(rng);
    let mut t = format!("com.example.{pkg}.{exception}: {message}\n");
    for _ in 0..3 {
        let class = camel(rng);
        let method = word(rng);
        let line = 10 + rng.below(400);
        t.push_str(&format!(
            "\tat com.example.{pkg}.{class}.{method}({class}.java:{line})\n"
        ));
    }
    t.push_str("\tat org.junit.platform.engine.Runner.run(Runner.java:42)\n");
    t
}

fn make_error(
    errors: &mut Vec<SynthError>,
    family: Family,
    message: String,
    trace: String,
    drift_of: Option<usize>,
) -> usize {
    let fp = fastproof_model::fingerprint::fingerprint(Some(&message), Some(&trace))
        .expect("message and trace are set");
    let id = errors.len();
    errors.push(SynthError {
        id,
        family,
        group: family.group().into(),
        frames: crate::frames::frames(&trace),
        message,
        trace,
        fingerprint: fp.hash,
        message_norm: fp.normalized_message,
        drift_of,
    });
    id
}

fn new_error(rng: &mut Rng, errors: &mut Vec<SynthError>, family: Family) -> usize {
    let exception = format!("{}{}Exception", camel(rng), camel(rng));
    let message = match family {
        Family::ProductBug | Family::NormalizerDrift => {
            format!("{} {} {}", camel(rng), phrase(rng, 5), 100 + rng.below(900))
        }
        Family::EnvironmentNoise => {
            if rng.chance(0.5) {
                format!(
                    "getaddrinfo ENOTFOUND {}.{}.example.com",
                    word(rng),
                    word(rng)
                )
            } else {
                format!("503 Service Unavailable from {} {}", word(rng), word(rng))
            }
        }
        Family::TimingFlaky => format!("{} {} after {} ms", camel(rng), phrase(rng, 4), 5000),
        Family::OrderDependent => format!("{} {} state {}", camel(rng), phrase(rng, 4), word(rng)),
        Family::InfraBurst => format!("{} {} node {}", camel(rng), phrase(rng, 4), word(rng)),
        Family::MockUnmatched => format!(
            "{} {} /api/{}/{}",
            camel(rng),
            phrase(rng, 3),
            word(rng),
            word(rng)
        ),
    };
    let trace = trace_of(rng, &exception, &message);
    make_error(errors, family, message, trace, None)
}

/// The other form of a drifting error: the same words with the number moved into a new
/// suffix, which the normalizer keeps apart (another fingerprint, a very similar message).
fn drift_form(rng: &mut Rng, errors: &mut Vec<SynthError>, of: usize) -> usize {
    let base = errors[of].clone();
    let message = format!(
        "{} for request {}",
        base.message
            .rsplit_once(' ')
            .map(|(a, _)| a)
            .unwrap_or(&base.message),
        word(rng)
    );
    let trace = base.trace.replacen(&base.message, &message, 1);
    make_error(errors, base.family, message, trace, Some(of))
}

struct Active {
    error: usize,
    tests: Vec<u32>,
    /// Chance to fail per launch.
    rate: f64,
    until: u32,
    /// For drifting errors: the launch from which the other form shows, and that form.
    drift: Option<(u32, usize)>,
}

const TRIAGERS: [&str; 3] = ["ann", "bob", "eve"];
const BRANCHES: [&str; 3] = ["main", "main", "release"];
const ENVIRONMENTS: [&str; 2] = ["staging", "qa"];

fn test_case(seed: u64, test: u32) -> Uuid {
    let mut r = Rng::new(seed.wrapping_mul(31).wrapping_add(test as u64));
    Uuid::from_u64_pair(r.next_u64(), (r.next_u64() & 0x0fff_ffff_ffff_ffff) | 1)
}

fn wrong_group(rng: &mut Rng, truth: &str) -> String {
    let others: Vec<&str> = GROUPS.iter().copied().filter(|g| *g != truth).collect();
    rng.pick(&others).to_string()
}

/// Generates the launches, errors and decisions of a profile. The same profile (and seed)
/// gives the same data.
pub fn generate(p: &SynthProfile) -> SynthData {
    let mut rng = Rng::new(p.seed);
    let start = DateTime::parse_from_rfc3339("2026-01-05T08:00:00Z")
        .expect("constant")
        .with_timezone(&Utc);
    let tests = p.tests.max(10);
    let mut errors: Vec<SynthError> = Vec::new();
    let mut active: Vec<Active> = Vec::new();
    // Errors present from the start: environment noise, flaky and order-dependent tests,
    // unmatched mock requests.
    let noise: Vec<usize> = (0..3)
        .map(|_| new_error(&mut rng, &mut errors, Family::EnvironmentNoise))
        .collect();
    let spawn = |rng: &mut Rng,
                 errors: &mut Vec<SynthError>,
                 active: &mut Vec<Active>,
                 family: Family,
                 from: u32| {
        let e = new_error(rng, errors, family);
        let (n_tests, rate, life) = match family {
            Family::ProductBug | Family::NormalizerDrift => {
                (1 + rng.below(3) as u32, 0.95, 5 + rng.below(25) as u32)
            }
            Family::TimingFlaky => (1, 0.2, 40 + rng.below(80) as u32),
            Family::OrderDependent => (2, 0.15, 40 + rng.below(80) as u32),
            Family::MockUnmatched => (1, 0.3, 10 + rng.below(30) as u32),
            Family::EnvironmentNoise | Family::InfraBurst => (0, 0.0, 0),
        };
        let first = rng.below(tests as u64) as u32;
        let tests_of: Vec<u32> = (0..n_tests).map(|i| (first + i * 7) % tests).collect();
        let drift = (family == Family::NormalizerDrift).then(|| {
            let at = from + 1 + rng.below(life.max(2) as u64 / 2) as u32;
            (at, drift_form(rng, errors, e))
        });
        active.push(Active {
            error: e,
            tests: tests_of,
            rate,
            until: from + life,
            drift,
        });
    };
    let per_kind = (tests / 50).max(2);
    for _ in 0..per_kind {
        spawn(&mut rng, &mut errors, &mut active, Family::TimingFlaky, 0);
        spawn(
            &mut rng,
            &mut errors,
            &mut active,
            Family::OrderDependent,
            0,
        );
        spawn(&mut rng, &mut errors, &mut active, Family::MockUnmatched, 0);
    }
    let mut launches = Vec::new();
    let mut truth = Vec::new();
    let mut decisions = Vec::new();
    // The group a person believes an error has (by the first form of a drifting error).
    let mut belief: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for li in 0..p.launches {
        let at = start + Duration::hours(3 * li as i64);
        // New errors of this launch.
        let mut n_new = 0;
        let mut x = p.product_bugs_per_launch;
        while x > 0.0 {
            if rng.chance(x.min(1.0)) {
                n_new += 1;
            }
            x -= 1.0;
        }
        for _ in 0..n_new {
            let fam = if rng.chance(p.drift_share) {
                Family::NormalizerDrift
            } else {
                Family::ProductBug
            };
            spawn(&mut rng, &mut errors, &mut active, fam, li);
        }
        // Flaky, order and mock errors end and new ones replace them.
        let ended: Vec<Family> = active
            .iter()
            .filter(|a| a.until < li)
            .map(|a| errors[a.error].family)
            .filter(|f| {
                matches!(
                    f,
                    Family::TimingFlaky | Family::OrderDependent | Family::MockUnmatched
                )
            })
            .collect();
        active.retain(|a| a.until >= li);
        for f in ended {
            spawn(&mut rng, &mut errors, &mut active, f, li);
        }
        let mut failed: Vec<SynthResult> = Vec::new();
        let mut taken = std::collections::BTreeSet::new();
        let mut fail = |test: u32, error: usize, failed: &mut Vec<SynthResult>| {
            if taken.insert(test) {
                failed.push(SynthResult {
                    test,
                    test_case_id: test_case(p.seed, test),
                    name: format!("test_{test:04}"),
                    error,
                });
            }
        };
        if rng.chance(p.burst_rate) {
            let e = new_error(&mut rng, &mut errors, Family::InfraBurst);
            let share = 0.2 + rng.unit() * 0.3;
            for t in 0..tests {
                if rng.chance(share) {
                    fail(t, e, &mut failed);
                }
            }
        }
        for a in &active {
            if !rng.chance(a.rate) {
                continue;
            }
            let e = match a.drift {
                Some((from, other)) if li >= from => other,
                _ => a.error,
            };
            for t in &a.tests {
                fail(*t, e, &mut failed);
            }
        }
        if rng.chance(0.3) {
            let e = *rng.pick(&noise);
            for _ in 0..1 + rng.below(5) {
                fail(rng.below(tests as u64) as u32, e, &mut failed);
            }
        }
        failed.sort_by_key(|r| r.test);
        let launch = SynthLaunch {
            index: li,
            at,
            branch: rng.pick(&BRANCHES).to_string(),
            env_key: rng.pick(&ENVIRONMENTS).to_string(),
            failed,
        };
        // The person triages the clusters of the launch (by fingerprint), in a fixed order.
        let mut clusters: std::collections::BTreeMap<usize, Vec<&SynthResult>> =
            std::collections::BTreeMap::new();
        for r in &launch.failed {
            clusters.entry(r.error).or_default().push(r);
        }
        for (k, (e, results)) in clusters.into_iter().enumerate() {
            if !rng.chance(p.triage_rate) {
                continue;
            }
            let err = &errors[e];
            let root = err.drift_of.unwrap_or(e);
            let true_group = err.group.clone();
            let (group, new_error) = match belief.get(&root).cloned() {
                None => {
                    let g = if rng.chance(p.triager_accuracy) {
                        true_group.clone()
                    } else {
                        wrong_group(&mut rng, &true_group)
                    };
                    belief.insert(root, g.clone());
                    (g, true)
                }
                Some(b) => {
                    let b = if b != true_group && rng.chance(p.correction_rate) {
                        belief.insert(root, true_group.clone());
                        true_group.clone()
                    } else {
                        b
                    };
                    if rng.chance(p.slip_rate) {
                        (wrong_group(&mut rng, &b), false)
                    } else {
                        (b, false)
                    }
                }
            };
            truth.push(DecisionTruth {
                launch: li,
                error: e,
                family: err.family,
                group: true_group.clone(),
                correct: group == true_group,
                new_error,
            });
            decisions.push(DecisionEvent {
                v: 1,
                at: at + Duration::minutes(30 + k as i64),
                fingerprint: err.fingerprint.clone(),
                message_norm: err.message_norm.clone(),
                frames: err.frames.clone(),
                test_case_id: Some(results[0].test_case_id),
                env_key: Some(launch.env_key.clone()),
                branch: Some(launch.branch.clone()),
                defect_type_id: type_id(&group),
                group,
                previous_type_id: None,
                by: rng.pick(&TRIAGERS).to_string(),
                results: results.len() as u32,
            });
        }
        launches.push(launch);
    }
    SynthData {
        launches,
        errors,
        truth,
        decisions,
    }
}

/// `n` distinct errors of mixed families (for the bench: one cluster each).
pub fn distinct_errors(seed: u64, n: usize) -> Vec<SynthError> {
    let mut rng = Rng::new(seed);
    let mut errors = Vec::with_capacity(n);
    for i in 0..n {
        let family = Family::ALL[i % (Family::ALL.len() - 1)];
        new_error(&mut rng, &mut errors, family);
    }
    errors
}

/// What the CI thresholds read from a synthetic backtest: the precision of the `same` and
/// `high` bands and the share of first decisions of new errors that got no suggestion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthScore {
    pub decisions: u64,
    pub same_suggested: u64,
    pub same_precision: f64,
    pub high_suggested: u64,
    pub high_precision: f64,
    pub new_errors: u64,
    pub new_without_suggestion: f64,
    /// Share of all decisions whose rank-1 suggestion named the true group.
    pub truth_precision: f64,
}

/// Runs the harness over generated data and measures it against what the generator knows.
pub fn evaluate(data: &SynthData, opts: &crate::backtest::BacktestOptions) -> SynthScore {
    let mut b = crate::backtest::Backtest::new(opts);
    let (mut new_errors, mut new_quiet, mut suggested, mut truth_ok) = (0u64, 0u64, 0u64, 0u64);
    for (ev, t) in data.decisions.iter().zip(&data.truth) {
        let top = b.step(ev);
        if t.new_error {
            new_errors += 1;
            if top.is_none() {
                new_quiet += 1;
            }
        }
        if let Some(c) = top {
            suggested += 1;
            if c.group == t.group {
                truth_ok += 1;
            }
        }
    }
    let r = b.finish();
    let ratio = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    SynthScore {
        decisions: r.decisions,
        same_suggested: r.bands[0].suggested,
        same_precision: r.bands[0].precision.unwrap_or(0.0),
        high_suggested: r.bands[1].suggested,
        high_precision: r.bands[1].precision.unwrap_or(0.0),
        new_errors,
        new_without_suggestion: ratio(new_quiet, new_errors),
        truth_precision: ratio(truth_ok, suggested),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::{BacktestOptions, run};

    #[test]
    fn the_same_profile_gives_the_same_data() {
        let p = SynthProfile::named("s").unwrap();
        let a = generate(&p);
        assert_eq!(a, generate(&p));
        assert_ne!(a.decisions, generate(&p.clone().with_seed(8)).decisions);
        assert_eq!(a.launches.len(), 120);
        assert_eq!(a.truth.len(), a.decisions.len());
        let families: std::collections::BTreeSet<Family> =
            a.errors.iter().map(|e| e.family).collect();
        assert_eq!(families.len(), 7, "{families:?}");
        // Drifted forms have another fingerprint and a similar message.
        let d = a.errors.iter().find(|e| e.drift_of.is_some()).unwrap();
        let o = &a.errors[d.drift_of.unwrap()];
        assert_ne!(d.fingerprint, o.fingerprint);
        assert!(crate::trigram::similarity(&d.message_norm, &o.message_norm) >= 0.4);
        // Mistakes are made.
        let wrong = a.truth.iter().filter(|t| !t.correct).count();
        assert!(wrong > 0 && wrong < a.truth.len() / 5, "{wrong}");
        // Decisions are in time order.
        assert!(a.decisions.windows(2).all(|w| w[0].at <= w[1].at));
        assert!(SynthProfile::named("x").is_none());
    }

    /// The CI thresholds of contract §13 "S" on profile `s` with its fixed seed: `same` ≥ 0.97,
    /// `high` ≥ 0.85, new errors without a suggestion ≥ 0.9.
    #[test]
    fn the_synthetic_thresholds_hold() {
        for name in ["s", "drift"] {
            let data = generate(&SynthProfile::named(name).unwrap());
            let s = evaluate(&data, &BacktestOptions::default());
            eprintln!("{name}: {s:?}");
            assert!(s.same_suggested >= 50, "{name}: {s:?}");
            assert!(s.same_precision >= 0.97, "{name}: {s:?}");
            assert!(s.high_suggested > 0, "{name}: {s:?}");
            assert!(s.high_precision >= 0.85, "{name}: {s:?}");
            assert!(s.new_without_suggestion >= 0.9, "{name}: {s:?}");
            let report = run(&data.decisions, &BacktestOptions::default());
            assert_eq!(report.decisions, s.decisions);
        }
    }
}
