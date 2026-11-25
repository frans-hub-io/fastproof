//! Triage suggestions of the analysis role: the `suggestions` step, the memory fold and seed
//! jobs, the nightly backtest and the `backtest` task (contract §7.2, §7.3).

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use fastproof_store::analysis::memory::FOLD_BATCH;
use fastproof_store::analysis::suggest::AUTO_PRINCIPAL;
use fastproof_store::analysis::types::{
    BacktestReport, BacktestRequest, ClaimedTask, LaunchCtx, StepOutcome,
};
use serde_json::json;

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build computes suggestions (`AnalysisFeatures.suggestions`).
pub const AVAILABLE: bool = true;
/// Wall budget of one fold run.
pub const FOLD_BUDGET: Duration = Duration::from_secs(8);
/// Wall budget of one seed run (all projects).
pub const SEED_BUDGET: Duration = Duration::from_secs(40);
/// Projects seeded per run, at most.
pub const SEED_PROJECTS: i64 = 20;
/// Projects backtested per nightly run, at most.
pub const NIGHTLY_PROJECTS: i64 = 200;
/// Wall time of one nightly run, at most (the projects left come first the next night).
pub const NIGHTLY_BUDGET: Duration = Duration::from_secs(1800);
/// Clusters asked of ClickHouse history per launch, at most.
pub const HISTORY_LIMIT: usize = 20;

fn ctx<'a>(job: &'a LaunchJob) -> LaunchCtx<'a> {
    LaunchCtx {
        project_id: job.project.id,
        project_key: &job.project.key,
        launch: &job.launch,
        settings: &job.settings,
        limits: &job.limits,
        now: Utc::now(),
    }
}

/// Step `suggestions`: the candidates of every undecided cluster, then auto-apply when the
/// project enabled it and the analysis gate is open.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let c = ctx(job);
    let history = if s.clickhouse.is_some() {
        let facts = s.store.cluster_facts(&c).await?;
        super::clickhouse::history_candidates(s, &job.project, &facts, HISTORY_LIMIT)
            .await?
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut out = s.store.compute_suggestions(&c, &history).await?;
    if let Some(a) = s.store.auto_apply(&c).await? {
        out.detail.insert(
            "autoApplied".into(),
            json!({"clusters": a.clusters, "results": a.results, "batchId": a.batch_id,
                "capped": a.capped}),
        );
        if a.clusters > 0 {
            let mut e = franshub_access::AuditEvent::anonymous(
                AUTO_PRINCIPAL,
                Some("system"),
                "suggestion.autoApply",
                json!({"launchId": job.launch.id, "batchId": a.batch_id, "clusters": a.clusters,
                    "results": a.results}),
            );
            e.scope = Some(job.project.key.clone());
            s.platform.audit(e).await;
        }
    }
    Ok(out)
}

/// Job `analysis.memory.fold` (10 s): people's decisions into the memory.
pub async fn fold(s: &AppState) -> anyhow::Result<()> {
    let n = s
        .store
        .fold_decisions(
            FOLD_BATCH,
            FOLD_BUDGET,
            s.cfg.analysis.limits.memory_max_rows,
        )
        .await?;
    if n > 0 {
        tracing::debug!(rows = n, "decisions folded into the triage memory");
    }
    Ok(())
}

/// Job `analysis.memory.seed` (1 min): projects whose memory was never seeded learn from the
/// triage people made before the journal (once per project).
pub async fn seed(s: &AppState) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    for p in s.store.memory_seed_pending(SEED_PROJECTS).await? {
        let left = SEED_BUDGET.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        let settings = s.store.project_settings(p).await?;
        if !settings.analysis.enabled {
            continue;
        }
        let done = s
            .store
            .seed_triage_memory(p, &s.cfg.analysis.limits, left)
            .await?;
        if done {
            tracing::info!(project = %p, "the triage memory of a project is seeded");
        }
    }
    Ok(())
}

/// Sets `fastproof_analysis_backtest_precision{band}`: over the latest report of each project
/// with `analysis.backtest.nightly`, correct over suggested per band.
pub async fn precision_gauge(s: &AppState) -> anyhow::Result<()> {
    let reports = s.store.latest_backtests(10_000).await?;
    let mut sums: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    for r in &reports {
        for b in &r.bands {
            let e = sums.entry(b.band.as_str()).or_default();
            e.0 += b.correct;
            e.1 += b.suggested;
        }
    }
    for (band, (correct, suggested)) in sums {
        if suggested > 0 {
            metrics::gauge!("fastproof_analysis_backtest_precision", "band" => band)
                .set(correct as f64 / suggested as f64);
        }
    }
    Ok(())
}

/// Job `analysis.backtest.nightly` (`17 3 * * *`): a backtest of the default window for each
/// project with `analysis.backtest.nightly` and decisions newer than its latest report.
pub async fn backtest_nightly(s: &AppState) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    for (p, _) in s.store.backtest_due_projects(NIGHTLY_PROJECTS).await? {
        let left = NIGHTLY_BUDGET.saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        match s
            .store
            .run_backtest_within(
                p,
                &BacktestRequest::default(),
                AUTO_PRINCIPAL,
                &s.cfg.analysis.limits,
                left.min(fastproof_store::analysis::backtest::BACKTEST_BUDGET),
            )
            .await
        {
            Ok(r) => log_report(&r),
            Err(e) => {
                tracing::warn!(project = %p, error = %format!("{e:#}"), "a nightly backtest failed")
            }
        }
    }
    precision_gauge(s).await
}

fn log_report(r: &BacktestReport) {
    tracing::info!(
        decisions = r.decisions,
        coverage = r.coverage,
        "backtest done"
    );
}

/// Task kind `backtest` (`POST .../analysis/backtest`): the report of the project's history,
/// stored; the task's result names it.
pub async fn run_backtest_task(
    s: &AppState,
    task: &ClaimedTask,
) -> anyhow::Result<serde_json::Value> {
    let req: BacktestRequest = serde_json::from_value(task.params.clone()).unwrap_or_default();
    let r = s
        .store
        .run_backtest(
            task.project_id,
            &req,
            &task.created_by,
            &s.cfg.analysis.limits,
        )
        .await?;
    log_report(&r);
    precision_gauge(s).await?;
    Ok(
        json!({"backtestId": r.id, "decisions": r.decisions, "coverage": r.coverage,
        "truncated": r.truncated}),
    )
}
