//! Types of FastProof Analysis (contract `76` §6): the DTOs of every area (serde camelCase,
//! OpenAPI schemas), the project settings area `analysis`, the instance limits and the row and
//! context types the areas share.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa::openapi::{Ref, RefOr, schema::Schema};
use uuid::Uuid;

pub use fastproof_analysis::model::*;

use crate::logs::LogLevel;
use crate::tm::{TmError, TmGateMode, double_option};
use crate::{Defect, Launch};

use super::settings::ProjectSettings;
use super::triage::TriageIssue;

/// Result of the analysis store operations that may answer an API error.
pub type AnalysisResult<T> = Result<T, TmError>;

fn schema_ref(name: &str) -> RefOr<Schema> {
    RefOr::Ref(Ref::from_schema_name(name))
}
fn metric_ref() -> RefOr<Schema> {
    schema_ref("Metric")
}
fn gate_status_ref() -> RefOr<Schema> {
    schema_ref("GateStatus")
}
fn gate_ref() -> RefOr<Schema> {
    schema_ref("Gate")
}
fn rule_result_list() -> utoipa::openapi::schema::Array {
    utoipa::openapi::schema::Array::new(schema_ref("RuleResult"))
}

// Core: journal and undo.

/// The path a triage decision came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecisionVia {
    Api,
    Mcp,
    Rp,
    Rule,
    Retro,
    Suggestion,
    Auto,
    Undo,
}

impl DecisionVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Mcp => "mcp",
            Self::Rp => "rp",
            Self::Rule => "rule",
            Self::Retro => "retro",
            Self::Suggestion => "suggestion",
            Self::Auto => "auto",
            Self::Undo => "undo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "api" => Self::Api,
            "mcp" => Self::Mcp,
            "rp" => Self::Rp,
            "rule" => Self::Rule,
            "retro" => Self::Retro,
            "suggestion" => Self::Suggestion,
            "auto" => Self::Auto,
            "undo" => Self::Undo,
            _ => return None,
        })
    }
}

/// Who made a triage decision: a person, an AI agent, a rule or auto-apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecisionSource {
    User,
    Ai,
    Rule,
    Auto,
}

impl DecisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Ai => "ai",
            Self::Rule => "rule",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Self::User,
            "ai" => Self::Ai,
            "rule" => Self::Rule,
            "auto" => Self::Auto,
            _ => return None,
        })
    }
}

/// The triage state of a result before or after a decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageState {
    pub defect: Option<Defect>,
    pub muted: bool,
    pub mute_until: Option<DateTime<Utc>>,
}

/// A row of the decision journal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageDecision {
    pub id: i64,
    pub batch_id: Uuid,
    pub result_id: Uuid,
    pub launch_id: Uuid,
    pub fingerprint: Option<String>,
    pub via: DecisionVia,
    pub source: DecisionSource,
    pub by: String,
    pub at: DateTime<Utc>,
    pub before: TriageState,
    pub after: TriageState,
    pub suggestion_id: Option<Uuid>,
    pub reverts_batch: Option<Uuid>,
    /// The batch is not a rule batch and at least one of its results is still at its `after`.
    pub undoable: bool,
}

/// `GET .../triage/history`: rows of a result (newest first) or of a batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageHistory {
    pub items: Vec<TriageDecision>,
    pub next: Option<String>,
}

/// Body of `POST .../triage/undo`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TriageUndo {
    pub batch_id: Uuid,
}

/// Why a result of an undone batch was left as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriageUndoSkipCode {
    /// Someone changed the result after the batch.
    ChangedSince,
    /// The result is gone (deleted or rolled up).
    ResultGone,
    /// The result's launch was merged into another.
    LaunchMerged,
}

/// A result an undo left as it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageUndoSkip {
    pub result_id: Uuid,
    pub code: TriageUndoSkipCode,
}

/// Outcome of an undo: the new batch (none when nothing was restored).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageUndoResult {
    pub batch_id: Option<Uuid>,
    pub reverts: Uuid,
    pub restored: u64,
    pub skipped: Vec<TriageUndoSkip>,
}

/// State of the analysis replica gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateView {
    pub gate_open: bool,
    pub mode: TmGateMode,
    pub settle_until: Option<DateTime<Utc>>,
}

/// Which analysis features answer for a project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisFeatures {
    pub suggestions: bool,
    pub errors: bool,
    pub log_patterns: bool,
    pub gate_analytics: bool,
    pub flaky_hints: bool,
    pub signals: bool,
    pub analytics: bool,
    pub portfolio: bool,
    pub cross_project_similar: bool,
}

