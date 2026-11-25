//! The analysis of one launch: the steps of every area in a fixed order, each timed, isolated
//! from the others' failures and bounded by the launch's wall budget.

use std::time::{Duration, Instant};

use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::types::{
    AnalysisLaunchRow, AnalysisLimits, LaunchSteps, QueueReason, StepOutcome,
};
use fastproof_store::{Launch, Project};

use crate::state::AppState;

/// What every step of the analysis of a launch gets.
pub struct LaunchJob {
    pub project: Project,
    pub launch: Launch,
    pub settings: ProjectSettings,
    pub reason: QueueReason,
    pub limits: AnalysisLimits,
}

/// The steps, in the order they run.
pub const STEPS: [&str; 5] = ["errors", "signals", "logRules", "suggestions", "flakyHints"];

/// The job of a queue row; `None` when its project or launch is gone.
pub async fn job(s: &AppState, row: &AnalysisLaunchRow) -> anyhow::Result<Option<LaunchJob>> {
    let Some(key) = s.store.project_key(row.project_id).await? else {
        return Ok(None);
    };
    let Some(project) = s.store.project(&key).await? else {
        return Ok(None);
    };
    let Some(launch) = s.store.launch(row.project_id, row.launch_id).await? else {
        return Ok(None);
    };
    let settings = s.store.project_settings(row.project_id).await?;
    Ok(Some(LaunchJob {
        project,
        launch,
        settings,
        reason: row.reason,
        limits: s.cfg.analysis.limits.clone(),
    }))
}

async fn step(s: &AppState, job: &LaunchJob, name: &'static str) -> anyhow::Result<StepOutcome> {
    match name {
        "errors" => super::errors::on_launch(s, job).await,
        "signals" => super::signals::on_launch(s, job).await,
        "logRules" => super::log_rules::on_launch(s, job).await,
        "suggestions" => super::suggestions::on_launch(s, job).await,
        _ => super::flaky_hints::on_launch(s, job).await,
    }
}

fn slot<'a>(steps: &'a mut LaunchSteps, name: &str) -> &'a mut Option<StepOutcome> {
    match name {
        "errors" => &mut steps.errors,
        "signals" => &mut steps.signals,
        "logRules" => &mut steps.log_rules,
        "suggestions" => &mut steps.suggestions,
        _ => &mut steps.flaky_hints,
    }
}

/// Runs the analysis of a claimed queue row: builds its [`LaunchJob`] and runs the steps (none
/// when the project or the launch is gone).
pub async fn process(s: &AppState, row: &AnalysisLaunchRow) -> anyhow::Result<LaunchSteps> {
    Ok(match job(s, row).await? {
        Some(j) => run_steps(s, &j).await,
        None => LaunchSteps::default(),
    })
}

/// Runs the steps of a launch in the order of [`STEPS`]: each with its own timing
/// (`fastproof_analysis_job_seconds{job="step.<name>"}`) and error isolation (a failing step is
/// recorded `failed` and the next ones still run), within `launch_timeout_seconds` for the
/// whole launch (steps beyond it are `skipped`). A project with `analysis.enabled = false` has
/// every step skipped. Ends with the live event `analysis.updated {launchId, steps}`.
pub async fn run_steps(s: &AppState, job: &LaunchJob) -> LaunchSteps {
    run_steps_with(s, job, |name| Box::pin(step(s, job, name))).await
}

/// A step's future, as [`run_steps_with`] takes it.
pub type StepFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<StepOutcome>> + Send + 'a>>;

/// [`run_steps`] with the step code given (tests run failing or slow steps through it).
pub async fn run_steps_with<'a, F>(s: &'a AppState, job: &'a LaunchJob, mut run: F) -> LaunchSteps
where
    F: FnMut(&'static str) -> StepFuture<'a>,
{
    let mut steps = LaunchSteps::default();
    let started = Instant::now();
    let budget = Duration::from_secs(job.limits.launch_timeout_seconds);
    for name in STEPS {
        let outcome = if !job.settings.analysis.enabled {
            StepOutcome::skipped("analysis is off for the project")
        } else {
            match budget.checked_sub(started.elapsed()) {
                None => {
                    metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "launch")
                        .increment(1);
                    StepOutcome::skipped("the launch's analysis time ran out")
                }
                Some(left) => {
                    let t = Instant::now();
                    let r = tokio::time::timeout(left, run(name)).await;
                    let elapsed = t.elapsed();
                    metrics::histogram!("fastproof_analysis_job_seconds", "job" => format!("step.{name}"))
                        .record(elapsed.as_secs_f64());
                    let o = match r {
                        Ok(Ok(o)) => o,
                        Ok(Err(e)) => {
                            tracing::warn!(launch = %job.launch.id, step = name, error = %format!("{e:#}"),
                                "an analysis step failed");
                            StepOutcome::failed(&format!("{e:#}"))
                        }
                        Err(_) => {
                            metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "launch")
                                .increment(1);
                            StepOutcome::failed("the launch's analysis time ran out")
                        }
                    };
                    o.timed(elapsed.as_millis() as u64)
                }
            }
        };
        *slot(&mut steps, name) = Some(outcome);
    }
    if let Err(e) = notify(s, job, &steps).await {
        tracing::debug!(error = %format!("{e:#}"), "notifying analysis.updated failed");
    }
    steps
}

async fn notify(s: &AppState, job: &LaunchJob, steps: &LaunchSteps) -> anyhow::Result<()> {
    let mut conn = s.store.pool().acquire().await?;
    fastproof_store::events::notify(
        &mut conn,
        &fastproof_store::events::LiveEvent {
            project: job.project.key.clone(),
            kind: "analysis.updated".into(),
            data: serde_json::json!({"launchId": job.launch.id, "steps": steps}),
        },
    )
    .await
}
