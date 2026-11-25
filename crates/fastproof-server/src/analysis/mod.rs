//! The analysis role (contract `76` §4.4): with `FASTPROOF_ANALYSIS=on` on a replica whose
//! `FASTPROOF_ROLES` has `analysis`, a scheduler of the role's jobs (the jobs table is
//! cluster-wide and claimed by name, so several role replicas are safe) and
//! `FASTPROOF_ANALYSIS_WORKERS` launch workers. Finished launches come from the outbox
//! (`analysis.consume`), wait in `analysis_launches` and are analysed by [`pipeline::process`].

pub mod clickhouse;
pub mod errors;
pub mod flaky_hints;
pub mod log_rules;
pub mod pipeline;
pub mod signals;
pub mod suggestions;
pub mod tasks;

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use fastproof_store::analysis::types::{AnalysisFeatures, AnalysisLaunchRow, CapabilityInputs};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::scheduler::Scheduler;
use crate::state::AppState;

/// The outbox cursor of the analysis consumer (`integration_cursors.name`).
pub const CURSOR: &str = "analysis";
/// `launch.finished` events read per consumer run, at most.
pub const CONSUME_BATCH: i64 = 500;
/// Lease of a launch a worker analyses; renewed every [`RENEW`].
pub const LEASE: Duration = Duration::from_secs(60);
/// How often a worker renews its lease.
pub const RENEW: Duration = Duration::from_secs(20);
/// How long an idle worker waits before it looks again (it is woken by the consumer sooner).
pub const IDLE: Duration = Duration::from_secs(2);
/// Days of the ClickHouse aggregates until the ClickHouse settings carry them.
pub const CLICKHOUSE_DAYS: u32 = 730;

static WAKE: LazyLock<Notify> = LazyLock::new(Notify::new);

/// What [`run_once`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunOnce {
    /// Launches the consumer queued (`None`: another replica held the cursor).
    pub consumed: Option<u64>,
    /// Launches analysed.
    pub processed: u64,
}

/// The features whose code is in this build (each area flips its own constant).
pub fn implemented() -> AnalysisFeatures {
    AnalysisFeatures {
        suggestions: suggestions::AVAILABLE,
        errors: errors::AVAILABLE,
        log_patterns: log_rules::AVAILABLE,
        gate_analytics: crate::api::gates_history::AVAILABLE,
        flaky_hints: flaky_hints::AVAILABLE,
        signals: signals::AVAILABLE,
        analytics: clickhouse::ANALYTICS,
        portfolio: clickhouse::PORTFOLIO,
        cross_project_similar: clickhouse::CROSS_PROJECT_SIMILAR,
    }
}

/// What the capabilities of a project are computed from on this replica.
pub fn capability_inputs(s: &AppState) -> CapabilityInputs {
    CapabilityInputs {
        instance_enabled: s.cfg.analysis.enabled,
        clickhouse: s.clickhouse.is_some(),
        implemented: implemented(),
        clickhouse_days: CLICKHOUSE_DAYS,
        limits: s.cfg.analysis.limits.clone(),
    }
}

/// The scheduler jobs of the role.
pub fn jobs() -> anyhow::Result<Scheduler> {
    let mut sch = Scheduler::new();
    sch.register("analysis.consume", "2s", |s: AppState| async move {
        consume(&s).await.map(|_| ())
    })?;
    sch.register("analysis.prune", "1h", |s: AppState| async move {
        let t = Instant::now();
        let n = s.store.prune_analysis(Duration::from_secs(300)).await?;
        timed("analysis.prune", t);
        if n > 0 {
            tracing::info!(rows = n, "analysis pruned");
        }
        Ok(())
    })?;
    sch.register("analysis.tasks.run", "10s", |s: AppState| async move {
        tasks::run(&s, Duration::from_secs(90)).await.map(|_| ())
    })?;
    sch.register("analysis.memory.fold", "10s", |s: AppState| async move {
        suggestions::fold(&s).await
    })?;
    sch.register("analysis.memory.seed", "1m", |s: AppState| async move {
        suggestions::seed(&s).await
    })?;
    sch.register(
        "analysis.backtest.nightly",
        "17 3 * * *",
        |s: AppState| async move { suggestions::backtest_nightly(&s).await },
    )?;
    sch.register("analysis.errors.issues", "1h", |s: AppState| async move {
        errors::issues(&s).await
    })?;
    sch.register(
        "analysis.signals.resolve",
        "30s",
        |s: AppState| async move { signals::resolve(&s).await },
    )?;
    sch.register(
        "analysis.flaky.daily",
        "41 2 * * *",
        |s: AppState| async move { flaky_hints::daily(&s).await },
    )?;
    sch.register("analysis.ch.backfill", "1m", |s: AppState| async move {
        clickhouse::backfill(&s).await
    })?;
    sch.register("analysis.ch.rollup", "1h", |s: AppState| async move {
        clickhouse::rollup(&s).await
    })?;
    Ok(sch)
}

fn timed(job: &'static str, t: Instant) {
    metrics::histogram!("fastproof_analysis_job_seconds", "job" => job)
        .record(t.elapsed().as_secs_f64());
}

