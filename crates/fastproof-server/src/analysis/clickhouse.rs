//! ClickHouse reads of Analysis that other areas call, and the ClickHouse jobs (contract
//! §7.9). Every read answers `None` ("no ClickHouse here, use PostgreSQL") until C fills it.
//!
//! Owner: C (night B).

use chrono::{DateTime, Utc};
use fastproof_store::Project;
use fastproof_store::analysis::rules::Rule;
use fastproof_store::analysis::types::{
    ClusterFacts, ErrorTimelinePoint, LogMatch, MemoryRow, SeriesRun,
};
use uuid::Uuid;

use crate::state::AppState;

/// Whether this build answers the analytics endpoints (`AnalysisFeatures.analytics`).
pub const ANALYTICS: bool = false;
/// Whether this build answers the portfolio (`AnalysisFeatures.portfolio`).
pub const PORTFOLIO: bool = false;
/// Whether this build answers cross-project similar failures
/// (`AnalysisFeatures.crossProjectSimilar`).
pub const CROSS_PROJECT_SIMILAR: bool = false;

/// Job `analysis.ch.backfill` (1 min). Stub.
pub async fn backfill(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}

/// Job `analysis.ch.rollup` (1 h). Stub.
pub async fn rollup(s: &AppState) -> anyhow::Result<()> {
    let _ = s;
    Ok(())
}

/// Memory rows from the ClickHouse triage history for clusters (S calls it). Stub: `None`.
pub async fn history_candidates(
    s: &AppState,
    project: &Project,
    clusters: &[ClusterFacts],
    limit: usize,
) -> anyhow::Result<Option<Vec<(String, MemoryRow)>>> {
    let _ = (s, project, clusters, limit);
    Ok(None)
}

/// Daily occurrences of an error (E calls it). Stub: `None`.
pub async fn error_timeline(
    s: &AppState,
    project: &Project,
    fingerprint: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> anyhow::Result<Option<Vec<ErrorTimelinePoint>>> {
    let _ = (s, project, fingerprint, from, to);
    Ok(None)
}

/// Log lines a log rule matches since a time (P calls it). Stub: `None`.
pub async fn retro_log_matches(
    s: &AppState,
    project: &Project,
    rule: &Rule,
    since: DateTime<Utc>,
    max: usize,
) -> anyhow::Result<Option<Vec<LogMatch>>> {
    let _ = (s, project, rule, since, max);
    Ok(None)
}

/// Runs of a series over the ClickHouse TTL (H calls it). Stub: `None`.
pub async fn series_history(
    s: &AppState,
    project: &Project,
    series_id: Uuid,
    days: u32,
) -> anyhow::Result<Option<Vec<SeriesRun>>> {
    let _ = (s, project, series_id, days);
    Ok(None)
}
