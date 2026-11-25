//! Scoring, bands and ranking of triage suggestion candidates (contract §7.2).
//!
//! A suggestion is a transparent weighted sum over what people decided before, not a learned
//! model: every contribution is a row of the table below and is shown to the user as a
//! [`WhyItem`] with its weight and a plain-language `detail`. The live path (the `suggestions`
//! step) and the backtest call the same functions, so both score identically.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::model::{
    Candidate, ClusterFacts, MAX_CANDIDATES, MemoryRow, SignalSource, SuggestionBand,
    SuggestionSignal, WhyItem, WhySignal,
};

/// The same canonical fingerprint (or one of its aliases).
pub const W_SAME_FINGERPRINT: f64 = 0.60;
/// Times the message similarity, only when the fingerprint differs.
pub const W_MESSAGE: f64 = 0.45;
/// Message similarity below this gives no candidate (when the fingerprint differs).
pub const MIN_MESSAGE_SIMILARITY: f64 = 0.40;
/// Times the Jaccard index of the stack frames.
pub const W_FRAMES: f64 = 0.20;
/// A test case of the cluster was decided this way.
pub const W_TEST_CASE: f64 = 0.10;
/// The cluster's environment was among the decided ones.
pub const W_ENVIRONMENT: f64 = 0.05;
/// The cluster's branch was among the decided ones.
pub const W_BRANCH: f64 = 0.03;
/// Per confirmation, up to [`MAX_CONFIRMED`].
pub const W_CONFIRMED: f64 = 0.03;
/// Confirmations add at most this.
pub const MAX_CONFIRMED: f64 = 0.10;
/// Per override (a person changed this decision to another type).
pub const W_OVERRIDDEN: f64 = -0.15;
/// The age factor removes at most this share of the score.
pub const MAX_AGE_PENALTY: f64 = 0.3;
/// Days over which the age penalty grows to its maximum.
pub const AGE_DAYS: f64 = 365.0;
/// Another type is remembered for the same fingerprint: both candidates lose this.
pub const W_CONFLICT: f64 = -0.10;
/// Fixed score of a rule candidate (a disabled rule, or one that did not fire, matches).
pub const W_RULE: f64 = 0.55;
/// An exact signal for a group adds this to that group's best candidate.
pub const W_SIGNAL_EXACT: f64 = 0.30;
/// A time-window signal for a group adds this to that group's best candidate.
pub const W_SIGNAL_WINDOW: f64 = 0.15;
/// The lowest score of the `high` band.
pub const HIGH_MIN: f64 = 0.70;
/// The project default of the lowest score shown (`analysis.suggest.minScore`).
pub const DEFAULT_MIN_SCORE: f64 = 0.40;
/// Confirmations that make a conflict-free same-fingerprint row `same` without a shared test.
pub const SAME_CONFIRMED: i32 = 3;

/// How candidates are cut to what a cluster shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankOptions {
    /// `analysis.suggest.minScore` (0.40 to 0.95).
    pub min_score: f64,
    /// `analysis.suggest.maxCandidates` (1 to 3).
    pub max_candidates: usize,
}

impl Default for RankOptions {
    fn default() -> Self {
        Self {
            min_score: DEFAULT_MIN_SCORE,
            max_candidates: MAX_CANDIDATES,
        }
    }
}

/// A signal about the cluster's results, as the boost row of the table reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalFact {
    pub source: SignalSource,
    pub kind: String,
    /// `exact` (matched by trace id or attempt) rather than a time window.
    pub exact: bool,
}

/// The defect group a signal points to: infrastructure signals (grid, CI, Kubernetes) to
/// `systemIssue`, a mock's unmatched request or stub error to `automationBug`, a fault the mock
/// injected on purpose to `noDefect`.
pub fn signal_group(s: &SignalFact) -> Option<&'static str> {
    match s.source {
        SignalSource::Grid | SignalSource::Ci | SignalSource::K8s => Some("systemIssue"),
        SignalSource::Mock => match s.kind.as_str() {
            "unmatched" | "stub_error" => Some("automationBug"),
            "injected_fault" => Some("noDefect"),
            _ => None,
        },
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn clamp(x: f64) -> f64 {
    round3(x.clamp(0.0, 1.0))
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as i64)
}

