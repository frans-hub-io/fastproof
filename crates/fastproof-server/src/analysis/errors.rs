//! Unique errors of the analysis role: the `errors` step and the issue-state job (contract
//! §7.4).
//!
//! Owner: E. Until it fills this file every entry point is a stub.

use fastproof_store::analysis::types::StepOutcome;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build tracks unique errors (`AnalysisFeatures.errors`).
pub const AVAILABLE: bool = false;

/// Step `errors`. Stub: skipped.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let _ = (s, job);
    Ok(StepOutcome::skipped("not available"))
}

/// Job `analysis.errors.issues` (1 h). Stub.
pub async fn issues(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}
