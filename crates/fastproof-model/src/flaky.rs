//! Flakiness across runs and status transitions, ported from Allure 3 v3.33.0
//! (`core-api/src/utils/flaky.ts`, `core/src/utils/flaky.ts`, `core/src/utils/new.ts`).

use serde::{Deserialize, Serialize};

use crate::status::Status;

/// Flaky detection settings (Allure 3 `flakyDetection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FlakyOptions {
    /// Comparable past executions to use: `-1` disables, `0` means all since the last gap.
    pub history_depth: i32,
    /// Identical outcomes in a row that reset earlier instability (at least 1).
    pub stabilization_period: u32,
    /// Also infer flakiness for currently passed tests.
    pub include_passed_tests: bool,
}

impl Default for FlakyOptions {
    fn default() -> Self {
        FlakyOptions {
            history_depth: 10,
            stabilization_period: 5,
            include_passed_tests: false,
        }
    }
}

/// Instability of `current` given the past statuses of its series, newest first.
///
/// `None` in `history_newest_first` is a run where the test was missing (a gap); it ends
/// the usable history. Returns `Some(true)` unstable, `Some(false)` stable and `None` when
/// there is not enough evidence or detection is disabled.
pub fn test_flakiness(
    current: Status,
    history_newest_first: &[Option<Status>],
    history_depth: i32,
    stabilization_period: u32,
) -> Option<bool> {
    if history_depth == -1 || !current.is_significant() {
        return None;
    }
    let mut statuses: Vec<Status> = Vec::new();
    for entry in history_newest_first {
        let Some(status) = entry else { break };
        if !status.is_significant() {
            continue;
        }
        statuses.push(*status);
        if history_depth > 0 && statuses.len() == history_depth as usize {
            break;
        }
    }
    if statuses.is_empty() {
        return None;
    }
    statuses.reverse();
    statuses.push(current);

    let period = if history_depth > 0 {
        (stabilization_period as usize).min(history_depth as usize + 1)
    } else {
        stabilization_period as usize
    };
    let mut anchor = 0;
    let mut streak = 0;
    for i in 0..statuses.len() {
        streak = if i > 0 && statuses[i] == statuses[i - 1] {
            streak + 1
        } else {
            1
        };
        if streak >= period {
            anchor = i;
        }
    }
    let changes = (anchor + 1..statuses.len())
        .filter(|&i| statuses[i] != statuses[i - 1])
        .count();
    if changes <= 1 {
        return Some(false);
    }
    // Only P -> F -> P and P -> B -> P tolerate a two-change recovery.
    Some(!(changes == 2 && statuses[anchor] == Status::Passed && current == Status::Passed))
}

/// Allure 3 flaky detector: only failed/broken results (and passed ones when
/// `include_passed_tests`) can be flaky.
pub fn is_flaky(
    current: Status,
    history_newest_first: &[Option<Status>],
    options: &FlakyOptions,
) -> bool {
    if current != Status::Failed
        && current != Status::Broken
        && !(options.include_passed_tests && current == Status::Passed)
    {
        return false;
    }
    test_flakiness(
        current,
        history_newest_first,
        options.history_depth,
        options.stabilization_period,
    ) == Some(true)
}

/// Status change compared to the previous significant status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    New,
    Fixed,
    Regressed,
    Malfunctioned,
}

/// Allure 3 `getStatusTransition`: `history_newest_first` holds the statuses of the runs
/// where the series was present (gaps removed).
pub fn status_transition(current: Status, history_newest_first: &[Status]) -> Option<Transition> {
    if history_newest_first.is_empty() {
        return Some(Transition::New);
    }
    let last = history_newest_first
        .iter()
        .find(|s| !matches!(s, Status::Unknown | Status::Skipped));
    if last == Some(&current) {
        return None;
    }
    match current {
        Status::Passed => Some(Transition::Fixed),
        Status::Failed => Some(Transition::Regressed),
        Status::Broken => Some(Transition::Malfunctioned),
        _ => None,
    }
}
