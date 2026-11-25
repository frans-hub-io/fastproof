//! The backtest on stored history (contract §7.3): [`PgSource`] pages a project's
//! `triage_memory_events` in time order into the pure harness
//! ([`fastproof_analysis::backtest`]); reports are kept in `analysis_backtests` (the newest
//! [`BACKTESTS_KEPT`] per project). This is the path a real project's history takes: nothing
//! here is specific to synthetic data.

use chrono::{DateTime, Duration, Utc};
use fastproof_analysis::backtest::{Backtest, BacktestOptions, BacktestSummary};
use fastproof_analysis::score::RankOptions;
use serde_json::Value;
use uuid::Uuid;

use super::memory::{EventRow, event_of};
use super::types::{
    AnalysisLimits, BACKTESTS_KEPT, BacktestBand, BacktestConfusion, BacktestReport,
    BacktestRequest, BacktestSource, DecisionEvent, MAX_CANDIDATES,
};
use crate::{Store, json_no_nul, no_nul};

/// Days of history of a backtest by default.
pub const DEFAULT_DAYS: u32 = 180;
/// Days of history of a backtest, at most.
pub const MAX_DAYS: u32 = 730;
/// Events read per page.
pub const PAGE: i64 = 5000;

/// A report from a summary of the harness.
pub fn report_of(
    summary: BacktestSummary,
    source: BacktestSource,
    id: Uuid,
    created_by: &str,
    created_at: DateTime<Utc>,
) -> BacktestReport {
    BacktestReport {
        id,
        source,
        from: summary.from,
        to: summary.to,
        decisions: summary.decisions,
        coverage: summary.coverage,
        no_suggestion: summary.no_suggestion,
        bands: summary
            .bands
            .into_iter()
            .map(|b| BacktestBand {
                band: b.band,
                suggested: b.suggested,
                correct: b.correct,
                correct_group: b.correct_group,
                precision: b.precision,
                shown: b.shown,
            })
            .collect(),
        confusions: summary
            .confusions
            .into_iter()
            .map(|c| BacktestConfusion {
                expected: c.expected,
                suggested: c.suggested,
                count: c.count,
            })
            .collect(),
        truncated: summary.truncated,
        created_at,
        created_by: created_by.to_string(),
    }
}

/// The harness options of a project: its suggestion settings and the instance's event limit.
pub fn options_of(
    settings: &super::settings::ProjectSettings,
    limits: &AnalysisLimits,
) -> BacktestOptions {
    BacktestOptions {
        rank: RankOptions {
            min_score: settings.analysis.suggest.min_score,
            max_candidates: (settings.analysis.suggest.max_candidates as usize)
                .clamp(1, MAX_CANDIDATES),
        },
        max_events: limits.backtest_max_events,
    }
}

/// Pages a project's decision events by `(decided_at, id)` from a time.
pub struct PgSource<'a> {
    store: &'a Store,
    project_id: Uuid,
    from: DateTime<Utc>,
    cursor: Option<(DateTime<Utc>, i64)>,
    page: i64,
    done: bool,
}

impl<'a> PgSource<'a> {
    pub fn new(store: &'a Store, project_id: Uuid, from: DateTime<Utc>, page: i64) -> Self {
        Self {
            store,
            project_id,
            from,
            cursor: None,
            page: page.clamp(1, 10_000),
            done: false,
        }
    }

    /// The next page in time order; empty at the end.
    pub async fn next_page(&mut self) -> anyhow::Result<Vec<DecisionEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, decided_at, fingerprint, message_norm, frames, test_case_id, env_key, branch, \
               defect_type_id, grp, previous_type_id, by, results \
             from triage_memory_events where project_id = $1 and decided_at >= $2 \
               and ($3::timestamptz is null or (decided_at, id) > ($3, $4)) \
             order by decided_at, id limit $5",
        )
        .bind(self.project_id)
        .bind(self.from)
        .bind(self.cursor.map(|c| c.0))
        .bind(self.cursor.map(|c| c.1).unwrap_or(0))
        .bind(self.page)
        .fetch_all(self.store.pool())
        .await?;
        if (rows.len() as i64) < self.page {
            self.done = true;
        }
        if let Some(last) = rows.last() {
            self.cursor = Some((last.1, last.0));
        }
        Ok(rows.into_iter().map(event_of).collect())
    }
}