fn times(n: i32) -> String {
    if n == 1 {
        "1 time".into()
    } else {
        format!("{n} times")
    }
}

fn short_fp(fp: &str) -> &str {
    &fp[..fp.len().min(8)]
}

fn family(signal: WhySignal) -> Option<SuggestionSignal> {
    Some(match signal {
        WhySignal::SameFingerprint
        | WhySignal::TestCase
        | WhySignal::Environment
        | WhySignal::Branch
        | WhySignal::Confirmed => SuggestionSignal::Memory,
        WhySignal::Message | WhySignal::Frames => SuggestionSignal::Similar,
        WhySignal::Rule => SuggestionSignal::Rule,
        WhySignal::History => SuggestionSignal::History,
        WhySignal::Mock | WhySignal::Grid | WhySignal::Infra => SuggestionSignal::Signal,
        WhySignal::Overridden | WhySignal::Age | WhySignal::Conflict => return None,
    })
}

/// The family of the largest positive contribution.
pub fn signal_of(why: &[WhyItem], fallback: SuggestionSignal) -> SuggestionSignal {
    why.iter()
        .filter(|w| w.weight > 0.0)
        .filter_map(|w| family(w.signal).map(|f| (w.weight, f)))
        .fold(
            None,
            |best: Option<(f64, SuggestionSignal)>, (w, f)| match best {
                Some((bw, _)) if bw >= w => best,
                _ => Some((w, f)),
            },
        )
        .map(|(_, f)| f)
        .unwrap_or(fallback)
}

fn days_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    ((to - from).num_seconds() as f64 / 86_400.0).max(0.0)
}

/// Scores one memory row for a cluster; `None` when the row is no candidate (another
/// fingerprint whose message is less than 40% similar). The band is set against the default
/// minimum score; [`rank`] sets it again with the project's settings and the conflicts.
pub fn score(cluster: &ClusterFacts, m: &MemoryRow, now: DateTime<Utc>) -> Option<Candidate> {
    let same = m.fingerprint == cluster.fingerprint || cluster.aliases.contains(&m.fingerprint);
    let mut why = Vec::new();
    if same {
        let detail = if m.fingerprint == cluster.fingerprint {
            format!("The same error (fingerprint {})", short_fp(&m.fingerprint))
        } else {
            format!(
                "The same error under an alias (fingerprint {})",
                short_fp(&m.fingerprint)
            )
        };
        why.push(WhyItem {
            signal: WhySignal::SameFingerprint,
            weight: W_SAME_FINGERPRINT,
            detail,
        });
    } else {
        let s = crate::trigram::similarity(&cluster.message_norm, &m.message_norm);
        if s < MIN_MESSAGE_SIMILARITY {
            return None;
        }
        why.push(WhyItem {
            signal: WhySignal::Message,
            weight: round3(W_MESSAGE * s),
            detail: format!("The message is {} similar", pct(s)),
        });
    }
    let j = crate::frames::jaccard(&cluster.frames, &m.frames);
    if j > 0.0 {
        let a: BTreeSet<&str> = cluster.frames.iter().map(String::as_str).collect();
        let b: BTreeSet<&str> = m.frames.iter().map(String::as_str).collect();
        why.push(WhyItem {
            signal: WhySignal::Frames,
            weight: round3(W_FRAMES * j),
            detail: format!(
                "{} of {} stack frames are shared",
                a.intersection(&b).count(),
                a.union(&b).count()
            ),
        });
    }
    let shared_tests = cluster
        .test_case_ids
        .iter()
        .filter(|t| m.test_case_ids.contains(t))
        .count();
    if shared_tests > 0 {
        why.push(WhyItem {
            signal: WhySignal::TestCase,
            weight: W_TEST_CASE,
            detail: if shared_tests == 1 {
                "The same test was decided this way".into()
            } else {
                format!("{shared_tests} of these tests were decided this way")
            },
        });
    }
    if let Some(e) = &cluster.env_key
        && m.env_keys.contains(e)
    {
        why.push(WhyItem {
            signal: WhySignal::Environment,
            weight: W_ENVIRONMENT,
            detail: format!("Decided this way on the environment {e}"),
        });
    }
    if let Some(b) = &cluster.branch
        && m.branches.contains(b)
    {
        why.push(WhyItem {
            signal: WhySignal::Branch,
            weight: W_BRANCH,
            detail: format!("Decided this way on the branch {b}"),
        });
    }
    if m.confirmed > 0 {
        why.push(WhyItem {
            signal: WhySignal::Confirmed,
            weight: round3((W_CONFIRMED * m.confirmed as f64).min(MAX_CONFIRMED)),
            detail: format!(
                "Decided this way {} by people, last by {}",
                times(m.confirmed),
                m.last_decided_by
            ),
        });
    }
    if m.overridden > 0 {
        why.push(WhyItem {
            signal: WhySignal::Overridden,
            weight: round3(W_OVERRIDDEN * m.overridden as f64),
            detail: format!(
                "People changed this decision to another type {}",
                times(m.overridden)
            ),
        });
    }
    let sum: f64 = why.iter().map(|w| w.weight).sum();
    let d = days_between(m.last_decided_at, now);
    let factor = 1.0 - (d / AGE_DAYS).min(MAX_AGE_PENALTY);
    if factor < 1.0 {
        why.push(WhyItem {
            signal: WhySignal::Age,
            weight: round3(factor - 1.0),
            detail: format!("Last decided {} days ago", d.floor() as i64),
        });
    }
    let score = clamp(sum * factor);
    let signal = signal_of(&why, SuggestionSignal::Memory);
    let mut c = Candidate {
        fingerprint: m.fingerprint.clone(),
        defect_type_id: m.defect_type_id,
        group: m.group.clone(),
        score,
        band: None,
        signal,
        why,
        same_fingerprint: same,
        conflict: false,
        confirmed: m.confirmed,
        overridden: m.overridden,
        last_decided_at: Some(m.last_decided_at),
        rule_id: None,
    };
    c.band = band(&c, shares_test(cluster, m), DEFAULT_MIN_SCORE);
    Some(c)
}