/// Data windows of a project's analysis, in days.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisWindows {
    pub raw_days: u32,
    pub memory_days: u32,
    /// 30 on PostgreSQL, the ClickHouse TTL with ClickHouse.
    pub analytics_days: u32,
}

/// `GET .../analysis/capabilities`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisCapabilities {
    /// The instance and the project have Analysis on.
    pub enabled: bool,
    pub instance_enabled: bool,
    pub project_enabled: bool,
    pub clickhouse: bool,
    /// `pg_trgm` is installed.
    pub trigram: bool,
    /// Every replica knows the analysis formats.
    pub gate_open: bool,
    pub features: AnalysisFeatures,
    pub windows: AnalysisWindows,
    /// Feature names that need ClickHouse.
    pub with_click_house: Vec<String>,
}

/// What the server knows for [`crate::Store::analysis_capabilities`].
#[derive(Clone, Debug, Default)]
pub struct CapabilityInputs {
    /// `FASTPROOF_ANALYSIS=on`.
    pub instance_enabled: bool,
    /// ClickHouse is configured for Analysis.
    pub clickhouse: bool,
    /// Features whose code is in this build.
    pub implemented: AnalysisFeatures,
    /// Days of the ClickHouse aggregates (`FASTPROOF_CLICKHOUSE_AGG_TTL_DAYS`).
    pub clickhouse_days: u32,
    pub limits: AnalysisLimits,
}

/// State of a launch in the analysis queue (`none`: never queued).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisLaunchState {
    Queued,
    Running,
    Done,
    Failed,
    None,
}

impl AnalysisLaunchState {
    pub fn parse(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::None,
        }
    }
}

/// `GET .../launches/{id}/analysis`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisLaunchStatus {
    pub state: AnalysisLaunchState,
    pub queued_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub steps: LaunchSteps,
}

/// Outcome of a pipeline step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisStepStatus {
    Done,
    Partial,
    Skipped,
    Failed,
}

/// What one step of the analysis of a launch did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcome {
    pub status: AnalysisStepStatus,
    pub millis: u64,
    #[schema(value_type = Object)]
    pub detail: serde_json::Map<String, serde_json::Value>,
}

fn detail_of(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Null => serde_json::Map::new(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".into(), other);
            m
        }
    }
}

impl StepOutcome {
    fn new(status: AnalysisStepStatus, detail: serde_json::Value) -> Self {
        Self {
            status,
            millis: 0,
            detail: detail_of(detail),
        }
    }

    /// The step did all its work.
    pub fn done(detail: serde_json::Value) -> Self {
        Self::new(AnalysisStepStatus::Done, detail)
    }

    /// The step stopped early (a budget ran out); `detail` says what is missing.
    pub fn partial(detail: serde_json::Value) -> Self {
        Self::new(AnalysisStepStatus::Partial, detail)
    }

    /// The step did not run: `{reason}`.
    pub fn skipped(reason: &str) -> Self {
        Self::new(
            AnalysisStepStatus::Skipped,
            serde_json::json!({ "reason": reason }),
        )
    }

    /// The step failed: `{error}`.
    pub fn failed(error: &str) -> Self {
        Self::new(
            AnalysisStepStatus::Failed,
            serde_json::json!({ "error": error }),
        )
    }

    /// With the step's wall time.
    pub fn timed(mut self, millis: u64) -> Self {
        self.millis = millis;
        self
    }
}

/// The steps of the analysis of a launch, as recorded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSteps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<StepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<StepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_rules: Option<StepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<StepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky_hints: Option<StepOutcome>,
}

// Settings.

/// `analysis.suggest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AnalysisSuggestSettings {
    /// 0.40 to 0.95.
    pub min_score: f64,
    /// 1 to 3.
    pub max_candidates: u32,
}

impl Default for AnalysisSuggestSettings {
    fn default() -> Self {
        Self {
            min_score: 0.4,
            max_candidates: 3,
        }
    }
}

/// `analysis.autoApply`: rank-1 suggestions of these bands are applied by themselves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AnalysisAutoApplySettings {
    pub enabled: bool,
    /// Non-empty, only `same`.
    pub bands: Vec<SuggestionBand>,
}

impl Default for AnalysisAutoApplySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bands: vec![SuggestionBand::Same],
        }
    }
}

/// `analysis.backtest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AnalysisBacktestSettings {
    pub nightly: bool,
}

impl Default for AnalysisBacktestSettings {
    fn default() -> Self {
        Self { nightly: true }
    }
}

