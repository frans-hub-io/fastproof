//! Dashboards of a project: stored definitions (name, owner, lock, widgets on a 12-column grid)
//! and the data of their widgets. Every widget reads existing aggregates (`launches` counters,
//! `series_daily`, `series_stats`, `failure_fingerprints`) within a bounded window; the two
//! per-launch widgets read the results of one launch.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
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
/// Most recent attribute rows the cumulative trend reads.
const ATTRIBUTE_LAUNCHES: i64 = 5000;
/// Newest tracked issues (open first) the unique bugs widget counts links for.
pub const BUG_CANDIDATES: i64 = 500;

#[derive(sqlx::FromRow)]
struct CumulativeRow {
    value: String,
    launches: i64,
    first_at: DateTime<Utc>,
    last_at: DateTime<Utc>,
    total: i64,
    passed: i64,
    failed: i64,
    broken: i64,
    skipped: i64,
    unknown: i64,
}

#[derive(sqlx::FromRow)]
struct BugRow {
    id: Uuid,
    tracker_id: String,
    tracker_type: String,
    issue_key: String,
    url: Option<String>,
    title: String,
    state: String,
    closed: bool,
    created_at: DateTime<Utc>,
    links: i64,
    tests: i64,
}

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
    CumulativeTrend,
    UniqueBugs,
    ActivityStream,
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
    Attribute,
    Bugs,
    Activity,
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
            CumulativeTrend => Source::Attribute,
            UniqueBugs => Source::Bugs,
            ActivityStream => Source::Activity,
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
    /// Launch attribute the cumulative trend groups by (for example `build`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    /// What the popular-patterns widget counts (fingerprints by default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patterns: Option<super::types::WidgetPatterns>,
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
    /// Display name of the owner; the API shows it only to the owner and to project managers.
    pub owner: String,
    /// The caller owns this dashboard.
    #[serde(default)]
    pub owned: bool,
    #[serde(skip)]
    pub owner_key: String,
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
    #[serde(default)]
    pub owned: bool,
    #[serde(skip)]
    pub owner_key: String,
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
            || w.x > GRID_COLUMNS - w.w
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
            (&s.attribute, 64, "attribute"),
        ] {
            if v.as_ref()
                .is_some_and(|v| v.chars().count() > max || !no_nul_in(v))
            {
                return Err(format!("{what} has at most {max} characters"));
            }
        }
    }
    check_computations(&b.widgets)
}

/// Distinct trend computations a request may run.
pub const MAX_TREND_KEYS: usize = 4;
/// Distinct scans of one launch's results a request may run.
pub const MAX_LAUNCH_SCANS: usize = 3;
/// Distinct attribute groupings and activity streams a dashboard may hold.
pub const MAX_ATTRIBUTE_SCANS: usize = 3;
pub const MAX_ACTIVITY_STREAMS: usize = 2;

/// Widgets that differ only in how they draw share one query, so only distinct queries count.
fn check_computations(widgets: &[BoardWidget]) -> Result<(), String> {
    let mut trends = HashSet::new();
    let mut scans = HashSet::new();
    let mut attrs = HashSet::new();
    let mut streams = HashSet::new();
    for w in widgets {
        let s = &w.settings;
        match w.kind.source() {
            Source::Trend => {
                trends.insert(trend_key(s));
            }
            Source::Launch => {
                scans.insert((
                    w.kind,
                    s.launch_id,
                    s.branch.clone(),
                    s.environment.clone(),
                    s.group_by.clone().filter(|g| !g.is_empty()),
                ));
            }
            Source::Attribute => {
                attrs.insert((
                    trend_key(s),
                    s.limit,
                    s.attribute.clone().filter(|a| !a.is_empty()),
                ));
            }
            Source::Activity => {
                streams.insert((s.days, s.limit));
            }
            _ => {}
        }
    }
    if attrs.len() > MAX_ATTRIBUTE_SCANS {
        return Err(format!(
            "a dashboard has at most {MAX_ATTRIBUTE_SCANS} different cumulative trends"
        ));
    }
    if streams.len() > MAX_ACTIVITY_STREAMS {
        return Err(format!(
            "a dashboard has at most {MAX_ACTIVITY_STREAMS} different activity streams"
        ));
    }
    if trends.len() > MAX_TREND_KEYS {
        return Err(format!(
            "widgets of a dashboard use at most {MAX_TREND_KEYS} different combinations of days, bucket, branch and environment for trends and lists of tests"
        ));
    }
    if scans.len() > MAX_LAUNCH_SCANS {
        return Err(format!(
            "a dashboard has at most {MAX_LAUNCH_SCANS} different per-launch widgets"
        ));
    }
    Ok(())
}

