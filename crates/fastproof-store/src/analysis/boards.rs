//! Dashboards of a project: stored definitions (name, owner, lock, widgets on a 12-column grid)
//! and the data of their widgets. Every widget reads existing aggregates (`launches` counters,
//! `series_daily`, `series_stats`, `failure_fingerprints`) within a bounded window; the two
//! per-launch widgets read the results of one launch.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::dashboard::{Bucket, Dashboard, DashboardQuery};
use super::settings::ProjectSettings;
use crate::{Store, no_nul};

/// Dashboards of a project, at most.
pub const MAX_BOARDS: i64 = 50;
/// Widgets of a dashboard, at most.
pub const MAX_WIDGETS: usize = 24;
/// Columns of the grid.
pub const GRID_COLUMNS: i32 = 12;
/// Rows a widget may span, and the highest row it may start in.
pub const MAX_WIDGET_ROWS: i32 = 12;
pub const MAX_ROW: i32 = 1000;
/// Longest window of a widget, in days.
pub const MAX_DAYS: u32 = 730;
/// Most launches, list items or table rows of a widget.
pub const MAX_LIMIT: u32 = 50;
/// Default window and item count.
pub const DEFAULT_DAYS: u32 = 30;
pub const DEFAULT_LIMIT: u32 = 10;
/// Components listed by the component health widget.
const COMPONENTS: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetType {
    LaunchStatistics,
    OverallStatistics,
    InvestigatedPercentage,
    LaunchesComparison,
    LaunchesDuration,
    LaunchesTable,
    FailedTrend,
    NonPassedTrend,
    TestCasesGrowth,
    PassingRatePerLaunch,
    PassingRateSummary,
    FlakyTests,
    MostFailingTests,
    PopularPatterns,
    LaunchExecution,
    ComponentHealth,
    PassRateTrend,
    Durations,
    DefectTrend,
    LaunchesPerDay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Launches,
    Trend,
    Growth,
    Patterns,
    Launch,
}

