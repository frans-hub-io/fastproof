//! Triage suggestions of the analysis role: the `suggestions` step, the memory fold and seed
//! jobs, the nightly backtest and the `backtest` task (contract §7.2, §7.3).
//!
//! Owner: S. Until it fills this file every entry point is a stub.

use fastproof_store::analysis::types::{ClaimedTask, StepOutcome};

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build computes suggestions (`AnalysisFeatures.suggestions`).
pub const AVAILABLE: bool = false;

/// Step `suggestions`. Stub: skipped.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let _ = (s, job);
    Ok(StepOutcome::skipped("not available"))
}

/// Job `analysis.memory.fold` (10 s). Stub.
pub async fn fold(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}

/// Job `analysis.memory.seed` (1 min). Stub.
pub async fn seed(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}

/// Job `analysis.backtest.nightly` (`17 3 * * *`). Stub.
pub async fn backtest_nightly(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}

/// Task kind `backtest`. Stub: fails the task.
pub async fn run_backtest_task(
    s: &AppState,
    task: &ClaimedTask,
) -> anyhow::Result<serde_json::Value> {
    let _ = (s, task);
    anyhow::bail!("not available")
}
