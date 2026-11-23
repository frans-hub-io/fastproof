//! Merging launches (CI shards, re-runs that no CI id joined) into a new launch or into one of
//! them.
//!
//! The attempts of the source launches are copied into the target, and every series of the
//! target gets its result recomputed from all its attempts with the retry semantics of ingest:
//! a later run of the same test becomes a new attempt and the latest attempt is the result
//! ([`fastproof_model::group_attempts`]). The sources are then hidden (`merged_into`, and
//! `debug` so history, statistics, gates and dashboards leave them out), the analysis of the
//! target runs as after an upload (transitions, flakiness, defects, mutes), its counters are
//! recounted, and the series statistics are rebuilt without the sources.
//!
//! A merge can be undone until `undo_until`: the target (when the merge created it) or the
//! copied attempts (when it merged into an existing launch) go, and the sources come back.
//! Afterwards the sources become tombstones: their results, attempts, logs and containers are
//! deleted, while the launch row, its uploads and its attachment links and references stay
//! (copied attempts may reference attachment variants of a source by id) until the target is
//! deleted. Merges are serialized per project, and the launch rows are locked in id order, so
//! an ingest into the target waits for the merge (and the other way round).

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use fastproof_model::{AttemptKey, Status, group_attempts};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::analysis::ProjectSettings;
use crate::analysis::stats::{self, STATUS_CHARS};
use crate::ingest::{LaunchRef, TX_ATTEMPTS, retry_pause, retryable};
use crate::{Project, Store, json_no_nul, no_nul};

/// Launches one merge takes, at most (the target included).
pub const MAX_MERGE_LAUNCHES: usize = 20;
/// Series analysed per statement group (bounds the rows the analysis holds at once).
const ANALYSIS_CHUNK: usize = 1000;
/// Attributes of a new target, at most (as a launch start).
const MAX_ATTRIBUTES: usize = 100;
/// Merges the maintenance job handles per kind and run, at most.
const JOB_BATCH: i64 = 20;

/// Body of `POST /api/v1/projects/{key}/launches/merge`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchMergeRequest {
    /// The launches to merge, 2 to 20, the target included when `into` is given.
    pub launch_ids: Vec<Uuid>,
    /// Merge into this launch (one of `launchIds`) instead of a new launch.
    #[serde(default)]
    pub into: Option<Uuid>,
    /// Name of the new launch (default: the name of the earliest source).
    #[serde(default)]
    pub name: Option<String>,
    /// No undo: the sources are deleted right after the merge (needs `settings:write`).
    #[serde(default, rename = "final")]
    pub final_merge: bool,
    /// Check and count without merging (status `preview`).
    #[serde(default)]
    pub dry_run: bool,
}

/// What a merge did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct MergeCounts {
    /// Attempts copied into the target (in a preview: attempts of the sources).
    pub attempts: i64,
    /// Attempts the target had already (same client key, test and start), not copied again.
    pub duplicates: i64,
    /// Attempts whose client key another test of the target used, copied under a new key.
    pub rekeyed: i64,
    /// Results of the target created or recomputed.
    pub results: i64,
    /// Tests run in more than one of the merged launches: their results got more attempts.
    pub shared_series: i64,
    /// Attempts that reached a source after the merge and were copied later.
    pub late_attempts: i64,
}

/// A launch a merge took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeSource {
    pub id: Uuid,
    /// Absent once the launch is deleted.
    pub number: Option<i64>,
    pub name: Option<String>,
}

/// A merge of launches.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMerge {
    /// Absent in a preview.
    pub id: Option<Uuid>,
    /// Absent in a preview of a merge into a new launch.
    pub target_id: Option<Uuid>,
    pub target_number: Option<i64>,
    /// The merge created the target (else it merged into one of the launches).
    pub created_target: bool,
    pub sources: Vec<MergeSource>,
    /// `preview`, `active` (can be undone until `undoUntil`), `undone`, `final` (the sources
    /// are deleted) or `deleted` (the target was deleted, and the sources with it).
    pub status: String,
    /// Merged without undo.
    #[serde(rename = "final")]
    pub final_merge: bool,
    pub counts: MergeCounts,
    pub created_by: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub undo_until: Option<DateTime<Utc>>,
    pub undone_at: Option<DateTime<Utc>>,
    pub undone_by: Option<String>,
    pub finalized_at: Option<DateTime<Utc>>,
}

/// Why a merge or an undo was refused.
#[derive(Debug)]
pub enum MergeError {
    NotFound(&'static str),
    Invalid(String),
    Conflict {
        code: &'static str,
        message: String,
        details: serde_json::Value,
    },
    TooLarge {
        attempts: i64,
        limit: i64,
    },
    Other(anyhow::Error),
}

impl From<anyhow::Error> for MergeError {
    fn from(e: anyhow::Error) -> Self {
        MergeError::Other(e)
    }
}

impl From<sqlx::Error> for MergeError {
    fn from(e: sqlx::Error) -> Self {
        MergeError::Other(e.into())
    }
}

fn conflict(
    code: &'static str,
    message: impl Into<String>,
    details: serde_json::Value,
) -> MergeError {
    MergeError::Conflict {
        code,
        message: message.into(),
        details,
    }
}

/// Server settings of merges.
#[derive(Clone, Copy, Debug)]
pub struct MergeOptions {
    /// How long a merge can be undone.
    pub undo_window: chrono::Duration,
    /// Attempts of the sources of one merge, at most.
    pub max_attempts: i64,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            undo_window: chrono::Duration::hours(24),
            max_attempts: 200_000,
        }
    }
}

/// What the maintenance job did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeJobReport {
    /// Merges whose sources got late attempts, copied into the target.
    pub synced: u64,
    /// Merges whose undo window ended (sources turned into tombstones).
    pub finalized: u64,
    /// Hidden launches deleted because their target is gone.
    pub orphans_deleted: u64,
}

/// Validates the shape of a merge request.
pub fn validate(req: &LaunchMergeRequest) -> Result<(), MergeError> {
    let mut ids = req.launch_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != req.launch_ids.len() {
        return Err(MergeError::Invalid("launchIds has duplicates".into()));
    }
    if ids.len() < 2 || ids.len() > MAX_MERGE_LAUNCHES {
        return Err(MergeError::Invalid(format!(
            "launchIds must name 2 to {MAX_MERGE_LAUNCHES} launches"
        )));
    }
    if let Some(t) = req.into
        && !ids.contains(&t)
    {
        return Err(MergeError::Invalid("into must be one of launchIds".into()));
    }
    if let Some(n) = &req.name {
        if req.into.is_some() {
            return Err(MergeError::Invalid(
                "name applies only to a merge into a new launch".into(),
            ));
        }
        if n.trim().is_empty() || n.len() > 300 {
            return Err(MergeError::Invalid("name must have 1 to 300 bytes".into()));
        }
    }
    Ok(())
}

/// A launch as a merge reads it (locked).
#[derive(Clone, Debug)]
struct Row0 {
    id: Uuid,
    number: i64,
    name: String,
    status: String,
    debug: bool,
    branch: Option<String>,
    env_key: Option<String>,
    created_at: DateTime<Utc>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    rolled_up_at: Option<DateTime<Utc>>,
    manual_state: Option<String>,
    merged_into: Option<Uuid>,
    attempt_seq: i64,
    ci: Option<serde_json::Value>,
    vcs: Option<serde_json::Value>,
    environment: Option<serde_json::Value>,
    attributes: serde_json::Value,
    created_by_can_sync: bool,
    sync_blocked: bool,
    important: bool,
}

const ROW_SELECT: &str = "select id, number, name, status, debug, branch, env_key, created_at, started_at, \
    finished_at, rolled_up_at, manual_state, merged_into, attempt_seq, ci, vcs, environment, \
    attributes, created_by_can_sync, sync_blocked, important from launches";

fn row0(r: &PgRow) -> Result<Row0, sqlx::Error> {
    Ok(Row0 {
        id: r.try_get("id")?,
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        status: r.try_get("status")?,
        debug: r.try_get("debug")?,
        branch: r.try_get("branch")?,
        env_key: r.try_get("env_key")?,
        created_at: r.try_get("created_at")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
        rolled_up_at: r.try_get("rolled_up_at")?,
        manual_state: r.try_get("manual_state")?,
        merged_into: r.try_get("merged_into")?,
        attempt_seq: r.try_get("attempt_seq")?,
        ci: r.try_get("ci")?,
        vcs: r.try_get("vcs")?,
        environment: r.try_get("environment")?,
        attributes: r.try_get("attributes")?,
        created_by_can_sync: r.try_get("created_by_can_sync")?,
        sync_blocked: r.try_get("sync_blocked")?,
        important: r.try_get("important")?,
    })
}

