//! Types shared by the live analysis path, the store and the backtest (serde camelCase,
//! OpenAPI schemas), and the fixed constants of the analysis contract.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Suggestions shown per cluster, at most.
pub const MAX_CANDIDATES: usize = 3;
/// Decisions in a band of a backtest before its precision is shown.
pub const PRECISION_MIN_DECISIONS: u32 = 50;
/// Confirmations of a memory row before a rule draft is offered.
pub const RULE_OFFER_CONFIRMATIONS: i32 = 3;
/// Test cases kept per memory row, newest first.
pub const MEMORY_TEST_CASES: usize = 20;
/// Environment keys kept per memory row.
pub const MEMORY_ENV_KEYS: usize = 10;
/// Branches kept per memory row.
pub const MEMORY_BRANCHES: usize = 10;
/// Characters of a normalized message (as the fingerprint catalog).
pub const MESSAGE_NORM_CHARS: usize = 1000;
/// Branches of a project tracked by name; others are stored as [`OTHER_BRANCH`].
pub const TRACKED_BRANCHES: usize = 50;
/// The branch name of untracked branches (not a valid git ref character).
pub const OTHER_BRANCH: &str = "*";
/// Days a fingerprint must be absent on the baseline branch to count as a new error.
pub const NEW_ERRORS_DAYS: i64 = 30;
/// Rows of one undone batch, at most.
pub const MAX_UNDO_ROWS: i64 = 20_000;
/// Launches of one gate simulation, at most.
pub const SIMULATE_MAX_LAUNCHES: usize = 200;
/// Signals of one push request, at most.
pub const MAX_SIGNALS_PER_REQUEST: usize = 1000;
/// Bytes of the serialized `detail` of a signal, at most.
pub const MAX_SIGNAL_DETAIL_BYTES: usize = 4096;
/// Bytes of a signals push body, at most.
pub const MAX_SIGNALS_BODY_BYTES: usize = 1024 * 1024;
/// Signals stored per attempt, at most.
pub const MAX_SIGNALS_PER_ATTEMPT: usize = 50;
/// Days the PostgreSQL fallback of the analytics endpoints covers.
pub const ANALYTICS_PG_DAYS: i64 = 30;
/// Projects of one portfolio answer, at most.
pub const PORTFOLIO_MAX_PROJECTS: usize = 200;
/// Backtest reports kept per project.
pub const BACKTESTS_KEPT: i64 = 20;

/// Band of a suggestion: `same` (the same error decided the same way before), `high`, `low`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionBand {
    Same,
    High,
    Low,
}

impl SuggestionBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::High => "high",
            Self::Low => "low",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "same" => Some(Self::Same),
            "high" => Some(Self::High),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// The family of the largest contribution to a suggestion's score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionSignal {
    Memory,
    Similar,
    Rule,
    History,
    Signal,
}

impl SuggestionSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Similar => "similar",
            Self::Rule => "rule",
            Self::History => "history",
            Self::Signal => "signal",
        }
    }
}

/// One reason in the "why" of a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WhySignal {
    SameFingerprint,
    Message,
    Frames,
    TestCase,
    Environment,
    Branch,
    Confirmed,
    Overridden,
    Age,
    Conflict,
    Rule,
    History,
    Mock,
    Grid,
    Infra,
}

/// A contribution to a suggestion's score.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WhyItem {
    pub signal: WhySignal,
    /// The added score; for `age` the factor minus 1.
    pub weight: f64,
    pub detail: String,
}

/// A person's triage decision as the memory and the backtest see it (also the line of the
/// NDJSON export, where `by` is a hash).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEvent {
    /// Format version, 1.
    pub v: u8,
    pub at: DateTime<Utc>,
    pub fingerprint: String,
    pub message_norm: String,
    pub frames: Vec<String>,
    pub test_case_id: Option<Uuid>,
    pub env_key: Option<String>,
    pub branch: Option<String>,
    pub defect_type_id: Uuid,
    pub group: String,
    pub previous_type_id: Option<Uuid>,
    pub by: String,
    pub results: u32,
}