/// `analysis.flaky`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AnalysisFlakySettings {
    /// 0.05 to 1.0.
    pub hint_min_score: f64,
}

impl Default for AnalysisFlakySettings {
    fn default() -> Self {
        Self {
            hint_min_score: 0.2,
        }
    }
}

/// The project's settings area `analysis` (the instance switch is `FASTPROOF_ANALYSIS`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct AnalysisSettings {
    /// The project's opt-out.
    pub enabled: bool,
    /// 30 to 3650.
    pub memory_days: u32,
    /// 1 to 100.
    pub resolve_after_launches: u32,
    pub suggest: AnalysisSuggestSettings,
    pub auto_apply: AnalysisAutoApplySettings,
    pub backtest: AnalysisBacktestSettings,
    pub flaky: AnalysisFlakySettings,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_days: 365,
            resolve_after_launches: 10,
            suggest: AnalysisSuggestSettings::default(),
            auto_apply: AnalysisAutoApplySettings::default(),
            backtest: AnalysisBacktestSettings::default(),
            flaky: AnalysisFlakySettings::default(),
        }
    }
}

impl AnalysisSettings {
    /// Whether this is the default (it is then left out of the stored document).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Checks ranges; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        if !(30..=3650).contains(&self.memory_days) {
            return Err("analysis.memoryDays must be 30..=3650".into());
        }
        if !(1..=100).contains(&self.resolve_after_launches) {
            return Err("analysis.resolveAfterLaunches must be 1..=100".into());
        }
        let m = self.suggest.min_score;
        if !m.is_finite() || !(0.4..=0.95).contains(&m) {
            return Err("analysis.suggest.minScore must be 0.40..=0.95".into());
        }
        if !(1..=3).contains(&self.suggest.max_candidates) {
            return Err("analysis.suggest.maxCandidates must be 1..=3".into());
        }
        let b = &self.auto_apply.bands;
        if b.is_empty() || b.iter().any(|b| *b != SuggestionBand::Same) {
            return Err("analysis.autoApply.bands must be [\"same\"]".into());
        }
        let h = self.flaky.hint_min_score;
        if !h.is_finite() || !(0.05..=1.0).contains(&h) {
            return Err("analysis.flaky.hintMinScore must be 0.05..=1.0".into());
        }
        Ok(())
    }
}

/// Instance limits of Analysis (`FASTPROOF_ANALYSIS_*`, contract §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisLimits {
    pub launch_timeout_seconds: u64,
    pub max_clusters: u32,
    pub candidate_rows: u32,
    pub memory_max_rows: u64,
    pub log_cpu_ms: u64,
    pub log_last_bytes: u64,
    pub log_attempts: u32,
    pub max_log_rules: u32,
    pub recompute_seconds: u64,
    pub backtest_max_events: u64,
    pub signals_per_minute: u32,
    pub fastmock_attempts: u32,
    /// `FASTPROOF_ANALYSIS_GATE`.
    pub gate_mode: TmGateMode,
    /// `FASTPROOF_ANALYSIS_GATE_SETTLE_SECONDS`.
    pub gate_settle_seconds: u64,
}

impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            launch_timeout_seconds: 120,
            max_clusters: 2000,
            candidate_rows: 5000,
            memory_max_rows: 200_000,
            log_cpu_ms: 2000,
            log_last_bytes: 262_144,
            log_attempts: 2000,
            max_log_rules: 100,
            recompute_seconds: 30,
            backtest_max_events: 200_000,
            signals_per_minute: 600,
            fastmock_attempts: 100,
            gate_mode: TmGateMode::Auto,
            gate_settle_seconds: 600,
        }
    }
}

// Internal row and context types.

/// How a triage write is journaled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionCtx {
    pub via: DecisionVia,
    pub suggestion_id: Option<Uuid>,
    /// Overrides the request's `source` (internal callers: `auto`).
    pub source: Option<DecisionSource>,
    /// Leave results whose defect a person or an agent chose untouched.
    pub skip_protected: bool,
    /// The batch an undo restores.
    pub reverts: Option<Uuid>,
}

impl DecisionCtx {
    /// A decision through a path.
    pub fn new(via: DecisionVia) -> Self {
        Self {
            via,
            suggestion_id: None,
            source: None,
            skip_protected: false,
            reverts: None,
        }
    }

    /// REST `POST .../triage` (and the UI).
    pub fn api() -> Self {
        Self::new(DecisionVia::Api)
    }
}