/// Locks launches of a project in id order (as every merge does, so merges and ingest of the
/// same launches queue instead of deadlocking).
async fn lock_launches(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> anyhow::Result<Vec<Row0>> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ROW_SELECT} where project_id = $1 and id = any($2) order by id for update"
    )))
    .bind(project_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(row0).collect::<Result<_, _>>()?)
}

/// Serializes merges, undos and finalizations of a project.
async fn project_lock(conn: &mut PgConnection, project_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 7720))")
        .bind(format!("fastproof-merge:{project_id}"))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The project of a merge, as its writes need it.
#[derive(Clone, Debug)]
struct Ctx {
    id: Uuid,
    key: String,
    settings: ProjectSettings,
    purge_batch: i64,
}

async fn ctx(conn: &mut PgConnection, project: &Project, purge_batch: i64) -> anyhow::Result<Ctx> {
    let settings: serde_json::Value =
        sqlx::query_scalar("select settings from projects where id = $1")
            .bind(project.id)
            .fetch_one(&mut *conn)
            .await?;
    Ok(Ctx {
        id: project.id,
        key: project.key.clone(),
        settings: ProjectSettings::from_value(settings),
        purge_batch,
    })
}

/// A source: id, creation time (the partition of its rows) and its order in the merge.
#[derive(Clone, Copy, Debug)]
struct Src {
    id: Uuid,
    at: DateTime<Utc>,
}

fn src_arrays(sources: &[Src]) -> (Vec<Uuid>, Vec<DateTime<Utc>>, Vec<i32>) {
    (
        sources.iter().map(|s| s.id).collect(),
        sources.iter().map(|s| s.at).collect(),
        (0..sources.len() as i32).collect(),
    )
}

async fn count_attempts(conn: &mut PgConnection, sources: &[Src]) -> anyhow::Result<i64> {
    let (ids, ats, _) = src_arrays(sources);
    Ok(sqlx::query_scalar(
        "select count(*) from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join attempts a on a.launch_id = s.id and a.launch_created_at = s.at",
    )
    .bind(&ids)
    .bind(&ats)
    .fetch_one(&mut *conn)
    .await?)
}

/// Series with results in the sources, sorted.
async fn source_series(conn: &mut PgConnection, sources: &[Src]) -> anyhow::Result<Vec<Uuid>> {
    let (ids, ats, _) = src_arrays(sources);
    Ok(sqlx::query_scalar(
        "select distinct r.series_id from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join results r on r.launch_id = s.id and r.launch_created_at = s.at order by 1",
    )
    .bind(&ids)
    .bind(&ats)
    .fetch_all(&mut *conn)
    .await?)
}

/// Series with results in more than one of these launches.
async fn shared_series(conn: &mut PgConnection, launches: &[Src]) -> anyhow::Result<i64> {
    let (ids, ats, _) = src_arrays(launches);
    Ok(sqlx::query_scalar(
        "select count(*) from (select r.series_id from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join results r on r.launch_id = s.id and r.launch_created_at = s.at \
         group by r.series_id having count(*) > 1) x",
    )
    .bind(&ids)
    .bind(&ats)
    .fetch_one(&mut *conn)
    .await?)
}

/// The target of a merge.
#[derive(Clone, Copy, Debug)]
struct Target {
    id: Uuid,
    created_at: DateTime<Utc>,
}

/// Creates the launch a merge goes into: numbered next, created when the earliest source was
/// (so it takes the sources' place in history), CI and VCS data of the earliest source, the
/// attributes of all of them.
async fn insert_target(
    conn: &mut PgConnection,
    ctx: &Ctx,
    sources: &[Row0],
    name: Option<&str>,
    by: &str,
) -> anyhow::Result<(Target, i64)> {
    let first = &sources[0];
    let number: i64 = sqlx::query_scalar(
        "update projects set launch_seq = launch_seq + 1, last_launch_at = now() where id = $1 returning launch_seq",
    )
    .bind(ctx.id)
    .fetch_one(&mut *conn)
    .await?;
    let mut attrs: Vec<serde_json::Value> = Vec::new();
    let mut seen = HashSet::new();
    for s in sources {
        if let Some(a) = s.attributes.as_array() {
            for kv in a {
                if attrs.len() < MAX_ATTRIBUTES && seen.insert(kv.to_string()) {
                    attrs.push(kv.clone());
                }
            }
        }
    }
    let env_key = if sources.iter().all(|s| s.env_key == first.env_key) {
        first.env_key.clone()
    } else {
        None
    };
    let status = if sources.iter().any(|s| s.status == "interrupted") {
        "interrupted"
    } else {
        "finished"
    };
    let created_at = sources
        .iter()
        .map(|s| s.created_at)
        .min()
        .unwrap_or(first.created_at);
    let started_at = sources
        .iter()
        .map(|s| s.started_at)
        .min()
        .unwrap_or(first.started_at);
    let finished_at = sources.iter().filter_map(|s| s.finished_at).max();
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status, debug, created_at, started_at, finished_at, \
           ci, vcs, environment, attributes, branch, env_key, created_by, created_by_can_sync, sync_blocked, important) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
    )
    .bind(id)
    .bind(ctx.id)
    .bind(number)
    .bind(no_nul(name.unwrap_or(&first.name)))
    .bind(status)
    .bind(first.debug)
    .bind(created_at)
    .bind(started_at)
    .bind(finished_at)
    .bind(first.ci.clone())
    .bind(first.vcs.clone())
    .bind(first.environment.clone())
    .bind(json_no_nul(serde_json::Value::Array(attrs)))
    .bind(first.branch.clone())
    .bind(env_key)
    .bind(no_nul(by))
    .bind(sources.iter().all(|s| s.created_by_can_sync))
    .bind(sources.iter().any(|s| s.sync_blocked))
    .bind(sources.iter().any(|s| s.important))
    .execute(&mut *conn)
    .await?;
    Ok((Target { id, created_at }, number))
}

/// Series statistics before a merge or an undo changes which launches count, with how many
/// of their characters belong to launches created since `window` (which the rebuild replaces).
struct StatsSnapshot {
    window: DateTime<Utc>,
    rows: HashMap<Uuid, (String, i32)>,
    since: HashMap<Uuid, i64>,
}

/// Locks the statistics rows of `series` (in order, like ingest) and counts their results in
/// counted launches (not debug, so neither hidden sources) created since `window`.
async fn stats_snapshot(
    conn: &mut PgConnection,
    project_id: Uuid,
    series: &[Uuid],
    window: DateTime<Utc>,
) -> anyhow::Result<StatsSnapshot> {
    let mut rows = HashMap::with_capacity(series.len());
    let mut since = HashMap::with_capacity(series.len());
    for chunk in series.chunks(ANALYSIS_CHUNK) {
        let got: Vec<(Uuid, String, i32)> = sqlx::query_as(
            "select series_id, last_statuses, runs_tracked from series_stats where series_id = any($1) \
             order by series_id for update",
        )
        .bind(chunk)
        .fetch_all(&mut *conn)
        .await?;
        rows.extend(got.into_iter().map(|(s, l, n)| (s, (l, n))));
        let counted: Vec<(Uuid, i64)> = sqlx::query_as(
            "select r.series_id, count(*) from results r join launches l on l.id = r.launch_id \
             where r.series_id = any($1) and r.project_id = $2 and r.launch_created_at >= $3 and not l.debug \
             group by r.series_id",
        )
        .bind(chunk)
        .bind(project_id)
        .bind(window)
        .fetch_all(&mut *conn)
        .await?;
        since.extend(counted);
    }
    Ok(StatsSnapshot {
        window,
        rows,
        since,
    })
}

/// The status string of a series after a rebuild: the characters of launches created before
/// the window are kept, those since are replaced by the results of the counted launches since
/// then, in creation order. Returns the string and the runs tracked.
fn rebuilt_statuses(
    old: Option<&(String, i32)>,
    before_since: i64,
    tail: &[Status],
) -> (String, i32) {
    let (old_s, old_runs) = old.map(|(s, n)| (s.as_str(), *n)).unwrap_or(("", 0));
    let chars: Vec<char> = old_s.chars().collect();
    let keep = chars.len().saturating_sub(before_since.max(0) as usize);
    let mut out: Vec<char> = chars[..keep].to_vec();
    out.extend(tail.iter().map(|s| stats::encode(*s)));
    if out.len() > STATUS_CHARS {
        out.drain(..out.len() - STATUS_CHARS);
    }
    let runs = (old_runs as i64 - before_since + tail.len() as i64).max(out.len() as i64);
    (out.into_iter().collect(), runs.min(i32::MAX as i64) as i32)
}

