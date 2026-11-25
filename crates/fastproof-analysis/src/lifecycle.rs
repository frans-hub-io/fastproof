//! The state of a unique error as a pure function of stored facts (contract `76` §7.4).
//!
//! An error (a canonical fingerprint) is `new` when it occurred in one launch only, `active`
//! when it occurred again, `resolved` when it stopped occurring (for every tracked branch it
//! occurred on, at least `resolveAfterLaunches` finished launches of that branch after its last
//! occurrence there, or a linked issue was closed and a finished launch followed without it),
//! `regressed` when it occurred again after such a resolution, and `ignored` when a person said
//! so. [`state`] reads only facts that depend on the set of finished launches, never on the
//! order in which they were analysed, so analysing launches out of order gives the same states.
//! [`replay`] is the event-by-event reading of the same rules (in launch order), used by the
//! tests as the reference.

use std::collections::BTreeMap;

use crate::model::ErrorState;

/// Finished launches of one branch without the error, after a point (capped by the caller).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchCount {
    pub branch: String,
    pub launches: u32,
}

/// What the state of one error is computed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorFacts {
    /// A person set the error to `ignored`.
    pub ignored: bool,
    /// Occurrences in finished, non-debug launches: at least one.
    pub seen: bool,
    /// It occurred in exactly one launch.
    pub single: bool,
    /// For every branch the error occurred on: finished launches of that branch after its last
    /// occurrence there.
    pub tail: Vec<BranchCount>,
    /// For every branch the error occurred on before its latest occurrence: finished launches of
    /// that branch between its last occurrence there and the latest occurrence.
    pub gap: Vec<BranchCount>,
    /// A linked issue is closed, the close is after the latest occurrence and a finished launch
    /// of a tracked branch followed the close.
    pub issue_closed_after: bool,
    /// A linked issue was closed between the previous and the latest occurrence and a finished
    /// launch of a tracked branch followed the close before the latest occurrence.
    pub issue_closed_in_gap: bool,
    /// `analysis.resolveAfterLaunches`.
    pub resolve_after: u32,
}

/// Whether every branch has at least `k` launches (false without branches).
pub fn all_reached(counts: &[BranchCount], k: u32) -> bool {
    !counts.is_empty() && counts.iter().all(|c| c.launches >= k.max(1))
}

/// Whether the facts say the error is resolved now.
pub fn is_resolved(f: &ErrorFacts) -> bool {
    f.issue_closed_after || all_reached(&f.tail, f.resolve_after)
}

/// The state of an error; `None` when it never occurred in a finished, non-debug launch.
pub fn state(f: &ErrorFacts) -> Option<ErrorState> {
    if f.ignored {
        return Some(ErrorState::Ignored);
    }
    if !f.seen {
        return None;
    }
    if is_resolved(f) {
        return Some(ErrorState::Resolved);
    }
    if f.single {
        return Some(ErrorState::New);
    }
    if f.issue_closed_in_gap || all_reached(&f.gap, f.resolve_after) {
        return Some(ErrorState::Regressed);
    }
    Some(ErrorState::Active)
}

/// One launch of a replay: its branch and whether the error occurred in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayLaunch {
    pub branch: String,
    pub seen: bool,
}

/// The rules of §7.4 applied launch by launch in launch order, from no state: an occurrence
/// makes no state `new`, `new`, `regressed` or `active` `active`, and `resolved` `regressed`;
/// after a launch without it an open error becomes `resolved` once every branch it occurred on
/// had `k` launches since its last occurrence there. Returns the state after each launch.
pub fn replay(launches: &[ReplayLaunch], k: u32) -> Vec<Option<ErrorState>> {
    let k = k.max(1);
    let mut state: Option<ErrorState> = None;
    let mut since: BTreeMap<&str, u32> = BTreeMap::new();
    let mut out = Vec::with_capacity(launches.len());
    for l in launches {
        if l.seen {
            state = Some(match state {
                None => ErrorState::New,
                Some(ErrorState::Resolved) => ErrorState::Regressed,
                Some(ErrorState::Ignored) => ErrorState::Ignored,
                Some(_) => ErrorState::Active,
            });
            since.insert(l.branch.as_str(), 0);
        } else {
            if let Some(n) = since.get_mut(l.branch.as_str()) {
                *n += 1;
            }
            if matches!(
                state,
                Some(ErrorState::New | ErrorState::Active | ErrorState::Regressed)
            ) && !since.is_empty()
                && since.values().all(|n| *n >= k)
            {
                state = Some(ErrorState::Resolved);
            }
        }
        out.push(state);
    }
    out
}