/// Whether a test case of the cluster was decided in the memory row.
pub fn shares_test(cluster: &ClusterFacts, m: &MemoryRow) -> bool {
    cluster
        .test_case_ids
        .iter()
        .any(|t| m.test_case_ids.contains(t))
}

/// A candidate from a rule whose dry run matches the cluster's sample (a disabled rule, or an
/// enabled one that did not fire): fixed score [`W_RULE`].
pub fn rule_candidate(
    rule_id: Uuid,
    rule_name: &str,
    defect_type_id: Uuid,
    group: &str,
    enabled: bool,
) -> Candidate {
    let detail = if enabled {
        format!("The rule \"{rule_name}\" matches but did not apply")
    } else {
        format!("The disabled rule \"{rule_name}\" matches")
    };
    let why = vec![WhyItem {
        signal: WhySignal::Rule,
        weight: W_RULE,
        detail,
    }];
    let mut c = Candidate {
        fingerprint: String::new(),
        defect_type_id,
        group: group.to_string(),
        score: W_RULE,
        band: None,
        signal: SuggestionSignal::Rule,
        why,
        same_fingerprint: false,
        conflict: false,
        confirmed: 0,
        overridden: 0,
        last_decided_at: None,
        rule_id: Some(rule_id),
    };
    c.band = band(&c, false, DEFAULT_MIN_SCORE);
    c
}

/// The band of a scored candidate: `same` for the same fingerprint without conflict or
/// overrides and with a shared test and a confirmation, or three confirmations; else `high`
/// from 0.70, `low` from `min_score`, `None` below.
pub fn band(c: &Candidate, shared_test: bool, min_score: f64) -> Option<SuggestionBand> {
    if c.same_fingerprint
        && !c.conflict
        && c.overridden == 0
        && c.rule_id.is_none()
        && ((shared_test && c.confirmed >= 1) || c.confirmed >= SAME_CONFIRMED)
    {
        return Some(SuggestionBand::Same);
    }
    if c.score >= HIGH_MIN {
        Some(SuggestionBand::High)
    } else if c.score >= min_score - 1e-9 {
        Some(SuggestionBand::Low)
    } else {
        None
    }
}

fn shared_test_of(c: &Candidate) -> bool {
    c.why.iter().any(|w| w.signal == WhySignal::TestCase)
}