impl Store {
    /// Runs a backtest over the project's decision events of the last `days` (default 180)
    /// and stores the report (the newest 20 per project are kept).
    pub async fn run_backtest(
        &self,
        project_id: Uuid,
        req: &BacktestRequest,
        by: &str,
        limits: &AnalysisLimits,
    ) -> anyhow::Result<BacktestReport> {
        let days = req.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
        let settings = self.project_settings(project_id).await?;
        let opts = options_of(&settings, limits);
        let from = Utc::now() - Duration::days(days as i64);
        let last = self.last_decision_event(project_id).await?;
        let mut source = PgSource::new(self, project_id, from, PAGE);
        let mut harness = Backtest::new(&opts);
        let mut read = 0u64;
        loop {
            let page = source.next_page().await?;
            if page.is_empty() {
                break;
            }
            read += page.len() as u64;
            harness.feed(&page);
            if read > opts.max_events {
                break;
            }
            tokio::task::yield_now().await;
        }
        let report = report_of(
            harness.finish(),
            BacktestSource::Project,
            Uuid::now_v7(),
            by,
            Utc::now(),
        );
        self.save_backtest(
            project_id,
            &report,
            serde_json::json!({"days": days, "lastEventId": last}),
        )
        .await?;
        Ok(report)
    }

    /// Stores a report and keeps the newest [`BACKTESTS_KEPT`] of the project.
    pub async fn save_backtest(
        &self,
        project_id: Uuid,
        report: &BacktestReport,
        params: Value,
    ) -> anyhow::Result<()> {
        let source = serde_json::to_value(report.source)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "insert into analysis_backtests (id, project_id, source, params, report, decisions, created_by, created_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(report.id)
        .bind(project_id)
        .bind(source.as_str().unwrap_or("project"))
        .bind(json_no_nul(params))
        .bind(json_no_nul(serde_json::to_value(report)?))
        .bind(report.decisions.min(i32::MAX as u64) as i32)
        .bind(no_nul(&report.created_by))
        .bind(report.created_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "delete from analysis_backtests where project_id = $1 and id not in \
               (select id from analysis_backtests where project_id = $1 order by created_at desc, id desc limit $2)",
        )
        .bind(project_id)
        .bind(BACKTESTS_KEPT)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The newest backtest report of a project.
    pub async fn latest_backtest(
        &self,
        project_id: Uuid,
    ) -> anyhow::Result<Option<BacktestReport>> {
        let r: Option<Value> = sqlx::query_scalar(
            "select report from analysis_backtests where project_id = $1 order by created_at desc, id desc limit 1",
        )
        .bind(project_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(r.and_then(|v| serde_json::from_value(v).ok()))
    }

    /// Projects with `analysis.backtest.nightly` whose decision events are newer than their
    /// latest report (the nightly job's work), with when that report was made.
    pub async fn backtest_due_projects(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, Option<DateTime<Utc>>)>> {
        let rows: Vec<(Uuid, Value, Option<DateTime<Utc>>)> = sqlx::query_as(
            "select p.id, p.settings, (select max(b.created_at) from analysis_backtests b where b.project_id = p.id) \
             from projects p where exists (select 1 from triage_memory_events e where e.project_id = p.id \
               and e.id > coalesce((select max((b.params->>'lastEventId')::bigint) from analysis_backtests b \
                 where b.project_id = p.id and b.params ? 'lastEventId'), 0)) \
             order by p.id limit $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter(|(_, s, _)| {
                let a = super::settings::ProjectSettings::from_value(s.clone()).analysis;
                a.enabled && a.backtest.nightly
            })
            .map(|(p, _, at)| (p, at))
            .collect())
    }

    /// The id of a project's newest decision event (0 without any).
    pub async fn last_decision_event(&self, project_id: Uuid) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "select coalesce(max(id), 0) from triage_memory_events where project_id = $1",
        )
        .bind(project_id)
        .fetch_one(self.pool())
        .await?)
    }

    /// The latest report of each project with `analysis.backtest.nightly`, for the
    /// instance-wide precision gauge.
    pub async fn latest_backtests(&self, limit: i64) -> anyhow::Result<Vec<BacktestReport>> {
        let rows: Vec<(Value, Value)> = sqlx::query_as(
            "select distinct on (b.project_id) b.report, p.settings from analysis_backtests b \
             join projects p on p.id = b.project_id order by b.project_id, b.created_at desc, b.id desc limit $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter(|(_, s)| {
                let a = super::settings::ProjectSettings::from_value(s.clone()).analysis;
                a.enabled && a.backtest.nightly
            })
            .filter_map(|(r, _)| serde_json::from_value(r).ok())
            .collect())
    }
}
