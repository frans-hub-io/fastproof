//! Import of the history of Allure reports: the job rows of an import, and the storing of one
//! past report as a launch whose results join the series live results use.
//!
//! A past report becomes a finished launch dated at its first result, flagged by the attribute
//! `imported=allure-history`. Its results go through the same path as uploaded ones
//! ([`Store::persist_attempts`], which runs the analysis hook), oldest report first, so
//! transitions and flakiness of each result see the reports before it. Afterwards the
//! statistics of the series it touched are rebuilt from the stored results, because
//! the hook appends statuses in arrival order and a past report may be older than live runs.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use fastproof_model::identity::environment_hash;
use fastproof_model::{
    HistoryPoint, HistoryResult, ParsedResult, SourceFormat, SourceRef, Stage, Status,
    StatusDetails, TestIdentity, is_flaky, split_series_key,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::postgres::PgRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::analysis::stats;
use crate::ingest::{LaunchRef, NewAttempt};
use crate::launches::LaunchStart;
use crate::{Attribute, LaunchEnvironment, LaunchVcs, Project, Store, no_nul, ts_ms};

/// Attribute key and value that flag a launch made from history files.
pub const IMPORT_ATTRIBUTE: (&str, &str) = ("imported", "allure-history");
/// Results written per transaction.
pub const CHUNK: usize = 500;
/// Series looked up per query.
const LOOKUP: usize = 5000;
/// Launch attributes kept per past report (trend data and the import flags).
const MAX_ATTRIBUTES: usize = 70;
/// Hours a past report may lie ahead of the clock.
const FUTURE_SLACK_HOURS: i64 = 24;

/// Imports a project may have queued or running.
pub const MAX_ACTIVE_PER_PROJECT: i64 = 1;
/// Imports queued or running on the whole server.
pub const MAX_ACTIVE_TOTAL: i64 = 2;
/// Imports a project may create per 24 hours.
pub const MAX_PER_PROJECT_DAY: i64 = 20;
/// Seconds without progress after which a running import is considered lost.
pub const STALE_AFTER_SECONDS: i64 = 300;

/// A file of an import, kept in blob storage until the import ends.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFile {
    pub name: String,
    pub blob_key: String,
    pub size: i64,
}

/// An import of Allure history.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryImport {
    pub id: Uuid,
    /// `allure2` or `allure3`.
    pub source: String,
    pub branch: Option<String>,
    pub environment: Option<String>,
    /// `queued`, `running`, `done` or `failed`.
    pub status: String,
    /// Past reports found (known once the files are read).
    pub points_total: i64,
    pub points_done: i64,
    pub launches_created: i64,
    /// Results stored.
    pub results_imported: i64,
    /// Results that a former import already stored.
    pub results_existing: i64,
    /// Results of tests the project does not know and the file does not name (Allure 2).
    pub results_unmatched: i64,
    /// Results or whole reports left out: older than the retention window, in the future, or
    /// not usable.
    pub results_skipped: i64,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Why an import was not created.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateDenied {
    Busy,
    Quota,
}

/// An import with what the worker needs.
#[derive(Clone, Debug)]
pub struct ClaimedImport {
    pub import: HistoryImport,
    pub project_id: Uuid,
    pub project_key: String,
    pub files: Vec<ImportFile>,
}

macro_rules! columns {
    () => {
        "id, source, branch, environment, status, points_total, points_done, launches_created, \
         results_imported, results_existing, results_unmatched, results_skipped, warnings, error, \
         created_by, created_at, finished_at"
    };
}

fn row(r: &PgRow) -> Result<HistoryImport, sqlx::Error> {
    let warnings: serde_json::Value = r.try_get("warnings")?;
    Ok(HistoryImport {
        id: r.try_get("id")?,
        source: r.try_get("source")?,
        branch: r.try_get("branch")?,
        environment: r.try_get("environment")?,
        status: r.try_get("status")?,
        points_total: r.try_get("points_total")?,
        points_done: r.try_get("points_done")?,
        launches_created: r.try_get("launches_created")?,
        results_imported: r.try_get("results_imported")?,
        results_existing: r.try_get("results_existing")?,
        results_unmatched: r.try_get("results_unmatched")?,
        results_skipped: r.try_get("results_skipped")?,
        warnings: serde_json::from_value(warnings).unwrap_or_default(),
        error: r.try_get("error")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        finished_at: r.try_get("finished_at")?,
    })
}

