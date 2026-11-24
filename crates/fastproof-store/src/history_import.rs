//! Import of the history of Allure reports: the job rows of an import, and the storing of one
//! past report as a launch whose results join the series live results use.
//!
//! A past report becomes a finished launch dated at its first result, flagged by the attribute
//! `imported=allure-history`. Its results go through the writer of uploaded results
//! ([`crate::ingest::persist_attempts_in`] in history mode: no analysis hook, no rules, mutes,
//! defects or outbox events) in transactions that also bring the series statistics up to date,
//! oldest report first. The statistics keep time order whatever the order of arrival: a past
//! report may be older than live runs.

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
/// Minutes a past report may lie ahead of the clock.
const FUTURE_SLACK_MINUTES: i64 = 5;
/// The oldest report an import takes, whatever the retention window: older ones would make a
/// monthly partition of results and attempts each.
pub const MAX_IMPORT_AGE_DAYS: i64 = 3650;
/// Derived launch ids tried for one past report.
const LAUNCH_SALTS: u32 = 4;

/// Imports a project may have queued or running.
pub const MAX_ACTIVE_PER_PROJECT: i64 = 1;
/// Imports a caller may have queued or running (a caller with rights in several projects
/// cannot take every slot).
pub const MAX_ACTIVE_PER_OWNER: i64 = 1;
/// Imports queued or running on the whole server.
pub const MAX_ACTIVE_TOTAL: i64 = 4;
/// Hours a queued import may wait for a worker (imports run one at a time per replica).
pub const QUEUED_STALE_HOURS: i64 = 12;
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

/// The caps on imports queued or running; counts only, no lock.
async fn active_check(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    owner: &str,
) -> anyhow::Result<Result<(), CreateDenied>> {
    let (mine, owned, total): (i64, i64, i64) = sqlx::query_as(
        "select count(*) filter (where project_id = $1), count(*) filter (where owner_key = $2), count(*) \
         from history_imports where status in ('queued', 'running')",
    )
    .bind(project_id)
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    if mine >= MAX_ACTIVE_PER_PROJECT || owned >= MAX_ACTIVE_PER_OWNER || total >= MAX_ACTIVE_TOTAL
    {
        return Ok(Err(CreateDenied::Busy));
    }
    let day: i64 = sqlx::query_scalar(
        "select count(*) from history_imports where project_id = $1 and created_at > now() - interval '24 hours'",
    )
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?;
    if day >= MAX_PER_PROJECT_DAY {
        return Ok(Err(CreateDenied::Quota));
    }
    Ok(Ok(()))
}