/// Adds the boost of the cluster's signals to the best candidate of each group they point to
/// (the strongest signal per group: exact [`W_SIGNAL_EXACT`], window [`W_SIGNAL_WINDOW`]).
/// Signals only boost: a group without a candidate gets none.
pub fn apply_signals(cands: &mut [Candidate], signals: &[SignalFact]) {
    let mut per_group: BTreeMap<&'static str, &SignalFact> = BTreeMap::new();
    for s in signals {
        let Some(g) = signal_group(s) else {
            continue;
        };
        match per_group.get(g) {
            Some(prev) if prev.exact || !s.exact => {}
            _ => {
                per_group.insert(g, s);
            }
        }
    }
    for (group, s) in per_group {
        let best = cands
            .iter_mut()
            .filter(|c| c.group == group)
            .max_by(|a, b| a.score.total_cmp(&b.score));
        let Some(c) = best else {
            continue;
        };
        let weight = if s.exact {
            W_SIGNAL_EXACT
        } else {
            W_SIGNAL_WINDOW
        };
        let signal = match s.source {
            SignalSource::Grid => WhySignal::Grid,
            SignalSource::Mock => WhySignal::Mock,
            SignalSource::Ci | SignalSource::K8s => WhySignal::Infra,
        };
        let how = if s.exact {
            "on these attempts"
        } else {
            "around the time of these attempts"
        };
        c.why.push(WhyItem {
            signal,
            weight,
            detail: format!("The {} reported {} {how}", s.source.as_str(), s.kind),
        });
        c.score = clamp(c.score + weight);
        c.signal = signal_of(&c.why, c.signal);
    }
}

fn band_order(b: Option<SuggestionBand>) -> u8 {
    match b {
        Some(SuggestionBand::Same) => 0,
        Some(SuggestionBand::High) => 1,
        Some(SuggestionBand::Low) => 2,
        None => 3,
    }
}

/// The order of candidates: band (`same`, `high`, `low`), score, the newest decision, then
/// fingerprint and type so that equal candidates always come in the same order.
pub fn order(a: &Candidate, b: &Candidate) -> Ordering {
    band_order(a.band)
        .cmp(&band_order(b.band))
        .then(b.score.total_cmp(&a.score))
        .then(b.last_decided_at.cmp(&a.last_decided_at))
        .then(a.fingerprint.cmp(&b.fingerprint))
        .then(a.defect_type_id.cmp(&b.defect_type_id))
}

/// What a cluster shows: conflicts marked (several types remembered for the same
/// fingerprint, each −0.10), the signals' boost added, bands set with the project's minimum,
/// candidates below it dropped, ordered by [`order`], one per defect type, at most
/// `max_candidates`.
pub fn rank(
    mut cands: Vec<Candidate>,
    opts: &RankOptions,
    signals: &[SignalFact],
) -> Vec<Candidate> {
    let same_types: BTreeSet<Uuid> = cands
        .iter()
        .filter(|c| c.same_fingerprint)
        .map(|c| c.defect_type_id)
        .collect();
    if same_types.len() > 1 {
        for c in cands.iter_mut().filter(|c| c.same_fingerprint) {
            c.conflict = true;
            c.why.push(WhyItem {
                signal: WhySignal::Conflict,
                weight: W_CONFLICT,
                detail: "People decided this error as another type too".into(),
            });
            c.score = clamp(c.score + W_CONFLICT);
        }
    }
    apply_signals(&mut cands, signals);
    for c in cands.iter_mut() {
        c.band = band(c, shared_test_of(c), opts.min_score);
    }
    cands.retain(|c| c.band.is_some());
    cands.sort_by(order);
    let mut seen = BTreeSet::new();
    cands.retain(|c| seen.insert(c.defect_type_id));
    cands.truncate(opts.max_candidates.clamp(1, MAX_CANDIDATES));
    cands
}

/// Scores every memory row for a cluster and ranks them ([`score`] then [`rank`]).
pub fn suggest<'a>(
    cluster: &ClusterFacts,
    rows: impl IntoIterator<Item = &'a MemoryRow>,
    extra: Vec<Candidate>,
    now: DateTime<Utc>,
    opts: &RankOptions,
    signals: &[SignalFact],
) -> Vec<Candidate> {
    let mut cands: Vec<Candidate> = rows
        .into_iter()
        .filter_map(|m| score(cluster, m, now))
        .collect();
    cands.extend(extra);
    rank(cands, opts, signals)
}

