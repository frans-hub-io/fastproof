//! ClickHouse reads of Analysis that other areas call, and the ClickHouse jobs (contract
//! §7.9). Every read answers `None` ("no ClickHouse here, use PostgreSQL") without
//! ClickHouse, and also when ClickHouse fails (logged), so a ClickHouse outage degrades to
//! the PostgreSQL answers instead of failing the analysis steps.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_store::Project;
use fastproof_store::analysis::rules::Rule;
use fastproof_store::analysis::types::{
    ClusterFacts, ErrorTimelinePoint, LogMatch, MemoryRow, SeriesRun,
};
use fastproof_store::stream::ChBackfill;
use fastproof_stream::clickhouse::{ClickHouse, ROLLUP_DAYS};
use fastproof_stream::sink::{SinkBatch, write_triage};
use uuid::Uuid;

use crate::state::AppState;

/// Whether this build answers the analytics endpoints (`AnalysisFeatures.analytics`).
pub const ANALYTICS: bool = true;
/// Whether this build answers the portfolio (`AnalysisFeatures.portfolio`).
pub const PORTFOLIO: bool = true;
/// Whether this build answers cross-project similar failures
/// (`AnalysisFeatures.crossProjectSimilar`).
pub const CROSS_PROJECT_SIMILAR: bool = true;

/// Decisions the backfill sends per run, at most.
pub const BACKFILL_DECISIONS: i64 = 5000;
/// Results the backfill re-sends per run, about (whole launches).
pub const BACKFILL_RESULTS: usize = 5000;
/// Wall time of one backfill run, at most.
pub const BACKFILL_BUDGET: Duration = Duration::from_secs(45);
/// History rows asked per fingerprint.
pub const HISTORY_PER_FINGERPRINT: usize = 3;
/// Fingerprints (canonical and merged) asked of the history per launch, at most.
pub const HISTORY_FINGERPRINTS: usize = 200;
/// Runs of a series read from ClickHouse, at most (the newest).
pub const SERIES_RUNS: usize = 1000;

fn ch(s: &AppState) -> Option<&ClickHouse> {
    s.clickhouse.as_deref()
}

fn degraded<T>(what: &'static str, e: anyhow::Error) -> anyhow::Result<Option<T>> {
    metrics::counter!("fastproof_analysis_clickhouse_errors_total", "read" => what).increment(1);
    tracing::warn!(read = what, error = %format!("{e:#}"), "ClickHouse read failed; using PostgreSQL");
    Ok(None)
}

fn timed(job: &'static str, t: Instant) {
    metrics::histogram!("fastproof_analysis_job_seconds", "job" => job)
        .record(t.elapsed().as_secs_f64());
}

