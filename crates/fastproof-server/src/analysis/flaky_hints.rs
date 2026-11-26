//! Flaky causes of the analysis role: the `flaky_hints` step and the daily job (contract
//! §7.8). The step recomputes the hints of the series whose status changed in the launch (a
//! transition, or flaky against earlier runs or within the run) and that are flaky or score at
//! least `analysis.flaky.hintMinScore`; the daily job recomputes the other flaky series and drops
//! the hints of series that settled. With ClickHouse the window is the whole history there
//! (and the `time` kind can speak); without it the last `rawDays` days, at most 50 runs.
//!
//! Both are bounded in wall time: the step by [`STEP_BUDGET`] (the series left are the daily
//! job's), the daily job by [`DAILY_BUDGET`], shared fairly between the projects (each gets the
//! time left divided by the projects left, starting from a project that changes every day). A
//! series that cannot be computed is skipped, not retried in a loop, and once ClickHouse fails
//! to answer the rest of the step or run reads PostgreSQL without asking it again.

use std::future::Future;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_store::Project;
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::types::{AnalyticsBackend, SeriesRun, StepOutcome};
use serde_json::json;
use uuid::Uuid;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build computes flaky-cause hints (`AnalysisFeatures.flakyHints`).
pub const AVAILABLE: bool = true;
/// Wall time of the step for one launch, at most (the series left are recomputed by the daily
/// job); the launch's own `launch_timeout_seconds` still applies when it is shorter.
pub const STEP_BUDGET: Duration = Duration::from_secs(30);
/// Wall time of one daily run, at most (the series left come first the next day).
pub const DAILY_BUDGET: Duration = Duration::from_secs(1800);
/// Series recomputed per project and daily run, at most.
pub const DAILY_PROJECT_SERIES: usize = 5000;

/// Counts of recomputed series and why a batch stopped.
#[derive(Debug, Default)]
pub struct Batch {
    pub series: u64,
    pub with_hints: u64,
    pub clickhouse: u64,
    /// Series whose computation failed (skipped; logged).
    pub failed: u64,
    /// The deadline came before every series was computed.
    pub out_of_time: bool,
    /// ClickHouse is configured but did not answer: the rest reads PostgreSQL.
    pub clickhouse_down: bool,
    last_error: Option<String>,
}

/// Recomputes the hints of `ids` until `deadline`. `history` reads a series' ClickHouse window;
/// it is asked only while `clickhouse` is set and has not failed in this batch (an error or no
/// answer turns it off for the rest of `b`, so a slow or broken ClickHouse costs one wait, not
/// one per series). Errors of a series are logged and counted, and the next series is computed.
#[allow(clippy::too_many_arguments)]
pub async fn recompute_with<F, Fut>(
    s: &AppState,
    project: &Project,
    settings: &ProjectSettings,
    ids: &[Uuid],
    deadline: Instant,
    clickhouse: bool,
    b: &mut Batch,
    mut history: F,
) where
    F: FnMut(Uuid) -> Fut,
    Fut: Future<Output = anyhow::Result<Option<Vec<SeriesRun>>>>,
{
    for id in ids {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            b.out_of_time = true;
            return;
        };
        if left.is_zero() {
            b.out_of_time = true;
            return;
        }
        let h = if clickhouse && !b.clickhouse_down {
            match tokio::time::timeout(left, history(*id)).await {
                Ok(Ok(Some(h))) => Some(h),
                Ok(Ok(None)) => {
                    b.clickhouse_down = true;
                    tracing::warn!(project = %project.key,
                        "ClickHouse gave no series history; flaky causes read PostgreSQL for the rest of this run");
                    None
                }
                Ok(Err(e)) => {
                    b.clickhouse_down = true;
                    tracing::warn!(project = %project.key, series = %id, error = %format!("{e:#}"),
                        "reading the series history from ClickHouse failed; flaky causes read PostgreSQL for the rest of this run");
                    None
                }
                Err(_) => {
                    b.out_of_time = true;
                    return;
                }
            }
        } else {
            None
        };
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            b.out_of_time = true;
            return;
        };
        let r = tokio::time::timeout(
            left,
            s.store
                .compute_flaky_hints(project.id, *id, settings, h, Utc::now()),
        )
        .await;
        match r {
            Ok(Ok(Some(out))) => {
                b.series += 1;
                b.with_hints += !out.hints.is_empty() as u64;
                b.clickhouse += (out.backend == AnalyticsBackend::Clickhouse) as u64;
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => {
                b.failed += 1;
                let e = format!("{e:#}");
                tracing::warn!(project = %project.key, series = %id, error = %e,
                    "computing the flaky causes of a series failed; skipped");
                b.last_error = Some(e);
            }
            Err(_) => {
                b.out_of_time = true;
                return;
            }
        }
    }
}

async fn recompute(
    s: &AppState,
    project: &Project,
    settings: &ProjectSettings,
    ids: &[Uuid],
    deadline: Instant,
    b: &mut Batch,
) {
    recompute_with(
        s,
        project,
        settings,
        ids,
        deadline,
        s.clickhouse.is_some(),
        b,
        |id| super::clickhouse::series_history(s, project, id, super::CLICKHOUSE_DAYS),
    )
    .await
}