/// Who asks for an undo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoActor {
    /// Holds `results:mute` in the project.
    pub can_mute: bool,
    /// An AI agent (it may undo only its own batches).
    pub agent: bool,
    /// The principal's name.
    pub name: String,
}

/// Who accepts a suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptActor {
    pub agent: bool,
    pub can_mute: bool,
}

/// Why a launch is in the analysis queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum QueueReason {
    Finished,
    Recompute,
    Backfill,
}

impl QueueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Recompute => "recompute",
            Self::Backfill => "backfill",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "recompute" => Self::Recompute,
            "backfill" => Self::Backfill,
            _ => Self::Finished,
        }
    }
}

/// A row of `analysis_launches`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisLaunchRow {
    pub launch_id: Uuid,
    pub project_id: Uuid,
    pub state: AnalysisLaunchState,
    pub reason: QueueReason,
    pub due_at: DateTime<Utc>,
    pub attempts: i32,
    pub lease_owner: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
    pub steps: LaunchSteps,
    pub error: Option<String>,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// An `analysis_tasks` row claimed by the analysis role.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub params: serde_json::Value,
    pub created_by: String,
    pub owner: String,
}

/// What the steps of the analysis of a launch see.
#[derive(Clone, Copy, Debug)]
pub struct LaunchCtx<'a> {
    pub project_id: Uuid,
    pub project_key: &'a str,
    pub launch: &'a Launch,
    pub settings: &'a ProjectSettings,
    pub limits: &'a AnalysisLimits,
    pub now: DateTime<Utc>,
}

/// Query of `GET .../triage/history`: `result_id` or `batch_id`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriageHistoryQuery {
    pub result_id: Option<Uuid>,
    pub batch_id: Option<Uuid>,
    /// The `next` of the previous page.
    pub before: Option<String>,
    pub limit: i64,
}

/// A log line a log rule matched (night B, ClickHouse retro).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogMatch {
    pub launch_id: Uuid,
    pub result_key: String,
    pub attempt: u32,
    pub seq: u64,
    pub excerpt: String,
}

/// One run of a series, as the flaky-cause classifiers see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesRun {
    pub launch_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub flaky_in_run: bool,
    pub duration_ms: Option<i64>,
    pub env_key: Option<String>,
    pub branch: Option<String>,
    pub host: Option<String>,
    pub thread: Option<String>,
    pub ci_runner: Option<String>,
    pub attempt_fingerprints: Vec<String>,
    pub signals: Vec<String>,
}

/// A change of the state of a unique error after a launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorStateChange {
    pub fingerprint: String,
    pub from: Option<ErrorState>,
    pub to: ErrorState,
    pub branch: String,
    pub message: String,
}

// Suggestions and memory (S).

/// State of a suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionState {
    Open,
    Accepted,
    Rejected,
    Superseded,
    Expired,
}

/// The decision a suggestion learned from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSample {
    pub result_id: Option<Uuid>,
    pub launch_id: Option<Uuid>,
    pub launch_number: Option<i64>,
    pub decided_by: String,
    pub decided_at: DateTime<Utc>,
    pub confirmed: i32,
    pub overridden: i32,
}

/// A triage suggestion for a cluster.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub id: Uuid,
    pub rank: i16,
    pub band: SuggestionBand,
    pub score: f64,
    /// The band's precision in the latest backtest with at least 50 decisions in it.
    pub precision: Option<f64>,
    pub defect_type_id: Uuid,
    pub group: String,
    pub defect_name: String,
    pub comment: Option<String>,
    pub issue_url: Option<String>,
    pub issue_key: Option<String>,
    pub signal: SuggestionSignal,
    pub why: Vec<WhyItem>,
    pub sample: Option<DecisionSample>,
    pub rule_id: Option<Uuid>,
    pub state: SuggestionState,
    pub rule_draft_offered: bool,
}

/// The commits between the last good launch and the first failing one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommitRange {
    pub last_good: Option<String>,
    pub first: Option<String>,
    pub compare_url: Option<String>,
}

/// Who decided a cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ClusterDecided {
    Person,
    Rule,
}

/// Why a cluster has no suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum NoSuggestion {
    NewError,
    NoHistory,
    BelowThreshold,
    Decided,
}

/// Signals of a cluster's results, counted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptSignalBrief {
    pub source: SignalSource,
    pub kind: String,
    pub confidence: SignalConfidence,
    pub count: i64,
}