/// A counted result of a series: status, flaky, launch, launch creation time.
type TailRow = (Status, bool, Uuid, DateTime<Utc>);

/// An attempt of a target: series, id, status, start, order.
type AttemptRow = (Uuid, Uuid, String, Option<DateTime<Utc>>, i64);

/// Rebuilds the statistics of `series` from the snapshot and the results now counted.
async fn rebuild_stats(
    conn: &mut PgConnection,
    ctx: &Ctx,
    snap: &StatsSnapshot,
    series: &[Uuid],
) -> anyhow::Result<()> {
    let window = ctx.settings.flaky.depth() as usize + 1;
    for chunk in series.chunks(ANALYSIS_CHUNK) {
        let rows: Vec<(Uuid, String, bool, Uuid, DateTime<Utc>)> = sqlx::query_as(
            "select r.series_id, r.status, r.flaky, r.launch_id, r.launch_created_at from results r \
             join launches l on l.id = r.launch_id \
             where r.series_id = any($1) and r.project_id = $2 and r.launch_created_at >= $3 and not l.debug \
             order by r.series_id, r.launch_created_at, l.number",
        )
        .bind(chunk)
        .bind(ctx.id)
        .bind(snap.window)
        .fetch_all(&mut *conn)
        .await?;
        let mut tails: HashMap<Uuid, Vec<TailRow>> = HashMap::new();
        for (s, st, fl, l, at) in rows {
            tails
                .entry(s)
                .or_default()
                .push((Status::parse_lenient(&st), fl, l, at));
        }
        let mut ids = Vec::new();
        let mut strings = Vec::new();
        let mut bits = Vec::new();
        let mut runs = Vec::new();
        let mut scores: Vec<Option<f32>> = Vec::new();
        let mut last: Vec<Option<String>> = Vec::new();
        let mut last_launch: Vec<Option<Uuid>> = Vec::new();
        let mut flaky = Vec::new();
        let mut seen_at: Vec<Option<DateTime<Utc>>> = Vec::new();
        for s in chunk {
            let tail = tails.remove(s).unwrap_or_default();
            let old = snap.rows.get(s);
            if old.is_none() && tail.is_empty() {
                continue;
            }
            let statuses: Vec<Status> = tail.iter().map(|t| t.0).collect();
            let (string, n) =
                rebuilt_statuses(old, snap.since.get(s).copied().unwrap_or(0), &statuses);
            let decoded = stats::decode(&string);
            let mut b: i64 = 0;
            for (i, st) in decoded.iter().rev().take(63).enumerate() {
                if st.is_failure() {
                    b |= 1 << i;
                }
            }
            ids.push(*s);
            scores.push(stats::flaky_score(&decoded, window));
            strings.push(string);
            bits.push(b);
            runs.push(n);
            let latest = tail.last();
            last.push(latest.map(|t| t.0.as_str().to_string()));
            last_launch.push(latest.map(|t| t.2));
            flaky.push(latest.is_some_and(|t| t.1));
            seen_at.push(latest.map(|t| t.3));
        }
        if ids.is_empty() {
            continue;
        }
        sqlx::query(
            "insert into series_stats (series_id, project_id, last_statuses, status_bits, runs_tracked, flaky_score, \
               last_status, last_launch_id, flaky, first_seen_at, last_seen_at, updated_at) \
             select s, $1, st, b, r, sc, ls, ll, fl, sa, sa, now() \
             from unnest($2::uuid[], $3::text[], $4::bigint[], $5::int[], $6::real[], $7::text[], $8::uuid[], \
               $9::bool[], $10::timestamptz[]) as t(s, st, b, r, sc, ls, ll, fl, sa) \
             on conflict (series_id) do update set last_statuses = excluded.last_statuses, \
               status_bits = excluded.status_bits, runs_tracked = excluded.runs_tracked, \
               flaky_score = excluded.flaky_score, \
               last_status = coalesce(excluded.last_status, series_stats.last_status), \
               last_launch_id = excluded.last_launch_id, flaky = excluded.flaky, \
               first_seen_at = coalesce(series_stats.first_seen_at, excluded.first_seen_at), updated_at = now()",
        )
        .bind(ctx.id)
        .bind(&ids)
        .bind(&strings)
        .bind(&bits)
        .bind(&runs)
        .bind(&scores)
        .bind(&last)
        .bind(&last_launch)
        .bind(&flaky)
        .bind(&seen_at)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Recomputes the results of `series` in launch `t` from all their attempts (the latest by
/// start, then by order, is the result; mixed outcomes are flaky in the run). The test's
/// name, labels and a triage decision come from the source result of the winning attempt
/// when that attempt was copied; a person's decision on the target result wins. Results left
/// without attempts are deleted. Returns the series that still have a result.
async fn recompute_results(
    conn: &mut PgConnection,
    t: Target,
    series: &[Uuid],
    kinds: &HashMap<Uuid, i32>,
) -> anyhow::Result<Vec<Uuid>> {
    let mut kept = Vec::with_capacity(series.len());
    for chunk in series.chunks(ANALYSIS_CHUNK * 10) {
        let mut all: Vec<AttemptRow> = sqlx::query_as(
            "select series_id, id, status, started_at, seq from attempts \
             where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
        )
        .bind(t.id)
        .bind(t.created_at)
        .bind(chunk)
        .fetch_all(&mut *conn)
        .await?;
        all.sort_by_key(|a| a.4);
        let results: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select series_id, id from results where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
        )
        .bind(t.id)
        .bind(t.created_at)
        .bind(chunk)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let keys: Vec<String> = all.iter().map(|a| a.0.to_string()).collect();
        let items: Vec<AttemptKey<'_>> = all
            .iter()
            .zip(keys.iter())
            .map(|(a, k)| AttemptKey {
                series_key: k.as_str(),
                start: a.3.map(|x| x.timestamp_millis()),
                status: Status::parse_lenient(&a.2),
            })
            .collect();
        let groups = group_attempts(&items);
        let (mut rids, mut aids, mut counts, mut fl, mut ak) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut with_attempts = HashSet::new();
        for g in &groups {
            let latest = &all[g.latest];
            let Some(rid) = results.get(&latest.0) else {
                continue;
            };
            with_attempts.insert(latest.0);
            rids.push(*rid);
            aids.push(latest.1);
            counts.push(g.attempts.len() as i32);
            fl.push(g.flaky_in_run);
            ak.push(kinds.get(&latest.0).copied().unwrap_or(0));
        }
        if !rids.is_empty() {
            sqlx::query(
                "update results r set status = w.status, started_at = w.started_at, stopped_at = w.stopped_at, \
                   duration_ms = w.duration_ms, message = w.message, \
                   name = coalesce(sr.name, r.name), full_name = coalesce(sr.full_name, r.full_name), \
                   suite_path = coalesce(sr.suite_path, r.suite_path), labels = coalesce(sr.labels, r.labels), \
                   attempt_count = t.ac, flaky_in_run = t.fl, attachment_kinds = r.attachment_kinds | t.ak, \
                   defect = case when r.defect ->> 'source' in ('user', 'ai') then r.defect \
                     when sr.defect ->> 'source' in ('user', 'ai') then sr.defect else r.defect end, \
                   updated_at = now() \
                 from unnest($1::uuid[], $2::uuid[], $3::int[], $4::bool[], $5::int[]) as t(rid, aid, ac, fl, ak) \
                 join attempts w on w.id = t.aid and w.launch_created_at = $6 \
                 left join launches sl on sl.id = w.merged_from \
                 left join results sr on sr.launch_id = sl.id and sr.launch_created_at = sl.created_at \
                   and sr.series_id = w.series_id \
                 where r.id = t.rid and r.launch_created_at = $6",
            )
            .bind(&rids)
            .bind(&aids)
            .bind(&counts)
            .bind(&fl)
            .bind(&ak)
            .bind(t.created_at)
            .execute(&mut *conn)
            .await?;
        }
        let empty: Vec<Uuid> = results
            .iter()
            .filter(|(s, _)| !with_attempts.contains(*s))
            .map(|(_, r)| *r)
            .collect();
        if !empty.is_empty() {
            sqlx::query("delete from results where id = any($1) and launch_created_at = $2")
                .bind(&empty)
                .bind(t.created_at)
                .execute(&mut *conn)
                .await?;
        }
        kept.extend(chunk.iter().filter(|s| with_attempts.contains(*s)));
    }
    Ok(kept)
}

/// Attachment kinds of the sources' results per series.
async fn source_kinds(
    conn: &mut PgConnection,
    sources: &[Src],
) -> anyhow::Result<HashMap<Uuid, i32>> {
    let (ids, ats, _) = src_arrays(sources);
    Ok(sqlx::query_as::<_, (Uuid, i32)>(
        "select r.series_id, bit_or(r.attachment_kinds) from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join results r on r.launch_id = s.id and r.launch_created_at = s.at group by r.series_id",
    )
    .bind(&ids)
    .bind(&ats)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect())
}

/// Copies the attempts (and containers, attachment links, log chunks) of `sources` into the
/// target, recomputes its results, hides the sources when `hide`, analyses the target and
/// rebuilds the statistics. Copying again copies only what is new (idempotent).
async fn apply(
    conn: &mut PgConnection,
    ctx: &Ctx,
    t: Target,
    sources: &[Src],
    hide: bool,
) -> anyhow::Result<MergeCounts> {
    let mut counts = MergeCounts::default();
    let series = source_series(conn, sources).await?;
    let window = sources
        .iter()
        .map(|s| s.at)
        .chain([t.created_at])
        .min()
        .unwrap_or(t.created_at);
    let snap = stats_snapshot(conn, ctx.id, &series, window).await?;
    let (ids, ats, ords) = src_arrays(sources);

    let result_ids: Vec<Uuid> = (0..series.len()).map(|_| Uuid::now_v7()).collect();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
           full_name, suite_path, status, labels, attachment_kinds) \
         select ($5::uuid[])[row_number() over (order by x.series_id)], $1, $2, $3, x.test_case_id, x.series_id, \
           x.name, x.full_name, x.suite_path, x.status, x.labels, 0 \
         from (select distinct on (r.series_id) r.series_id, r.test_case_id, r.name, r.full_name, r.suite_path, \
             r.status, r.labels \
           from unnest($4::uuid[], $6::timestamptz[], $7::int[]) as s(id, at, ord) \
           join results r on r.launch_id = s.id and r.launch_created_at = s.at \
           where not exists (select 1 from results o where o.launch_id = $3 and o.launch_created_at = $1 \
             and o.series_id = r.series_id) \
           order by r.series_id, s.ord desc) x \
         on conflict (launch_id, series_id, launch_created_at) do nothing",
    )
    .bind(t.created_at)
    .bind(ctx.id)
    .bind(t.id)
    .bind(&ids)
    .bind(&result_ids)
    .bind(&ats)
    .bind(&ords)
    .execute(&mut *conn)
    .await?;

    let n = count_attempts(conn, sources).await?;
    let seq0: i64 = sqlx::query_scalar("select attempt_seq from launches where id = $1")
        .bind(t.id)
        .fetch_one(&mut *conn)
        .await?;
    let attempt_ids: Vec<Uuid> = (0..n).map(|_| Uuid::now_v7()).collect();
    // A client key the target (or an earlier source) uses for another test or start gets the
    // source's prefix; the same key for the same test and start is the same attempt, copied
    // once. A copy reported running is replaced when the source completed it.
    let copied: Vec<(Uuid, bool)> = sqlx::query_as(
        "with src as materialized (select a.id, a.launch_id, a.series_id, a.client_key, a.seq, a.status, a.raw_status, \
             a.started_at, a.stopped_at, a.duration_ms, a.message, a.trace, a.source_uuid, a.trace_id, a.detail, \
             a.steps_z, a.logs_z, a.fingerprint, s.ord \
           from unnest($4::uuid[], $5::timestamptz[], $6::int[]) as s(id, at, ord) \
           join attempts a on a.launch_id = s.id and a.launch_created_at = s.at), \
         keyed as (select src.*, row_number() over (order by src.ord, src.seq) as n, \
             case when exists (select 1 from attempts o where o.launch_id = $3 and o.launch_created_at = $1 \
                 and o.client_key = src.client_key and o.merged_from is distinct from src.launch_id \
                 and (o.series_id <> src.series_id or o.started_at is distinct from src.started_at)) \
               or exists (select 1 from src e where e.client_key = src.client_key and e.ord < src.ord \
                 and (e.series_id <> src.series_id or e.started_at is distinct from src.started_at)) \
             then 'm:' || src.launch_id::text || ':' || src.client_key else src.client_key end as new_key, \
             exists (select 1 from src e where e.client_key = src.client_key and e.ord < src.ord \
               and e.series_id = src.series_id and e.started_at is not distinct from src.started_at) as repeated \
           from src) \
         insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, upload_id, \
           client_key, seq, status, raw_status, started_at, stopped_at, duration_ms, message, trace, source_uuid, \
           trace_id, detail, steps_z, logs_z, fingerprint, merged_from) \
         select ($7::uuid[])[k.n], $1, $2, $3, r.id, k.series_id, null, k.new_key, $8 + k.n, k.status, k.raw_status, \
           k.started_at, k.stopped_at, k.duration_ms, k.message, k.trace, k.source_uuid, k.trace_id, k.detail, \
           k.steps_z, k.logs_z, k.fingerprint, k.launch_id \
         from keyed k join results r on r.launch_id = $3 and r.launch_created_at = $1 and r.series_id = k.series_id \
         where not k.repeated \
         on conflict (launch_id, client_key, launch_created_at) do update set status = excluded.status, \
           result_id = excluded.result_id, series_id = excluded.series_id, raw_status = excluded.raw_status, \
           started_at = excluded.started_at, stopped_at = excluded.stopped_at, duration_ms = excluded.duration_ms, \
           message = excluded.message, trace = excluded.trace, source_uuid = excluded.source_uuid, \
           trace_id = excluded.trace_id, detail = excluded.detail, steps_z = excluded.steps_z, \
           logs_z = excluded.logs_z, fingerprint = excluded.fingerprint, merged_from = excluded.merged_from \
           where attempts.status = 'running' and excluded.status <> 'running' \
         returning series_id, left(client_key, 39) = 'm:' || coalesce(merged_from::text, '') || ':'",
    )
    .bind(t.created_at)
    .bind(ctx.id)
    .bind(t.id)
    .bind(&ids)
    .bind(&ats)
    .bind(&ords)
    .bind(&attempt_ids)
    .bind(seq0)
    .fetch_all(&mut *conn)
    .await?;
    counts.attempts = copied.len() as i64;
    counts.duplicates = n - counts.attempts;
    counts.rekeyed = copied.iter().filter(|c| c.1).count() as i64;
    let mut affected: Vec<Uuid> = if hide {
        series.clone()
    } else {
        copied
            .iter()
            .map(|c| c.0)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    affected.sort();
    affected.dedup();

    sqlx::query(
        "insert into launch_containers (launch_id, uuid, name, children, fixtures_z, start_ms, merged_from) \
         select $1, c.uuid, c.name, c.children, c.fixtures_z, c.start_ms, c.launch_id \
         from unnest($2::uuid[], $3::int[]) as s(id, ord) join launch_containers c on c.launch_id = s.id \
         order by s.ord on conflict (launch_id, uuid) do nothing",
    )
    .bind(t.id)
    .bind(&ids)
    .bind(&ords)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "insert into attachment_links (attachment_id, launch_id, merged_from) \
         select distinct on (l.attachment_id) l.attachment_id, $1, l.launch_id from attachment_links l \
         where l.launch_id = any($2) order by l.attachment_id, l.launch_id on conflict do nothing",
    )
    .bind(t.id)
    .bind(&ids)
    .execute(&mut *conn)
    .await?;
    let log_bytes: i64 = sqlx::query_scalar(
        "with ins as (insert into attempt_log_chunks (project_id, launch_id, result_key, attempt, first_seq, \
             last_seq, line_count, levels, bytes, first_ts, last_ts, lines_z, created_at, merged_from) \
           select c.project_id, $1, c.result_key, c.attempt, c.first_seq, c.last_seq, c.line_count, c.levels, \
             c.bytes, c.first_ts, c.last_ts, c.lines_z, c.created_at, c.launch_id \
           from (select distinct on (c.result_key, c.attempt, c.first_seq) c.project_id, c.launch_id, \
               c.result_key, c.attempt, c.first_seq, c.last_seq, c.line_count, c.levels, c.bytes, c.first_ts, \
               c.last_ts, c.lines_z, c.created_at \
             from unnest($2::uuid[], $3::int[]) as s(id, ord) join attempt_log_chunks c on c.launch_id = s.id \
             order by c.result_key, c.attempt, c.first_seq, s.ord) c \
           where not exists (select 1 from attempt_log_chunks o where o.launch_id = $1 \
             and o.result_key = c.result_key and o.attempt = c.attempt and o.first_seq = c.first_seq) \
           returning bytes) \
         select coalesce(sum(bytes), 0)::bigint from ins",
    )
    .bind(t.id)
    .bind(&ids)
    .bind(&ords)
    .fetch_one(&mut *conn)
    .await?;
    if log_bytes > 0 {
        sqlx::query(
            "insert into launch_log_bytes (launch_id, bytes) values ($1, $2) \
             on conflict (launch_id) do update set bytes = launch_log_bytes.bytes + excluded.bytes",
        )
        .bind(t.id)
        .bind(log_bytes)
        .execute(&mut *conn)
        .await?;
    }

    if hide {
        sqlx::query(
            "update launches set merged_into = $2, debug = true, updated_at = now() where id = any($1)",
        )
        .bind(&ids)
        .bind(t.id)
        .execute(&mut *conn)
        .await?;
    }

    let kinds = if affected.is_empty() {
        HashMap::new()
    } else {
        source_kinds(conn, sources).await?
    };
    let kept = recompute_results(conn, t, &affected, &kinds).await?;
    counts.results = kept.len() as i64;
    analyse(conn, ctx, t, &kept).await?;
    crate::ingest::recount(conn, t.id, t.created_at, Some(seq0 + n)).await?;
    rebuild_stats(conn, ctx, &snap, &series).await?;
    if hide {
        repoint_fingerprints(conn, ctx.id, &ids, t.id, t.id).await?;
    }
    Ok(counts)
}

