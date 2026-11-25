//! Unique errors of the analysis role: the `errors` step and the issue-state job (contract
//! `76` §7.4).

use std::collections::BTreeMap;

use chrono::Utc;
use fastproof_store::analysis::types::{ErrorState, LaunchCtx, StepOutcome};
use serde_json::json;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build tracks unique errors (`AnalysisFeatures.errors`).
pub const AVAILABLE: bool = true;

/// Step `errors`: the branches and states of the launch's errors and of the errors it may have
/// resolved, then the `errorRegressed` deliveries of its regressions.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let ctx = LaunchCtx {
        project_id: job.project.id,
        project_key: &job.project.key,
        launch: &job.launch,
        settings: &job.settings,
        limits: &job.limits,
        now: Utc::now(),
    };
    let step = s.store.error_step(&ctx).await?;
    if let Some(reason) = step.skipped {
        return Ok(StepOutcome::skipped(reason));
    }
    let mut transitions: BTreeMap<&str, u64> = BTreeMap::new();
    for c in &step.changes {
        *transitions.entry(c.to.as_str()).or_default() += 1;
    }
    let regressed = step
        .changes
        .iter()
        .filter(|c| c.to == ErrorState::Regressed)
        .count();
    let mut detail = json!({
        "errors": step.errors,
        "evaluated": step.evaluated,
        "branch": step.branch,
        "transitions": transitions,
    });
    if regressed > 0 {
        match crate::integrations::notify::error_events(s, &job.project, &job.launch, &step.changes)
            .await
        {
            Ok(n) => detail["notified"] = json!(n),
            Err(e) => {
                tracing::warn!(project = %job.project.key, launch = %job.launch.id,
                    error = %format!("{e:#}"), "queueing errorRegressed deliveries failed");
                detail["notifyError"] = json!("queueing the deliveries failed");
            }
        }
    }
    Ok(if step.truncated {
        detail["truncated"] = json!(true);
        StepOutcome::partial(detail)
    } else {
        StepOutcome::done(detail)
    })
}

/// Job `analysis.errors.issues` (1 h): errors linked to a closed issue and open errors not seen
/// for a day are computed again (a closed issue resolves an error once a launch ran without it;
/// launches of other branches may have resolved a quiet one).
pub async fn issues(s: &AppState) -> anyhow::Result<()> {
    let projects: Vec<(uuid::Uuid, u32)> = s
        .store
        .all_project_settings()
        .await?
        .into_iter()
        .filter(|(_, _, st)| st.analysis.enabled)
        .map(|(id, _, st)| (id, st.analysis.resolve_after_launches))
        .collect();
    let ids: Vec<uuid::Uuid> = projects.iter().map(|p| p.0).collect();
    let k: BTreeMap<uuid::Uuid, u32> = projects.into_iter().collect();
    let mut changed = 0usize;
    for (project, fps) in s.store.error_recheck_targets(&ids).await? {
        let resolve_after = k.get(&project).copied().unwrap_or(10);
        match s
            .store
            .recompute_error_states(project, &fps, resolve_after)
            .await
        {
            Ok(changes) => {
                for c in &changes {
                    metrics::counter!("fastproof_analysis_error_transitions_total", "to" => c.to.as_str())
                        .increment(1);
                }
                changed += changes.len();
            }
            Err(e) => {
                tracing::warn!(project = %project, error = %format!("{e:#}"), "re-checking unique errors failed");
            }
        }
    }
    if changed > 0 {
        tracing::info!(changed, "unique errors re-checked");
    }
    Ok(())
}