fn trend_key(s: &WidgetSettings) -> TrendKey {
    (
        s.branch.clone().filter(|b| !b.is_empty()),
        s.environment.clone().filter(|b| !b.is_empty()),
        s.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS),
        s.bucket == Some(WidgetBucket::Week),
    )
}

#[derive(sqlx::FromRow)]
struct BoardRow {
    id: Uuid,
    name: String,
    description: String,
    owner: String,
    owner_key: String,
    locked: bool,
    revision: i64,
    widgets: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn board_of(r: BoardRow) -> Result<Board, BoardError> {
    let widgets = serde_json::from_value(r.widgets)
        .map_err(|e| BoardError::Other(anyhow::anyhow!("stored widgets: {e}")))?;
    Ok(Board {
        id: r.id,
        name: r.name,
        description: r.description,
        owner: r.owner,
        owned: false,
        owner_key: r.owner_key,
        locked: r.locked,
        revision: r.revision,
        widgets,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    name: String,
    description: String,
    owner: String,
    owner_key: String,
    locked: bool,
    revision: i64,
    widget_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

type PatternRow = (String, String, i64, DateTime<Utc>, DateTime<Utc>);

const BOARD_COLS: &str =
    "id, name, description, owner, owner_key, locked, revision, widgets, created_at, updated_at";

/// Who changes a dashboard: a stable identity (`token:<id>` or `<kind>:<name>`), the name shown
/// to others, and whether the caller manages the project (may override a lock).
#[derive(Clone, Copy, Debug)]
pub struct Actor<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub manager: bool,
}

impl Store {
    pub async fn list_boards(&self, project_id: Uuid) -> anyhow::Result<Vec<BoardSummary>> {
        let rows: Vec<SummaryRow> = sqlx::query_as(
            "select id, name, description, owner, owner_key, locked, revision, \
               jsonb_array_length(widgets)::bigint as widget_count, created_at, updated_at \
             from dashboards where project_id = $1 order by created_at, id limit $2",
        )
        .bind(project_id)
        .bind(MAX_BOARDS)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| BoardSummary {
                id: r.id,
                name: r.name,
                description: r.description,
                owner: r.owner,
                owned: false,
                owner_key: r.owner_key,
                locked: r.locked,
                revision: r.revision,
                widget_count: r.widget_count,
                created_at: r.created_at,
                updated_at: r.updated_at,
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

    /// Creates a dashboard; the count check and the insert run under a per-project lock, so
    /// parallel creates cannot pass the limit.
    pub async fn create_board(
        &self,
        project_id: Uuid,
        owner: Actor<'_>,
        w: &BoardWrite,
    ) -> Result<Board, BoardError> {
        validate(w).map_err(BoardError::Invalid)?;
        let widgets = serde_json::to_value(&w.widgets)
            .map_err(|e| BoardError::Other(anyhow::anyhow!("widgets: {e}")))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("dashboards:{project_id}"))
            .execute(&mut *tx)
            .await?;
        let n: i64 = sqlx::query_scalar("select count(*) from dashboards where project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
        if n >= MAX_BOARDS {
            return Err(BoardError::Limit);
        }
        let r: BoardRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "insert into dashboards (id, project_id, name, description, owner, owner_key, widgets, updated_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $5) returning {BOARD_COLS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(w.name.trim())
        .bind(&w.description)
        .bind(no_nul(owner.name))
        .bind(no_nul(owner.key))
        .bind(widgets)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        board_of(r)
    }

    /// Replaces name, description and widgets if the revision matches. A locked dashboard is
    /// changed only by its owner or by a manager.
    pub async fn update_board(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        actor: Actor<'_>,
        w: &BoardWrite,
    ) -> Result<Board, BoardError> {
        validate(w).map_err(BoardError::Invalid)?;
        let widgets = serde_json::to_value(&w.widgets)
            .map_err(|e| BoardError::Other(anyhow::anyhow!("widgets: {e}")))?;
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "update dashboards set name = $4, description = $5, widgets = $6, revision = revision + 1, \
               updated_by = $7, updated_at = now() \
             where project_id = $1 and id = $2 and revision = $3 and (not locked or owner_key = $8 or $9) \
             returning {BOARD_COLS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(revision)
        .bind(w.name.trim())
        .bind(&w.description)
        .bind(widgets)
        .bind(no_nul(actor.name))
        .bind(no_nul(actor.key))
        .bind(actor.manager)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => board_of(r),
            None => Err(self
                .why_not_changed(project_id, id, revision, actor)
                .await?),
        }
    }

    /// Locks or unlocks if the revision matches; only the owner or a manager may.
    pub async fn set_board_lock(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        locked: bool,
        actor: Actor<'_>,
    ) -> Result<Board, BoardError> {
        let r: Option<BoardRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "update dashboards set locked = $4, revision = revision + 1, updated_by = $5, updated_at = now() \
             where project_id = $1 and id = $2 and revision = $3 and (owner_key = $6 or $7) returning {BOARD_COLS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(revision)
        .bind(locked)
        .bind(no_nul(actor.name))
        .bind(no_nul(actor.key))
        .bind(actor.manager)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => board_of(r),
            None => Err(self
                .why_not_changed(project_id, id, revision, actor)
                .await?),
        }
    }

    /// Deletes a dashboard if the revision matches; a locked one only for its owner or a manager.
    pub async fn delete_board(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        actor: Actor<'_>,
    ) -> Result<(), BoardError> {
        let n = sqlx::query(
            "delete from dashboards where project_id = $1 and id = $2 and revision = $3 \
               and (not locked or owner_key = $4 or $5)",
        )
        .bind(project_id)
        .bind(id)
        .bind(revision)
        .bind(no_nul(actor.key))
        .bind(actor.manager)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 1 {
            self.board_cache.forget(id);
            return Ok(());
        }
        Err(self
            .why_not_changed(project_id, id, revision, actor)
            .await?)
    }

    async fn why_not_changed(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        actor: Actor<'_>,
    ) -> Result<BoardError, BoardError> {
        let cur: Option<(i64, bool, String, Option<String>)> = sqlx::query_as(
            "select revision, locked, owner_key, updated_by from dashboards where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match cur {
            None => BoardError::NotFound,
            Some((rev, locked, owner_key, by)) => {
                if locked && owner_key != actor.key && !actor.manager {
                    BoardError::Locked
                } else if rev != revision {
                    BoardError::Stale {
                        current_revision: rev,
                        changed_by: by,
                    }
                } else {
                    BoardError::Locked
                }
            }
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

/// The launches of one value of a launch attribute, added up.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CumulativePoint {
    pub value: String,
    pub launches: i64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub broken: i64,
    pub skipped: i64,
    pub unknown: i64,
}

/// A tracked issue and the tests that point to it.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BugPoint {
    pub id: Uuid,
    pub tracker_id: String,
    pub tracker_type: String,
    pub key: String,
    pub url: Option<String>,
    pub title: String,
    pub state: String,
    pub closed: bool,
    pub links: i64,
    pub tests: i64,
    pub created_at: DateTime<Utc>,
}

/// One audit record of the project, without its detail.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPoint {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub principal: String,
    pub action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WidgetData {
    Cumulative {
        attribute: String,
        values: Vec<CumulativePoint>,
    },
    Bugs {
        items: Vec<BugPoint>,
    },
    Activity {
        items: Vec<ActivityPoint>,
    },
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

fn is_timeout(e: &anyhow::Error) -> bool {
    e.to_string().contains("statement timeout")
}

/// Time limits of dashboard data.
#[derive(Clone, Copy, Debug)]
pub struct BoardLimits {
    /// Time one request may spend on all widgets; later widgets answer an error.
    pub total: Duration,
    /// Statement timeout of every widget query.
    pub per_widget: Duration,
    /// How long computed data is shared between viewers.
    pub cache_ttl: Duration,
}

impl Default for BoardLimits {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(25),
            per_widget: Duration::from_secs(10),
            cache_ttl: Duration::from_secs(60),
        }
    }
}

/// Connections dashboard widgets may hold at once, per replica.
const BOARD_CONNECTIONS: u32 = 3;
const CACHE_ENTRIES: usize = 256;

type CacheMap = HashMap<(Uuid, i64), (Instant, Arc<BoardData>)>;

/// Data of saved dashboards by (id, revision), shared by every viewer for a short time.
#[derive(Clone, Debug, Default)]
pub struct BoardCache {
    inner: Arc<Mutex<CacheMap>>,
}

impl BoardCache {
    fn map(&self) -> std::sync::MutexGuard<'_, CacheMap> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, id: Uuid, revision: i64, ttl: Duration) -> Option<Arc<BoardData>> {
        let m = self.map();
        m.get(&(id, revision))
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, d)| d.clone())
    }