/// A cluster of a launch with its suggestions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSuggestions {
    pub fingerprint: String,
    pub message: Option<String>,
    pub count: i64,
    pub sample_result_id: Uuid,
    pub defect: Option<Defect>,
    pub decided: Option<ClusterDecided>,
    pub is_new: bool,
    pub new_on_branch: bool,
    pub error_state: Option<ErrorState>,
    pub commits: Option<CommitRange>,
    pub signals: Vec<AttemptSignalBrief>,
    pub suggestions: Vec<Suggestion>,
    pub no_suggestion: Option<NoSuggestion>,
}

/// State of the suggestions of a launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum LaunchSuggestionsState {
    Disabled,
    Pending,
    Ready,
    Partial,
}

/// `GET .../launches/{id}/suggestions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSuggestions {
    pub launch_id: Uuid,
    pub state: LaunchSuggestionsState,
    pub computed_at: Option<DateTime<Utc>>,
    pub clusters: Vec<ClusterSuggestions>,
    pub truncated: bool,
}

/// What an accepted suggestion applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SuggestionApplyTo {
    Cluster,
    Results,
}

/// Body of `POST .../suggestions/{id}/accept`.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuggestionAccept {
    pub apply_to: Option<SuggestionApplyTo>,
    /// With `applyTo: results`, at most 1000 of the cluster.
    pub result_ids: Option<Vec<Uuid>>,
    pub comment: Option<String>,
    pub issue: Option<TriageIssue>,
}

/// Outcome of an accepted suggestion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionAccepted {
    pub suggestion_id: Uuid,
    pub updated: u64,
    pub skipped: u64,
    pub batch_id: Option<Uuid>,
}

/// Body of `POST .../suggestions/{id}/reject`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuggestionReject {
    /// At most 500 bytes.
    pub reason: Option<String>,
}

/// A memory entry as the API shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub fingerprint: String,
    pub defect_type_id: Uuid,
    pub group: String,
    pub defect_name: String,
    pub message_norm: String,
    pub frames: Vec<String>,
    pub confirmed: i32,
    pub overridden: i32,
    pub comment: Option<String>,
    pub issue_url: Option<String>,
    pub issue_key: Option<String>,
    pub test_cases: i64,
    pub branches: Vec<String>,
    pub environments: Vec<String>,
    pub first_decided_at: DateTime<Utc>,
    pub last_decided_at: DateTime<Utc>,
    pub last_decided_by: String,
    pub sample: Option<DecisionSample>,
}

/// `GET .../analysis/memory`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryList {
    pub items: Vec<MemoryEntry>,
}

/// Body of `POST .../analysis/backtest`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BacktestRequest {
    /// 1 to 730, default 180.
    pub days: Option<u32>,
}

/// Precision of one band in a backtest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BacktestBand {
    pub band: SuggestionBand,
    pub suggested: u64,
    pub correct: u64,
    pub correct_group: u64,
    pub precision: Option<f64>,
    /// `suggested` reached 50.
    pub shown: bool,
}

/// A frequent mistake in a backtest (group names).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BacktestConfusion {
    pub expected: String,
    pub suggested: String,
    pub count: u64,
}

/// Where the events of a backtest came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum BacktestSource {
    Project,
    Synthetic,
    Events,
}

/// A backtest report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BacktestReport {
    pub id: Uuid,
    pub source: BacktestSource,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub decisions: u64,
    pub coverage: f64,
    pub no_suggestion: u64,
    pub bands: Vec<BacktestBand>,
    pub confusions: Vec<BacktestConfusion>,
    pub truncated: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

// Unique errors (E).

/// Order of the errors list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorSort {
    #[default]
    LastSeen,
    FirstSeen,
    Occurrences,
    Launches,
}

/// State of the issue linked to an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorIssueState {
    Open,
    Closed,
    Creating,
}

/// The issue linked to an error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorIssue {
    pub url: String,
    pub key: Option<String>,
    pub state: ErrorIssueState,
}

/// A unique error in a list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSummary {
    pub fingerprint: String,
    pub message: String,
    pub state: ErrorState,
    pub state_at: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub occurrences: i64,
    pub tests: i64,
    pub launches: i64,
    pub branches: Vec<String>,
    pub regressed_count: i32,
    pub alias_of: Option<String>,
    pub aliases: i64,
    pub issue: Option<ErrorIssue>,
    pub group: Option<String>,
    /// 14 daily counts, oldest first.
    pub spark: Vec<i64>,
}

/// `GET .../errors`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorList {
    pub items: Vec<ErrorSummary>,
    pub next: Option<String>,
}

/// Query of the errors list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorQuery {
    pub state: Option<ErrorState>,
    pub branch: Option<String>,
    pub q: Option<String>,
    pub sort: ErrorSort,
    pub before: Option<String>,
    pub limit: i64,
}

