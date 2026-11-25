//! Flaky causes of the analysis role: the `flaky_hints` step and the daily job (contract
//! §7.8).
//!
//! Owner: H (night B). Until it fills this file every entry point is a stub.

use fastproof_store::analysis::types::StepOutcome;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build computes flaky-cause hints (`AnalysisFeatures.flakyHints`).
pub const AVAILABLE: bool = false;

/// Step `flaky_hints`. Stub: skipped.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let _ = (s, job);
    Ok(StepOutcome::skipped("not available"))
}

/// Job `analysis.flaky.daily` (`41 2 * * *`). Stub.
pub async fn daily(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}
