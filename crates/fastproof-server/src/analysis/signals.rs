//! Signals of the analysis role: the `signals` step (FastMock phase 1, resolution) and the
//! resolution job (contract §7.7).
//!
//! Owner: K (night B). Until it fills this file every entry point is a stub.

use fastproof_store::analysis::types::StepOutcome;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build takes signals (`AnalysisFeatures.signals`).
pub const AVAILABLE: bool = false;

/// Step `signals`. Stub: skipped.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let _ = (s, job);
    Ok(StepOutcome::skipped("not available"))
}

/// Job `analysis.signals.resolve` (30 s). Stub.
pub async fn resolve(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}