/// The facts of [`state`] computed from a whole launch sequence in launch order (what the
/// store reads from its tables); for the tests.
pub fn facts_of(launches: &[ReplayLaunch], k: u32) -> ErrorFacts {
    let seen_at: Vec<usize> = launches
        .iter()
        .enumerate()
        .filter(|(_, l)| l.seen)
        .map(|(i, _)| i)
        .collect();
    let mut f = ErrorFacts {
        resolve_after: k,
        seen: !seen_at.is_empty(),
        single: seen_at.len() == 1,
        ..ErrorFacts::default()
    };
    let Some(&latest) = seen_at.last() else {
        return f;
    };
    let count = |branch: &str, after: usize, before: usize| -> u32 {
        launches[after + 1..before]
            .iter()
            .filter(|l| l.branch == branch)
            .count() as u32
    };
    let mut last: BTreeMap<&str, usize> = BTreeMap::new();
    for &i in &seen_at {
        last.insert(launches[i].branch.as_str(), i);
    }
    f.tail = last
        .iter()
        .map(|(b, &i)| BranchCount {
            branch: b.to_string(),
            launches: count(b, i, launches.len()),
        })
        .collect();
    let mut before: BTreeMap<&str, usize> = BTreeMap::new();
    for &i in seen_at.iter().filter(|&&i| i < latest) {
        before.insert(launches[i].branch.as_str(), i);
    }
    f.gap = before
        .iter()
        .map(|(b, &i)| BranchCount {
            branch: b.to_string(),
            launches: count(b, i, latest),
        })
        .collect();
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(spec: &str) -> Vec<ReplayLaunch> {
        spec.split_whitespace()
            .map(|t| ReplayLaunch {
                branch: t.trim_end_matches('!').to_string(),
                seen: t.ends_with('!'),
            })
            .collect()
    }

    fn bc(branch: &str, launches: u32) -> BranchCount {
        BranchCount {
            branch: branch.into(),
            launches,
        }
    }

    #[test]
    fn states_follow_the_rules() {
        let f = |single, tail: Vec<BranchCount>, gap: Vec<BranchCount>| ErrorFacts {
            seen: true,
            single,
            tail,
            gap,
            resolve_after: 3,
            ..ErrorFacts::default()
        };
        assert_eq!(state(&ErrorFacts::default()), None);
        assert_eq!(
            state(&f(true, vec![bc("m", 0)], vec![])),
            Some(ErrorState::New)
        );
        assert_eq!(
            state(&f(true, vec![bc("m", 3)], vec![])),
            Some(ErrorState::Resolved)
        );
        assert_eq!(
            state(&f(false, vec![bc("m", 2)], vec![bc("m", 0)])),
            Some(ErrorState::Active)
        );
        assert_eq!(
            state(&f(false, vec![bc("m", 0)], vec![bc("m", 3)])),
            Some(ErrorState::Regressed)
        );
        // Every tracked branch must reach the count.
        assert_eq!(
            state(&f(false, vec![bc("m", 5), bc("dev", 2)], vec![bc("m", 0)])),
            Some(ErrorState::Active)
        );
        assert_eq!(
            state(&f(
                false,
                vec![bc("m", 5), bc("dev", 2)],
                vec![bc("m", 3), bc("dev", 1)]
            )),
            Some(ErrorState::Active)
        );
        let mut ignored = f(true, vec![bc("m", 9)], vec![]);
        ignored.ignored = true;
        assert_eq!(state(&ignored), Some(ErrorState::Ignored));
        let mut closed = f(false, vec![bc("m", 1)], vec![bc("m", 0)]);
        closed.issue_closed_after = true;
        assert_eq!(state(&closed), Some(ErrorState::Resolved));
        let mut reopened = f(false, vec![bc("m", 0)], vec![bc("m", 1)]);
        reopened.issue_closed_in_gap = true;
        assert_eq!(state(&reopened), Some(ErrorState::Regressed));
        assert!(!all_reached(&[], 3));
    }

    #[test]
    fn replay_walks_through_the_life_of_an_error() {
        use ErrorState::*;
        let s = replay(&seq("m! m m! m m m m! m"), 3);
        assert_eq!(
            s,
            vec![
                Some(New),
                Some(New),
                Some(Active),
                Some(Active),
                Some(Active),
                Some(Resolved),
                Some(Regressed),
                Some(Regressed)
            ]
        );
        // A second branch keeps it open until both have K launches without it.
        let s = replay(&seq("m! d! m m m d d m d"), 3);
        assert_eq!(s.last(), Some(&Some(Resolved)));
        assert_eq!(s[6], Some(Active));
        assert_eq!(s[7], Some(Active));
        // Launches of a branch the error never occurred on do not count.
        let s = replay(&seq("m! x x x x"), 3);
        assert_eq!(s.last(), Some(&Some(New)));
        assert_eq!(replay(&seq("m m"), 3), vec![None, None]);
    }

    /// A small deterministic generator (SplitMix64) for the property tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn state_of_the_facts_equals_the_replay_on_random_sequences() {
        let mut rng = Rng(0x5eed_e770);
        for case in 0..5000 {
            let len = 1 + rng.below(40) as usize;
            let branches = 1 + rng.below(3);
            let p = 1 + rng.below(6);
            let k = 1 + rng.below(4) as u32;
            let launches: Vec<ReplayLaunch> = (0..len)
                .map(|_| ReplayLaunch {
                    branch: format!("b{}", rng.below(branches)),
                    seen: rng.below(10) < p,
                })
                .collect();
            let want = *replay(&launches, k).last().unwrap();
            let got = state(&facts_of(&launches, k));
            assert_eq!(got, want, "case {case}: k={k} {launches:?}");
            // Invariants of the replay: `regressed` only right after `resolved` or itself,
            // `resolved` only when every branch had K launches without the error.
            let states = replay(&launches, k);
            for i in 0..states.len() {
                let before = if i == 0 { None } else { states[i - 1] };
                if states[i] == Some(ErrorState::Regressed) && before != states[i] {
                    assert_eq!(before, Some(ErrorState::Resolved), "case {case} at {i}");
                }
                if states[i] == Some(ErrorState::Resolved) && before != states[i] {
                    let f = facts_of(&launches[..=i], k);
                    assert!(all_reached(&f.tail, k), "case {case} at {i}");
                }
            }
        }
    }
}