/// An error on one branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBranch {
    pub branch: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_launch_id: Option<Uuid>,
    pub occurrences: i64,
    pub launches_since: i64,
}

/// A test an error occurs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorTest {
    pub test_case_id: Uuid,
    pub name: String,
    pub occurrences: i64,
}

/// Occurrences of an error on one day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorTimelinePoint {
    pub day: chrono::NaiveDate,
    pub occurrences: i64,
    pub launches: i64,
}

/// Backend of an analytics answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum AnalyticsBackend {
    Postgres,
    Clickhouse,
}

/// The window an analytics answer covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsWindow {
    pub backend: AnalyticsBackend,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// Less than asked for (the PostgreSQL fallback covers at most 30 days).
    pub partial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `GET .../errors/{fingerprint}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    #[serde(flatten)]
    pub summary: ErrorSummary,
    pub alias_list: Vec<String>,
    pub branch_list: Vec<ErrorBranch>,
    pub commits: Option<CommitRange>,
    pub mttr_seconds: Option<i64>,
    pub decisions: Vec<MemoryEntry>,
    pub test_list: Vec<ErrorTest>,
    pub timeline: Vec<ErrorTimelinePoint>,
    pub window: AnalyticsWindow,
}

/// `GET .../errors/{fingerprint}/timeline`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorTimeline {
    pub window: AnalyticsWindow,
    pub items: Vec<ErrorTimelinePoint>,
}

/// State a person may set on an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorPatchState {
    Ignored,
    Active,
}

/// Body of `PATCH .../errors/{fingerprint}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ErrorPatchState>,
    /// The canonical error this one is an alias of; `null` splits it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub alias_of: Option<Option<String>>,
}

// Gate analytics (G). Metric, GateStatus, Gate and RuleResult are server types: here they are
// stored as their JSON and referenced by name in the schema.

/// A failed rule of a gate evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateRuleOutcome {
    pub gate: String,
    #[schema(schema_with = metric_ref)]
    pub metric: String,
    pub actual: Option<f64>,
    pub expected: f64,
}

/// One gate evaluation in the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateHistoryItem {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub branch: Option<String>,
    pub evaluated_at: DateTime<Utc>,
    #[schema(schema_with = gate_status_ref)]
    pub status: String,
    pub failed: Vec<GateRuleOutcome>,
}

/// Aggregates of one gate over the window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateHistoryGate {
    pub gate: String,
    pub evaluations: i64,
    pub failed: i64,
    pub flips: i64,
    pub mean_seconds_to_green: Option<f64>,
    #[schema(schema_with = metric_ref, nullable)]
    pub top_failing_metric: Option<String>,
}

/// Evaluations per branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateHistoryBranch {
    pub branch: String,
    pub evaluations: i64,
    pub failed: i64,
}

/// `GET .../gates/history`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateHistory {
    pub items: Vec<GateHistoryItem>,
    pub next: Option<String>,
    pub gates: Vec<GateHistoryGate>,
    pub branches: Vec<GateHistoryBranch>,
}

/// Query of the gate history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateHistoryQuery {
    pub gate: Option<String>,
    pub branch: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub before: Option<String>,
    pub limit: i64,
}

/// Body of `POST .../gates/simulate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateSimulate {
    #[schema(schema_with = gate_ref)]
    pub gate: serde_json::Value,
    /// 1 to 200, default 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launches: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// A simulated evaluation of one launch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateSimulationItem {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub branch: Option<String>,
    pub created_at: DateTime<Utc>,
    #[schema(schema_with = gate_status_ref)]
    pub status: String,
    #[schema(schema_with = rule_result_list)]
    pub results: Vec<serde_json::Value>,
}

/// `POST .../gates/simulate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GateSimulation {
    pub evaluated: u64,
    pub failed: u64,
    pub skipped: u64,
    pub items: Vec<GateSimulationItem>,
}

// Log patterns (P).

/// A log stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum LogStreamName {
    Stdout,
    Stderr,
    Log,
    Step,
}

/// `match.log` of a rule: a pattern on the tail of failed attempts' logs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleLogMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<Vec<LogLevel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<Vec<LogStreamName>>,
    /// 1024 to 1048576.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bytes: Option<u32>,
}

/// `match.signals[]` of a rule: a stored signal of this source (and kind) on the result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleSignalMatch {
    pub source: SignalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// A pattern mark on a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResultPattern {
    pub pattern: String,
    pub rule_id: Uuid,
    pub attempt: i32,
    /// At most 300 characters.
    pub excerpt: String,
}