/// One run of `analysis.consume`: finished launches from the outbox into the queue (the first
/// run also queues the recent launches of every project); wakes the workers of this replica.
pub async fn consume(s: &AppState) -> anyhow::Result<Option<u64>> {
    let t = Instant::now();
    let n = s
        .store
        .consume_analysis_launches(CURSOR, s.cfg.analysis.settle, CONSUME_BATCH)
        .await?;
    timed("analysis.consume", t);
    if n.is_some_and(|n| n > 0) {
        WAKE.notify_waiters();
    }
    Ok(n)
}

/// Analyses a claimed launch: renews the lease while the steps run, then records them (an
/// error of the run itself queues the launch again, see `finish_analysis_launch`).
pub async fn analyse(s: &AppState, row: AnalysisLaunchRow, owner: &str) -> anyhow::Result<()> {
    analyse_with(s, &row, owner, pipeline::process(s, &row)).await
}

/// Aborts the lease renewal of a launch when the analysis ends, however it ends.
struct Renewal(tokio::task::JoinHandle<()>);

impl Drop for Renewal {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// [`analyse`] with the run given (tests run their own): a panic of the run is recorded as
/// its error like a failed run, so the worker keeps working and the launch is retried and
/// failed in time, instead of the worker task ending while the lease is still renewed.
pub async fn analyse_with<F>(
    s: &AppState,
    row: &AnalysisLaunchRow,
    owner: &str,
    run: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<fastproof_store::analysis::types::LaunchSteps>>,
{
    use futures::FutureExt;
    let renew = {
        let s = s.clone();
        let owner = owner.to_string();
        let launch = row.launch_id;
        Renewal(tokio::spawn(async move {
            loop {
                tokio::time::sleep(RENEW).await;
                match s.store.renew_analysis_launch(launch, &owner, LEASE).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(launch = %launch, "the lease of an analysed launch was lost");
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "renewing an analysis lease failed")
                    }
                }
            }
        }))
    };
    let t = Instant::now();
    let r = match std::panic::AssertUnwindSafe(run).catch_unwind().await {
        Ok(r) => r,
        Err(p) => {
            let what = p
                .downcast_ref::<&str>()
                .map(|m| m.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(anyhow::anyhow!("the analysis panicked: {what}"))
        }
    };
    drop(renew);
    timed("analysis.launch", t);
    let (steps, error) = match r {
        Ok(steps) => (steps, None),
        Err(e) => {
            tracing::warn!(launch = %row.launch_id, error = %format!("{e:#}"), "analysing a launch failed");
            (Default::default(), Some(format!("{e:#}")))
        }
    };
    s.store
        .finish_analysis_launch(row.launch_id, owner, &steps, error.as_deref())
        .await?;
    Ok(())
}

async fn worker(s: AppState, n: usize, stop: CancellationToken) {
    let owner = format!("{}-analysis-{n}", s.instance);
    loop {
        if stop.is_cancelled() {
            return;
        }
        match s.store.claim_analysis_launch(&owner, LEASE).await {
            Ok(Some(row)) => {
                if let Err(e) = analyse(&s, row, &owner).await {
                    tracing::warn!(error = %format!("{e:#}"), "recording an analysed launch failed");
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "claiming a launch for analysis failed")
            }
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = WAKE.notified() => {}
            _ = tokio::time::sleep(IDLE) => {}
        }
    }
}

async fn gauges(s: AppState, stop: CancellationToken) {
    loop {
        if let Ok(d) = s.store.analysis_queue_depth().await {
            metrics::gauge!("fastproof_analysis_queue", "state" => "queued").set(d.queued as f64);
            metrics::gauge!("fastproof_analysis_queue", "state" => "running").set(d.running as f64);
            metrics::gauge!("fastproof_analysis_queue", "state" => "failed").set(d.failed as f64);
            metrics::gauge!("fastproof_analysis_lag_seconds").set(d.lag_seconds);
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}

/// The role: its scheduler (jobs of `paused_jobs` that are the role's stay paused), its
/// workers and its gauges, until `stop`.
pub async fn run(state: AppState, stop: CancellationToken) {
    let mut sched = match jobs() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "the analysis jobs are invalid");
            return;
        }
    };
    let names = sched.names();
    let paused: Vec<String> = state
        .cfg
        .paused_jobs
        .iter()
        .filter(|j| names.contains(j))
        .cloned()
        .collect();
    if let Err(e) = sched.pause(&paused) {
        tracing::error!(error = %format!("{e:#}"), "pausing analysis jobs failed");
        return;
    }
    let mut tasks = vec![tokio::spawn(sched.run(state.clone(), stop.clone()))];
    for n in 0..state.cfg.analysis.workers {
        tasks.push(tokio::spawn(worker(state.clone(), n, stop.clone())));
    }
    tasks.push(tokio::spawn(gauges(state.clone(), stop.clone())));
    tracing::info!(
        workers = state.cfg.analysis.workers,
        "FastProof Analysis is running"
    );
    futures::future::join_all(tasks).await;
}

/// For tests (with `FASTPROOF_ANALYSIS_WORKERS=0` and the role's jobs paused): consumes the
/// outbox once, then analyses every due launch inline.
pub async fn run_once(state: &AppState) -> anyhow::Result<RunOnce> {
    let consumed = consume(state).await?;
    let owner = format!("{}-run-once", state.instance);
    let mut processed = 0;
    while let Some(row) = state.store.claim_analysis_launch(&owner, LEASE).await? {
        analyse(state, row, &owner).await?;
        processed += 1;
    }
    Ok(RunOnce {
        consumed,
        processed,
    })
}