/// A row of the triage memory: what people decided for one fingerprint and defect type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRow {
    pub fingerprint: String,
    pub defect_type_id: Uuid,
    pub group: String,
    pub message_norm: String,
    pub frames: Vec<String>,
    pub test_case_ids: Vec<Uuid>,
    pub env_keys: Vec<String>,
    pub branches: Vec<String>,
    pub comment: Option<String>,
    pub issue_url: Option<String>,
    pub issue_key: Option<String>,
    pub sample_result_id: Option<Uuid>,
    pub sample_launch_id: Option<Uuid>,
    pub confirmed: i32,
    pub overridden: i32,
    pub first_decided_at: DateTime<Utc>,
    pub last_decided_at: DateTime<Utc>,
    pub last_decided_by: String,
}

/// What scoring knows about a cluster of a launch (one canonical fingerprint).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterFacts {
    /// The canonical fingerprint.
    pub fingerprint: String,
    /// Fingerprints aliased to it.
    pub aliases: Vec<String>,
    pub message_norm: String,
    pub frames: Vec<String>,
    pub test_case_ids: Vec<Uuid>,
    pub env_key: Option<String>,
    pub branch: Option<String>,
    /// Results of the launch in the cluster.
    pub results: u32,
    pub sample_result_id: Option<Uuid>,
}

/// A scored candidate for a cluster, before persistence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Fingerprint of the memory row (or rule) the candidate comes from.
    pub fingerprint: String,
    pub defect_type_id: Uuid,
    pub group: String,
    /// Clamped to `[0, 1]`, 3 decimals.
    pub score: f64,
    /// `None` below the project's minimum score.
    pub band: Option<SuggestionBand>,
    pub signal: SuggestionSignal,
    pub why: Vec<WhyItem>,
    pub same_fingerprint: bool,
    pub conflict: bool,
    pub confirmed: i32,
    pub overridden: i32,
    pub last_decided_at: Option<DateTime<Utc>>,
    /// The rule of a `rule` candidate.
    pub rule_id: Option<Uuid>,
}

/// State of a unique error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorState {
    New,
    Active,
    Resolved,
    Regressed,
    Ignored,
}

impl ErrorState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Active => "active",
            Self::Resolved => "resolved",
            Self::Regressed => "regressed",
            Self::Ignored => "ignored",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "active" => Some(Self::Active),
            "resolved" => Some(Self::Resolved),
            "regressed" => Some(Self::Regressed),
            "ignored" => Some(Self::Ignored),
            _ => None,
        }
    }
}

/// Kind of a flaky-cause hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FlakyHintKind {
    Timing,
    Environment,
    Order,
    Retry,
    Infra,
    Time,
}

/// Confidence of a flaky-cause hint.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HintConfidence {
    Low,
    Medium,
    High,
}

/// Where a signal about a test attempt comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SignalSource {
    Mock,
    Grid,
    Ci,
    K8s,
}

impl SignalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Grid => "grid",
            Self::Ci => "ci",
            Self::K8s => "k8s",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_the_contract_ones() {
        let j = |v: serde_json::Value| v.as_str().unwrap().to_string();
        assert_eq!(j(serde_json::json!(SignalSource::K8s)), "k8s");
        assert_eq!(
            j(serde_json::to_value(WhySignal::SameFingerprint).unwrap()),
            "sameFingerprint"
        );
        assert_eq!(
            j(serde_json::to_value(ErrorState::Regressed).unwrap()),
            "regressed"
        );
        assert_eq!(
            j(serde_json::to_value(SuggestionBand::Same).unwrap()),
            "same"
        );
        for s in ["new", "active", "resolved", "regressed", "ignored"] {
            assert_eq!(ErrorState::parse(s).unwrap().as_str(), s);
        }
        assert!(
            SuggestionBand::Same < SuggestionBand::High
                && SuggestionBand::High < SuggestionBand::Low
        );
    }
}