/// Job `analysis.ch.backfill` (1 min): after ClickHouse gains schema version 2, sends the
/// decisions journaled before (except rule batches, from id 0 up to the newest id when the
/// backfill started, [`BACKFILL_DECISIONS`] per run) and writes again the results of the
/// launches within the raw TTL whose defect columns matter, oldest first. Then does nothing.
pub async fn backfill(s: &AppState) -> anyhow::Result<()> {
    let Some(ch) = ch(s) else {
        return Ok(());
    };
    let t = Instant::now();
    let mut st = match s.store.ch_backfill().await? {
        Some(st) => st,
        None => ChBackfill {
            until: s.store.max_decision_id().await?,
            ..ChBackfill::default()
        },
    };
    if st.done() {
        return Ok(());
    }
    // The sink creates the schema when it starts; until then there is nothing to fill.
    if ch.schema_version().await.unwrap_or(0) < 2 {
        return Ok(());
    }
    if !st.decisions_done {
        let rows = s
            .store
            .sink_triage_after(st.decision, st.until, BACKFILL_DECISIONS)
            .await?;
        match rows.last().map(|r| r.decision_id) {
            None => st.decisions_done = true,
            Some(last) => {
                let full = rows.len() as i64 == BACKFILL_DECISIONS;
                for part in rows.chunks(1000) {
                    st.decisions += write_triage(&s.store, ch, part.to_vec()).await?;
                    let written: BTreeSet<(String, Uuid)> = part
                        .iter()
                        .map(|r| (r.project.clone(), r.launch_id))
                        .collect();
                    forget_deleted(s, ch, written.into_iter().collect()).await?;
                }
                st.decision = last;
                st.decisions_done = !full;
            }
        }
        s.store.put_ch_backfill(&st).await?;
    }
    if !st.results_done && t.elapsed() < BACKFILL_BUDGET {
        let since = Utc::now() - chrono::Duration::days(i64::from(ch_raw_days(s)));
        let mut sent = 0usize;
        loop {
            let after = st.launch_at.zip(st.launch_id);
            let launches = s.store.backfill_launches(since, after, 20).await?;
            if launches.is_empty() {
                st.results_done = true;
                break;
            }
            for (at, id) in launches {
                let ids = s.store.backfill_result_ids(id, at).await?;
                for part in ids.chunks(fastproof_stream::sink::SINK_RESULTS) {
                    let b = SinkBatch {
                        results: s.store.sink_results(part).await?,
                        ..SinkBatch::default()
                    };
                    if !b.is_empty() {
                        fastproof_stream::sink::Sink::write(ch, &b, &b.token()).await?;
                        let written: BTreeSet<(String, Uuid)> = b
                            .results
                            .iter()
                            .map(|r| (r.project.clone(), r.launch_id))
                            .collect();
                        forget_deleted(s, ch, written.into_iter().collect()).await?;
                    }
                }
                sent += ids.len();
                st.results += ids.len() as u64;
                st.launch_at = Some(at);
                st.launch_id = Some(id);
            }
            s.store.put_ch_backfill(&st).await?;
            if sent >= BACKFILL_RESULTS || t.elapsed() >= BACKFILL_BUDGET {
                break;
            }
        }
    }
    s.store.put_ch_backfill(&st).await?;
    if st.done() {
        tracing::info!(
            decisions = st.decisions,
            results = st.results,
            "ClickHouse backfill of Analysis finished"
        );
    }
    timed("analysis.ch.backfill", t);
    Ok(())
}

/// Removes from ClickHouse the rows of launches deleted in PostgreSQL while the backfill
/// was writing them: the sink may have applied the launch's deletion between the backfill's
/// read and its write, which would otherwise bring the deleted rows back. Returns how many
/// launches were removed.
pub async fn forget_deleted(
    s: &AppState,
    ch: &ClickHouse,
    launches: Vec<(String, Uuid)>,
) -> anyhow::Result<usize> {
    if launches.is_empty() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = launches.iter().map(|l| l.1).collect();
    let live: HashSet<Uuid> =
        sqlx::query_scalar::<_, Uuid>("select id from launches where id = any($1)")
            .bind(&ids)
            .fetch_all(s.store.pool())
            .await?
            .into_iter()
            .collect();
    let gone: Vec<(String, Uuid)> = launches
        .into_iter()
        .filter(|l| !live.contains(&l.1))
        .collect();
    let n = gone.len();
    if n > 0 {
        fastproof_stream::sink::Sink::delete(
            ch,
            &fastproof_stream::sink::Deletions {
                launches: gone,
                projects: Vec::new(),
            },
        )
        .await?;
    }
    Ok(n)
}

fn ch_raw_days(s: &AppState) -> u32 {
    s.cfg.clickhouse.as_ref().map_or(90, |c| c.raw_ttl_days)
}

/// Job `analysis.ch.rollup` (1 h): recomputes `defect_daily` for the last [`ROLLUP_DAYS`]
/// days; the first run after the backfill finished recomputes the whole raw TTL. Project by
/// project (live projects with data in the window) and [`ROLLUP_DAYS`] days per statement: a
/// project whose rollup fails is logged and counted and the others go on.
pub async fn rollup(s: &AppState) -> anyhow::Result<()> {
    let Some(ch) = ch(s) else {
        return Ok(());
    };
    if ch.schema_version().await.unwrap_or(0) < 2 {
        return Ok(());
    }
    let t = Instant::now();
    let st = s.store.ch_backfill().await?;
    let full = st.as_ref().is_some_and(|b| b.done() && !b.rollup_full);
    let n = if full { ch_raw_days(s) } else { ROLLUP_DAYS };
    let today = Utc::now().date_naive();
    let days: Vec<chrono::NaiveDate> = (0..=i64::from(n))
        .rev()
        .map(|i| today - chrono::Duration::days(i))
        .collect();
    let live: HashSet<String> = s
        .store
        .projects()
        .await?
        .into_iter()
        .map(|p| p.key)
        .collect();
    let projects = ch.projects_since(days[0]).await?;
    for p in projects.iter().filter(|p| live.contains(*p)) {
        for part in days.chunks(ROLLUP_DAYS as usize) {
            if let Err(e) = ch.rollup_defects(p, part).await {
                metrics::counter!("fastproof_analysis_clickhouse_errors_total", "read" => "rollup")
                    .increment(1);
                tracing::warn!(project = %p, error = %format!("{e:#}"), "the defect_daily rollup of a project failed");
                break;
            }
        }
    }
    if full && let Some(mut st) = st {
        st.rollup_full = true;
        s.store.put_ch_backfill(&st).await?;
    }
    timed("analysis.ch.rollup", t);
    Ok(())
}