impl Store {
    /// A cheap look at the caps before a request body is read; [`Store::create_history_import`]
    /// checks again under a lock.
    pub async fn history_import_precheck(
        &self,
        project_id: Uuid,
        owner: &str,
    ) -> anyhow::Result<Result<(), CreateDenied>> {
        let mut conn = self.pool.acquire().await?;
        active_check(&mut conn, project_id, owner).await
    }

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
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('history-imports', 820))")
            .execute(&mut *tx)
            .await?;
        if let Err(d) = active_check(&mut tx, project_id, owner).await? {
            return Ok(Err(d));
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

    /// An import and the key of its project, for the audit of its end.
    pub async fn history_import_with_project(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Option<(HistoryImport, String)>> {
        let r = sqlx::query(concat!(
            "select ",
            columns!(),
            ", (select p.key from projects p where p.id = history_imports.project_id) as project_key \
             from history_imports where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        r.map(|r| Ok((row(&r)?, r.try_get("project_key")?)))
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
                or (status = 'queued' and updated_at < now() - make_interval(hours => $2::int))",
        )
        .bind(STALE_AFTER_SECONDS as f64)
        .bind(QUEUED_STALE_HOURS as i32)
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
    full_name: String,
    display_name: String,
    env_key: Option<String>,
}

/// A result of a past report that will be stored, with the names it takes.
struct Prepared<'a> {
    result: &'a HistoryResult,
    key: String,
    env: Option<String>,
    names: Option<(String, String)>,
    known: Option<&'a Known>,
}

fn external_id(t: &ImportTarget, key: &str, salt: u32) -> String {
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
    if salt > 0 {
        h.update(salt.to_be_bytes());
    }
    format!(
        "allure-history:{}:{}",
        t.source,
        &hex::encode(h.finalize())[..40]
    )
}

/// The series key of a result in this import and its environment. Allure 2 keys have no
/// environment part, which live results of a named environment carry; a key with a part must
/// come with the environment it hashes, and the other way round.
fn effective_key(r: &HistoryResult, env: Option<&str>) -> Option<(String, Option<String>)> {
    let (_, _, part) = split_series_key(&r.series_key)?;
    match (part, r.environment.as_deref()) {
        (Some(h), Some(e)) => (environment_hash(Some(e)).as_deref() == Some(h))
            .then(|| (r.series_key.clone(), Some(e.to_string()))),
        (Some(_), None) | (None, Some(_)) => None,
        (None, None) => match environment_hash(env) {
            Some(h) => Some((format!("{}.{h}", r.series_key), env.map(str::to_string))),
            None => Some((r.series_key.clone(), None)),
        },
    }
}

/// Whether every time of a result lies in `[from, to]` (ms).
fn in_window(r: &HistoryResult, from: i64, to: i64) -> bool {
    let ok = |t: i64| (from..=to).contains(&t);
    r.start_ms.is_none_or(ok)
        && r.stop_ms.is_none_or(ok)
        && match (r.start_ms, r.duration_ms) {
            (Some(s), Some(d)) => ok(s.saturating_add(d)),
            _ => true,
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

fn parsed(p: &Prepared<'_>, point_start: i64) -> Option<ParsedResult> {
    let r = p.result;
    let (test_case_hash, parameters_hash, _) = split_series_key(&p.key)?;
    let (full_name, display_name, env_key) = match (p.known, &p.names) {
        (Some(k), _) => (
            k.full_name.clone(),
            k.display_name.clone(),
            k.env_key.clone(),
        ),
        (None, Some((f, d))) => (f.clone(), d.clone(), p.env.clone()),
        (None, None) => {
            let name = r.name.clone().or_else(|| r.full_name.clone())?;
            (
                r.full_name.clone().unwrap_or_else(|| name.clone()),
                name,
                p.env.clone(),
            )
        }
    };
    let identity = TestIdentity {
        full_name: full_name.clone(),
        display_name: display_name.clone(),
        test_case_hash: test_case_hash.to_string(),
        parameters_hash: parameters_hash.to_string(),
        series_key: p.key.clone(),
        source: SourceRef::default(),
        environment_hash: split_series_key(&p.key).and_then(|k| k.2.map(str::to_string)),
        env_key,
        ..Default::default()
    };
    let start = r
        .start_ms
        .or(r.stop_ms.map(|s| s - r.duration_ms.unwrap_or(0)))
        .unwrap_or(point_start);
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

/// Attempts of a result: its earlier attempts, then the final one.
fn attempts_of(p: &ParsedResult, retries: &[Status]) -> Vec<NewAttempt> {
    let n = retries.len();
    let mut out = Vec::with_capacity(n + 1);
    for (i, s) in retries.iter().enumerate() {
        let mut a = p.clone();
        a.status = *s;
        a.attempt = Some(i as u32);
        let back = (n - i) as i64;
        a.start = a.start.map(|t| t - back);
        a.stop = a.stop.map(|t| t - back);
        a.duration_ms = a.duration_ms.map(|d| d.min((back - 1).max(0)));
        a.status_details.message = None;
        out.push(NewAttempt {
            client_key: format!("ah:{}:{i}", p.identity.series_key),
            result: a,
        });
    }
    let mut last = p.clone();
    last.attempt = (n > 0).then_some(n as u32);
    out.push(NewAttempt {
        client_key: format!("ah:{}:{n}", p.identity.series_key),
        result: last,
    });
    out
}

type RecentRow = (Uuid, String, DateTime<Utc>, Uuid, Option<String>);

/// A recent result of a series, newest first.
struct Recent {
    status: String,
    launch_id: Uuid,
    at: DateTime<Utc>,
    branch: Option<String>,
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
                "select s.series_key, t.full_name, t.display_name, s.env_key \
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
                        full_name: r.try_get("full_name")?,
                        display_name: r.try_get("display_name")?,
                        env_key: r.try_get("env_key")?,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Names of test cases that exist, by hash: a new series of a known test case takes the
    /// current names, never those of an old report.
    async fn case_names(
        &self,
        project_id: Uuid,
        hashes: &[String],
    ) -> anyhow::Result<HashMap<String, (String, String)>> {
        let mut out = HashMap::new();
        for chunk in hashes.chunks(LOOKUP) {
            let rows: Vec<(String, String, String)> = sqlx::query_as(
                "select test_case_hash, full_name, display_name from test_cases \
                 where project_id = $1 and test_case_hash = any($2)",
            )
            .bind(project_id)
            .bind(chunk)
            .fetch_all(&self.pool)
            .await?;
            out.extend(rows.into_iter().map(|(h, f, d)| (h, (f, d))));
        }
        Ok(out)
    }

    /// Stores one past report as a launch (a launch of the same report that an import made is
    /// completed, not repeated). Results of tests the project does not know are stored only
    /// when the report names them; results dated outside the window are left out.
    pub async fn import_history_point(
        &self,
        project: &Project,
        target: &ImportTarget,
        point: &HistoryPoint,
        now: DateTime<Utc>,
    ) -> anyhow::Result<PointOutcome> {
        let mut out = PointOutcome::default();
        let not_before = target
            .not_before
            .max(now - Duration::days(MAX_IMPORT_AGE_DAYS));
        let not_after = now + Duration::minutes(FUTURE_SLACK_MINUTES);
        let timestamp = ts_ms(Some(point.timestamp_ms));
        if !timestamp.is_some_and(|t| t >= not_before && t <= not_after) {
            out.skipped = point.results.len() as u64;
            return Ok(out);
        }
        let (from_ms, to_ms) = (not_before.timestamp_millis(), not_after.timestamp_millis());
        let mut seen = HashSet::new();
        let mut items: Vec<(&HistoryResult, String, Option<String>)> = Vec::new();
        for r in &point.results {
            if !in_window(r, from_ms, to_ms) {
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
        let unknown_hashes: Vec<String> = keys
            .iter()
            .filter(|k| !known.contains_key(*k))
            .filter_map(|k| split_series_key(k).map(|p| p.0.to_string()))
            .collect();
        let names = self.case_names(project.id, &unknown_hashes).await?;
        let mut work: Vec<Prepared<'_>> = Vec::with_capacity(items.len());
        for (r, key, env) in items {
            let k = known.get(&key);
            let n = split_series_key(&key).and_then(|p| names.get(p.0).cloned());
            if k.is_none() && n.is_none() && r.name.is_none() && r.full_name.is_none() {
                out.unmatched += 1;
                continue;
            }
            work.push(Prepared {
                result: r,
                key,
                env,
                names: n,
                known: k,
            });
        }
        if work.is_empty() {
            return Ok(out);
        }
        let first_ms = work
            .iter()
            .filter_map(|p| p.result.start_ms.or(p.result.stop_ms))
            .chain([point.timestamp_ms])
            .min()
            .unwrap_or(point.timestamp_ms);
        let last_ms = work
            .iter()
            .filter_map(|p| p.result.stop_ms.or(p.result.start_ms))
            .chain([first_ms])
            .max()
            .unwrap_or(first_ms);
        let created_at = ts_ms(Some(first_ms)).unwrap_or(not_after).min(now);
        let finished_at = ts_ms(Some(last_ms))
            .unwrap_or(created_at)
            .clamp(created_at, now);

        let launch = self
            .history_launch(project, target, point, created_at, finished_at, &mut out)
            .await?;
        let settings = self.project_settings(project.id).await?;
        let mut stored = 0u64;
        for chunk in work.chunks(CHUNK) {
            let mut attempts = Vec::with_capacity(chunk.len());
            for p in chunk {
                if let Some(r) = parsed(p, point.timestamp_ms) {
                    attempts.extend(attempts_of(&r, &p.result.retries));
                }
            }
            stored += self
                .store_history_chunk(&launch, attempts, &settings)
                .await?;
        }
        out.imported = stored;
        out.existing = (work.len() as u64).saturating_sub(stored);
        Ok(out)
    }

    /// The launch of a past report: made now, dated in the past, or the one an earlier import
    /// of the same user made. A launch with the derived id that is not such a launch (a live
    /// one named alike, another user's) is never written into: the next derived id is tried.
    async fn history_launch(
        &self,
        project: &Project,
        target: &ImportTarget,
        point: &HistoryPoint,
        created_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        out: &mut PointOutcome,
    ) -> anyhow::Result<LaunchRef> {
        let name = match chrono::DateTime::from_timestamp_millis(point.timestamp_ms) {
            Some(t) => format!("{} {}", point.name, t.format("%Y-%m-%d %H:%M")),
            None => point.name.clone(),
        };
        for salt in 0..LAUNCH_SALTS {
            let ext = external_id(target, &point.key, salt);
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
            let inserted = crate::launches::insert_launch_with(
                &mut tx,
                project,
                &start,
                &target.created_by,
                false,
                false,
            )
            .await?;
            if let Some(id) = inserted {
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
                     where project_id = $1 and not debug and created_at <= now()) where id = $1",
                )
                .bind(project.id)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                out.launch_created = true;
                return Ok(LaunchRef {
                    id,
                    project_id: project.id,
                    project_key: project.key.clone(),
                    created_at,
                });
            }
            tx.rollback().await?;
            let found: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
                "select id, created_at from launches where project_id = $1 and external_id = $2 \
                   and not debug and created_by = $3 and not created_by_can_sync \
                   and attributes @> $4::jsonb and merged_into is null",
            )
            .bind(project.id)
            .bind(&ext)
            .bind(&target.created_by)
            .bind(serde_json::json!([{"key": IMPORT_ATTRIBUTE.0, "value": IMPORT_ATTRIBUTE.1}]))
            .fetch_optional(&self.pool)
            .await?;
            if let Some((id, at)) = found {
                return Ok(LaunchRef {
                    id,
                    project_id: project.id,
                    project_key: project.key.clone(),
                    created_at: at,
                });
            }
        }
        anyhow::bail!("the launch ids of this past report are taken by other launches")
    }

    /// Writes a chunk of attempts and brings the statistics of its series up to date in one
    /// transaction, so a chunk is either whole or absent and a repeated import has nothing
    /// to repair. Returns the results stored (0 when the chunk was stored before).
    async fn store_history_chunk(
        &self,
        launch: &LaunchRef,
        attempts: Vec<NewAttempt>,
        settings: &crate::analysis::settings::ProjectSettings,
    ) -> anyhow::Result<u64> {
        if attempts.is_empty() {
            return Ok(0);
        }
        crate::ingest::ensure_launch_partition(&self.pool, launch.created_at).await;
        let mut conn = self.pool.acquire().await?;
        let seq0 = crate::ingest::reserve_attempt_seq(&mut conn, launch.id, attempts.len()).await?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let r = async {
                let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
                let keys: HashMap<String, String> = attempts
                    .iter()
                    .map(|a| (a.client_key.clone(), a.result.identity.series_key.clone()))
                    .collect();
                let o = crate::ingest::persist_attempts_mode(
                    &mut tx,
                    launch,
                    None,
                    attempts.clone(),
                    seq0,
                    true,
                )
                .await?;
                if o.accepted == 0 {
                    tx.rollback().await?;
                    return Ok::<_, anyhow::Error>(0);
                }
                let accepted: Vec<String> = o
                    .accepted_keys
                    .iter()
                    .filter_map(|k| keys.get(k).cloned())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                repair_stats(&mut tx, launch, &accepted, settings).await?;
                tx.commit().await?;
                Ok(accepted.len() as u64)
            }
            .await;
            match r {
                Err(e)
                    if attempt < crate::ingest::TX_ATTEMPTS
                        && crate::ingest::retryable(&e).is_some() =>
                {
                    crate::ingest::retry_pause("history-import", &e, attempt).await
                }
                r => return r,
            }
        }
    }
}

/// Brings the statistics of series up to date after a past report's results were stored:
/// the status string keeps time order (the stored results of the series, newest 64, over the
/// older part the string already had), the times only widen, and the verdict fields (latest
/// status, launch and flaky flag per branch) change only when the past report is the
/// newest result. The rows are locked in id order like the analysis hook does.
async fn repair_stats(
    conn: &mut sqlx::PgConnection,
    launch: &LaunchRef,
    keys: &[String],
    settings: &crate::analysis::settings::ProjectSettings,
) -> anyhow::Result<()> {
    let found: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, test_case_id from series where project_id = $1 and series_key = any($2) order by id",
    )
    .bind(launch.project_id)
    .bind(keys)
    .fetch_all(&mut *conn)
    .await?;
    if found.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = found.iter().map(|f| f.0).collect();
    let mut cases: Vec<Uuid> = found.iter().map(|f| f.1).collect();
    cases.sort();
    cases.dedup();
    let at = launch.created_at;
    for (table, rows) in [("series", &ids), ("test_cases", &cases)] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update {table} t set first_seen_at = $2 from (select id from {table} where id = any($1) \
             order by id for no key update) l where t.id = l.id and t.first_seen_at > $2"
        )))
        .bind(rows)
        .bind(at)
        .execute(&mut *conn)
        .await?;
    }
    let existing = stats::lock_rows(conn, &ids, Some(launch.project_id)).await?;
    let recent: Vec<RecentRow> = sqlx::query_as(
        "select s.sid, h.status, h.launch_created_at, h.launch_id, h.branch from unnest($1::uuid[]) as s(sid) \
         cross join lateral (select r.status, r.launch_created_at, r.launch_id, l.branch from results r \
           join launches l on l.id = r.launch_id \
           where r.series_id = s.sid and r.project_id = $2 and not l.debug \
           order by r.launch_created_at desc, r.id desc limit $3) h",
    )
    .bind(&ids)
    .bind(launch.project_id)
    .bind(stats::STATUS_CHARS as i64 + 1)
    .fetch_all(&mut *conn)
    .await?;
    let mut by: BTreeMap<Uuid, Vec<Recent>> = BTreeMap::new();
    for (s, status, at, launch_id, branch) in recent {
        by.entry(s).or_default().push(Recent {
            status,
            launch_id,
            at,
            branch,
        });
    }
    let opts = settings.flaky.options();
    let window = settings.flaky.depth() as usize + 1;
    let n = ids.len();
    let mut u_ids = Vec::with_capacity(n);
    let mut u_str = Vec::with_capacity(n);
    let mut u_bits = Vec::with_capacity(n);
    let mut u_score: Vec<Option<f32>> = Vec::with_capacity(n);
    let mut u_newest = Vec::with_capacity(n);
    let mut u_last = Vec::with_capacity(n);
    let mut u_launch = Vec::with_capacity(n);
    let mut u_passed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut u_failed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut u_flaky = Vec::with_capacity(n);
    for sid in &ids {
        let Some(rows) = by.get(sid) else { continue };
        let old: Vec<char> = existing
            .get(sid)
            .map(|r| r.last_statuses.chars().collect())
            .unwrap_or_default();
        let total = rows.len();
        let keep = if total > stats::STATUS_CHARS {
            0
        } else {
            old.len().saturating_sub((total - 1).min(old.len()))
        };
        let mut s: Vec<char> = old[..keep].to_vec();
        s.extend(
            rows.iter()
                .take(stats::STATUS_CHARS)
                .rev()
                .map(|r| stats::encode(Status::parse_lenient(&r.status))),
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
        let newest = &rows[0];
        let cur = Status::parse_lenient(&newest.status);
        let past: Vec<Option<Status>> = rows[1..]
            .iter()
            .filter(|r| r.branch == newest.branch)
            .map(|r| Some(Status::parse_lenient(&r.status)))
            .collect();
        let latest = |want: fn(Status) -> bool| {
            rows.iter()
                .filter(|r| want(Status::parse_lenient(&r.status)))
                .map(|r| r.at)
                .max()
        };
        u_ids.push(*sid);
        u_score.push(stats::flaky_score(&statuses, window));
        u_str.push(text);
        u_bits.push(bits);
        u_newest.push(newest.launch_id == launch.id);
        u_last.push(cur.as_str().to_string());
        u_launch.push(newest.launch_id);
        u_passed.push(latest(|s| s == Status::Passed));
        u_failed.push(latest(Status::is_failure));
        u_flaky.push(is_flaky(cur, &past, &opts));
    }
    sqlx::query(
        "update series_stats st set last_statuses = t.s, status_bits = t.b, flaky_score = t.sc, \
           runs_tracked = st.runs_tracked + 1, \
           last_status = case when t.nw then t.ls else st.last_status end, \
           last_launch_id = case when t.nw then t.li else st.last_launch_id end, \
           flaky = case when t.nw then t.fl else st.flaky end, \
           last_passed_at = greatest(st.last_passed_at, t.pa), \
           last_failed_at = greatest(st.last_failed_at, t.fa), \
           first_seen_at = least(coalesce(st.first_seen_at, $11), $11), \
           last_seen_at = greatest(st.last_seen_at, $11), updated_at = now() \
         from unnest($1::uuid[], $2::text[], $3::bigint[], $4::real[], $5::bool[], $6::text[], $7::uuid[], \
           $8::timestamptz[], $9::timestamptz[], $10::bool[]) \
           as t(i, s, b, sc, nw, ls, li, pa, fa, fl) where st.series_id = t.i",
    )
    .bind(&u_ids)
    .bind(&u_str)
    .bind(&u_bits)
    .bind(&u_score)
    .bind(&u_newest)
    .bind(&u_last)
    .bind(&u_launch)
    .bind(&u_passed)
    .bind(&u_failed)
    .bind(&u_flaky)
    .bind(at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