/// The analysis of ingest (transitions, flakiness, defects, mutes, statistics) for results
/// of `t`, a chunk of series at a time.
async fn analyse(
    conn: &mut PgConnection,
    ctx: &Ctx,
    t: Target,
    series: &[Uuid],
) -> anyhow::Result<()> {
    let lref = LaunchRef {
        id: t.id,
        project_id: ctx.id,
        project_key: ctx.key.clone(),
        created_at: t.created_at,
    };
    for chunk in series.chunks(ANALYSIS_CHUNK) {
        crate::analysis::after_persist(&mut *conn, &lref, &[], chunk).await?;
    }
    Ok(())
}

/// Failure fingerprints first or last seen in `from` now name `first` / `last`.
async fn repoint_fingerprints(
    conn: &mut PgConnection,
    project_id: Uuid,
    from: &[Uuid],
    first: Uuid,
    last: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        "update failure_fingerprints set \
           first_launch_id = case when first_launch_id = any($2) then $3 else first_launch_id end, \
           last_launch_id = case when last_launch_id = any($2) then $4 else last_launch_id end \
         where project_id = $1 and (first_launch_id = any($2) or last_launch_id = any($2))",
    )
    .bind(project_id)
    .bind(from)
    .bind(first)
    .bind(last)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A stored merge.