/// Memory rows from the ClickHouse triage history for clusters (S calls it): people's
/// decisions on the cluster's fingerprint or a fingerprint merged into it, keyed by the
/// cluster's fingerprint; types deleted since are left out.
pub async fn history_candidates(
    s: &AppState,
    project: &Project,
    clusters: &[ClusterFacts],
    limit: usize,
) -> anyhow::Result<Option<Vec<(String, MemoryRow)>>> {
    let Some(ch) = ch(s) else {
        return Ok(None);
    };
    let clusters: Vec<&ClusterFacts> = clusters.iter().take(limit).collect();
    let mut fps: BTreeSet<String> = BTreeSet::new();
    for c in &clusters {
        for f in std::iter::once(&c.fingerprint).chain(c.aliases.iter()) {
            if fps.len() < HISTORY_FINGERPRINTS {
                fps.insert(f.clone());
            }
        }
    }
    let fps: Vec<String> = fps.into_iter().collect();
    let rows = match ch
        .history(&project.key, &fps, HISTORY_PER_FINGERPRINT)
        .await
    {
        Ok(r) => r,
        Err(e) => return degraded("history", e),
    };
    let types: Vec<Uuid> = rows
        .iter()
        .map(|r| r.defect_type_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let live: HashSet<Uuid> = s
        .store
        .existing_defect_types(project.id, &types)
        .await?
        .into_iter()
        .collect();
    let mut out = Vec::new();
    for r in rows {
        if !live.contains(&r.defect_type_id) {
            continue;
        }
        for c in &clusters {
            if c.fingerprint == r.fingerprint || c.aliases.contains(&r.fingerprint) {
                out.push((c.fingerprint.clone(), r.clone()));
            }
        }
    }
    Ok(Some(out))
}

/// Daily occurrences of an error and the fingerprints merged into it (E calls it).
pub async fn error_timeline(
    s: &AppState,
    project: &Project,
    fingerprint: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Option<Vec<ErrorTimelinePoint>>> {
    let Some(ch) = ch(s) else {
        return Ok(None);
    };
    let members = s.store.error_members(project.id, fingerprint).await?;
    match ch.error_timeline(&project.key, &members, from, to).await {
        Ok(items) => Ok(Some(items)),
        Err(e) => degraded("error_timeline", e),
    }
}

/// Log lines a log rule matches in the listed launches since a time (P calls it). A rule
/// without `match.log`, or a pattern ClickHouse cannot run, answers `None` (P then reads
/// PostgreSQL).
pub async fn retro_log_matches(
    s: &AppState,
    project: &Project,
    rule: &Rule,
    since: DateTime<Utc>,
    launches: &[Uuid],
    max: usize,
) -> anyhow::Result<Option<Vec<LogMatch>>> {
    let Some(ch) = ch(s) else {
        return Ok(None);
    };
    let Some(m) = rule.matcher.log.as_ref() else {
        return Ok(None);
    };
    match ch.log_matches(&project.key, m, since, launches, max).await {
        Ok(items) => Ok(Some(items)),
        Err(e) => degraded("retro_log_matches", e),
    }
}

/// Runs of a series over the last `days` days (H calls it), oldest first, at most the newest
/// [`SERIES_RUNS`].
pub async fn series_history(
    s: &AppState,
    project: &Project,
    series_id: Uuid,
    days: u32,
) -> anyhow::Result<Option<Vec<SeriesRun>>> {
    let Some(ch) = ch(s) else {
        return Ok(None);
    };
    let since = Utc::now() - chrono::Duration::days(i64::from(days.max(1)));
    match ch
        .series_runs(&project.key, series_id, since, SERIES_RUNS)
        .await
    {
        Ok(items) => Ok(Some(items)),
        Err(e) => degraded("series_history", e),
    }
}