/// Results and launches marked with a pattern.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatternCount {
    pub pattern: String,
    pub results: i64,
    pub launches: i64,
    pub last_seen_at: DateTime<Utc>,
}

/// `GET .../analysis/patterns`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatternCounts {
    pub items: Vec<PatternCount>,
}

/// What the `PopularPatterns` widget counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WidgetPatterns {
    Fingerprints,
    LogPatterns,
}

// Signals (K, night B).

/// How a signal was matched to its attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum SignalConfidence {
    Exact,
    Window,
}

/// One pushed signal (`source` is `grid`, `ci` or `k8s`; `mock` signals are written by the
/// server only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignalItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_client_id: Option<String>,
    pub source: SignalSource,
    pub kind: String,
    pub at: DateTime<Utc>,
    /// Flat string, number or boolean values; at most 4 KiB serialized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub detail: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Body of `POST .../signals`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignalPush {
    /// 1 to 1000.
    pub items: Vec<SignalItem>,
}

/// A refused item of a push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignalRejection {
    pub index: u32,
    pub code: String,
}

/// `POST .../signals`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignalsAccepted {
    pub accepted: u64,
    pub matched: u64,
    pub pending: u64,
    pub rejected: Vec<SignalRejection>,
}

/// A stored signal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptSignal {
    pub id: i64,
    pub result_id: Option<Uuid>,
    pub attempt: Option<i32>,
    pub source: SignalSource,
    pub kind: String,
    pub at: DateTime<Utc>,
    pub confidence: SignalConfidence,
    #[schema(value_type = Object)]
    pub detail: serde_json::Map<String, serde_json::Value>,
}

/// `GET .../results/{id}/signals`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptSignals {
    pub items: Vec<AttemptSignal>,
}

// Flaky causes (H, night B).

/// What a hint suggests doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum HintAction {
    Mute,
    Issue,
    Rule,
}

/// A likely cause of a flaky test.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyHint {
    pub kind: FlakyHintKind,
    pub confidence: HintConfidence,
    pub summary: String,
    #[schema(value_type = Object)]
    pub evidence: serde_json::Map<String, serde_json::Value>,
    pub sample: u32,
    pub actions: Vec<HintAction>,
}

/// A hint in the flaky list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyHintBrief {
    pub kind: FlakyHintKind,
    pub confidence: HintConfidence,
}

/// `GET .../flaky/{seriesId}/hints`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyHints {
    pub series_id: Uuid,
    pub computed_at: Option<DateTime<Utc>>,
    pub window_runs: i32,
    pub failures: i32,
    pub infra_failures: i32,
    pub infra_share: Option<f64>,
    pub backend: AnalyticsBackend,
    pub hints: Vec<FlakyHint>,
}

// Analytics (C, night B).

/// Grouping of the failure analytics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FailureGroupBy {
    #[default]
    Fingerprint,
    DefectGroup,
    Suite,
    Env,
}

/// Query of the failure analytics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailureQuery {
    pub group_by: FailureGroupBy,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub branch: Option<String>,
    pub env: Option<String>,
    pub limit: i64,
}

/// One group of failures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureGroup {
    pub key: String,
    pub label: String,
    pub failures: i64,
    pub tests: i64,
    pub launches: i64,
    pub share: f64,
    pub trend: Vec<i64>,
}

/// `GET .../analytics/failures`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailureAnalytics {
    pub window: AnalyticsWindow,
    pub group_by: FailureGroupBy,
    pub total: i64,
    pub items: Vec<FailureGroup>,
}

/// A test that became slower.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DurationRegression {
    pub series_id: Uuid,
    pub test_case_id: Uuid,
    pub name: String,
    pub p50_recent_ms: i64,
    pub p50_before_ms: i64,
    pub ratio: f64,
    pub runs: i64,
}

/// `GET .../analytics/durations`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DurationRegressions {
    pub window: AnalyticsWindow,
    pub items: Vec<DurationRegression>,
}

/// Time spent on attempts that did not become final, per suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetryCost {
    pub suite: String,
    pub wasted_ms: i64,
    pub retries: i64,
    pub tests: i64,
}

/// `GET .../analytics/retries`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetryCosts {
    pub window: AnalyticsWindow,
    pub total_wasted_ms: i64,
    pub items: Vec<RetryCost>,
}

/// Flaky results on one day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyTrendPoint {
    pub day: chrono::NaiveDate,
    pub flaky: i64,
    pub runs: i64,
}