/// Whether a memory row earns the "make a rule" offer: three confirmations, no overrides.
pub fn rule_offer(m: &MemoryRow) -> bool {
    m.confirmed >= crate::model::RULE_OFFER_CONFIRMATIONS && m.overridden == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn t(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn row(fp: &str, ty: u128, group: &str) -> MemoryRow {
        MemoryRow {
            fingerprint: fp.into(),
            defect_type_id: t(ty),
            group: group.into(),
            message_norm: "connection refused to db port <n>".into(),
            frames: vec![],
            test_case_ids: vec![],
            env_keys: vec![],
            branches: vec![],
            comment: None,
            issue_url: None,
            issue_key: None,
            sample_result_id: None,
            sample_launch_id: None,
            confirmed: 0,
            overridden: 0,
            first_decided_at: now(),
            last_decided_at: now(),
            last_decided_by: "ann".into(),
        }
    }

    fn cluster(fp: &str) -> ClusterFacts {
        ClusterFacts {
            fingerprint: fp.into(),
            message_norm: "connection refused to db port <n>".into(),
            ..Default::default()
        }
    }

    fn weight(c: &Candidate, s: WhySignal) -> Option<f64> {
        c.why.iter().find(|w| w.signal == s).map(|w| w.weight)
    }

    #[test]
    fn each_row_of_the_table_has_its_why() {
        // Same fingerprint: +0.60, nothing else.
        let c = score(&cluster("a"), &row("a", 1, "productBug"), now()).unwrap();
        assert_eq!(c.score, 0.6);
        assert_eq!(weight(&c, WhySignal::SameFingerprint), Some(0.6));
        assert!(c.why[0].detail.contains("same error"));
        assert_eq!(c.signal, SuggestionSignal::Memory);
        assert_eq!(c.band, Some(SuggestionBand::Low));

        // An alias counts as the same fingerprint.
        let mut cl = cluster("a");
        cl.aliases = vec!["b".into()];
        let c = score(&cl, &row("b", 1, "productBug"), now()).unwrap();
        assert!(c.same_fingerprint && c.why[0].detail.contains("alias"));

        // Message similarity: 0.45 × s, only for another fingerprint; below 0.40 nothing.
        let mut m = row("b", 1, "productBug");
        m.message_norm = "connection refused to cache port <n>".into();
        let s = crate::trigram::similarity(&cluster("a").message_norm, &m.message_norm);
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(c.score, round3(0.45 * s));
        assert_eq!(c.signal, SuggestionSignal::Similar);
        assert!(c.why[0].detail.starts_with("The message is"));
        m.message_norm = "expected total <n> but was <n>".into();
        assert!(score(&cluster("a"), &m, now()).is_none());

        // Frames: 0.20 × Jaccard.
        let mut cl = cluster("a");
        cl.frames = vec!["f1".into(), "f2".into(), "f3".into()];
        let mut m = row("a", 1, "productBug");
        m.frames = vec!["f1".into(), "f2".into()];
        let c = score(&cl, &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Frames), Some(round3(0.2 * 2.0 / 3.0)));
        assert_eq!(
            c.why
                .iter()
                .find(|w| w.signal == WhySignal::Frames)
                .unwrap()
                .detail,
            "2 of 3 stack frames are shared"
        );

        // Test case, environment, branch.
        let mut cl = cluster("a");
        cl.test_case_ids = vec![t(10)];
        cl.env_key = Some("staging".into());
        cl.branch = Some("main".into());
        let mut m = row("a", 1, "productBug");
        m.test_case_ids = vec![t(10), t(11)];
        m.env_keys = vec!["staging".into()];
        m.branches = vec!["main".into()];
        let c = score(&cl, &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::TestCase), Some(0.1));
        assert_eq!(weight(&c, WhySignal::Environment), Some(0.05));
        assert_eq!(weight(&c, WhySignal::Branch), Some(0.03));
        assert_eq!(c.score, 0.78);

        // Confirmations: min(0.10, 0.03 × n); overrides: −0.15 × m.
        let mut m = row("a", 1, "productBug");
        m.confirmed = 2;
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Confirmed), Some(0.06));
        assert!(c.why[1].detail.contains("2 times") && c.why[1].detail.contains("ann"));
        m.confirmed = 9;
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Confirmed), Some(0.1));
        assert_eq!(c.score, 0.7);
        m.overridden = 1;
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Overridden), Some(-0.15));
        assert_eq!(c.score, 0.55);

        // Age: × (1 − min(0.3, d / 365)); the why weight is the factor − 1.
        let mut m = row("a", 1, "productBug");
        m.last_decided_at = now() - chrono::Duration::days(73);
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Age), Some(-0.2));
        assert_eq!(c.score, 0.48);
        assert!(c.why.last().unwrap().detail.contains("73 days"));
        m.last_decided_at = now() - chrono::Duration::days(3000);
        let c = score(&cluster("a"), &m, now()).unwrap();
        assert_eq!(weight(&c, WhySignal::Age), Some(-0.3));
        assert_eq!(c.score, 0.42);
    }

    #[test]
    fn conflicts_and_bands() {
        // Same band: same fingerprint, a shared test and one confirmation...
        let mut cl = cluster("a");
        cl.test_case_ids = vec![t(10)];
        let mut m = row("a", 1, "productBug");
        m.test_case_ids = vec![t(10)];
        m.confirmed = 1;
        let r = rank(
            vec![score(&cl, &m, now()).unwrap()],
            &RankOptions::default(),
            &[],
        );
        assert_eq!(r[0].band, Some(SuggestionBand::Same));
        // ...or three confirmations without a shared test.
        let mut m3 = row("a", 1, "productBug");
        m3.confirmed = 3;
        let r = rank(
            vec![score(&cluster("a"), &m3, now()).unwrap()],
            &RankOptions::default(),
            &[],
        );
        assert_eq!(r[0].band, Some(SuggestionBand::Same));
        // Two confirmations without a shared test: high or low by score.
        m3.confirmed = 2;
        let c = score(&cluster("a"), &m3, now()).unwrap();
        assert_eq!(c.band, Some(SuggestionBand::Low));
        // An override never gives same.
        m3.confirmed = 5;
        m3.overridden = 1;
        let c = score(&cluster("a"), &m3, now()).unwrap();
        assert_ne!(c.band, Some(SuggestionBand::Same));

        // Another type for the same fingerprint: both shown, −0.10 each, no same.
        let mut a = row("a", 1, "productBug");
        a.confirmed = 5;
        let mut b = row("a", 2, "systemIssue");
        b.confirmed = 1;
        let cands = vec![
            score(&cluster("a"), &a, now()).unwrap(),
            score(&cluster("a"), &b, now()).unwrap(),
        ];
        let r = rank(cands, &RankOptions::default(), &[]);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|c| c.conflict));
        assert_eq!(r[0].defect_type_id, t(1));
        assert_eq!(r[0].score, 0.6);
        assert_eq!(weight(&r[0], WhySignal::Conflict), Some(-0.1));
        assert_eq!(r[1].score, 0.53);
        assert!(r.iter().all(|c| c.band != Some(SuggestionBand::Same)));

        // The project's minimum score drops what is below it; max candidates cut.
        let r = rank(
            vec![
                score(&cluster("a"), &a, now()).unwrap(),
                score(&cluster("a"), &b, now()).unwrap(),
            ],
            &RankOptions {
                min_score: 0.55,
                max_candidates: 3,
            },
            &[],
        );
        assert_eq!(r.len(), 1);
        let r = rank(
            vec![
                score(&cluster("a"), &a, now()).unwrap(),
                score(&cluster("a"), &b, now()).unwrap(),
            ],
            &RankOptions {
                min_score: 0.4,
                max_candidates: 1,
            },
            &[],
        );
        assert_eq!(r.len(), 1);

        // One candidate per defect type: the best one.
        let mut other = row("z", 1, "productBug");
        other.message_norm = "connection refused to db port <n> again".into();
        let mut best = row("a", 1, "productBug");
        best.confirmed = 1;
        let r = rank(
            vec![
                score(&cluster("a"), &other, now()).unwrap(),
                score(&cluster("a"), &best, now()).unwrap(),
            ],
            &RankOptions::default(),
            &[],
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].fingerprint, "a");
    }

    #[test]
    fn signals_boost_the_best_candidate_of_their_group() {
        let a = row("a", 1, "productBug");
        let mut m = row("b", 2, "systemIssue");
        m.message_norm = "connection refused to db port <n> now".into();
        let cands = vec![
            score(&cluster("a"), &a, now()).unwrap(),
            score(&cluster("a"), &m, now()).unwrap(),
        ];
        let before = cands[1].score;
        let grid = SignalFact {
            source: SignalSource::Grid,
            kind: "node_lost".into(),
            exact: true,
        };
        let r = rank(
            cands.clone(),
            &RankOptions::default(),
            std::slice::from_ref(&grid),
        );
        let si = r.iter().find(|c| c.group == "systemIssue").unwrap();
        assert_eq!(si.score, clamp(before + 0.3));
        assert_eq!(weight(si, WhySignal::Grid), Some(0.3));
        assert_eq!(r[0].group, "systemIssue");
        // The message is still the largest contribution.
        assert_eq!(si.signal, SuggestionSignal::Similar);
        // A window signal adds 0.15; the strongest per group counts once.
        let window = SignalFact {
            source: SignalSource::K8s,
            kind: "oom_killed".into(),
            exact: false,
        };
        let r = rank(
            cands.clone(),
            &RankOptions::default(),
            &[window.clone(), window],
        );
        let si = r.iter().find(|c| c.group == "systemIssue").unwrap();
        assert_eq!(weight(si, WhySignal::Infra), Some(0.15));
        assert_eq!(si.why.len(), cands[1].why.len() + 1);
        // Mock kinds point to automationBug and noDefect; other kinds to nothing.
        let mock = |k: &str| SignalFact {
            source: SignalSource::Mock,
            kind: k.into(),
            exact: true,
        };
        assert_eq!(signal_group(&mock("unmatched")), Some("automationBug"));
        assert_eq!(signal_group(&mock("stub_error")), Some("automationBug"));
        assert_eq!(signal_group(&mock("injected_fault")), Some("noDefect"));
        assert_eq!(signal_group(&mock("latency")), None);
        // No candidate of the group: nothing is added.
        let r = rank(cands, &RankOptions::default(), &[mock("injected_fault")]);
        assert!(r.iter().all(|c| weight(c, WhySignal::Mock).is_none()));
    }

    #[test]
    fn rule_candidates_have_a_fixed_score() {
        let c = rule_candidate(t(9), "infra", t(2), "systemIssue", false);
        assert_eq!(c.score, 0.55);
        assert_eq!(c.band, Some(SuggestionBand::Low));
        assert_eq!(c.signal, SuggestionSignal::Rule);
        assert!(c.why[0].detail.contains("disabled rule \"infra\""));
        let r = rank(
            vec![c],
            &RankOptions {
                min_score: 0.6,
                max_candidates: 3,
            },
            &[],
        );
        assert!(r.is_empty());
        let mut m = row("a", 1, "productBug");
        assert!(!rule_offer(&m));
        m.confirmed = 3;
        assert!(rule_offer(&m));
        m.overridden = 1;
        assert!(!rule_offer(&m));
    }

    #[test]
    fn scores_are_clamped_and_rounded() {
        let mut cl = cluster("a");
        cl.test_case_ids = vec![t(1)];
        cl.env_key = Some("e".into());
        cl.branch = Some("b".into());
        cl.frames = vec!["x".into()];
        let mut m = row("a", 1, "productBug");
        m.test_case_ids = vec![t(1)];
        m.env_keys = vec!["e".into()];
        m.branches = vec!["b".into()];
        m.frames = vec!["x".into()];
        m.confirmed = 10;
        let c = score(&cl, &m, now()).unwrap();
        assert_eq!(c.score, 1.0);
        let r = rank(
            vec![c],
            &RankOptions::default(),
            &[SignalFact {
                source: SignalSource::Ci,
                kind: "runner_lost".into(),
                exact: true,
            }],
        );
        assert_eq!(r[0].score, 1.0);
        let mut m = row("a", 1, "productBug");
        m.overridden = 9;
        assert_eq!(score(&cluster("a"), &m, now()).unwrap().score, 0.0);
    }
}