/// Counters of what an import did, added up over its past reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointOutcome {
    pub launch_created: bool,
    pub imported: u64,
    pub existing: u64,
    pub unmatched: u64,
    pub skipped: u64,
}

impl PointOutcome {
    pub fn add(&mut self, o: PointOutcome) {
        self.launch_created |= o.launch_created;
        self.imported += o.imported;
        self.existing += o.existing;
        self.unmatched += o.unmatched;
        self.skipped += o.skipped;
    }
}

/// Where the past reports of an import go.
#[derive(Clone, Debug)]
pub struct ImportTarget {
    pub source: &'static str,
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub created_by: String,
    /// Reports older than this are left out (the project's retention window would delete them).
    pub not_before: DateTime<Utc>,
}

impl Store {
    /// Creates a queued import, or says why not.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_history_import(
        &self,
        id: Uuid,
        project_id: Uuid,
        source: &str,
        branch: Option<&str>,
        environment: Option<&str>,
        files: &[ImportFile],
        created_by: &str,
        owner: &str,
    ) -> anyhow::Result<Result<HistoryImport, CreateDenied>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 820))")
            .bind(project_id.to_string())
            .execute(&mut *tx)
            .await?;
        let (mine, total): (i64, i64) = sqlx::query_as(
            "select count(*) filter (where project_id = $1), count(*) from history_imports \
             where status in ('queued', 'running')",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if mine >= MAX_ACTIVE_PER_PROJECT || total >= MAX_ACTIVE_TOTAL {
            return Ok(Err(CreateDenied::Busy));
        }
        let day: i64 = sqlx::query_scalar(
            "select count(*) from history_imports where project_id = $1 and created_at > now() - interval '24 hours'",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if day >= MAX_PER_PROJECT_DAY {
            return Ok(Err(CreateDenied::Quota));
        }
        let r = sqlx::query(concat!(
            "insert into history_imports (id, project_id, source, branch, environment, files, created_by, owner_key) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning ",
            columns!()
        ))
        .bind(id)
        .bind(project_id)
        .bind(source)
        .bind(branch.map(no_nul))
        .bind(environment.map(no_nul))
        .bind(serde_json::to_value(files)?)
        .bind(no_nul(created_by))
        .bind(no_nul(owner))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Ok(row(&r)?))
    }

    /// The import with its owner identity.
    pub async fn history_import(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<(HistoryImport, String)>> {
        let r = sqlx::query(concat!(
            "select ",
            columns!(),
            ", owner_key from history_imports where project_id = $1 and id = $2"
        ))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(|r| Ok((row(&r)?, r.try_get("owner_key")?)))
            .transpose()
    }

    /// The newest imports of a project.
    pub async fn history_imports(
        &self,
        project_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Vec<(HistoryImport, String)>> {
        let rows = sqlx::query(concat!(
            "select ",
            columns!(),
            ", owner_key from history_imports where project_id = $1 order by created_at desc limit $2"
        ))
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((row(r)?, r.try_get("owner_key")?)))
            .collect()
    }

    /// Takes a queued import for running; `None` when it is not queued any more.
    pub async fn start_history_import(&self, id: Uuid) -> anyhow::Result<Option<ClaimedImport>> {
        let r = sqlx::query(concat!(
            "update history_imports set status = 'running', updated_at = now() \
             where id = $1 and status = 'queued' returning ",
            columns!(),
            ", project_id, files, \
             (select p.key from projects p where p.id = history_imports.project_id) as project_key"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(|r| {
            let files: serde_json::Value = r.try_get("files")?;
            Ok::<_, sqlx::Error>(ClaimedImport {
                import: row(&r)?,
                project_id: r.try_get("project_id")?,
                project_key: r.try_get("project_key")?,
                files: serde_json::from_value(files).unwrap_or_default(),
            })
        })
        .transpose()
        .map_err(Into::into)
    }

    /// Records progress; `false` when the import is not running any more (stop).
    pub async fn progress_history_import(
        &self,
        id: Uuid,
        points_total: i64,
        points_done: i64,
        totals: &PointOutcome,
        launches: i64,
        warnings: &[String],
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update history_imports set points_total = $2, points_done = $3, launches_created = $4, \
             results_imported = $5, results_existing = $6, results_unmatched = $7, results_skipped = $8, \
             warnings = $9, updated_at = now() where id = $1 and status = 'running'",
        )
        .bind(id)
        .bind(points_total)
        .bind(points_done)
        .bind(launches)
        .bind(totals.imported as i64)
        .bind(totals.existing as i64)
        .bind(totals.unmatched as i64)
        .bind(totals.skipped as i64)
        .bind(serde_json::to_value(warnings)?)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn touch_history_import(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "update history_imports set updated_at = now() where id = $1 and status = 'running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Marks the import done and forgets its files; the keys are returned for deletion.
    pub async fn finish_history_import(&self, id: Uuid) -> anyhow::Result<bool> {
        Ok(sqlx::query(
            "update history_imports set status = 'done', finished_at = now(), updated_at = now() \
             where id = $1 and status = 'running'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn fail_history_import(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update history_imports set status = 'failed', error = $2, finished_at = now(), updated_at = now() \
             where id = $1 and status in ('queued', 'running')",
        )
        .bind(id)
        .bind(error.chars().take(500).collect::<String>())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fails running imports without progress and imports queued for an hour.
    pub async fn fail_stale_history_imports(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "update history_imports set status = 'failed', error = 'the import was interrupted', \
             finished_at = now(), updated_at = now() \
             where (status = 'running' and updated_at < now() - make_interval(secs => $1)) \
                or (status = 'queued' and updated_at < now() - interval '1 hour')",
        )
        .bind(STALE_AFTER_SECONDS as f64)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Imports that ended and still name files: their files are deleted.
    pub async fn history_import_leftovers(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, Vec<ImportFile>)>> {
        let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
            "select id, files from history_imports where status in ('done', 'failed') \
             and files <> '[]'::jsonb and updated_at < now() - interval '1 minute' order by updated_at limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, f)| (id, serde_json::from_value(f).unwrap_or_default()))
            .collect())
    }

    pub async fn forget_history_import_files(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("update history_imports set files = '[]'::jsonb where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Removes the rows of imports older than 30 days.
    pub async fn delete_old_history_imports(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "delete from history_imports where status in ('done', 'failed') \
             and finished_at < now() - interval '30 days'",
        )
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}

/// A series that exists, with what its results need.
struct Known {
    series_id: Uuid,
    test_case_hash: String,
    full_name: String,
    display_name: String,
    parameters_hash: String,
    env_key: Option<String>,
    series_first: DateTime<Utc>,
    series_last: DateTime<Utc>,
    case_first: DateTime<Utc>,
    case_last: DateTime<Utc>,
}

/// First and last seen times of a series and its test case before the report was stored.
#[derive(Clone, Copy)]
struct Seen {
    series_first: DateTime<Utc>,
    series_last: DateTime<Utc>,
    case_first: DateTime<Utc>,
    case_last: DateTime<Utc>,
}

/// What a series had before the report was stored.
#[derive(Default)]
struct Before {
    last_statuses: String,
    raw: i64,
    last_passed_at: Option<DateTime<Utc>>,
    last_failed_at: Option<DateTime<Utc>>,
    last_flaky_at: Option<DateTime<Utc>>,
    seen: Option<Seen>,
}

fn external_id(t: &ImportTarget, key: &str) -> String {
    let mut h = Sha256::new();
    for part in [
        t.source,
        t.branch.as_deref().unwrap_or(""),
        t.environment.as_deref().unwrap_or(""),
        key,
    ] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    format!(
        "allure-history:{}:{}",
        t.source,
        &hex::encode(h.finalize())[..40]
    )
}

/// The series key of a result in this import: Allure 2 keys have no environment part, which
/// live results of a named environment carry.
fn effective_key(r: &HistoryResult, env: Option<&str>) -> Option<(String, Option<String>)> {
    let (_, _, has_env) = split_series_key(&r.series_key)?;
    if has_env.is_some() || r.environment.is_some() {
        return Some((r.series_key.clone(), r.environment.clone()));
    }
    match environment_hash(env) {
        Some(h) => Some((format!("{}.{h}", r.series_key), env.map(str::to_string))),
        None => Some((r.series_key.clone(), None)),
    }
}

fn attributes(source: &str, p: &HistoryPoint) -> Vec<Attribute> {
    let mut a = vec![
        Attribute {
            key: IMPORT_ATTRIBUTE.0.into(),
            value: IMPORT_ATTRIBUTE.1.into(),
        },
        Attribute {
            key: "history.source".into(),
            value: source.into(),
        },
    ];
    let mut add = |k: String, v: String| {
        if a.len() < MAX_ATTRIBUTES {
            a.push(Attribute { key: k, value: v });
        }
    };
    if let Some(u) = &p.url {
        add("history.reportUrl".into(), u.clone());
    }
    if let Some(t) = &p.trend.totals {
        for (k, v) in [
            ("total", t.total),
            ("passed", t.passed),
            ("failed", t.failed),
            ("broken", t.broken),
            ("skipped", t.skipped),
            ("unknown", t.unknown),
        ] {
            add(format!("history.report.{k}"), v.to_string());
        }
    }
    if let Some(d) = p.trend.duration_ms {
        add("history.report.durationMs".into(), d.to_string());
    }
    if let Some(v) = p.trend.retries {
        add("history.report.retries".into(), v.to_string());
    }
    if let Some(v) = p.trend.runs {
        add("history.report.runs".into(), v.to_string());
    }
    for (k, v) in &p.trend.categories {
        add(format!("history.category.{k}"), v.to_string());
    }
    for (k, v) in &p.trend.metrics {
        add(format!("history.metric.{k}"), v.to_string());
    }
    a
}

fn parsed(
    r: &HistoryResult,
    series_key: &str,
    env: Option<String>,
    known: Option<&Known>,
    point_start: i64,
) -> Option<ParsedResult> {
    let (test_case_hash, parameters_hash, _) = split_series_key(series_key)?;
    let (full_name, display_name, env_key) = match known {
        Some(k) => (
            k.full_name.clone(),
            k.display_name.clone(),
            k.env_key.clone(),
        ),
        None => {
            let name = r.name.clone().or_else(|| r.full_name.clone())?;
            (
                r.full_name.clone().unwrap_or_else(|| name.clone()),
                name,
                env,
            )
        }
    };
    let mut identity = TestIdentity {
        full_name: full_name.clone(),
        display_name: display_name.clone(),
        test_case_hash: known.map_or(test_case_hash.to_string(), |k| k.test_case_hash.clone()),
        parameters_hash: known.map_or(parameters_hash.to_string(), |k| k.parameters_hash.clone()),
        series_key: series_key.to_string(),
        source: SourceRef::default(),
        ..Default::default()
    };
    identity.environment_hash = split_series_key(series_key).and_then(|k| k.2.map(str::to_string));
    identity.env_key = env_key;
    let start = r
        .start_ms
        .or(r.stop_ms.map(|s| s - r.duration_ms.unwrap_or(0)));
    let start = start.unwrap_or(point_start);
    let stop = r.stop_ms.unwrap_or(start + r.duration_ms.unwrap_or(0));
    Some(ParsedResult {
        identity,
        name: display_name,
        full_name,
        status: r.status,
        start: Some(start),
        stop: Some(stop),
        duration_ms: r.duration_ms.or(Some((stop - start).max(0))),
        status_details: StatusDetails {
            message: r.message.clone(),
            ..Default::default()
        },
        stage: Stage::Finished,
        format: SourceFormat::AllureResults,
        source_file: Some("allure-history".into()),
        ..Default::default()
    })
}

impl Store {
    async fn known_series(
        &self,
        project_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<HashMap<String, Known>> {
        let mut out = HashMap::with_capacity(keys.len());
        for chunk in keys.chunks(LOOKUP) {
            let rows = sqlx::query(
                "select s.series_key, s.id, t.test_case_hash, t.full_name, t.display_name, \
                   s.parameters_hash, s.env_key, s.first_seen_at as sf, s.last_seen_at as sl, \
                   t.first_seen_at as tf, t.last_seen_at as tl \
                 from series s join test_cases t on t.id = s.test_case_id \
                 where s.project_id = $1 and s.series_key = any($2)",
            )
            .bind(project_id)
            .bind(chunk)
            .fetch_all(&self.pool)
            .await?;
            for r in rows {
                out.insert(
                    r.try_get::<String, _>("series_key")?,
                    Known {
                        series_id: r.try_get("id")?,
                        test_case_hash: r.try_get("test_case_hash")?,
                        full_name: r.try_get("full_name")?,
                        display_name: r.try_get("display_name")?,
                        parameters_hash: r.try_get("parameters_hash")?,
                        env_key: r.try_get("env_key")?,
                        series_first: r.try_get("sf")?,
                        series_last: r.try_get("sl")?,
                        case_first: r.try_get("tf")?,
                        case_last: r.try_get("tl")?,
                    },
                );
            }
        }
        Ok(out)
    }

    async fn before_state(
        &self,
        project_id: Uuid,
        known: &HashMap<String, Known>,
    ) -> anyhow::Result<HashMap<Uuid, Before>> {
        let ids: Vec<Uuid> = known.values().map(|k| k.series_id).collect();
        let mut out: HashMap<Uuid, Before> = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(LOOKUP) {
            let rows = sqlx::query(
                "select s.sid, st.last_statuses, st.last_passed_at, st.last_failed_at, st.last_flaky_at, \
                   (select count(*) from (select 1 from results r join launches l on l.id = r.launch_id \
                     where r.series_id = s.sid and r.project_id = $2 and not l.debug limit 64) x)::bigint as raw \
                 from unnest($1::uuid[]) as s(sid) left join series_stats st on st.series_id = s.sid",
            )
            .bind(chunk)
            .bind(project_id)
            .fetch_all(&self.pool)
            .await?;
            for r in rows {
                out.insert(
                    r.try_get("sid")?,
                    Before {
                        last_statuses: r
                            .try_get::<Option<String>, _>("last_statuses")?
                            .unwrap_or_default(),
                        raw: r.try_get("raw")?,
                        last_passed_at: r.try_get("last_passed_at")?,
                        last_failed_at: r.try_get("last_failed_at")?,
                        last_flaky_at: r.try_get("last_flaky_at")?,
                        seen: None,
                    },
                );
            }
        }
        for k in known.values() {
            if let Some(b) = out.get_mut(&k.series_id) {
                b.seen = Some(Seen {
                    series_first: k.series_first,
                    series_last: k.series_last,
                    case_first: k.case_first,
                    case_last: k.case_last,
                });
            }
        }
        Ok(out)
    }

    /// Stores one past report as a launch (a launch of the same report that exists is
    /// completed, not repeated). Results of tests the project does not know are stored only
    /// when the report names them.
    pub async fn import_history_point(
        &self,
        project: &Project,
        target: &ImportTarget,
        point: &HistoryPoint,
        now: DateTime<Utc>,
    ) -> anyhow::Result<PointOutcome> {
        let mut out = PointOutcome::default();
        let timestamp = ts_ms(Some(point.timestamp_ms));
        let usable = timestamp.is_some_and(|t| {
            t >= target.not_before && t <= now + Duration::hours(FUTURE_SLACK_HOURS)
        });
        if !usable {
            out.skipped = point.results.len() as u64;
            return Ok(out);
        }

        let (from_ms, to_ms) = (
            target.not_before.timestamp_millis(),
            (now + Duration::hours(FUTURE_SLACK_HOURS)).timestamp_millis(),
        );
        let mut seen = HashSet::new();
        let mut items: Vec<(&HistoryResult, String, Option<String>)> = Vec::new();
        for r in &point.results {
            let at = r.start_ms.or(r.stop_ms);
            if at.is_some_and(|t| t < from_ms || t > to_ms) {
                out.skipped += 1;
                continue;
            }
            match effective_key(r, target.environment.as_deref()) {
                Some((k, env)) if seen.insert(k.clone()) => items.push((r, k, env)),
                _ => out.skipped += 1,
            }
        }
        let keys: Vec<String> = items.iter().map(|i| i.1.clone()).collect();
        let known = self.known_series(project.id, &keys).await?;
        let mut work: Vec<(ParsedResult, &HistoryResult)> = Vec::with_capacity(items.len());
        for (r, key, env) in &items {
            match parsed(r, key, env.clone(), known.get(key), point.timestamp_ms) {
                Some(p) => work.push((p, r)),
                None => out.unmatched += 1,
            }
        }
        if work.is_empty() {
            return Ok(out);
        }
        let first_ms = work
            .iter()
            .filter_map(|(p, _)| p.start)
            .chain([point.timestamp_ms])
            .min()
            .unwrap_or(point.timestamp_ms);
        let last_ms = work
            .iter()
            .filter_map(|(p, _)| p.stop)
            .chain([first_ms])
            .max()
            .unwrap_or(first_ms);
        let created_at = ts_ms(Some(first_ms)).unwrap_or(timestamp.unwrap_or(now));
        let finished_at = ts_ms(Some(
            last_ms.max(first_ms + point.trend.duration_ms.unwrap_or(0)),
        ))
        .unwrap_or(created_at);

        let launch = self
            .history_launch(project, target, point, created_at, finished_at, &mut out)
            .await?;
        let before = self.before_state(project.id, &known).await?;
        let started_import = Utc::now();

        let mut accepted_series: HashSet<String> = HashSet::new();
        let mut imported_at: HashMap<String, (DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
        for chunk in work.chunks(CHUNK) {
            let mut attempts = Vec::with_capacity(chunk.len());
            for (p, r) in chunk {
                let n = r.retries.len();
                for (i, s) in r.retries.iter().enumerate() {
                    let mut a = p.clone();
                    a.status = *s;
                    a.attempt = Some(i as u32);
                    let back = (n - i) as i64;
                    a.start = a.start.map(|t| t - back);
                    a.stop = a.stop.map(|t| t - back);
                    a.duration_ms = a.duration_ms.map(|d| d.min((back - 1).max(0)));
                    a.status_details.message = None;
                    attempts.push(NewAttempt {
                        client_key: format!("ah:{}:{i}", p.identity.series_key),
                        result: a,
                    });
                }
                let mut last = p.clone();
                last.attempt = (n > 0).then_some(n as u32);
                attempts.push(NewAttempt {
                    client_key: format!("ah:{}:{n}", p.identity.series_key),
                    result: last,
                });
            }
            let keys: HashMap<String, String> = attempts
                .iter()
                .map(|a| (a.client_key.clone(), a.result.identity.series_key.clone()))
                .collect();
            let o = self.persist_attempts(&launch, None, attempts).await?;
            for k in &o.accepted_keys {
                if let Some(s) = keys.get(k) {
                    accepted_series.insert(s.clone());
                }
            }
            for (p, _) in chunk {
                if let (Some(a), Some(b)) = (ts_ms(p.start), ts_ms(p.stop.or(p.start))) {
                    imported_at.insert(p.identity.series_key.clone(), (a, b));
                }
            }
        }
        out.imported = work
            .iter()
            .filter(|(p, _)| accepted_series.contains(&p.identity.series_key))
            .count() as u64;
        out.existing = work.len() as u64 - out.imported;
        self.repair_after_import(
            project,
            &accepted_series,
            &known,
            &before,
            &imported_at,
            started_import,
        )
        .await?;
        Ok(out)
    }

    /// The launch of a past report: created dated in the past, or the one a former import made.
    async fn history_launch(
        &self,
        project: &Project,
        target: &ImportTarget,
        point: &HistoryPoint,
        created_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        out: &mut PointOutcome,
    ) -> anyhow::Result<LaunchRef> {
        let ext = external_id(target, &point.key);
        let name = match chrono::DateTime::from_timestamp_millis(point.timestamp_ms) {
            Some(t) => format!("{} {}", point.name, t.format("%Y-%m-%d %H:%M")),
            None => point.name.clone(),
        };
        let start = LaunchStart {
            external_id: Some(ext.clone()),
            name: name.chars().take(250).collect(),
            attributes: Some(attributes(target.source, point)),
            ci: None,
            vcs: target.branch.as_ref().map(|b| LaunchVcs {
                branch: Some(b.clone()),
                ..Default::default()
            }),
            environment: target.environment.as_ref().map(|e| LaunchEnvironment {
                key: Some(e.clone()),
                values: Default::default(),
            }),
            debug: Some(false),
        };
        let mut tx = self.pool.begin().await?;
        let inserted =
            crate::launches::insert_launch(&mut tx, project, &start, &target.created_by, false)
                .await?;
        let id = match inserted {
            Some(id) => {
                sqlx::query(
                    "update launches set created_at = $2, started_at = $2, finished_at = $3, \
                     status = 'finished', updated_at = now() where id = $1",
                )
                .bind(id)
                .bind(created_at)
                .bind(finished_at)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "update projects set last_launch_at = (select max(created_at) from launches \
                     where project_id = $1 and not debug) where id = $1",
                )
                .bind(project.id)
                .execute(&mut *tx)
                .await?;
                out.launch_created = true;
                id
            }
            None => {
                tx.rollback().await?;
                let l = self
                    .launch_by_external(project.id, &ext)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the launch of a past report vanished"))?;
                return Ok(LaunchRef {
                    id: l.id,
                    project_id: project.id,
                    project_key: project.key.clone(),
                    created_at: l.created_at,
                });
            }
        };
        tx.commit().await?;
        Ok(LaunchRef {
            id,
            project_id: project.id,
            project_key: project.key.clone(),
            created_at,
        })
    }

    /// Puts right what storing a past report changed in rows that live results share: first
    /// and last seen times of tests, and the status string and times of the series statistics.
    async fn repair_after_import(
        &self,
        project: &Project,
        accepted: &HashSet<String>,
        known: &HashMap<String, Known>,
        before: &HashMap<Uuid, Before>,
        times: &HashMap<String, (DateTime<Utc>, DateTime<Utc>)>,
        started: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if accepted.is_empty() {
            return Ok(());
        }
        let keys: Vec<String> = accepted.iter().cloned().collect();
        let mut ids: HashMap<String, (Uuid, Uuid)> = HashMap::new();
        for chunk in keys.chunks(LOOKUP) {
            let rows: Vec<(String, Uuid, Uuid)> = sqlx::query_as(
                "select series_key, id, test_case_id from series where project_id = $1 and series_key = any($2)",
            )
            .bind(project.id)
            .bind(chunk)
            .fetch_all(&self.pool)
            .await?;
            ids.extend(rows.into_iter().map(|(k, s, t)| (k, (s, t))));
        }
        let settings = self.project_settings(project.id).await?;
        let opts = settings.flaky.options();
        let window = settings.flaky.depth() as usize + 1;
        for chunk in keys.chunks(1000) {
            let mut s_ids = Vec::new();
            let mut s_first = Vec::new();
            let mut s_last = Vec::new();
            let mut c_ids = Vec::new();
            let mut c_first = Vec::new();
            let mut c_last = Vec::new();
            let mut stat_rows = Vec::new();
            for k in chunk {
                let Some(&(sid, tid)) = ids.get(k) else {
                    continue;
                };
                let (mn, mx) = times.get(k).copied().unwrap_or((started, started));
                let b = known.get(k).and_then(|kn| before.get(&kn.series_id));
                let (sf, sl, cf, cl) = match b.and_then(|b| b.seen) {
                    Some(x) => (
                        x.series_first.min(mn),
                        x.series_last.max(mx),
                        x.case_first.min(mn),
                        x.case_last.max(mx),
                    ),
                    None => (mn, mx, mn, mx),
                };
                s_ids.push(sid);
                s_first.push(sf);
                s_last.push(sl);
                c_ids.push(tid);
                c_first.push(cf);
                c_last.push(cl);
                stat_rows.push((sid, b));
            }
            sqlx::query(
                "update series s set first_seen_at = t.f, last_seen_at = case when s.last_seen_at >= $4 then t.l \
                 else greatest(s.last_seen_at, t.l) end from unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[]) \
                 as t(i, f, l) where s.id = t.i",
            )
            .bind(&s_ids)
            .bind(&s_first)
            .bind(&s_last)
            .bind(started)
            .execute(&self.pool)
            .await?;
            sqlx::query(
                "update test_cases c set first_seen_at = t.f, last_seen_at = case when c.last_seen_at >= $4 then t.l \
                 else greatest(c.last_seen_at, t.l) end from unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[]) \
                 as t(i, f, l) where c.id = t.i",
            )
            .bind(&c_ids)
            .bind(&c_first)
            .bind(&c_last)
            .bind(started)
            .execute(&self.pool)
            .await?;
            self.rebuild_stats(project.id, &stat_rows, &opts, window)
                .await?;
        }
        Ok(())
    }

    async fn rebuild_stats(
        &self,
        project_id: Uuid,
        rows: &[(Uuid, Option<&Before>)],
        opts: &fastproof_model::FlakyOptions,
        window: usize,
    ) -> anyhow::Result<()> {
        let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        let recent: Vec<(Uuid, String, DateTime<Utc>, Uuid)> = sqlx::query_as(
            "select s.sid, h.status, h.launch_created_at, h.launch_id from unnest($1::uuid[]) as s(sid) \
             cross join lateral (select r.status, r.launch_created_at, r.launch_id from results r \
               join launches l on l.id = r.launch_id \
               where r.series_id = s.sid and r.project_id = $2 and not l.debug \
               order by r.launch_created_at desc, r.id desc limit $3) h",
        )
        .bind(&ids)
        .bind(project_id)
        .bind(stats::STATUS_CHARS as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut by: BTreeMap<Uuid, Vec<(String, DateTime<Utc>, Uuid)>> = BTreeMap::new();
        for (s, st, at, l) in recent {
            by.entry(s).or_default().push((st, at, l));
        }
        let n = rows.len();
        let mut u_ids = Vec::with_capacity(n);
        let mut u_str = Vec::with_capacity(n);
        let mut u_bits = Vec::with_capacity(n);
        let mut u_score: Vec<Option<f32>> = Vec::with_capacity(n);
        let mut u_last = Vec::with_capacity(n);
        let mut u_launch = Vec::with_capacity(n);
        let mut u_passed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        let mut u_failed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        let mut u_flaky = Vec::with_capacity(n);
        let mut u_flaky_at: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        for (sid, b) in rows {
            let Some(mut newest_first) = by.remove(sid) else {
                continue;
            };
            let empty = Before::default();
            let b = b.unwrap_or(&empty);
            let old: Vec<char> = b.last_statuses.chars().collect();
            let keep = old.len().saturating_sub((b.raw as usize).min(old.len()));
            let mut s: Vec<char> = old[..keep].to_vec();
            s.extend(
                newest_first
                    .iter()
                    .rev()
                    .map(|(st, _, _)| stats::encode(Status::parse_lenient(st))),
            );
            if s.len() > stats::STATUS_CHARS {
                s.drain(..s.len() - stats::STATUS_CHARS);
            }
            let text: String = s.into_iter().collect();
            let statuses = stats::decode(&text);
            let mut bits: i64 = 0;
            for (i, st) in statuses.iter().rev().take(63).enumerate() {
                if st.is_failure() {
                    bits |= 1 << i;
                }
            }
            let (cur, rest) = match statuses.split_last() {
                Some((c, r)) => (*c, r),
                None => continue,
            };
            let past: Vec<Option<Status>> = rest.iter().rev().copied().map(Some).collect();
            newest_first.sort_by_key(|x| std::cmp::Reverse(x.1));
            let at = |want: fn(Status) -> bool| {
                newest_first
                    .iter()
                    .filter(|(st, _, _)| want(Status::parse_lenient(st)))
                    .map(|x| x.1)
                    .max()
            };
            u_ids.push(*sid);
            u_score.push(stats::flaky_score(&statuses, window));
            u_str.push(text);
            u_bits.push(bits);
            u_last.push(cur.as_str().to_string());
            u_launch.push(newest_first[0].2);
            u_passed.push(at(|s| s == Status::Passed).max(b.last_passed_at));
            u_failed.push(at(Status::is_failure).max(b.last_failed_at));
            let flaky = is_flaky(cur, &past, opts);
            u_flaky.push(flaky);
            u_flaky_at.push(b.last_flaky_at);
        }
        sqlx::query(
            "update series_stats st set last_statuses = t.s, status_bits = t.b, flaky_score = t.sc, \
               last_status = t.ls, last_launch_id = t.li, last_passed_at = t.pa, last_failed_at = t.fa, \
               flaky = t.fl, last_flaky_at = greatest(st.last_flaky_at, t.fla), updated_at = now() \
             from unnest($1::uuid[], $2::text[], $3::bigint[], $4::real[], $5::text[], $6::uuid[], \
               $7::timestamptz[], $8::timestamptz[], $9::bool[], $10::timestamptz[]) \
               as t(i, s, b, sc, ls, li, pa, fa, fl, fla) where st.series_id = t.i",
        )
        .bind(&u_ids)
        .bind(&u_str)
        .bind(&u_bits)
        .bind(&u_score)
        .bind(&u_last)
        .bind(&u_launch)
        .bind(&u_passed)
        .bind(&u_failed)
        .bind(&u_flaky)
        .bind(&u_flaky_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