impl WidgetType {
    fn source(self) -> Source {
        use WidgetType::*;
        match self {
            LaunchStatistics | OverallStatistics | LaunchesComparison | LaunchesDuration
            | LaunchesTable | FailedTrend | NonPassedTrend | PassingRatePerLaunch
            | PassingRateSummary => Source::Launches,
            InvestigatedPercentage
            | FlakyTests
            | MostFailingTests
            | PassRateTrend
            | Durations
            | DefectTrend
            | LaunchesPerDay => Source::Trend,
            TestCasesGrowth => Source::Growth,
            PopularPatterns => Source::Patterns,
            LaunchExecution | ComponentHealth => Source::Launch,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WidgetBucket {
    Day,
    Week,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WidgetView {
    Chart,
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WidgetChart {
    Bar,
    Line,
    Area,
}

/// What a widget shows; every field is optional. Windows are relative to now, so a dashboard
/// stays current and can be cloned into another project as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetSettings {
    /// Days back, 1 to 730 (default 30).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<WidgetBucket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Launches or list items, 1 to 50 (default 10; lists of tests hold at most 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<WidgetView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart: Option<WidgetChart>,
    /// Label name the component health widget groups by (default `suite`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    /// The launch of the per-launch widgets; the latest when empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardWidget {
    /// 1 to 32 characters of `a-z`, `0-9` and `-`, unique in the dashboard.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: WidgetType,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub settings: WidgetSettings,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardWrite {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub widgets: Vec<BoardWidget>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub owner: String,
    pub locked: bool,
    pub revision: i64,
    pub widgets: Vec<BoardWidget>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardSummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub owner: String,
    pub locked: bool,
    pub revision: i64,
    pub widget_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum BoardError {
    NotFound,
    Locked,
    Limit,
    Stale {
        current_revision: i64,
        changed_by: Option<String>,
    },
    Invalid(String),
    Other(anyhow::Error),
}

impl From<sqlx::Error> for BoardError {
    fn from(e: sqlx::Error) -> Self {
        BoardError::Other(e.into())
    }
}

fn no_nul_in(s: &str) -> bool {
    !s.contains('\0')
}

/// Checks a dashboard before it is stored; the message names the first problem.
pub fn validate(b: &BoardWrite) -> Result<(), String> {
    let n = b.name.trim().chars().count();
    if n == 0 || n > 120 {
        return Err("name must be 1 to 120 characters".into());
    }
    if b.description.chars().count() > 2000 {
        return Err("description has at most 2000 characters".into());
    }
    if !no_nul_in(&b.name) || !no_nul_in(&b.description) {
        return Err("text must not contain NUL characters".into());
    }
    if b.widgets.len() > MAX_WIDGETS {
        return Err(format!("a dashboard has at most {MAX_WIDGETS} widgets"));
    }
    let mut ids = HashSet::new();
    for w in &b.widgets {
        let id_ok = (1..=32).contains(&w.id.len())
            && w.id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
        if !id_ok || !ids.insert(w.id.as_str()) {
            return Err(
                "widget ids must be unique, 1 to 32 characters of a-z, 0-9 and -".to_string(),
            );
        }
        if w.title.chars().count() > 120 || !no_nul_in(&w.title) {
            return Err("a widget title has at most 120 characters".into());
        }
        if w.w < 1
            || w.w > GRID_COLUMNS
            || w.x < 0
            || w.x + w.w > GRID_COLUMNS
            || w.h < 1
            || w.h > MAX_WIDGET_ROWS
            || w.y < 0
            || w.y > MAX_ROW
        {
            return Err("a widget must fit the 12-column grid".into());
        }
        let s = &w.settings;
        if s.days.is_some_and(|d| !(1..=MAX_DAYS).contains(&d)) {
            return Err(format!("days must be 1 to {MAX_DAYS}"));
        }
        if s.limit.is_some_and(|l| !(1..=MAX_LIMIT).contains(&l)) {
            return Err(format!("limit must be 1 to {MAX_LIMIT}"));
        }
        for (v, max, what) in [
            (&s.branch, 200, "branch"),
            (&s.environment, 200, "environment"),
            (&s.group_by, 64, "groupBy"),
        ] {
            if v.as_ref()
                .is_some_and(|v| v.chars().count() > max || !no_nul_in(v))
            {
                return Err(format!("{what} has at most {max} characters"));
            }
        }
    }
    Ok(())
}

type BoardRow = (
    Uuid,
    String,
    String,
    String,
    bool,
    i64,
    serde_json::Value,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn board_of(r: BoardRow) -> Result<Board, BoardError> {
    let widgets = serde_json::from_value(r.6)
        .map_err(|e| BoardError::Other(anyhow::anyhow!("stored widgets: {e}")))?;
    Ok(Board {
        id: r.0,
        name: r.1,
        description: r.2,
        owner: r.3,
        locked: r.4,
        revision: r.5,
        widgets,
        created_at: r.7,
        updated_at: r.8,
    })
}

type SummaryRow = (
    Uuid,
    String,
    String,
    String,
    bool,
    i64,
    i64,
    DateTime<Utc>,
    DateTime<Utc>,
);

type PatternRow = (String, String, i64, DateTime<Utc>, DateTime<Utc>);

const BOARD_COLS: &str =
    "id, name, description, owner, locked, revision, widgets, created_at, updated_at";

impl Store {
    pub async fn list_boards(&self, project_id: Uuid) -> anyhow::Result<Vec<BoardSummary>> {
        let rows: Vec<SummaryRow> = sqlx::query_as(
            "select id, name, description, owner, locked, revision, jsonb_array_length(widgets)::bigint, \
               created_at, updated_at from dashboards where project_id = $1 order by created_at, id limit $2",
        )
        .bind(project_id)
        .bind(MAX_BOARDS)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| BoardSummary {
                id: r.0,
                name: r.1,
                description: r.2,
                owner: r.3,
                locked: r.4,
                revision: r.5,
                widget_count: r.6,
                created_at: r.7,
                updated_at: r.8,
            })
            .collect())
    }

    pub async fn board(&self, project_id: Uuid, id: Uuid) -> Result<Option<Board>, BoardError> {
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {BOARD_COLS} from dashboards where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(board_of).transpose()
    }

    pub async fn create_board(
        &self,
        project_id: Uuid,
        owner: &str,
        w: &BoardWrite,
    ) -> Result<Board, BoardError> {
        validate(w).map_err(BoardError::Invalid)?;
        let widgets = serde_json::to_value(&w.widgets)
            .map_err(|e| BoardError::Other(anyhow::anyhow!("widgets: {e}")))?;
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "insert into dashboards (id, project_id, name, description, owner, widgets, updated_by) \
             select $1, $2, $3, $4, $5, $6, $5 where (select count(*) from dashboards where project_id = $2) < $7 \
             returning {BOARD_COLS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(w.name.trim())
        .bind(&w.description)
        .bind(no_nul(owner))
        .bind(widgets)
        .bind(MAX_BOARDS)
        .fetch_optional(&self.pool)
        .await?;
        r.map(board_of).transpose()?.ok_or(BoardError::Limit)
    }

    /// Replaces name, description and widgets if the revision matches. A locked dashboard is
    /// changed only by its owner or when `manager` is set.
    pub async fn update_board(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        actor: &str,
        manager: bool,
        w: &BoardWrite,
    ) -> Result<Board, BoardError> {
        validate(w).map_err(BoardError::Invalid)?;
        let widgets = serde_json::to_value(&w.widgets)
            .map_err(|e| BoardError::Other(anyhow::anyhow!("widgets: {e}")))?;
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "update dashboards set name = $4, description = $5, widgets = $6, revision = revision + 1, \
               updated_by = $7, updated_at = now() \
             where project_id = $1 and id = $2 and revision = $3 and (not locked or owner = $7 or $8) \
             returning {BOARD_COLS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(revision)
        .bind(w.name.trim())
        .bind(&w.description)
        .bind(widgets)
        .bind(no_nul(actor))
        .bind(manager)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => board_of(r),
            None => Err(self
                .why_not_changed(project_id, id, Some(revision), actor, manager)
                .await?),
        }
    }

    /// Locks or unlocks; only the owner or a manager may.
    pub async fn set_board_lock(
        &self,
        project_id: Uuid,
        id: Uuid,
        locked: bool,
        actor: &str,
        manager: bool,
    ) -> Result<Board, BoardError> {
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "update dashboards set locked = $3, revision = revision + 1, updated_by = $4, updated_at = now() \
             where project_id = $1 and id = $2 and (owner = $4 or $5) returning {BOARD_COLS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(locked)
        .bind(no_nul(actor))
        .bind(manager)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => board_of(r),
            None => Err(match self.board(project_id, id).await {
                Ok(Some(_)) => BoardError::Locked,
                Ok(None) => BoardError::NotFound,
                Err(e) => e,
            }),
        }
    }

    /// Deletes a dashboard; a locked one only for its owner or a manager.
    pub async fn delete_board(
        &self,
        project_id: Uuid,
        id: Uuid,
        actor: &str,
        manager: bool,
    ) -> Result<(), BoardError> {
        let n = sqlx::query(
            "delete from dashboards where project_id = $1 and id = $2 and (not locked or owner = $3 or $4)",
        )
        .bind(project_id)
        .bind(id)
        .bind(no_nul(actor))
        .bind(manager)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 1 {
            return Ok(());
        }
        Err(self
            .why_not_changed(project_id, id, None, actor, manager)
            .await?)
    }

    async fn why_not_changed(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: Option<i64>,
        actor: &str,
        manager: bool,
    ) -> Result<BoardError, BoardError> {
        let cur: Option<(i64, bool, String, Option<String>)> = sqlx::query_as(
            "select revision, locked, owner, updated_by from dashboards where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match cur {
            None => BoardError::NotFound,
            Some((_, true, owner, _)) if owner != actor && !manager => BoardError::Locked,
            Some((rev, _, _, by)) if revision.is_some() => BoardError::Stale {
                current_revision: rev,
                changed_by: by,
            },
            Some(_) => BoardError::NotFound,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPoint {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub unknown: i64,
    pub flaky: i64,
    pub muted: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PatternPoint {
    pub fingerprint: String,
    pub message: String,
    pub occurrences: i64,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct GrowthPoint {
    pub t: NaiveDate,
    pub added: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPoint {
    pub name: String,
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefectCounts {
    pub product_bug: i64,
    pub automation_bug: i64,
    pub system_issue: i64,
    pub no_defect: i64,
    pub to_investigate: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WidgetData {
    Launches {
        launches: Vec<LaunchPoint>,
    },
    Trend {
        trend: Box<Dashboard>,
    },
    Growth {
        points: Vec<GrowthPoint>,
    },
    Patterns {
        items: Vec<PatternPoint>,
    },
    #[serde(rename_all = "camelCase")]
    Launch {
        launch: Option<LaunchPoint>,
        defects: DefectCounts,
        components: Vec<ComponentPoint>,
        group_by: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WidgetResult {
    Ok { data: WidgetData },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BoardData {
    pub generated_at: DateTime<Utc>,
    pub widgets: HashMap<String, WidgetResult>,
}

#[derive(sqlx::FromRow)]
struct LaunchRow {
    id: Uuid,
    number: i64,
    name: String,
    status: String,
    created_at: DateTime<Utc>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    branch: Option<String>,
    env_key: Option<String>,
    count_total: i32,
    count_passed: i32,
    count_failed: i32,
    count_broken: i32,
    count_skipped: i32,
    count_unknown: i32,
    count_flaky: i32,
    count_muted: i32,
}

fn point(r: LaunchRow) -> LaunchPoint {
    LaunchPoint {
        id: r.id,
        number: r.number,
        name: r.name,
        status: r.status,
        started_at: r.started_at,
        duration_ms: r
            .finished_at
            .map(|f| (f - r.started_at).num_milliseconds().max(0)),
        finished_at: r.finished_at,
        branch: r.branch,
        environment: r.env_key,
        total: r.count_total.into(),
        passed: r.count_passed.into(),
        failed: r.count_failed.into(),
        broken: r.count_broken.into(),
        skipped: r.count_skipped.into(),
        unknown: r.count_unknown.into(),
        flaky: r.count_flaky.into(),
        muted: r.count_muted.into(),
    }
}

const LAUNCH_COLS: &str = "id, number, name, status, created_at, started_at, finished_at, branch, env_key, \
    count_total, count_passed, count_failed, count_broken, count_skipped, count_unknown, count_flaky, count_muted";

type TrendKey = (Option<String>, Option<String>, u32, bool);

impl Store {
    /// The data of every widget. A widget that fails answers an error entry and the others
    /// still come back. Widgets that read the same trend share one computation.
    pub async fn board_data(
        &self,
        project_id: Uuid,
        settings: &ProjectSettings,
        widgets: &[BoardWidget],
    ) -> BoardData {
        let mut trends: HashMap<TrendKey, Dashboard> = HashMap::new();
        let mut out = HashMap::new();
        for w in widgets.iter().take(MAX_WIDGETS) {
            let r = match self.widget_data(project_id, settings, w, &mut trends).await {
                Ok(data) => WidgetResult::Ok { data },
                Err(e) => {
                    tracing::warn!(widget = %w.id, error = %e, "dashboard widget failed");
                    let timeout = e.to_string().contains("statement timeout");
                    WidgetResult::Error {
                        message: if timeout {
                            "the widget took too long; narrow its range".into()
                        } else {
                            "the widget could not be computed".into()
                        },
                    }
                }
            };
            out.insert(w.id.clone(), r);
        }
        BoardData {
            generated_at: Utc::now(),
            widgets: out,
        }
    }

    async fn widget_data(
        &self,
        project_id: Uuid,
        settings: &ProjectSettings,
        w: &BoardWidget,
        trends: &mut HashMap<TrendKey, Dashboard>,
    ) -> anyhow::Result<WidgetData> {
        let s = &w.settings;
        let branch = s.branch.clone().filter(|b| !b.is_empty());
        let env = s.environment.clone().filter(|b| !b.is_empty());
        let days = s.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
        let limit = i64::from(s.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT));
        let since = Utc::now() - chrono::Duration::days(i64::from(days));
        match w.kind.source() {
            Source::Launches => {
                let rows: Vec<LaunchRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "select {LAUNCH_COLS} from launches where project_id = $1 and not debug and created_at >= $2 \
                       and ($3::text is null or branch = $3) and ($4::text is null or env_key = $4) \
                     order by created_at desc, id limit $5"
                )))
                .bind(project_id)
                .bind(since)
                .bind(branch.as_deref().map(no_nul))
                .bind(env.as_deref().map(no_nul))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
                let mut launches: Vec<LaunchPoint> = rows.into_iter().map(point).collect();
                launches.reverse();
                Ok(WidgetData::Launches { launches })
            }
            Source::Trend => {
                let weekly = s.bucket == Some(WidgetBucket::Week);
                let key = (branch.clone(), env.clone(), days, weekly);
                if !trends.contains_key(&key) {
                    let to = Utc::now().date_naive();
                    let from = to
                        .checked_sub_days(Days::new(u64::from(days) - 1))
                        .unwrap_or(to);
                    let d = self
                        .dashboard(
                            project_id,
                            settings,
                            &DashboardQuery {
                                branch,
                                environment: env,
                                from,
                                to,
                                bucket: if weekly { Bucket::Week } else { Bucket::Day },
                            },
                        )
                        .await?;
                    trends.insert(key.clone(), d);
                }
                let mut d = trends.get(&key).cloned().unwrap_or_else(|| unreachable!());
                let n = limit as usize;
                d.flaky_top.truncate(n);
                d.most_failing.truncate(n);
                Ok(WidgetData::Trend { trend: Box::new(d) })
            }
            Source::Growth => {
                let bucket = if s.bucket == Some(WidgetBucket::Week) {
                    "week"
                } else {
                    "day"
                };
                let rows: Vec<(NaiveDate, i64)> = sqlx::query_as(
                    "select date_trunc($1, first_seen_at at time zone 'UTC')::date, count(*) from test_cases \
                     where project_id = $2 and first_seen_at >= $3 group by 1 order by 1",
                )
                .bind(bucket)
                .bind(project_id)
                .bind(since)
                .fetch_all(&self.pool)
                .await?;
                Ok(WidgetData::Growth {
                    points: rows
                        .into_iter()
                        .map(|(t, added)| GrowthPoint { t, added })
                        .collect(),
                })
            }
            Source::Patterns => {
                let rows: Vec<PatternRow> = sqlx::query_as(
                    "select fingerprint, left(message, 500), occurrences, first_seen_at, last_seen_at \
                     from failure_fingerprints where project_id = $1 and last_seen_at >= $2 \
                     order by occurrences desc, fingerprint limit $3",
                )
                .bind(project_id)
                .bind(since)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
                Ok(WidgetData::Patterns {
                    items: rows
                        .into_iter()
                        .map(|r| PatternPoint {
                            fingerprint: r.0,
                            message: r.1,
                            occurrences: r.2,
                            first_seen_at: r.3,
                            last_seen_at: r.4,
                        })
                        .collect(),
                })
            }
            Source::Launch => {
                let group_by = s
                    .group_by
                    .clone()
                    .filter(|g| !g.is_empty())
                    .unwrap_or_else(|| "suite".into());
                let row: Option<LaunchRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                    "select {LAUNCH_COLS} from launches where project_id = $1 and not debug \
                       and ($2::uuid is null or id = $2) and ($3::text is null or branch = $3) \
                       and ($4::text is null or env_key = $4) \
                     order by created_at desc, id limit 1"
                )))
                .bind(project_id)
                .bind(s.launch_id)
                .bind(branch.as_deref().map(no_nul))
                .bind(env.as_deref().map(no_nul))
                .fetch_optional(&self.pool)
                .await?;
                let Some(lr) = row else {
                    return Ok(WidgetData::Launch {
                        launch: None,
                        defects: DefectCounts::default(),
                        components: Vec::new(),
                        group_by,
                    });
                };
                let created = lr.created_at;
                let launch = point(lr);
                let mut defects = DefectCounts::default();
                let mut components = Vec::new();
                if w.kind == WidgetType::LaunchExecution {
                    let rows: Vec<(String, i64)> = sqlx::query_as(
                        "select coalesce(defect->>'group', 'toInvestigate'), count(*) from results \
                         where launch_id = $1 and launch_created_at = $2 and status in ('failed', 'broken') group by 1",
                    )
                    .bind(launch.id)
                    .bind(created)
                    .fetch_all(&self.pool)
                    .await?;
                    for (g, n) in rows {
                        match g.as_str() {
                            "productBug" => defects.product_bug += n,
                            "automationBug" => defects.automation_bug += n,
                            "systemIssue" => defects.system_issue += n,
                            "noDefect" => defects.no_defect += n,
                            _ => defects.to_investigate += n,
                        }
                    }
                } else {
                    let rows: Vec<(String, i64, i64, i64, i64, i64)> = sqlx::query_as(
                        "select coalesce(x.v, '(none)'), count(*), \
                           count(*) filter (where r.status = 'passed'), count(*) filter (where r.status = 'failed'), \
                           count(*) filter (where r.status = 'broken'), count(*) filter (where r.status = 'skipped') \
                         from results r \
                         left join lateral (select l->>'value' as v from jsonb_array_elements(r.labels) l \
                           where l->>'name' = $3 limit 1) x on true \
                         where r.launch_id = $1 and r.launch_created_at = $2 group by 1 order by 2 desc, 1 limit $4",
                    )
                    .bind(launch.id)
                    .bind(created)
                    .bind(no_nul(&group_by))
                    .bind(COMPONENTS)
                    .fetch_all(&self.pool)
                    .await?;
                    components = rows
                        .into_iter()
                        .map(|r| ComponentPoint {
                            name: r.0.chars().take(200).collect(),
                            total: r.1,
                            passed: r.2,
                            failed: r.3,
                            broken: r.4,
                            skipped: r.5,
                        })
                        .collect();
                }
                Ok(WidgetData::Launch {
                    launch: Some(launch),
                    defects,
                    components,
                    group_by,
                })
            }
        }
    }
}
