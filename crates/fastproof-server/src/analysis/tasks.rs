//! The `analysis_tasks` kinds of the analysis role (`backtest`, `log_rule.apply`): claimed by
//! job `analysis.tasks.run` and dispatched to their area. The standard `analysis.tasks` job
//! runs only `rule.apply`.

use std::time::{Duration, Instant};

use fastproof_store::analysis::types::ClaimedTask;

use crate::state::AppState;

/// Task kinds of the analysis role.
pub const KINDS: [&str; 2] = ["backtest", "log_rule.apply"];
/// Lease of a claimed task; renewed by the task's progress.
pub const LEASE: Duration = Duration::from_secs(600);

/// Runs a claimed task with the code of its kind.
pub async fn dispatch(s: &AppState, task: &ClaimedTask) -> anyhow::Result<serde_json::Value> {
    match task.kind.as_str() {
        "backtest" => super::suggestions::run_backtest_task(s, task).await,
        "log_rule.apply" => super::log_rules::run_retro_task(s, task).await,
        other => anyhow::bail!("unknown task kind {other}"),
    }
}

/// Claims and runs tasks of [`KINDS`] one at a time until none is left or `budget` is spent;
/// returns how many ran.
pub async fn run(s: &AppState, budget: Duration) -> anyhow::Result<u64> {
    let owner = format!("{}-analysis-tasks", s.instance);
    let started = Instant::now();
    let mut n = 0;
    while started.elapsed() < budget {
        let Some(task) = s.store.claim_analysis_task(&owner, &KINDS, LEASE).await? else {
            break;
        };
        let t = Instant::now();
        let r = dispatch(s, &task).await;
        metrics::histogram!("fastproof_analysis_job_seconds", "job" => format!("task.{}", task.kind))
            .record(t.elapsed().as_secs_f64());
        let outcome = r.map_err(|e| format!("{e:#}"));
        if let Err(e) = &outcome {
            tracing::warn!(task = %task.id, kind = %task.kind, error = %e, "an analysis task failed");
        }
        s.store.finish_analysis_task(&task, outcome).await?;
        n += 1;
    }
    Ok(n)
}