#[derive(Clone, Debug)]
struct MergeRow {
    id: Uuid,
    target_id: Uuid,
    target_created: bool,
    sources: Vec<Uuid>,
    source_seqs: Vec<i64>,
    debug: bool,
    status: String,
    final_merge: bool,
    counts: serde_json::Value,
    created_by: String,
    created_at: DateTime<Utc>,
    undo_until: DateTime<Utc>,
    undone_at: Option<DateTime<Utc>>,
    undone_by: Option<String>,
    finalized_at: Option<DateTime<Utc>>,
}

const MERGE_SELECT: &str = "select id, target_id, target_created, sources, source_seqs, debug, status, final, \
    counts, created_by, created_at, undo_until, undone_at, undone_by, finalized_at from launch_merges";

fn merge_row(r: &PgRow) -> Result<MergeRow, sqlx::Error> {
    Ok(MergeRow {
        id: r.try_get("id")?,
        target_id: r.try_get("target_id")?,
        target_created: r.try_get("target_created")?,
        sources: r.try_get("sources")?,
        source_seqs: r.try_get("source_seqs")?,
        debug: r.try_get("debug")?,
        status: r.try_get("status")?,
        final_merge: r.try_get("final")?,
        counts: r.try_get("counts")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        undo_until: r.try_get("undo_until")?,
        undone_at: r.try_get("undone_at")?,
        undone_by: r.try_get("undone_by")?,
        finalized_at: r.try_get("finalized_at")?,
    })
}

async fn merge_for_update(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> anyhow::Result<Option<MergeRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{MERGE_SELECT} where project_id = $1 and id = $2 for update"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.as_ref().map(merge_row).transpose()?)
}

fn add_counts(stored: &serde_json::Value, late: &MergeCounts) -> serde_json::Value {
    let mut c: MergeCounts = serde_json::from_value(stored.clone()).unwrap_or_default();
    c.late_attempts += late.attempts;
    serde_json::to_value(c).unwrap_or_default()
}

/// Copies what reached the sources of an active merge since it was made or last synced.
async fn sync_in(conn: &mut PgConnection, ctx: &Ctx, m: &MergeRow) -> anyhow::Result<bool> {
    let mut ids = m.sources.clone();
    ids.push(m.target_id);
    let rows = lock_launches(conn, ctx.id, &ids).await?;
    let Some(t) = rows.iter().find(|r| r.id == m.target_id) else {
        return Ok(false);
    };
    let t = Target {
        id: t.id,
        created_at: t.created_at,
    };
    let seqs: HashMap<Uuid, i64> = m
        .sources
        .iter()
        .copied()
        .zip(m.source_seqs.iter().copied())
        .collect();
    let mut sources: Vec<&Row0> = rows
        .iter()
        .filter(|r| r.id != m.target_id && m.sources.contains(&r.id))
        .collect();
    sources.sort_by_key(|r| (r.created_at, r.number));
    let changed = sources
        .iter()
        .any(|r| r.attempt_seq > seqs.get(&r.id).copied().unwrap_or(i64::MAX));
    if !changed {
        return Ok(false);
    }
    let srcs: Vec<Src> = sources
        .iter()
        .map(|r| Src {
            id: r.id,
            at: r.created_at,
        })
        .collect();
    let late = apply(conn, ctx, t, &srcs, false).await?;
    let now_seqs: Vec<i64> = m
        .sources
        .iter()
        .map(|id| {
            sources
                .iter()
                .find(|r| r.id == *id)
                .map(|r| r.attempt_seq)
                .unwrap_or(0)
        })
        .collect();
    sqlx::query(
        "update launch_merges set source_seqs = $2, synced_at = now(), counts = $3 where id = $1",
    )
    .bind(m.id)
    .bind(&now_seqs)
    .bind(add_counts(&m.counts, &late))
    .execute(&mut *conn)
    .await?;
    crate::events::notify_launch(&mut *conn, &ctx.key, t.id).await?;
    Ok(true)
}

/// Deletes what the sources of a merge hold except the launch rows, their uploads and their
/// attachment links and references, which stay as tombstones.
async fn tombstone(conn: &mut PgConnection, sources: &[Src]) -> anyhow::Result<()> {
    let (ids, ats, _) = src_arrays(sources);
    for table in ["attempts", "results"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {table} t using unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
             where t.launch_id = s.id and t.launch_created_at = s.at"
        )))
        .bind(&ids)
        .bind(&ats)
        .execute(&mut *conn)
        .await?;
    }
    for table in [
        "attempt_log_chunks",
        "launch_containers",
        "launch_gates",
        "case_sync_queue",
        "launch_log_bytes",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {table} where launch_id = any($1)"
        )))
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

