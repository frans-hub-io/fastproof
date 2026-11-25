//! Log rules of the analysis role: the `log_rules` step and the `log_rule.apply` task
//! (contract §7.6).
//!
//! Owner: P. Until it fills this file every entry point is a stub.

use fastproof_store::analysis::types::{ClaimedTask, StepOutcome};

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build evaluates log rules (`AnalysisFeatures.logPatterns`).
pub const AVAILABLE: bool = false;

/// Step `log_rules`. Stub: skipped.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let _ = (s, job);
    Ok(StepOutcome::skipped("not available"))
}

/// Task kind `log_rule.apply`. Stub: fails the task.
pub async fn run_retro_task(s: &AppState, task: &ClaimedTask) -> anyhow::Result<serde_json::Value> {
    let _ = (s, task);
    anyhow::bail!("not available")
}