    fn put(&self, id: Uuid, revision: i64, ttl: Duration, d: Arc<BoardData>) {
        let mut m = self.map();
        m.retain(|_, (at, _)| at.elapsed() < ttl);
        while m.len() >= CACHE_ENTRIES {
            let Some(oldest) = m.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| *k) else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert((id, revision), (Instant::now(), d));
    }

    pub(crate) fn forget(&self, id: Uuid) {
        self.map().retain(|(i, _), _| *i != id);
    }
}

/// The pool of dashboard queries, made on first use from the main pool's connection options.
#[derive(Clone, Debug, Default)]
pub struct BoardPool(Arc<OnceLock<PgPool>>);

impl Store {
    /// Sets the time limits of dashboard data (before the first use).
    pub fn with_board_limits(mut self, l: BoardLimits) -> Self {
        self.board_limits = l;
        self.board_pool = BoardPool::default();
        self
    }

    fn board_pool(&self) -> PgPool {
        self.board_pool
            .0
            .get_or_init(|| {
                let opts = (*self.pool.connect_options()).clone().options([(
                    "statement_timeout",
                    format!("{}ms", self.board_limits.per_widget.as_millis().max(1)),
                )]);
                PgPoolOptions::new()
                    .max_connections(BOARD_CONNECTIONS)
                    .acquire_timeout(Duration::from_secs(10))
                    .connect_lazy_with(opts)
            })
            .clone()
    }