/// `GET .../analytics/flaky-trend`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyTrend {
    pub window: AnalyticsWindow,
    pub items: Vec<FlakyTrendPoint>,
}

/// Resolved errors up to `le` seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MttrBucket {
    pub le: i64,
    pub count: i64,
}

/// `GET .../analytics/mttr`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MttrStats {
    pub window: AnalyticsWindow,
    pub resolved: i64,
    pub p50_seconds: Option<i64>,
    pub p90_seconds: Option<i64>,
    pub buckets: Vec<MttrBucket>,
}

/// A slow step name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SlowStep {
    pub name: String,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub count: i64,
}

/// `GET .../analytics/slow-steps`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SlowSteps {
    pub window: AnalyticsWindow,
    pub items: Vec<SlowStep>,
}

/// A project in the portfolio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortfolioProject {
    pub key: String,
    pub name: String,
    pub launches: i64,
    pub pass_rate: Option<f64>,
    pub failures: i64,
    pub new_errors: i64,
    pub regressed_errors: i64,
    pub flaky_tests: i64,
    pub mttr_p50_seconds: Option<i64>,
}

/// `GET /api/v1/analytics/portfolio`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Portfolio {
    pub window: AnalyticsWindow,
    pub items: Vec<PortfolioProject>,
}

/// A similar failure in another readable project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrossProjectSimilarItem {
    pub project: String,
    pub result_id: Uuid,
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub name: String,
    pub message: Option<String>,
    pub score: f64,
    pub group: Option<String>,
}

/// `GET /api/v1/analytics/similar`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrossProjectSimilar {
    pub items: Vec<CrossProjectSimilarItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults_validate_and_reject_unknown_keys() {
        let d = AnalysisSettings::default();
        assert!(d.is_default());
        d.validate().unwrap();
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"enabled": true, "memoryDays": 365, "resolveAfterLaunches": 10,
                "suggest": {"minScore": 0.4, "maxCandidates": 3},
                "autoApply": {"enabled": false, "bands": ["same"]},
                "backtest": {"nightly": true}, "flaky": {"hintMinScore": 0.2}})
        );
        let partial: AnalysisSettings =
            serde_json::from_value(serde_json::json!({"enabled": false})).unwrap();
        assert!(!partial.enabled && partial.memory_days == 365);
        for bad in [
            serde_json::json!({"ai": {"enabled": true}}),
            serde_json::json!({"suggest": {"minScore": 0.5, "x": 1}}),
        ] {
            assert!(serde_json::from_value::<AnalysisSettings>(bad).is_err());
        }
        let s = AnalysisSettings {
            memory_days: 29,
            ..Default::default()
        };
        assert!(s.validate().unwrap_err().contains("memoryDays"));
        let mut s = AnalysisSettings::default();
        s.suggest.min_score = 0.39;
        assert!(s.validate().unwrap_err().contains("minScore"));
        let mut s = AnalysisSettings::default();
        s.auto_apply.bands = vec![SuggestionBand::High];
        assert!(s.validate().unwrap_err().contains("bands"));
        s.auto_apply.bands.clear();
        assert!(s.validate().is_err());
        let mut s = AnalysisSettings::default();
        s.flaky.hint_min_score = 1.5;
        assert!(s.validate().unwrap_err().contains("hintMinScore"));
    }

    #[test]
    fn step_outcomes_and_names() {
        let s = StepOutcome::skipped("not available").timed(3);
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"status": "skipped", "millis": 3, "detail": {"reason": "not available"}})
        );
        let steps = LaunchSteps {
            log_rules: Some(StepOutcome::done(serde_json::json!({"rules": 1}))),
            ..Default::default()
        };
        let v = serde_json::to_value(&steps).unwrap();
        assert!(v.get("logRules").is_some() && v.get("errors").is_none());
        assert_eq!(
            serde_json::to_value(TriageUndoSkipCode::ChangedSince).unwrap(),
            "changed_since"
        );
        for v in [
            DecisionVia::Api,
            DecisionVia::Mcp,
            DecisionVia::Rp,
            DecisionVia::Rule,
            DecisionVia::Retro,
            DecisionVia::Suggestion,
            DecisionVia::Auto,
            DecisionVia::Undo,
        ] {
            assert_eq!(DecisionVia::parse(v.as_str()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
        let patch: ErrorPatch =
            serde_json::from_value(serde_json::json!({"aliasOf": null})).unwrap();
        assert_eq!(patch.alias_of, Some(None));
    }
}