/// Step `flaky_hints`.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let min = job.settings.analysis.flaky.hint_min_score;
    let (ids, more) = s
        .store
        .flaky_hint_targets(job.project.id, &job.launch, min)
        .await?;
    let budget = STEP_BUDGET.min(Duration::from_secs(job.limits.launch_timeout_seconds));
    let mut b = Batch::default();
    recompute(
        s,
        &job.project,
        &job.settings,
        &ids,
        Instant::now() + budget,
        &mut b,
    )
    .await;
    if b.series == 0 && b.failed > 0 {
        anyhow::bail!(b.last_error.unwrap_or_default());
    }
    let mut detail = json!({
        "series": b.series,
        "withHints": b.with_hints,
        "backend": if b.clickhouse > 0 { "clickhouse" } else { "postgres" },
    });
    if more {
        detail["truncated"] = json!(true);
    }
    if b.out_of_time {
        metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "flaky_hints")
            .increment(1);
        detail["outOfTime"] = json!(true);
    }
    if b.failed > 0 {
        detail["failed"] = json!(b.failed);
    }
    if b.clickhouse_down {
        detail["clickhouseDown"] = json!(true);
    }
    Ok(if more || b.out_of_time || b.failed > 0 {
        StepOutcome::partial(detail)
    } else {
        StepOutcome::done(detail)
    })
}

/// Time of the next project of a daily run: the time left shared by the projects left.
pub fn project_share(left: Duration, projects_left: usize) -> Duration {
    left / projects_left.clamp(1, u32::MAX as usize) as u32
}

/// Index of the project a daily run starts from: one further every day, so a project late in
/// the order is not always the one the budget runs out before.
pub fn first_project(day: DateTime<Utc>, projects: usize) -> usize {
    if projects == 0 {
        return 0;
    }
    (day.timestamp().div_euclid(86_400)).rem_euclid(projects as i64) as usize
}

/// What a daily run did.
#[derive(Debug, Default)]
pub struct DailyRun {
    /// Projects whose series were looked at.
    pub projects: u64,
    pub series: u64,
    pub failed: u64,
    pub dropped: u64,
    /// Projects whose share of the time ran out before their last series.
    pub out_of_time: u64,
}

/// Job `analysis.flaky.daily` (`41 2 * * *`): every project with Analysis on, its flaky series
/// whose hints are missing or older than a day, within [`DAILY_BUDGET`].
pub async fn daily(s: &AppState) -> anyhow::Result<()> {
    let r = daily_within(s, DAILY_BUDGET, Utc::now()).await?;
    if r.series > 0 || r.dropped > 0 || r.failed > 0 {
        tracing::info!(
            projects = r.projects,
            series = r.series,
            dropped = r.dropped,
            failed = r.failed,
            out_of_time = r.out_of_time,
            "flaky causes recomputed"
        );
    }
    Ok(())
}

/// [`daily`] with its budget and day given: each project gets [`project_share`] of the time
/// left, starting from [`first_project`]; a project whose reads fail is logged and skipped.
pub async fn daily_within(
    s: &AppState,
    budget: Duration,
    day: DateTime<Utc>,
) -> anyhow::Result<DailyRun> {
    let end = Instant::now() + budget;
    let mut projects: Vec<(Uuid, String, ProjectSettings)> = s
        .store
        .all_project_settings()
        .await?
        .into_iter()
        .filter(|(_, _, st)| st.analysis.enabled)
        .collect();
    let start = first_project(day, projects.len());
    projects.rotate_left(start);
    let n = projects.len();
    let mut run = DailyRun::default();
    let mut b = Batch::default();
    for (i, (id, key, settings)) in projects.into_iter().enumerate() {
        let Some(left) = end
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
        else {
            tracing::info!("flaky causes: daily budget spent; the rest goes first tomorrow");
            break;
        };
        let deadline = Instant::now() + project_share(left, n - i);
        let min = settings.analysis.flaky.hint_min_score;
        match s.store.drop_settled_flaky_hints(id, min).await {
            Ok(n) => run.dropped += n,
            Err(e) => tracing::warn!(project = %key, error = %format!("{e:#}"),
                "dropping settled flaky hints failed"),
        }
        let project = match s.store.project(&key).await {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(project = %key, error = %format!("{e:#}"),
                    "reading the project failed; its flaky causes are retried tomorrow");
                continue;
            }
        };
        let ids = match s
            .store
            .stale_flaky_hint_series(id, min, Utc::now(), DAILY_PROJECT_SERIES as i64)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(project = %key, error = %format!("{e:#}"),
                    "listing stale flaky series failed; the project is retried tomorrow");
                continue;
            }
        };
        run.projects += 1;
        let (series, failed) = (b.series, b.failed);
        b.out_of_time = false;
        recompute(s, &project, &settings, &ids, deadline, &mut b).await;
        run.series += b.series - series;
        run.failed += b.failed - failed;
        run.out_of_time += b.out_of_time as u64;
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daily_budget_is_shared_and_the_start_moves() {
        let left = Duration::from_secs(1800);
        assert_eq!(project_share(left, 3), Duration::from_secs(600));
        assert_eq!(project_share(left, 1), left);
        assert_eq!(project_share(left, 0), left);
        let day = |d: i64| DateTime::<Utc>::from_timestamp(d * 86_400 + 9_000, 0).unwrap();
        assert_eq!(first_project(day(0), 0), 0);
        let starts: Vec<usize> = (0..6).map(|d| first_project(day(d), 3)).collect();
        assert_eq!(starts, vec![0, 1, 2, 0, 1, 2]);
    }
}