    /// The statement timeout a dashboard query runs with (for tests and diagnostics).
    pub async fn board_statement_timeout(&self) -> anyhow::Result<String> {
        Ok(sqlx::query_scalar("show statement_timeout")
            .fetch_one(&self.board_pool())
            .await?)
    }

    /// Data of a stored dashboard still held from an earlier request.
    pub fn cached_board_data(&self, id: Uuid, revision: i64) -> Option<Arc<BoardData>> {
        self.board_cache
            .get(id, revision, self.board_limits.cache_ttl)
    }

    /// Computes the data of a stored dashboard and keeps it for other viewers.
    pub async fn board_data_cached(
        &self,
        project_id: Uuid,
        id: Uuid,
        revision: i64,
        settings: &ProjectSettings,
        widgets: &[BoardWidget],
    ) -> Arc<BoardData> {
        if let Some(d) = self.cached_board_data(id, revision) {
            return d;
        }
        let d = Arc::new(self.board_data(project_id, settings, widgets).await);
        self.board_cache
            .put(id, revision, self.board_limits.cache_ttl, d.clone());
        d
    }

    /// The data of every widget. A widget that fails answers an error entry and the others
    /// still come back. Widgets that read the same trend share one computation. The whole
    /// request stops at [`BoardLimits::total`] and every query at [`BoardLimits::per_widget`].
    pub async fn board_data(
        &self,
        project_id: Uuid,
        settings: &ProjectSettings,
        widgets: &[BoardWidget],
    ) -> BoardData {
        let me = Store {
            pool: self.board_pool(),
            ..self.clone()
        };
        let deadline = tokio::time::Instant::now() + self.board_limits.total;
        let mut trends: HashMap<TrendKey, Dashboard> = HashMap::new();
        let mut out = HashMap::new();
        for w in widgets.iter().take(MAX_WIDGETS) {
            if w.kind.source() == Source::Activity {
                continue;
            }
            let too_long = || WidgetResult::Error {
                message: "the widget took too long; narrow its range".into(),
            };
            let r = if tokio::time::Instant::now() >= deadline {
                too_long()
            } else {
                match tokio::time::timeout_at(
                    deadline,
                    me.widget_data(project_id, settings, w, &mut trends),
                )
                .await
                {
                    Ok(Ok(data)) => WidgetResult::Ok { data },
                    Ok(Err(e)) => {
                        tracing::warn!(widget = %w.id, error = %e, "dashboard widget failed");
                        if crate::analysis::boards::is_timeout(&e) {
                            too_long()
                        } else {
                            WidgetResult::Error {
                                message: "the widget could not be computed".into(),
                            }
                        }
                    }
                    Err(_) => too_long(),
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
                let key = trend_key(s);
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
            Source::Activity => anyhow::bail!("activity streams are read by the API"),
            Source::Attribute => {
                let attribute = s.attribute.clone().filter(|a| !a.is_empty());
                let Some(attribute) = attribute else {
                    return Ok(WidgetData::Cumulative {
                        attribute: String::new(),
                        values: Vec::new(),
                    });
                };
                let rows: Vec<CumulativeRow> = sqlx::query_as(
                    "with idx as (select a.launch_id, a.value from launch_attr_index a \
                       join launches l on l.id = a.launch_id and not l.debug \
                         and ($4::text is null or l.branch = $4) and ($5::text is null or l.env_key = $5) \
                       where a.project_id = $1 and a.key = $2 and a.launch_created_at >= $3 \
                       order by a.launch_created_at desc limit $6) \
                     select idx.value, count(*)::bigint as launches, min(l.started_at) as first_at, \
                       max(l.started_at) as last_at, sum(l.count_total)::bigint as total, \
                       sum(l.count_passed)::bigint as passed, sum(l.count_failed)::bigint as failed, \
                       sum(l.count_broken)::bigint as broken, sum(l.count_skipped)::bigint as skipped, \
                       sum(l.count_unknown)::bigint as unknown \
                     from idx join launches l on l.id = idx.launch_id \
                     group by idx.value order by min(l.started_at) desc, idx.value limit $7",
                )
                .bind(project_id)
                .bind(no_nul(&attribute))
                .bind(since)
                .bind(branch.as_deref().map(no_nul))
                .bind(env.as_deref().map(no_nul))
                .bind(ATTRIBUTE_LAUNCHES)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
                let mut values: Vec<CumulativePoint> = rows
                    .into_iter()
                    .map(|r| CumulativePoint {
                        value: r.value.chars().take(200).collect(),
                        launches: r.launches,
                        first_at: r.first_at,
                        last_at: r.last_at,
                        total: r.total,
                        passed: r.passed,
                        failed: r.failed,
                        broken: r.broken,
                        skipped: r.skipped,
                        unknown: r.unknown,
                    })
                    .collect();
                values.reverse();
                Ok(WidgetData::Cumulative { attribute, values })
            }
            Source::Bugs => {
                let rows: Vec<BugRow> = sqlx::query_as(
                    "select i.id, i.tracker_id, i.tracker_type, i.issue_key, i.url, left(i.title, 300) as title, \
                       left(i.state, 100) as state, i.category = 'closed' as closed, i.created_at, \
                       coalesce(c.links, 0)::bigint as links, coalesce(c.tests, 0)::bigint as tests \
                     from (select * from tracker_issues where project_id = $1 and category in ('open', 'closed') \
                         and issue_key is not null and (category = 'open' or updated_at >= $2) \
                       order by (category = 'open') desc, created_at desc, id limit $4) i \
                     left join lateral ( \
                       select count(*) as links, sum(case when x.result_id is not null then 1 else 0 end \
                         + cardinality(x.series_ids)) as tests \
                       from (select result_id, series_ids from tracker_issue_links where issue_id = i.id limit 100) x) c on true \
                     order by (i.category = 'open') desc, coalesce(c.tests, 0) desc, i.created_at desc, i.id limit $3",
                )
                .bind(project_id)
                .bind(since)
                .bind(limit)
                .bind(BUG_CANDIDATES)
                .fetch_all(&self.pool)
                .await?;
                Ok(WidgetData::Bugs {
                    items: rows
                        .into_iter()
                        .map(|r| BugPoint {
                            id: r.id,
                            tracker_id: r.tracker_id,
                            tracker_type: r.tracker_type,
                            key: r.issue_key,
                            url: r.url.filter(|u| !u.is_empty()),
                            title: r.title,
                            state: r.state,
                            closed: r.closed,
                            links: r.links,
                            tests: r.tests,
                            created_at: r.created_at,
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