impl Store {
    /// Merges launches (see the module documentation). Returns the merge and whether this call
    /// made it (`false`: the same merge exists and only what reached the sources since was
    /// copied, or a preview).
    pub async fn merge_launches(
        &self,
        project: &Project,
        req: &LaunchMergeRequest,
        by: &str,
        opts: MergeOptions,
    ) -> Result<(LaunchMerge, bool), MergeError> {
        validate(req)?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let r = async {
                let mut tx = self.pool.begin().await?;
                let c = ctx(&mut tx, project, self.purge_batch).await?;
                match merge_in(&mut tx, &c, req, by, opts).await {
                    Ok(o @ Outcome::Preview(_)) => {
                        tx.rollback().await?;
                        Ok(o)
                    }
                    Ok(o) => {
                        tx.commit().await?;
                        Ok(o)
                    }
                    Err(e) => {
                        let _ = tx.rollback().await;
                        Err(e)
                    }
                }
            }
            .await;
            match r {
                Err(MergeError::Other(e)) if attempt < TX_ATTEMPTS && retryable(&e).is_some() => {
                    retry_pause("merge", &e, attempt).await
                }
                Err(e) => return Err(e),
                Ok(Outcome::Preview(p)) => return Ok((*p, false)),
                Ok(Outcome::Merge { id, created }) => {
                    if req.final_merge && created {
                        self.finalize_merge(project.id, id).await?;
                    }
                    let m = self
                        .merge(project.id, id)
                        .await?
                        .ok_or(MergeError::NotFound("merge"))?;
                    return Ok((m, created));
                }
            }
        }
    }

    /// A merge of the project.
    pub async fn merge(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<LaunchMerge>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{MERGE_SELECT} where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match r {
            Some(r) => {
                let m = merge_row(&r)?;
                Ok(self.expand(project_id, vec![m]).await?.pop())
            }
            None => Ok(None),
        }
    }

    /// The merges a launch took part in (as target or source), newest first, at most 50.
    pub async fn launch_merges(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
    ) -> anyhow::Result<Vec<LaunchMerge>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{MERGE_SELECT} where project_id = $1 and (target_id = $2 or sources @> array[$2]::uuid[]) \
             order by created_at desc limit 50"
        )))
        .bind(project_id)
        .bind(launch_id)
        .fetch_all(&self.pool)
        .await?;
        let rows: Vec<MergeRow> = rows.iter().map(merge_row).collect::<Result<_, _>>()?;
        self.expand(project_id, rows).await
    }

    /// Who created each of these launches of the project (missing ids are left out).
    pub async fn launch_creators(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, Option<String>>> {
        Ok(sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select id, created_by from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    async fn expand(
        &self,
        project_id: Uuid,
        rows: Vec<MergeRow>,
    ) -> anyhow::Result<Vec<LaunchMerge>> {
        let mut ids: Vec<Uuid> = rows
            .iter()
            .flat_map(|m| m.sources.iter().copied().chain([m.target_id]))
            .collect();
        ids.sort();
        ids.dedup();
        let named: HashMap<Uuid, (i64, String)> = sqlx::query_as::<_, (Uuid, i64, String)>(
            "select id, number, name from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(i, n, s)| (i, (n, s)))
        .collect();
        Ok(rows
            .into_iter()
            .map(|m| LaunchMerge {
                id: Some(m.id),
                target_id: Some(m.target_id),
                target_number: named.get(&m.target_id).map(|v| v.0),
                created_target: m.target_created,
                sources: m
                    .sources
                    .iter()
                    .map(|s| MergeSource {
                        id: *s,
                        number: named.get(s).map(|v| v.0),
                        name: named.get(s).map(|v| v.1.clone()),
                    })
                    .collect(),
                status: m.status,
                final_merge: m.final_merge,
                counts: serde_json::from_value(m.counts).unwrap_or_default(),
                created_by: Some(m.created_by),
                created_at: Some(m.created_at),
                undo_until: (!m.final_merge).then_some(m.undo_until),
                undone_at: m.undone_at,
                undone_by: m.undone_by,
                finalized_at: m.finalized_at,
            })
            .collect())
    }

    /// Undoes an active merge within its window: a target the merge created is deleted (it
    /// must hold nothing but the merge), or the copied attempts leave the existing target; the
    /// sources come back as they were.
    pub async fn undo_merge(
        &self,
        project: &Project,
        id: Uuid,
        by: &str,
    ) -> Result<LaunchMerge, MergeError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let r = async {
                let mut tx = self.pool.begin().await?;
                let c = ctx(&mut tx, project, self.purge_batch).await?;
                match undo_in(&mut tx, &c, id, by).await {
                    Ok(()) => {
                        tx.commit().await?;
                        Ok(())
                    }
                    Err(e) => {
                        let _ = tx.rollback().await;
                        Err(e)
                    }
                }
            }
            .await;
            match r {
                Err(MergeError::Other(e)) if attempt < TX_ATTEMPTS && retryable(&e).is_some() => {
                    retry_pause("merge_undo", &e, attempt).await
                }
                Err(e) => return Err(e),
                Ok(()) => {
                    return self
                        .merge(project.id, id)
                        .await?
                        .ok_or(MergeError::NotFound("merge"));
                }
            }
        }
    }

    /// Copies late attempts of an active merge into its target. Returns whether anything was
    /// copied.
    pub async fn sync_merge(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let Some(project) = self.project_by_id(project_id).await? else {
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let c = ctx(&mut tx, &project, self.purge_batch).await?;
        project_lock(&mut tx, project_id).await?;
        let Some(m) = merge_for_update(&mut tx, project_id, id).await? else {
            return Ok(false);
        };
        if m.status != "active" {
            return Ok(false);
        }
        let done = sync_in(&mut tx, &c, &m).await?;
        tx.commit().await?;
        Ok(done)
    }

    /// Ends the undo window of a merge: late attempts are copied, then the sources become
    /// tombstones. Returns false when the merge is not active.
    pub async fn finalize_merge(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let Some(project) = self.project_by_id(project_id).await? else {
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let c = ctx(&mut tx, &project, self.purge_batch).await?;
        project_lock(&mut tx, project_id).await?;
        let Some(m) = merge_for_update(&mut tx, project_id, id).await? else {
            return Ok(false);
        };
        if m.status != "active" {
            return Ok(false);
        }
        sync_in(&mut tx, &c, &m).await?;
        let rows = lock_launches(&mut tx, project_id, &m.sources).await?;
        let srcs: Vec<Src> = rows
            .iter()
            .map(|r| Src {
                id: r.id,
                at: r.created_at,
            })
            .collect();
        tombstone(&mut tx, &srcs).await?;
        sqlx::query(
            "update launch_merges set status = 'final', finalized_at = now() where id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "launch.merge_finalized",
            Some(&project.key),
            serde_json::json!({"mergeId": id, "targetId": m.target_id, "sources": m.sources}),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The merge job: copies late attempts of active merges, finalizes merges whose undo
    /// window ended and deletes hidden launches whose target is gone (the target was deleted:
    /// its sources go with it). Bounded per run.
    pub async fn merge_maintenance(&self) -> anyhow::Result<MergeJobReport> {
        let mut report = MergeJobReport::default();
        let late: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select m.project_id, m.id from launch_merges m where m.status = 'active' and exists \
               (select 1 from unnest(m.sources, m.source_seqs) as s(id, seq) join launches l on l.id = s.id \
                where l.attempt_seq > s.seq) \
             order by m.created_at limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (p, id) in late {
            if self.sync_merge(p, id).await? {
                report.synced += 1;
            }
        }
        let due: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select project_id, id from launch_merges where status = 'active' and undo_until <= now() \
             order by undo_until limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (p, id) in due {
            if self.finalize_merge(p, id).await? {
                report.finalized += 1;
            }
        }
        let orphans: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "select l.id, l.project_id, p.key from launches l join projects p on p.id = l.project_id \
             where l.merged_into is not null \
               and not exists (select 1 from launches t where t.id = l.merged_into) \
             order by l.id limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (id, project_id, key) in orphans {
            let mut tx = self.pool.begin().await?;
            project_lock(&mut tx, project_id).await?;
            let gone: Option<Uuid> = sqlx::query_scalar(
                "select l.id from launches l where l.id = $1 and l.merged_into is not null \
                   and not exists (select 1 from launches t where t.id = l.merged_into) for update",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            if gone.is_none() {
                continue;
            }
            crate::purge::Purge::new(&mut tx, self.purge_batch, crate::purge::Mode::Atomic)
                .launches(&[id])
                .await?;
            sqlx::query(
                "update launch_merges set status = 'deleted', finalized_at = coalesce(finalized_at, now()) \
                 where project_id = $1 and sources @> array[$2]::uuid[] and status = 'active'",
            )
            .bind(project_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            crate::events::emit(
                &mut tx,
                "launch.deleted",
                Some(&key),
                serde_json::json!({"id": id, "launchId": id, "projectId": project_id}),
            )
            .await?;
            tx.commit().await?;
            report.orphans_deleted += 1;
        }
        Ok(report)
    }

    async fn project_by_id(&self, id: Uuid) -> anyhow::Result<Option<Project>> {
        let key: Option<String> =
            sqlx::query_scalar("select key from projects where id = $1 and deleted_at is null")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        match key {
            Some(k) => self.project(&k).await,
            None => Ok(None),
        }
    }
}

/// What [`merge_in`] decided.
enum Outcome {
    Preview(Box<LaunchMerge>),
    Merge { id: Uuid, created: bool },
}

/// An active merge of exactly these sources into `into` (or into a launch it created).
async fn existing_merge(
    conn: &mut PgConnection,
    project_id: Uuid,
    sources: &[Row0],
    into: Option<Uuid>,
) -> anyhow::Result<Option<MergeRow>> {
    let targets: Vec<Option<Uuid>> = sources
        .iter()
        .map(|s| s.merged_into)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let [Some(target)] = targets.as_slice() else {
        return Ok(None);
    };
    let target = *target;
    if into.is_some_and(|i| i != target) {
        return Ok(None);
    }
    let want: BTreeSet<Uuid> = sources.iter().map(|s| s.id).collect();
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{MERGE_SELECT} where project_id = $1 and target_id = $2 and status = 'active' for update"
    )))
    .bind(project_id)
    .bind(target)
    .fetch_all(&mut *conn)
    .await?;
    for r in &rows {
        let m = merge_row(r)?;
        let have: BTreeSet<Uuid> = m.sources.iter().copied().collect();
        if have == want && m.target_created == into.is_none() {
            return Ok(Some(m));
        }
    }
    Ok(None)
}

async fn merge_in(
    conn: &mut PgConnection,
    ctx: &Ctx,
    req: &LaunchMergeRequest,
    by: &str,
    opts: MergeOptions,
) -> Result<Outcome, MergeError> {
    project_lock(conn, ctx.id).await?;
    let rows = lock_launches(conn, ctx.id, &req.launch_ids).await?;
    if rows.len() != req.launch_ids.len() {
        return Err(MergeError::NotFound("launch"));
    }
    let target_row = req
        .into
        .and_then(|t| rows.iter().find(|r| r.id == t))
        .cloned();
    let mut sources: Vec<Row0> = rows
        .iter()
        .filter(|r| Some(r.id) != req.into)
        .cloned()
        .collect();
    sources.sort_by_key(|r| (r.created_at, r.number));

    if !req.dry_run
        && let Some(m) = existing_merge(conn, ctx.id, &sources, req.into).await?
    {
        sync_in(conn, ctx, &m).await?;
        return Ok(Outcome::Merge {
            id: m.id,
            created: false,
        });
    }
    for r in &rows {
        if let Some(t) = r.merged_into {
            return Err(conflict(
                "launch_merged",
                format!("launch #{} is merged into another launch", r.number),
                serde_json::json!({"launchId": r.id, "mergedInto": t}),
            ));
        }
        if r.manual_state.is_some() {
            return Err(conflict(
                "launch_manual",
                format!(
                    "launch #{} has a manual run; manual runs are not merged",
                    r.number
                ),
                serde_json::json!({"launchId": r.id}),
            ));
        }
    }
    let running: Vec<Uuid> = sources
        .iter()
        .filter(|r| r.status == "running")
        .map(|r| r.id)
        .collect();
    if !running.is_empty() {
        return Err(conflict(
            "launch_running",
            "finish the launches to merge first (only the target may still run)",
            serde_json::json!({"launchIds": running}),
        ));
    }
    let src_ids: Vec<Uuid> = sources.iter().map(|r| r.id).collect();
    let busy: Vec<Uuid> = sqlx::query_scalar(
        "select distinct launch_id from uploads where launch_id = any($1) and status in ('queued', 'processing')",
    )
    .bind(&src_ids)
    .fetch_all(&mut *conn)
    .await?;
    if !busy.is_empty() {
        return Err(conflict(
            "launch_busy",
            "uploads of the launches to merge are still being processed",
            serde_json::json!({"launchIds": busy}),
        ));
    }
    let first = &rows[0];
    if rows.iter().any(|r| r.debug != first.debug) {
        return Err(conflict(
            "merge_mixed",
            "debug and regular launches are not merged together",
            serde_json::json!({"field": "debug"}),
        ));
    }
    if rows.iter().any(|r| r.branch != first.branch) {
        return Err(conflict(
            "merge_mixed",
            "launches of different branches are not merged",
            serde_json::json!({"field": "branch"}),
        ));
    }
    if let Some(r) = rows.iter().find(|r| r.rolled_up_at.is_some() && !r.debug) {
        return Err(conflict(
            "launch_rolled_up",
            format!(
                "launch #{} is already in the daily history aggregates",
                r.number
            ),
            serde_json::json!({"launchId": r.id}),
        ));
    }
    let srcs: Vec<Src> = sources
        .iter()
        .map(|r| Src {
            id: r.id,
            at: r.created_at,
        })
        .collect();
    let n = count_attempts(conn, &srcs).await?;
    if n > opts.max_attempts {
        return Err(MergeError::TooLarge {
            attempts: n,
            limit: opts.max_attempts,
        });
    }
    let all: Vec<Src> = rows
        .iter()
        .map(|r| Src {
            id: r.id,
            at: r.created_at,
        })
        .collect();
    let shared = shared_series(conn, &all).await?;
    let source_list: Vec<MergeSource> = sources
        .iter()
        .map(|r| MergeSource {
            id: r.id,
            number: Some(r.number),
            name: Some(r.name.clone()),
        })
        .collect();
    if req.dry_run {
        let series = source_series(conn, &srcs).await?;
        return Ok(Outcome::Preview(Box::new(LaunchMerge {
            id: None,
            target_id: target_row.as_ref().map(|t| t.id),
            target_number: target_row.as_ref().map(|t| t.number),
            created_target: target_row.is_none(),
            sources: source_list,
            status: "preview".into(),
            final_merge: req.final_merge,
            counts: MergeCounts {
                attempts: n,
                results: series.len() as i64,
                shared_series: shared,
                ..Default::default()
            },
            created_by: None,
            created_at: None,
            undo_until: (!req.final_merge).then(|| Utc::now() + opts.undo_window),
            undone_at: None,
            undone_by: None,
            finalized_at: None,
        })));
    }

    let (target, number) = match &target_row {
        Some(t) => (
            Target {
                id: t.id,
                created_at: t.created_at,
            },
            t.number,
        ),
        None => insert_target(conn, ctx, &sources, req.name.as_deref(), by).await?,
    };
    let mut counts = apply(conn, ctx, target, &srcs, true).await?;
    counts.shared_series = shared;
    let delta = target_row.is_none() as i64 - sources.len() as i64;
    sqlx::query(
        "update projects set launch_count = greatest(launch_count + $2, 0), updated_at = now() where id = $1",
    )
    .bind(ctx.id)
    .bind(delta)
    .execute(&mut *conn)
    .await?;
    let id = Uuid::now_v7();
    let undo_until = if req.final_merge {
        Utc::now()
    } else {
        Utc::now() + opts.undo_window
    };
    let seqs: Vec<i64> = sources.iter().map(|r| r.attempt_seq).collect();
    sqlx::query(
        "insert into launch_merges (id, project_id, target_id, target_created, sources, source_seqs, debug, \
           final, counts, created_by, undo_until) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(ctx.id)
    .bind(target.id)
    .bind(target_row.is_none())
    .bind(&src_ids)
    .bind(&seqs)
    .bind(first.debug)
    .bind(req.final_merge)
    .bind(serde_json::to_value(&counts).map_err(anyhow::Error::from)?)
    .bind(no_nul(by))
    .bind(undo_until)
    .execute(&mut *conn)
    .await?;
    if target_row.is_none() {
        crate::events::emit(
            &mut *conn,
            "launch.created",
            Some(&ctx.key),
            serde_json::json!({"id": target.id, "number": number}),
        )
        .await?;
    }
    crate::events::emit(
        &mut *conn,
        "launch.merged",
        Some(&ctx.key),
        serde_json::json!({"mergeId": id, "targetId": target.id, "number": number,
            "createdTarget": target_row.is_none(), "sources": src_ids}),
    )
    .await?;
    let status: String = sqlx::query_scalar("select status from launches where id = $1")
        .bind(target.id)
        .fetch_one(&mut *conn)
        .await?;
    if status != "running" {
        // The quality gate (and what follows it) judges the merged launch.
        crate::events::emit(
            &mut *conn,
            "launch.finished",
            Some(&ctx.key),
            serde_json::json!({"id": target.id, "status": status}),
        )
        .await?;
        sqlx::query(
            "update launch_gates set state = 'waiting', queued_at = now() where launch_id = $1 and state = 'done'",
        )
        .bind(target.id)
        .execute(&mut *conn)
        .await?;
    }
    crate::events::notify_launch(&mut *conn, &ctx.key, target.id).await?;
    for s in &src_ids {
        crate::events::notify_launch(&mut *conn, &ctx.key, *s).await?;
    }
    Ok(Outcome::Merge { id, created: true })
}

async fn undo_in(conn: &mut PgConnection, ctx: &Ctx, id: Uuid, by: &str) -> Result<(), MergeError> {
    project_lock(conn, ctx.id).await?;
    let m = merge_for_update(conn, ctx.id, id)
        .await?
        .ok_or(MergeError::NotFound("merge"))?;
    if m.status != "active" {
        return Err(conflict(
            "merge_not_active",
            format!("the merge is {}", m.status),
            serde_json::json!({"status": m.status}),
        ));
    }
    if m.final_merge {
        return Err(conflict(
            "merge_final",
            "the merge was made without undo",
            serde_json::json!({}),
        ));
    }
    if m.undo_until <= Utc::now() {
        return Err(conflict(
            "merge_expired",
            "the undo window of the merge has ended",
            serde_json::json!({"undoUntil": m.undo_until}),
        ));
    }
    let mut ids = m.sources.clone();
    ids.push(m.target_id);
    let rows = lock_launches(conn, ctx.id, &ids).await?;
    let Some(t) = rows.iter().find(|r| r.id == m.target_id).cloned() else {
        return Err(conflict(
            "merge_target_gone",
            "the merged launch was deleted",
            serde_json::json!({}),
        ));
    };
    if let Some(into) = t.merged_into {
        return Err(conflict(
            "merge_target_merged",
            "the merged launch was merged again; undo that merge first",
            serde_json::json!({"mergedInto": into}),
        ));
    }
    if t.rolled_up_at.is_some() && !t.debug {
        return Err(conflict(
            "merge_target_rolled_up",
            "the merged launch is already in the daily history aggregates",
            serde_json::json!({}),
        ));
    }
    let target = Target {
        id: t.id,
        created_at: t.created_at,
    };
    let mut sources: Vec<&Row0> = rows.iter().filter(|r| r.id != t.id).collect();
    sources.sort_by_key(|r| (r.created_at, r.number));
    let srcs: Vec<Src> = sources
        .iter()
        .map(|r| Src {
            id: r.id,
            at: r.created_at,
        })
        .collect();
    let src_ids: Vec<Uuid> = srcs.iter().map(|s| s.id).collect();
    let series = source_series(conn, &srcs).await?;
    let window = srcs
        .iter()
        .map(|s| s.at)
        .chain([t.created_at])
        .min()
        .unwrap_or(t.created_at);
    if m.target_created {
        let own: bool = sqlx::query_scalar(
            "select exists (select 1 from attempts where launch_id = $1 and launch_created_at = $2 \
               and merged_from is null) or exists (select 1 from uploads where launch_id = $1)",
        )
        .bind(t.id)
        .bind(t.created_at)
        .fetch_one(&mut *conn)
        .await?;
        if own {
            return Err(conflict(
                "merge_target_changed",
                "results were added to the merged launch after the merge; delete it instead",
                serde_json::json!({"targetId": t.id}),
            ));
        }
    }
    let snap = stats_snapshot(conn, ctx.id, &series, window).await?;
    sqlx::query(
        "update launches set merged_into = null, debug = $2, rolled_up_at = case when $2 then rolled_up_at end, \
           updated_at = now() where id = any($1)",
    )
    .bind(&src_ids)
    .bind(m.debug)
    .execute(&mut *conn)
    .await?;
    if m.target_created {
        crate::purge::Purge::new(&mut *conn, ctx.purge_batch, crate::purge::Mode::Atomic)
            .launches(&[t.id])
            .await?;
        if let (Some(f), Some(l)) = (src_ids.first(), src_ids.last()) {
            repoint_fingerprints(conn, ctx.id, &[t.id], *f, *l).await?;
        }
        crate::events::emit(
            &mut *conn,
            "launch.deleted",
            Some(&ctx.key),
            serde_json::json!({"id": t.id, "launchId": t.id, "projectId": ctx.id}),
        )
        .await?;
    } else {
        let mut affected: Vec<Uuid> = sqlx::query_scalar(
            "delete from attempts where launch_id = $1 and launch_created_at = $2 and merged_from = any($3) \
             returning series_id",
        )
        .bind(t.id)
        .bind(t.created_at)
        .bind(&src_ids)
        .fetch_all(&mut *conn)
        .await?;
        affected.sort();
        affected.dedup();
        for table in ["launch_containers", "attachment_links"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {table} where launch_id = $1 and merged_from = any($2)"
            )))
            .bind(t.id)
            .bind(&src_ids)
            .execute(&mut *conn)
            .await?;
        }
        let freed: i64 = sqlx::query_scalar(
            "with d as (delete from attempt_log_chunks where launch_id = $1 and merged_from = any($2) \
               returning bytes) select coalesce(sum(bytes), 0)::bigint from d",
        )
        .bind(t.id)
        .bind(&src_ids)
        .fetch_one(&mut *conn)
        .await?;
        if freed > 0 {
            sqlx::query(
                "update launch_log_bytes set bytes = greatest(bytes - $2, 0) where launch_id = $1",
            )
            .bind(t.id)
            .bind(freed)
            .execute(&mut *conn)
            .await?;
        }
        let kept = recompute_results(conn, target, &affected, &HashMap::new()).await?;
        analyse(conn, ctx, target, &kept).await?;
        crate::ingest::recount(conn, t.id, t.created_at, None).await?;
        crate::events::notify_launch(&mut *conn, &ctx.key, t.id).await?;
    }
    rebuild_stats(conn, ctx, &snap, &series).await?;
    let delta = src_ids.len() as i64 - m.target_created as i64;
    sqlx::query(
        "update projects set launch_count = greatest(launch_count + $2, 0), updated_at = now() where id = $1",
    )
    .bind(ctx.id)
    .bind(delta)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "update launch_merges set status = 'undone', undone_at = now(), undone_by = $2 where id = $1",
    )
    .bind(id)
    .bind(no_nul(by))
    .execute(&mut *conn)
    .await?;
    crate::events::emit(
        &mut *conn,
        "launch.unmerged",
        Some(&ctx.key),
        serde_json::json!({"mergeId": id, "targetId": t.id, "sources": src_ids}),
    )
    .await?;
    for s in &src_ids {
        crate::events::notify_launch(&mut *conn, &ctx.key, *s).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilt_statuses_replace_the_window() {
        let old = ("PPFPF".to_string(), 10);
        let (s, n) = rebuilt_statuses(Some(&old), 2, &[Status::Passed]);
        assert_eq!((s.as_str(), n), ("PPFP", 9));
        let (s, n) = rebuilt_statuses(None, 0, &[Status::Failed, Status::Passed]);
        assert_eq!((s.as_str(), n), ("FP", 2));
        let (s, n) = rebuilt_statuses(Some(&old), 9, &[]);
        assert_eq!((s.as_str(), n), ("", 1));
        let long = ("P".repeat(STATUS_CHARS), 200);
        let (s, n) = rebuilt_statuses(Some(&long), 1, &[Status::Broken, Status::Failed]);
        assert_eq!(s.len(), STATUS_CHARS);
        assert!(s.ends_with("BF"));
        assert_eq!(n, 201);
    }

    #[test]
    fn requests_are_checked() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let ok = LaunchMergeRequest {
            launch_ids: vec![a, b],
            ..Default::default()
        };
        assert!(validate(&ok).is_ok());
        for bad in [
            LaunchMergeRequest {
                launch_ids: vec![a],
                ..Default::default()
            },
            LaunchMergeRequest {
                launch_ids: vec![a, a],
                ..Default::default()
            },
            LaunchMergeRequest {
                launch_ids: vec![a, b],
                into: Some(Uuid::now_v7()),
                ..Default::default()
            },
            LaunchMergeRequest {
                launch_ids: vec![a, b],
                into: Some(a),
                name: Some("x".into()),
                ..Default::default()
            },
            LaunchMergeRequest {
                launch_ids: (0..21).map(|_| Uuid::now_v7()).collect(),
                ..Default::default()
            },
        ] {
            assert!(matches!(validate(&bad), Err(MergeError::Invalid(_))));
        }
    }
}
