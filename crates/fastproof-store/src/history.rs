//! History reads behind [`HistoryStore`], so a column store (ClickHouse) can serve them on large
//! installations later. The trait uses plain types only: no sqlx in its signatures.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::no_nul;

/// Filters of a history query.
#[derive(Clone, Debug, Default)]
pub struct HistoryFilter {
    pub branch: Option<String>,
    pub environment: Option<String>,
    /// Include debug launches.
    pub debug: bool,
    pub limit: i64,
}

/// One result of a test case in a launch.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub result_id: Uuid,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub branch: Option<String>,
    pub environment: Option<String>,
}

/// One result of a series.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeriesHistoryEntry {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub result_id: Uuid,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub flaky_in_run: bool,
}

/// Status counts of one launch, for trend charts.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrendPoint {
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub started_at: DateTime<Utc>,
    pub duration_ms: Option<i64>,
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub flaky: i64,
}

/// History queries, newest first.
#[async_trait]
pub trait HistoryStore: Send + Sync {
    /// Results of a test case across launches (all its series).
    async fn test_case_history(
        &self,
        project_id: Uuid,
        test_case_id: Uuid,
        filter: &HistoryFilter,
    ) -> anyhow::Result<Vec<HistoryEntry>>;

    /// The last `limit` results of a series.
    async fn series_history(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<SeriesHistoryEntry>>;

    /// Launch-level status counts over the last `limit` launches.
    async fn launch_trend(
        &self,
        project_id: Uuid,
        filter: &HistoryFilter,
    ) -> anyhow::Result<Vec<TrendPoint>>;
}

/// [`HistoryStore`] on the PostgreSQL results tables.
#[derive(Clone, Debug)]
pub struct PgHistoryStore {
    pool: PgPool,
}

impl PgHistoryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn launch_filters(qb: &mut QueryBuilder<Postgres>, f: &HistoryFilter) {
    if let Some(b) = &f.branch {
        qb.push(" and l.branch = ").push_bind(no_nul(b));
    }
    if let Some(e) = &f.environment {
        qb.push(" and l.env_key = ").push_bind(no_nul(e));
    }
    if !f.debug {
        qb.push(" and not l.debug");
    }
}

#[async_trait]
impl HistoryStore for PgHistoryStore {
    async fn test_case_history(
        &self,
        project_id: Uuid,
        test_case_id: Uuid,
        f: &HistoryFilter,
    ) -> anyhow::Result<Vec<HistoryEntry>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select r.launch_id, l.number, r.id, r.status, r.started_at, r.duration_ms, l.branch, l.env_key \
             from results r join launches l on l.id = r.launch_id where r.project_id = ",
        );
        qb.push_bind(project_id)
            .push(" and r.test_case_id = ")
            .push_bind(test_case_id);
        launch_filters(&mut qb, f);
        qb.push(" order by r.launch_created_at desc, l.number desc limit ")
            .push_bind(f.limit);
        let rows: Vec<(
            Uuid,
            i64,
            Uuid,
            String,
            Option<DateTime<Utc>>,
            Option<i64>,
            Option<String>,
            Option<String>,
        )> = qb.build_query_as().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| HistoryEntry {
                launch_id: r.0,
                launch_number: r.1,
                result_id: r.2,
                status: r.3,
                started_at: r.4,
                duration_ms: r.5,
                branch: r.6,
                environment: r.7,
            })
            .collect())
    }

    async fn series_history(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<SeriesHistoryEntry>> {
        let rows: Vec<(Uuid, i64, Uuid, String, Option<DateTime<Utc>>, Option<i64>, bool)> = sqlx::query_as(
            "select r.launch_id, l.number, r.id, r.status, r.started_at, r.duration_ms, r.flaky_in_run \
             from results r join launches l on l.id = r.launch_id \
             where r.project_id = $1 and r.series_id = $2 \
             order by r.launch_created_at desc, l.number desc limit $3",
        )
        .bind(project_id)
        .bind(series_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| SeriesHistoryEntry {
                launch_id: r.0,
                launch_number: r.1,
                result_id: r.2,
                status: r.3,
                started_at: r.4,
                duration_ms: r.5,
                flaky_in_run: r.6,
            })
            .collect())
    }

    async fn launch_trend(
        &self,
        project_id: Uuid,
        f: &HistoryFilter,
    ) -> anyhow::Result<Vec<TrendPoint>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "select l.id, l.number, l.started_at, \
             case when l.last_result_at >= l.first_result_at \
               then (extract(epoch from (l.last_result_at - l.first_result_at)) * 1000)::bigint end, \
             l.count_total::bigint, l.count_passed::bigint, l.count_failed::bigint, l.count_broken::bigint, \
             l.count_skipped::bigint, l.count_flaky::bigint from launches l where l.project_id = ",
        );
        qb.push_bind(project_id);
        launch_filters(&mut qb, f);
        qb.push(" order by l.number desc limit ").push_bind(f.limit);
        let rows: Vec<(
            Uuid,
            i64,
            DateTime<Utc>,
            Option<i64>,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        )> = qb.build_query_as().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| TrendPoint {
                launch_id: r.0,
                launch_number: r.1,
                started_at: r.2,
                duration_ms: r.3,
                total: r.4,
                passed: r.5,
                failed: r.6,
                broken: r.7,
                skipped: r.8,
                flaky: r.9,
            })
            .collect())
    }
}
