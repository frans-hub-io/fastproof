//! Merging launches (CI shards, re-runs that no CI id joined) into a new launch or into one of
//! them.
//!
//! A merge runs in two phases.
//!
//! 1. One transaction copies the attempts of the source launches into the target and
//!    recomputes every affected result from all its attempts with the retry semantics of
//!    ingest: a later run of the same test is a new attempt and the latest attempt is the
//!    result ([`fastproof_model::group_attempts`]). It hides the sources (`merged_into`, and
//!    `debug` so that history, statistics, gates and dashboards leave them out), recounts the
//!    target and queues the affected series in `launch_merge_work`. This phase takes no lock
//!    that ingest of other launches needs: only the rows of the merged launches.
//! 2. Short transactions, a chunk of series at a time and in ingest's lock order, run the
//!    analysis of ingest on the target (transitions, flakiness, defects, mutes) and rebuild the
//!    series statistics; each chunk removes its work rows, so a crash resumes where it stopped
//!    (the `launches.merges` job drains what is left).
//!
//! A merge can be undone until `undo_until`. Afterwards (or after a short grace for a merge
//! without undo) the sources become tombstones: their results, attempts, logs and containers
//! go, the launch row and uploads stay, attachment references move to the target. Hidden
//! launches leave with their target. Merges, undos and finalizations of a project never run
//! at the same time (`pg_try_advisory_xact_lock`: a second request gets `merge_busy`).

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
/// Series analysed per transaction of the second phase.
const ANALYSIS_CHUNK: usize = 500;
/// Attributes of a new target, at most (as a launch start).
const MAX_ATTRIBUTES: usize = 100;
/// Merges the maintenance job handles per kind and run, at most.
const JOB_BATCH: i64 = 20;
/// How long a merge without undo keeps its sources before they become tombstones, so that
/// writes already on their way (a Kafka writer behind) are copied first.
pub const FINAL_GRACE: chrono::Duration = chrono::Duration::minutes(2);
/// Days after finalization during which writes reaching a tombstone are still moved to the
/// target.
const LATE_DAYS: i32 = 7;
/// How long a request runs the second phase before leaving the rest to the job.
const REQUEST_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
/// How long the job runs the second phase of one merge per run.
const JOB_BUDGET: std::time::Duration = std::time::Duration::from_secs(50);
/// Lock wait of a merge transaction: a merge gives up instead of making ingest queue behind it.
const LOCK_TIMEOUT: &str = "set local lock_timeout = '5s'";

/// The statement that copies the attempts of the sources into the target (`$1` target creation
/// time, `$2` project, `$3` target, `$4`..`$6` source ids, creation times and order, `$7` new
/// attempt ids, `$8` the target's attempt sequence before). Collisions of client keys are found
/// with window functions over the keys only; the payload columns are read once, in the insert.
/// A key that an earlier source used for another test or start, or that the target holds for
/// another test or start (or for a running attempt of its own), is copied under a prefixed key:
/// `r:m:<source>:<id>` for results sent through the API (their log lines follow them),
/// `m:<source>:<key>` otherwise. The same key for the same test and start is one attempt.
pub const COPY_ATTEMPTS_SQL: &str = "with src as materialized ( \
       select a.id, a.launch_created_at as at, a.launch_id, a.series_id, a.client_key, a.seq, a.started_at, s.ord \
       from unnest($4::uuid[], $5::timestamptz[], $6::int[]) as s(id, at, ord) \
       join attempts a on a.launch_id = s.id and a.launch_created_at = s.at), \
     ranked as (select src.*, \
         row_number() over (order by src.ord, src.seq) as n, \
         row_number() over (partition by src.client_key order by src.ord, src.seq) as kn, \
         first_value(src.series_id) over (partition by src.client_key order by src.ord, src.seq) as fs, \
         first_value(src.started_at) over (partition by src.client_key order by src.ord, src.seq) as fa \
       from src), \
     keyed as (select r.id, r.at, r.n, r.kn, r.launch_id, r.series_id, r.client_key, \
         (r.series_id <> r.fs or r.started_at is distinct from r.fa) as other_test, \
         (t.client_key is not null and t.merged_from is distinct from r.launch_id \
           and (t.series_id <> r.series_id or t.started_at is distinct from r.started_at \
             or (t.merged_from is null and t.status = 'running'))) as taken \
       from ranked r left join attempts t on t.launch_id = $3 and t.launch_created_at = $1 \
         and t.client_key = r.client_key), \
     picked as (select k.id, k.at, k.n, k.launch_id, k.series_id, \
         case when k.other_test or k.taken then \
           case when k.client_key like 'r:%' then 'r:m:' || k.launch_id::text || ':' || substr(k.client_key, 3) \
           else 'm:' || k.launch_id::text || ':' || k.client_key end \
         else k.client_key end as new_key \
       from keyed k where k.kn = 1 or k.other_test or k.taken) \
   insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, upload_id, \
     client_key, seq, status, raw_status, started_at, stopped_at, duration_ms, message, trace, source_uuid, \
     trace_id, detail, steps_z, logs_z, fingerprint, merged_from) \
   select ($7::uuid[])[p.n], $1, $2, $3, r.id, p.series_id, null, p.new_key, $8 + p.n, a.status, a.raw_status, \
     a.started_at, a.stopped_at, a.duration_ms, a.message, a.trace, a.source_uuid, a.trace_id, a.detail, \
     a.steps_z, a.logs_z, a.fingerprint, p.launch_id \
   from picked p join attempts a on a.id = p.id and a.launch_created_at = p.at \
   join results r on r.launch_id = $3 and r.launch_created_at = $1 and r.series_id = p.series_id \
   on conflict (launch_id, client_key, launch_created_at) do update set status = excluded.status, \
     result_id = excluded.result_id, series_id = excluded.series_id, raw_status = excluded.raw_status, \
     started_at = excluded.started_at, stopped_at = excluded.stopped_at, duration_ms = excluded.duration_ms, \
     message = excluded.message, trace = excluded.trace, source_uuid = excluded.source_uuid, \
     trace_id = excluded.trace_id, detail = excluded.detail, steps_z = excluded.steps_z, \
     logs_z = excluded.logs_z, fingerprint = excluded.fingerprint \
     where attempts.status = 'running' and excluded.status <> 'running' \
       and attempts.merged_from is not distinct from excluded.merged_from \
   returning series_id, merged_from, client_key";

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
    /// No undo: the sources are deleted after a short grace (needs `settings:write`).
    #[serde(default, rename = "final")]
    pub final_merge: bool,
    /// Check and count without merging (status `preview`); takes no locks.
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
    /// `preview`, `active` (can be undone until `undoUntil`, unless `final`), `undone`,
    /// `final` (the sources are deleted) or `deleted` (the target was deleted, and the sources
    /// with it).
    pub status: String,
    /// Merged without undo.
    #[serde(rename = "final")]
    pub final_merge: bool,
    /// The analysis and statistics of the target are still being updated.
    pub analysing: bool,
    pub counts: MergeCounts,
    pub created_by: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    /// Until when the merge can be undone (absent for a merge without undo).
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
    Forbidden {
        code: &'static str,
        message: String,
    },
    TooLarge {
        what: &'static str,
        value: i64,
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

fn busy() -> MergeError {
    conflict(
        "merge_busy",
        "another merge, undo or finalization of this project is running; retry in a moment",
        serde_json::json!({}),
    )
}

/// True for errors after which a merge transaction may simply run again: deadlock,
/// serialization failure, lock timeout (SQLSTATE 55P03).
pub fn merge_retryable(e: &anyhow::Error) -> bool {
    retryable(e).is_some()
        || e.chain().any(|c| {
            matches!(c.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::Database(d)) if d.code().as_deref() == Some("55P03"))
        })
}

/// Server settings of merges.
#[derive(Clone, Copy, Debug)]
pub struct MergeOptions {
    /// How long a merge can be undone.
    pub undo_window: chrono::Duration,
    /// Attempts of the sources of one merge, at most.
    pub max_attempts: i64,
    /// Log bytes one launch may hold (`launch_log_bytes`), so the target stays within it.
    pub max_log_bytes: i64,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            undo_window: chrono::Duration::hours(24),
            max_attempts: 200_000,
            max_log_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Who merges or undoes: a display name, a stable key (the managed token id, else the
/// principal kind and name: `token:<id>` / `<kind>:<name>`), and what the caller may do.
#[derive(Clone, Debug, Default)]
pub struct MergeCaller {
    pub name: String,
    pub key: String,
    /// `cases:sync`: may make a launch trusted for code sync.
    pub can_sync: bool,
    /// `results:triage`: may merge others' launches and undo others' merges.
    pub can_triage: bool,
}

/// What the maintenance job did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeJobReport {
    /// Merges whose analysis was resumed and finished.
    pub analysed: u64,
    /// Merges whose sources got late attempts or log lines, copied into the target.
    pub synced: u64,
    /// Merges whose undo window ended (sources turned into tombstones).
    pub finalized: u64,
    /// Hidden launches deleted because their target is gone.
    pub orphans_deleted: u64,
    /// Merges that failed in this run (logged and counted; the others went on).
    pub errors: u64,
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

/// A launch as a merge reads it.
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
    test_plan_id: Option<Uuid>,
    merged_into: Option<Uuid>,
    attempt_seq: i64,
    created_by_key: Option<String>,
    ci: Option<serde_json::Value>,
    vcs: Option<serde_json::Value>,
    environment: Option<serde_json::Value>,
    attributes: serde_json::Value,
    created_by_can_sync: bool,
    sync_blocked: bool,
    important: bool,
}

impl Row0 {
    /// Code sync must not trust what this launch brought.
    fn untrusted(&self) -> bool {
        self.sync_blocked || !self.created_by_can_sync
    }

    fn src(&self) -> Src {
        Src {
            id: self.id,
            at: self.created_at,
        }
    }
}

const ROW_SELECT: &str = "select id, number, name, status, debug, branch, env_key, created_at, started_at, \
    finished_at, rolled_up_at, manual_state, test_plan_id, merged_into, attempt_seq, created_by_key, ci, vcs, \
    environment, attributes, created_by_can_sync, sync_blocked, important from launches";

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
        test_plan_id: r.try_get("test_plan_id")?,
        merged_into: r.try_get("merged_into")?,
        attempt_seq: r.try_get("attempt_seq")?,
        created_by_key: r.try_get("created_by_key")?,
        ci: r.try_get("ci")?,
        vcs: r.try_get("vcs")?,
        environment: r.try_get("environment")?,
        attributes: r.try_get("attributes")?,
        created_by_can_sync: r.try_get("created_by_can_sync")?,
        sync_blocked: r.try_get("sync_blocked")?,
        important: r.try_get("important")?,
    })
}

/// Reads launches of a project; `lock` takes their rows in id order (as every merge does, so
/// merges and ingest of the same launches queue instead of deadlocking).
async fn read_launches(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
    lock: bool,
) -> anyhow::Result<Vec<Row0>> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    let suffix = if lock { " for update" } else { "" };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{ROW_SELECT} where project_id = $1 and id = any($2) order by id{suffix}"
    )))
    .bind(project_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(row0).collect::<Result<_, _>>()?)
}

/// Serializes merges, undos and finalizations of a project without queueing: false when
/// another one holds the project.
async fn try_project_lock(conn: &mut PgConnection, project_id: Uuid) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended($1, 7720))")
            .bind(format!("fastproof-merge:{project_id}"))
            .fetch_one(&mut *conn)
            .await?,
    )
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

/// A source: id and creation time (the partition of its rows); its order is its position.
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

/// Attempts of the sources, counted up to `cap` (enough to compare with a limit).
async fn count_attempts(conn: &mut PgConnection, sources: &[Src], cap: i64) -> anyhow::Result<i64> {
    let (ids, ats, _) = src_arrays(sources);
    Ok(sqlx::query_scalar(
        "select count(*) from (select 1 from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join attempts a on a.launch_id = s.id and a.launch_created_at = s.at limit $3) x",
    )
    .bind(&ids)
    .bind(&ats)
    .bind(cap)
    .fetch_one(&mut *conn)
    .await?)
}

/// Series with results in the sources, with how many sources ran each, sorted.
async fn source_series(
    conn: &mut PgConnection,
    sources: &[Src],
) -> anyhow::Result<Vec<(Uuid, i32)>> {
    let (ids, ats, _) = src_arrays(sources);
    Ok(sqlx::query_as(
        "select r.series_id, count(*)::int from unnest($1::uuid[], $2::timestamptz[]) as s(id, at) \
         join results r on r.launch_id = s.id and r.launch_created_at = s.at group by 1 order by 1",
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

/// Log bytes the sources hold.
async fn source_log_bytes(conn: &mut PgConnection, ids: &[Uuid]) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar(
        "select coalesce(sum(bytes), 0)::bigint from launch_log_bytes where launch_id = any($1)",
    )
    .bind(ids)
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
/// attributes of all of them; trusted for code sync only when every source and the caller are.
async fn insert_target(
    conn: &mut PgConnection,
    ctx: &Ctx,
    sources: &[Row0],
    name: Option<&str>,
    caller: &MergeCaller,
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
    let untrusted = !caller.can_sync || sources.iter().any(Row0::untrusted);
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status, debug, created_at, started_at, finished_at, \
           ci, vcs, environment, attributes, branch, env_key, created_by, created_by_key, created_by_can_sync, \
           sync_blocked, important) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
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
    .bind(no_nul(&caller.name))
    .bind(no_nul(&caller.key))
    .bind(!untrusted)
    .bind(untrusted)
    .bind(sources.iter().any(|s| s.important))
    .execute(&mut *conn)
    .await?;
    Ok((Target { id, created_at }, number))
}

/// The status string of a series after a rebuild: its last `strip` characters (those of the
/// launches created since the merge's window began, as they were) are replaced by the results
/// of the launches counted now. Returns the string and the runs tracked.
fn rebuilt_statuses(old: Option<&(String, i32)>, strip: i64, tail: &[Status]) -> (String, i32) {
    let (old_s, old_runs) = old.map(|(s, n)| (s.as_str(), *n)).unwrap_or(("", 0));
    let chars: Vec<char> = old_s.chars().collect();
    let strip = (strip.max(0) as usize).min(chars.len());
    let mut out: Vec<char> = chars[..chars.len() - strip].to_vec();
    out.extend(tail.iter().map(|s| stats::encode(*s)));
    if out.len() > STATUS_CHARS {
        out.drain(..out.len() - STATUS_CHARS);
    }
    let runs = (old_runs as i64 - strip as i64 + tail.len() as i64).max(out.len() as i64);
    (out.into_iter().collect(), runs.min(i32::MAX as i64) as i32)
}

/// A counted result of a series: status, flaky, launch, launch creation time.
type TailRow = (Status, bool, Uuid, DateTime<Utc>);

/// An attempt of a target: series, id, status, start, order.
type AttemptRow = (Uuid, Uuid, String, Option<DateTime<Utc>>, i64);

/// Rebuilds the statistics of series (sorted, with the `delta` of their work rows: the
/// characters to strip are the counted results since `window` plus `delta`). Locks the rows in
/// order, after everything else of the transaction (ingest's order).
async fn rebuild_stats(
    conn: &mut PgConnection,
    ctx: &Ctx,
    window: DateTime<Utc>,
    work: &[(Uuid, i32)],
) -> anyhow::Result<()> {
    if work.is_empty() {
        return Ok(());
    }
    let series: Vec<Uuid> = work.iter().map(|w| w.0).collect();
    let flaky_window = ctx.settings.flaky.depth() as usize + 1;
    let olds: HashMap<Uuid, (String, i32)> = sqlx::query_as::<_, (Uuid, String, i32)>(
        "select series_id, last_statuses, runs_tracked from series_stats where series_id = any($1) \
         order by series_id for update",
    )
    .bind(&series)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(s, l, n)| (s, (l, n)))
    .collect();
    let rows: Vec<(Uuid, String, bool, Uuid, DateTime<Utc>)> = sqlx::query_as(
        "select r.series_id, r.status, r.flaky, r.launch_id, r.launch_created_at from results r \
         join launches l on l.id = r.launch_id \
         where r.series_id = any($1) and r.project_id = $2 and r.launch_created_at >= $3 and not l.debug \
         order by r.series_id, r.launch_created_at, l.number",
    )
    .bind(&series)
    .bind(ctx.id)
    .bind(window)
    .fetch_all(&mut *conn)
    .await?;
    let mut tails: HashMap<Uuid, Vec<TailRow>> = HashMap::new();
    for (s, st, fl, l, at) in rows {
        tails
            .entry(s)
            .or_default()
            .push((Status::parse_lenient(&st), fl, l, at));
    }
    let n = work.len();
    let (mut ids, mut strings, mut bits, mut runs) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let mut scores: Vec<Option<f32>> = Vec::with_capacity(n);
    let mut last: Vec<Option<String>> = Vec::with_capacity(n);
    let mut last_launch: Vec<Option<Uuid>> = Vec::with_capacity(n);
    let mut flaky = Vec::with_capacity(n);
    let mut seen_at: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    for (s, delta) in work {
        let tail = tails.remove(s).unwrap_or_default();
        let old = olds.get(s);
        if old.is_none() && tail.is_empty() {
            continue;
        }
        let statuses: Vec<Status> = tail.iter().map(|t| t.0).collect();
        let strip = tail.len() as i64 + *delta as i64;
        let (string, r) = rebuilt_statuses(old, strip, &statuses);
        let decoded = stats::decode(&string);
        let mut b: i64 = 0;
        for (i, st) in decoded.iter().rev().take(63).enumerate() {
            if st.is_failure() {
                b |= 1 << i;
            }
        }
        ids.push(*s);
        scores.push(stats::flaky_score(&decoded, flaky_window));
        strings.push(string);
        bits.push(b);
        runs.push(r);
        let latest = tail.last();
        last.push(latest.map(|t| t.0.as_str().to_string()));
        last_launch.push(latest.map(|t| t.2));
        flaky.push(latest.is_some_and(|t| t.1));
        seen_at.push(latest.map(|t| t.3));
    }
    if ids.is_empty() {
        return Ok(());
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
    for chunk in series.chunks(ANALYSIS_CHUNK * 20) {
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

/// Series of the target that had a result before a copy.
async fn target_series(
    conn: &mut PgConnection,
    t: Target,
    series: &[Uuid],
) -> anyhow::Result<HashSet<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "select series_id from results where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
    )
    .bind(t.id)
    .bind(t.created_at)
    .bind(series)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect())
}

/// Queues series for the second phase; an existing row keeps its delta (it still describes the
/// statistics as they are).
async fn queue_work(
    conn: &mut PgConnection,
    project_id: Uuid,
    merge_id: Uuid,
    work: &[(Uuid, i32)],
) -> anyhow::Result<()> {
    for chunk in work.chunks(10_000) {
        let ids: Vec<Uuid> = chunk.iter().map(|w| w.0).collect();
        let deltas: Vec<i32> = chunk.iter().map(|w| w.1).collect();
        sqlx::query(
            "insert into launch_merge_work (merge_id, project_id, series_id, delta) \
             select $1, $2, s, d from unnest($3::uuid[], $4::int[]) as t(s, d) \
             on conflict (merge_id, series_id) do nothing",
        )
        .bind(merge_id)
        .bind(project_id)
        .bind(&ids)
        .bind(&deltas)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// What a copy did.
struct Copied {
    counts: MergeCounts,
    /// Series whose attempts changed.
    affected: Vec<Uuid>,
}

/// Copies the attempts, containers, attachment links and log chunks of `sources` into the
/// target and recomputes its affected results. Copying again copies only what is new.
async fn copy_into(
    conn: &mut PgConnection,
    ctx: &Ctx,
    t: Target,
    sources: &[Src],
    series: &[Uuid],
    n: i64,
) -> anyhow::Result<Copied> {
    let mut counts = MergeCounts::default();
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

    let seq0: i64 = sqlx::query_scalar("select attempt_seq from launches where id = $1")
        .bind(t.id)
        .fetch_one(&mut *conn)
        .await?;
    let attempt_ids: Vec<Uuid> = (0..n).map(|_| Uuid::now_v7()).collect();
    let copied: Vec<(Uuid, Option<Uuid>, String)> = sqlx::query_as(COPY_ATTEMPTS_SQL)
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
    counts.duplicates = (n - counts.attempts).max(0);
    counts.rekeyed = copied
        .iter()
        .filter(|(_, from, key)| {
            from.is_some_and(|f| {
                let p = format!("m:{f}:");
                key.starts_with(&p) || key.strip_prefix("r:").is_some_and(|k| k.starts_with(&p))
            })
        })
        .count() as i64;
    let affected: Vec<Uuid> = copied
        .iter()
        .map(|c| c.0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

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
    // Log chunks follow their attempt: a rekeyed API result (`r:m:<source>:<id>`) gets its
    // lines under `m:<source>:<id>`, so chunks of two tests that shared an id stay apart.
    let log_bytes: i64 = sqlx::query_scalar(
        "with c0 as (select c.id, s.ord, c.attempt, c.first_seq, \
             case when exists (select 1 from attempts t where t.launch_id = $1 and t.launch_created_at = $4 \
                 and t.client_key = 'r:m:' || c.launch_id::text || ':' || c.result_key) \
               then 'm:' || c.launch_id::text || ':' || c.result_key else c.result_key end as rk \
           from unnest($2::uuid[], $3::int[]) as s(id, ord) join attempt_log_chunks c on c.launch_id = s.id), \
         pick as (select distinct on (rk, attempt, first_seq) id, rk from c0 order by rk, attempt, first_seq, ord), \
         ins as (insert into attempt_log_chunks (project_id, launch_id, result_key, attempt, first_seq, \
             last_seq, line_count, levels, bytes, first_ts, last_ts, lines_z, created_at, merged_from) \
           select c.project_id, $1, p.rk, c.attempt, c.first_seq, c.last_seq, c.line_count, c.levels, \
             c.bytes, c.first_ts, c.last_ts, c.lines_z, c.created_at, c.launch_id \
           from pick p join attempt_log_chunks c on c.id = p.id \
           where not exists (select 1 from attempt_log_chunks o where o.launch_id = $1 \
             and o.result_key = p.rk and o.attempt = c.attempt and o.first_seq = c.first_seq) \
           returning bytes) \
         select coalesce(sum(bytes), 0)::bigint from ins",
    )
    .bind(t.id)
    .bind(&ids)
    .bind(&ords)
    .bind(t.created_at)
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

    let kinds = if affected.is_empty() {
        HashMap::new()
    } else {
        source_kinds(conn, sources).await?
    };
    let kept = recompute_results(conn, t, &affected, &kinds).await?;
    counts.results = kept.len() as i64;
    crate::ingest::recount(conn, t.id, t.created_at, Some(seq0 + n)).await?;
    Ok(Copied { counts, affected })
}

/// Failure fingerprints first or last seen in the sources, as they were (restored on undo),
/// then pointed at the target: `last` always, `first` when the target is not newer.
async fn repoint_fingerprints(
    conn: &mut PgConnection,
    project_id: Uuid,
    sources: &[Src],
    t: Target,
) -> anyhow::Result<serde_json::Value> {
    let ids: Vec<Uuid> = sources.iter().map(|s| s.id).collect();
    let saved: Vec<(String, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "select fingerprint, first_launch_id, last_launch_id from failure_fingerprints \
         where project_id = $1 and (first_launch_id = any($2) or last_launch_id = any($2))",
    )
    .bind(project_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let older: Vec<Uuid> = sources
        .iter()
        .filter(|s| s.at >= t.created_at)
        .map(|s| s.id)
        .collect();
    sqlx::query(
        "update failure_fingerprints set \
           first_launch_id = case when first_launch_id = any($3) then $4 else first_launch_id end, \
           last_launch_id = case when last_launch_id = any($2) then $4 else last_launch_id end \
         where project_id = $1 and (first_launch_id = any($2) or last_launch_id = any($2))",
    )
    .bind(project_id)
    .bind(&ids)
    .bind(&older)
    .bind(t.id)
    .execute(&mut *conn)
    .await?;
    Ok(serde_json::Value::Array(
        saved
            .into_iter()
            .map(|(f, a, b)| serde_json::json!({"fp": f, "first": a, "last": b}))
            .collect(),
    ))
}

/// Puts back the fingerprints a merge pointed at its target.
async fn restore_fingerprints(
    conn: &mut PgConnection,
    project_id: Uuid,
    saved: Option<&serde_json::Value>,
    target: Uuid,
) -> anyhow::Result<()> {
    let Some(items) = saved.and_then(|v| v.as_array()) else {
        return Ok(());
    };
    let mut fps = Vec::new();
    let mut firsts: Vec<Option<Uuid>> = Vec::new();
    let mut lasts: Vec<Option<Uuid>> = Vec::new();
    for i in items {
        let Some(fp) = i.get("fp").and_then(|v| v.as_str()) else {
            continue;
        };
        fps.push(fp.to_string());
        firsts.push(i.get("first").and_then(|v| v.as_str()?.parse().ok()));
        lasts.push(i.get("last").and_then(|v| v.as_str()?.parse().ok()));
    }
    sqlx::query(
        "update failure_fingerprints f set \
           first_launch_id = case when f.first_launch_id = $5 then t.a else f.first_launch_id end, \
           last_launch_id = case when f.last_launch_id = $5 then t.b else f.last_launch_id end \
         from unnest($2::text[], $3::uuid[], $4::uuid[]) as t(fp, a, b) \
         where f.project_id = $1 and f.fingerprint = t.fp",
    )
    .bind(project_id)
    .bind(&fps)
    .bind(&firsts)
    .bind(&lasts)
    .bind(target)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Highest log chunk id per launch (0 without chunks), in the order of `ids`.
async fn log_marks(conn: &mut PgConnection, ids: &[Uuid]) -> anyhow::Result<Vec<i64>> {
    let rows: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "select s, coalesce((select max(c.id) from attempt_log_chunks c where c.launch_id = s), 0) \
         from unnest($1::uuid[]) as s",
    )
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    Ok(ids
        .iter()
        .map(|i| rows.get(i).copied().unwrap_or(0))
        .collect())
}

/// A stored merge.
#[derive(Clone, Debug)]
struct MergeRow {
    id: Uuid,
    target_id: Uuid,
    target_created: bool,
    sources: Vec<Uuid>,
    source_seqs: Vec<i64>,
    log_marks: Option<Vec<i64>>,
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
    fingerprints: Option<serde_json::Value>,
    window_start: Option<DateTime<Utc>>,
}

const MERGE_SELECT: &str = "select id, target_id, target_created, sources, source_seqs, log_marks, debug, status, \
    final, counts, created_by, created_at, undo_until, undone_at, undone_by, finalized_at, \
    fingerprints, window_start from launch_merges";

fn merge_row(r: &PgRow) -> Result<MergeRow, sqlx::Error> {
    Ok(MergeRow {
        id: r.try_get("id")?,
        target_id: r.try_get("target_id")?,
        target_created: r.try_get("target_created")?,
        sources: r.try_get("sources")?,
        source_seqs: r.try_get("source_seqs")?,
        log_marks: r.try_get("log_marks")?,
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
        fingerprints: r.try_get("fingerprints")?,
        window_start: r.try_get("window_start")?,
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

/// Copies what reached the sources of a merge since it was made or last synced (attempts, or
/// log lines only), queues the affected series and, for a finalized merge, deletes the copied
/// rows from the tombstones again. Returns whether anything was copied.
async fn sync_in(conn: &mut PgConnection, ctx: &Ctx, m: &MergeRow) -> anyhow::Result<bool> {
    let mut ids = m.sources.clone();
    ids.push(m.target_id);
    let rows = read_launches(conn, ctx.id, &ids, true).await?;
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
    let marks: HashMap<Uuid, i64> = m
        .sources
        .iter()
        .copied()
        .zip(m.log_marks.clone().unwrap_or_default())
        .collect();
    let mut sources: Vec<&Row0> = rows
        .iter()
        .filter(|r| r.id != m.target_id && m.sources.contains(&r.id))
        .collect();
    sources.sort_by_key(|r| (r.created_at, r.number));
    let src_ids: Vec<Uuid> = sources.iter().map(|r| r.id).collect();
    let now_marks = log_marks(conn, &src_ids).await?;
    let changed = sources.iter().zip(now_marks.iter()).any(|(r, mark)| {
        r.attempt_seq > seqs.get(&r.id).copied().unwrap_or(i64::MAX)
            || *mark > marks.get(&r.id).copied().unwrap_or(0)
    });
    if !changed {
        return Ok(false);
    }
    let srcs: Vec<Src> = sources.iter().map(|r| r.src()).collect();
    let series: Vec<Uuid> = source_series(conn, &srcs)
        .await?
        .into_iter()
        .map(|s| s.0)
        .collect();
    let had = target_series(conn, t, &series).await?;
    let n = count_attempts(conn, &srcs, i64::MAX).await?;
    let late = copy_into(conn, ctx, t, &srcs, &series, n).await?;
    let work: Vec<(Uuid, i32)> = late
        .affected
        .iter()
        .map(|s| (*s, if had.contains(s) { 0 } else { -1 }))
        .collect();
    if !m.debug {
        queue_work(conn, ctx.id, m.id, &work).await?;
    }
    if m.status == "final" {
        tombstone(conn, &srcs, t).await?;
    }
    let all_marks: Vec<i64> = m
        .sources
        .iter()
        .map(|id| {
            src_ids
                .iter()
                .position(|s| s == id)
                .map(|i| now_marks[i])
                .unwrap_or(0)
        })
        .collect();
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
        "update launch_merges set source_seqs = $2, log_marks = $3, synced_at = now(), counts = $4 where id = $1",
    )
    .bind(m.id)
    .bind(&now_seqs)
    .bind(&all_marks)
    .bind(add_counts(&m.counts, &late.counts))
    .execute(&mut *conn)
    .await?;
    crate::events::notify_launch(&mut *conn, &ctx.key, t.id).await?;
    Ok(true)
}

/// Deletes what the sources of a merge hold except the launch rows, their uploads and their
/// attachment links; their attachment references move to the target (copied attempts may name
/// them), unless the target has the same variant already (then they stay on the tombstone).
async fn tombstone(conn: &mut PgConnection, sources: &[Src], t: Target) -> anyhow::Result<()> {
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
    sqlx::query(
        "update attachment_refs r set launch_id = $2 \
         from (select distinct on (attachment_id, content_type, coalesce(original_name, '')) id \
           from attachment_refs where launch_id = any($1) \
           order by attachment_id, content_type, coalesce(original_name, ''), id) m \
         where r.id = m.id and not exists (select 1 from attachment_refs o where o.launch_id = $2 \
           and o.attachment_id = r.attachment_id and o.content_type = r.content_type \
           and coalesce(o.original_name, '') = coalesce(r.original_name, ''))",
    )
    .bind(&ids)
    .bind(t.id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Second phase of a merge, sync or undo: one chunk of queued series in a short transaction.
/// The target (when it exists) is analysed as after an upload, with its statistics push
/// suppressed (it is marked debug inside the transaction), then the statistics are rebuilt.
/// Returns the series handled (0: nothing left).
async fn work_chunk(conn: &mut PgConnection, ctx: &Ctx, m: &MergeRow) -> anyhow::Result<usize> {
    sqlx::query(LOCK_TIMEOUT).execute(&mut *conn).await?;
    let work: Vec<(Uuid, i32)> = sqlx::query_as(
        "select series_id, delta from launch_merge_work where merge_id = $1 order by series_id \
         limit $2 for update skip locked",
    )
    .bind(m.id)
    .bind(ANALYSIS_CHUNK as i64)
    .fetch_all(&mut *conn)
    .await?;
    if work.is_empty() {
        return Ok(0);
    }
    let series: Vec<Uuid> = work.iter().map(|w| w.0).collect();
    let target: Option<(DateTime<Utc>, bool)> = sqlx::query_as(
        "select created_at, debug from launches where id = $1 and project_id = $2 and merged_into is null for update",
    )
    .bind(m.target_id)
    .bind(ctx.id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((created_at, debug)) = target {
        let t = Target {
            id: m.target_id,
            created_at,
        };
        let present: Vec<Uuid> = {
            let mut v: Vec<Uuid> = target_series(conn, t, &series).await?.into_iter().collect();
            v.sort();
            v
        };
        if !present.is_empty() {
            if !debug {
                sqlx::query("update launches set debug = true where id = $1")
                    .bind(t.id)
                    .execute(&mut *conn)
                    .await?;
            }
            let lref = LaunchRef {
                id: t.id,
                project_id: ctx.id,
                project_key: ctx.key.clone(),
                created_at: t.created_at,
            };
            crate::analysis::after_persist(&mut *conn, &lref, &[], &present).await?;
            if !debug {
                sqlx::query("update launches set debug = false where id = $1")
                    .bind(t.id)
                    .execute(&mut *conn)
                    .await?;
            }
        }
    }
    if !m.debug {
        let window = m.window_start.unwrap_or(m.created_at);
        rebuild_stats(conn, ctx, window, &work).await?;
    }
    sqlx::query("delete from launch_merge_work where merge_id = $1 and series_id = any($2)")
        .bind(m.id)
        .bind(&series)
        .execute(&mut *conn)
        .await?;
    Ok(work.len())
}

impl Store {
    /// Merges launches (see the module documentation). Returns the merge and whether this call
    /// made it (`false`: the same merge exists and only what reached the sources since was
    /// copied, or a preview).
    pub async fn merge_launches(
        &self,
        project: &Project,
        req: &LaunchMergeRequest,
        caller: &MergeCaller,
        opts: MergeOptions,
    ) -> Result<(LaunchMerge, bool), MergeError> {
        validate(req)?;
        if req.dry_run {
            let mut conn = self.pool.acquire().await.map_err(anyhow::Error::from)?;
            let c = ctx(&mut conn, project, self.purge_batch).await?;
            return preview(&mut conn, &c, req, opts).await.map(|p| (p, false));
        }
        let mut attempt = 0;
        let (id, created) = loop {
            attempt += 1;
            let r = async {
                let mut tx = self.pool.begin().await?;
                let c = ctx(&mut tx, project, self.purge_batch).await?;
                match merge_in(&mut tx, &c, req, caller, opts).await {
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
                Err(MergeError::Other(e)) if attempt < TX_ATTEMPTS && merge_retryable(&e) => {
                    retry_pause("merge", &e, attempt).await
                }
                Err(e) => return Err(e),
                Ok(v) => break v,
            }
        };
        // The second phase runs here; what it does not finish, the job does.
        if let Err(e) = self.drain_merge(project.id, id, REQUEST_BUDGET).await {
            tracing::warn!(merge = %id, error = %format!("{e:#}"), "merge analysis left to the job");
        }
        let m = self
            .merge(project.id, id)
            .await?
            .ok_or(MergeError::NotFound("merge"))?;
        Ok((m, created))
    }

    /// Runs the second phase of a merge, chunk by chunk (each retried on deadlocks and lock
    /// timeouts), until it is done or `budget` has passed (the job goes on later). Returns the
    /// series handled.
    pub async fn drain_merge(
        &self,
        project_id: Uuid,
        id: Uuid,
        budget: std::time::Duration,
    ) -> anyhow::Result<usize> {
        let started = std::time::Instant::now();
        let Some(project) = self.project_by_id(project_id).await? else {
            return Ok(0);
        };
        let mut total = 0;
        loop {
            let mut attempt = 0;
            let n = loop {
                attempt += 1;
                let r = async {
                    let mut tx = self.pool.begin().await?;
                    let c = ctx(&mut tx, &project, self.purge_batch).await?;
                    let m = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "{MERGE_SELECT} where project_id = $1 and id = $2"
                    )))
                    .bind(project_id)
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    let Some(m) = m.as_ref().map(merge_row).transpose()? else {
                        return Ok::<_, anyhow::Error>(0);
                    };
                    let n = work_chunk(&mut tx, &c, &m).await?;
                    tx.commit().await?;
                    Ok(n)
                }
                .await;
                match r {
                    Err(e) if attempt < TX_ATTEMPTS && merge_retryable(&e) => {
                        retry_pause("merge_analysis", &e, attempt).await
                    }
                    r => break r?,
                }
            };
            total += n;
            if n == 0 || started.elapsed() > budget {
                return Ok(total);
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

    /// Who made a merge (its stable caller key), for the permission check of an undo.
    pub async fn merge_owner(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "select created_by_key from launch_merges where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
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

    /// The stable creator keys of these launches of the project (missing ids are left out;
    /// launches created before keys were recorded have none).
    pub async fn launch_owner_keys(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, Option<String>>> {
        Ok(sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select id, created_by_key from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    /// Records the stable key of the caller that created a launch.
    pub async fn set_launch_owner_key(&self, launch_id: Uuid, key: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update launches set created_by_key = $2 where id = $1 and created_by_key is null",
        )
        .bind(launch_id)
        .bind(no_nul(key))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether any of these results of the project belongs to a launch merged into another.
    pub async fn results_in_merged_launches(
        &self,
        project_id: Uuid,
        result_ids: &[Uuid],
    ) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar(
            "select exists (select 1 from results r join launches l on l.id = r.launch_id \
             where r.project_id = $1 and r.id = any($2) and l.merged_into is not null)",
        )
        .bind(project_id)
        .bind(result_ids)
        .fetch_one(&self.pool)
        .await?)
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
        let merge_ids: Vec<Uuid> = rows.iter().map(|m| m.id).collect();
        let pending: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "select distinct merge_id from launch_merge_work where merge_id = any($1)",
        )
        .bind(&merge_ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
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
                analysing: pending.contains(&m.id),
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
    /// must hold nothing but the merge, and a triage done on it needs `results:triage`), or
    /// the copied attempts leave the existing target; the sources come back as they were.
    pub async fn undo_merge(
        &self,
        project: &Project,
        id: Uuid,
        caller: &MergeCaller,
    ) -> Result<LaunchMerge, MergeError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let r = async {
                let mut tx = self.pool.begin().await?;
                let c = ctx(&mut tx, project, self.purge_batch).await?;
                match undo_in(&mut tx, &c, id, caller).await {
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
                Err(MergeError::Other(e)) if attempt < TX_ATTEMPTS && merge_retryable(&e) => {
                    retry_pause("merge_undo", &e, attempt).await
                }
                Err(e) => return Err(e),
                Ok(()) => {
                    if let Err(e) = self.drain_merge(project.id, id, REQUEST_BUDGET).await {
                        tracing::warn!(merge = %id, error = %format!("{e:#}"), "undo analysis left to the job");
                    }
                    return self
                        .merge(project.id, id)
                        .await?
                        .ok_or(MergeError::NotFound("merge"));
                }
            }
        }
    }

    async fn with_retry<F, Fut>(&self, path: &'static str, f: F) -> anyhow::Result<bool>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<bool>>,
    {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match f().await {
                Err(e) if attempt < TX_ATTEMPTS && merge_retryable(&e) => {
                    retry_pause(path, &e, attempt).await
                }
                r => return r,
            }
        }
    }

    /// Copies late attempts and log lines of a merge into its target. Returns whether anything
    /// was copied (false also when another merge operation holds the project).
    pub async fn sync_merge(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let Some(project) = self.project_by_id(project_id).await? else {
            return Ok(false);
        };
        let done = self
            .with_retry("merge_sync", || async {
                let mut tx = self.pool.begin().await?;
                sqlx::query(LOCK_TIMEOUT).execute(&mut *tx).await?;
                if !try_project_lock(&mut tx, project_id).await? {
                    return Ok(false);
                }
                let c = ctx(&mut tx, &project, self.purge_batch).await?;
                let Some(m) = merge_for_update(&mut tx, project_id, id).await? else {
                    return Ok(false);
                };
                if !matches!(m.status.as_str(), "active" | "final") {
                    return Ok(false);
                }
                let done = sync_in(&mut tx, &c, &m).await?;
                tx.commit().await?;
                Ok(done)
            })
            .await?;
        if done {
            self.drain_merge(project_id, id, REQUEST_BUDGET).await?;
        }
        Ok(done)
    }

    /// Ends the undo window of a merge: late attempts are copied, then the sources become
    /// tombstones. Returns false when the merge is not active (or the project is busy).
    pub async fn finalize_merge(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<bool> {
        let Some(project) = self.project_by_id(project_id).await? else {
            return Ok(false);
        };
        let done = self
            .with_retry("merge_finalize", || async {
                let mut tx = self.pool.begin().await?;
                sqlx::query(LOCK_TIMEOUT).execute(&mut *tx).await?;
                if !try_project_lock(&mut tx, project_id).await? {
                    return Ok(false);
                }
                let c = ctx(&mut tx, &project, self.purge_batch).await?;
                let Some(m) = merge_for_update(&mut tx, project_id, id).await? else {
                    return Ok(false);
                };
                if m.status != "active" {
                    return Ok(false);
                }
                sync_in(&mut tx, &c, &m).await?;
                let rows = read_launches(&mut tx, project_id, &m.sources, true).await?;
                let srcs: Vec<Src> = rows.iter().map(Row0::src).collect();
                let target: Option<DateTime<Utc>> =
                    sqlx::query_scalar("select created_at from launches where id = $1")
                        .bind(m.target_id)
                        .fetch_optional(&mut *tx)
                        .await?;
                if let Some(created_at) = target {
                    tombstone(
                        &mut tx,
                        &srcs,
                        Target {
                            id: m.target_id,
                            created_at,
                        },
                    )
                    .await?;
                }
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
            })
            .await?;
        if done {
            self.drain_merge(project_id, id, REQUEST_BUDGET).await?;
        }
        Ok(done)
    }

    /// The merge job: finishes analyses left over, copies late attempts and log lines of
    /// active (and recently finalized) merges, finalizes merges whose undo window ended and
    /// deletes hidden launches whose target is gone. Each merge is isolated: a failure is
    /// logged and counted (`fastproof_merge_job_errors_total`) and the job goes on. Merges of
    /// deleted projects are left to the project purge.
    pub async fn merge_maintenance(&self) -> anyhow::Result<MergeJobReport> {
        let mut report = MergeJobReport::default();
        let fail = |what: &'static str, id: Uuid, e: anyhow::Error, r: &mut MergeJobReport| {
            metrics::counter!("fastproof_merge_job_errors_total", "step" => what).increment(1);
            tracing::warn!(merge = %id, step = what, error = %format!("{e:#}"), "merge job step failed");
            r.errors += 1;
        };
        let pending: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select w.project_id, w.merge_id from (select distinct project_id, merge_id from launch_merge_work) w \
             join projects p on p.id = w.project_id and p.deleted_at is null limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (p, id) in pending {
            match self.drain_merge(p, id, JOB_BUDGET).await {
                Ok(n) if n > 0 => report.analysed += 1,
                Ok(_) => {}
                Err(e) => fail("analyse", id, e, &mut report),
            }
        }
        let late: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select m.project_id, m.id from launch_merges m join projects p on p.id = m.project_id \
             where p.deleted_at is null and (m.status = 'active' or (m.status = 'final' \
               and m.finalized_at > now() - make_interval(days => $2))) \
               and exists (select 1 from launches t where t.id = m.target_id) \
               and exists (select 1 from unnest(m.sources, m.source_seqs, \
                   coalesce(m.log_marks, array_fill(0::bigint, array[cardinality(m.sources)]))) as s(id, seq, mark) \
                 join launches l on l.id = s.id \
                 where l.attempt_seq > s.seq or exists (select 1 from attempt_log_chunks c \
                   where c.launch_id = s.id and c.id > s.mark)) \
             order by m.created_at limit $1",
        )
        .bind(JOB_BATCH)
        .bind(LATE_DAYS)
        .fetch_all(&self.pool)
        .await?;
        for (p, id) in late {
            match self.sync_merge(p, id).await {
                Ok(true) => report.synced += 1,
                Ok(false) => {}
                Err(e) => fail("sync", id, e, &mut report),
            }
        }
        let due: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "select m.project_id, m.id from launch_merges m join projects p on p.id = m.project_id \
             where p.deleted_at is null and m.status = 'active' and m.undo_until <= now() \
             order by m.undo_until limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (p, id) in due {
            match self.finalize_merge(p, id).await {
                Ok(true) => report.finalized += 1,
                Ok(false) => {}
                Err(e) => fail("finalize", id, e, &mut report),
            }
        }
        let orphans: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "select l.id, l.project_id, p.key from launches l join projects p on p.id = l.project_id \
             where p.deleted_at is null and l.merged_into is not null \
               and not exists (select 1 from launches t where t.id = l.merged_into) \
             order by l.id limit $1",
        )
        .bind(JOB_BATCH)
        .fetch_all(&self.pool)
        .await?;
        for (id, project_id, key) in orphans {
            match self.delete_orphan(id, project_id, &key).await {
                Ok(true) => report.orphans_deleted += 1,
                Ok(false) => {}
                Err(e) => fail("orphan", id, e, &mut report),
            }
        }
        Ok(report)
    }

    async fn delete_orphan(&self, id: Uuid, project_id: Uuid, key: &str) -> anyhow::Result<bool> {
        self.with_retry("merge_orphan", || async {
            let mut tx = self.pool.begin().await?;
            sqlx::query(LOCK_TIMEOUT).execute(&mut *tx).await?;
            if !try_project_lock(&mut tx, project_id).await? {
                return Ok(false);
            }
            let gone: Option<Uuid> = sqlx::query_scalar(
                "select l.id from launches l where l.id = $1 and l.merged_into is not null \
                   and not exists (select 1 from launches t where t.id = l.merged_into) for update",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            if gone.is_none() {
                return Ok(false);
            }
            crate::purge::Purge::new(&mut tx, self.purge_batch, crate::purge::Mode::Atomic)
                .launches(&[id])
                .await?;
            sqlx::query(
                "update launch_merges set status = 'deleted', finalized_at = coalesce(finalized_at, now()) \
                 where project_id = $1 and sources @> array[$2]::uuid[] and status in ('active', 'final')",
            )
            .bind(project_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "delete from launch_merge_work w using launch_merges m where w.merge_id = m.id \
                 and m.project_id = $1 and m.sources @> array[$2]::uuid[] and m.status = 'deleted' \
                 and not exists (select 1 from launches t where t.id = m.target_id)",
            )
            .bind(project_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            crate::events::emit(
                &mut tx,
                "launch.deleted",
                Some(key),
                serde_json::json!({"id": id, "launchId": id, "projectId": project_id}),
            )
            .await?;
            tx.commit().await?;
            Ok(true)
        })
        .await
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

/// What [`merge_in`] decided: the merge and whether this call made it.
type Outcome = (Uuid, bool);

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

/// The checks of a merge that do not change anything (shared by the preview).
fn check_rows(rows: &[Row0], sources: &[Row0]) -> Result<(), MergeError> {
    for r in rows {
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
        if r.test_plan_id.is_some() {
            return Err(conflict(
                "launch_plan",
                format!(
                    "launch #{} was started from a test plan; such launches are not merged",
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
    Ok(())
}

async fn check_busy_and_size(
    conn: &mut PgConnection,
    rows: &[Row0],
    sources: &[Row0],
    opts: MergeOptions,
) -> Result<i64, MergeError> {
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
    let srcs: Vec<Src> = sources.iter().map(Row0::src).collect();
    let n = count_attempts(conn, &srcs, opts.max_attempts + 1).await?;
    if n > opts.max_attempts {
        return Err(MergeError::TooLarge {
            what: "attempts",
            value: n,
            limit: opts.max_attempts,
        });
    }
    let all_ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let bytes = source_log_bytes(conn, &all_ids).await?;
    if bytes > opts.max_log_bytes {
        return Err(MergeError::TooLarge {
            what: "logBytes",
            value: bytes,
            limit: opts.max_log_bytes,
        });
    }
    Ok(n)
}

fn split(rows: &[Row0], into: Option<Uuid>) -> (Option<Row0>, Vec<Row0>) {
    let target = into.and_then(|t| rows.iter().find(|r| r.id == t)).cloned();
    let mut sources: Vec<Row0> = rows
        .iter()
        .filter(|r| Some(r.id) != into)
        .cloned()
        .collect();
    sources.sort_by_key(|r| (r.created_at, r.number));
    (target, sources)
}

/// A dry run: plain reads, no locks, attempts counted up to the limit.
async fn preview(
    conn: &mut PgConnection,
    ctx: &Ctx,
    req: &LaunchMergeRequest,
    opts: MergeOptions,
) -> Result<LaunchMerge, MergeError> {
    let rows = read_launches(conn, ctx.id, &req.launch_ids, false).await?;
    if rows.len() != req.launch_ids.len() {
        return Err(MergeError::NotFound("launch"));
    }
    let (target, sources) = split(&rows, req.into);
    check_rows(&rows, &sources)?;
    let n = check_busy_and_size(conn, &rows, &sources, opts).await?;
    let all: Vec<Src> = rows.iter().map(Row0::src).collect();
    let srcs: Vec<Src> = sources.iter().map(Row0::src).collect();
    let shared = shared_series(conn, &all).await?;
    let series = source_series(conn, &srcs).await?;
    Ok(LaunchMerge {
        id: None,
        target_id: target.as_ref().map(|t| t.id),
        target_number: target.as_ref().map(|t| t.number),
        created_target: target.is_none(),
        sources: sources
            .iter()
            .map(|r| MergeSource {
                id: r.id,
                number: Some(r.number),
                name: Some(r.name.clone()),
            })
            .collect(),
        status: "preview".into(),
        final_merge: req.final_merge,
        analysing: false,
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
    })
}

async fn merge_in(
    conn: &mut PgConnection,
    ctx: &Ctx,
    req: &LaunchMergeRequest,
    caller: &MergeCaller,
    opts: MergeOptions,
) -> Result<Outcome, MergeError> {
    sqlx::query(LOCK_TIMEOUT).execute(&mut *conn).await?;
    if !try_project_lock(conn, ctx.id).await? {
        return Err(busy());
    }
    let rows = read_launches(conn, ctx.id, &req.launch_ids, true).await?;
    if rows.len() != req.launch_ids.len() {
        return Err(MergeError::NotFound("launch"));
    }
    let (target_row, sources) = split(&rows, req.into);

    if let Some(m) = existing_merge(conn, ctx.id, &sources, req.into).await? {
        sync_in(conn, ctx, &m).await?;
        return Ok((m.id, false));
    }
    check_rows(&rows, &sources)?;
    if let Some(t) = &target_row
        && t.status != "running"
        && !caller.can_sync
        && t.created_by_key.as_deref() != Some(caller.key.as_str())
    {
        return Err(conflict(
            "launch_finished",
            "the launch to merge into is finished: only its creator or a caller with cases:sync may add to it",
            serde_json::json!({"launchId": t.id}),
        ));
    }
    let n = check_busy_and_size(conn, &rows, &sources, opts).await?;
    let all: Vec<Src> = rows.iter().map(Row0::src).collect();
    let srcs: Vec<Src> = sources.iter().map(Row0::src).collect();
    let shared = shared_series(conn, &all).await?;
    let src_ids: Vec<Uuid> = sources.iter().map(|r| r.id).collect();

    let (target, number) = match &target_row {
        Some(t) => {
            let untrusted = !caller.can_sync || sources.iter().any(Row0::untrusted);
            if untrusted {
                sqlx::query("update launches set sync_blocked = true where id = $1")
                    .bind(t.id)
                    .execute(&mut *conn)
                    .await?;
            }
            (
                Target {
                    id: t.id,
                    created_at: t.created_at,
                },
                t.number,
            )
        }
        None => insert_target(conn, ctx, &sources, req.name.as_deref(), caller).await?,
    };
    let series_counts = source_series(conn, &srcs).await?;
    let series: Vec<Uuid> = series_counts.iter().map(|s| s.0).collect();
    let had = target_series(conn, target, &series).await?;
    let marks = log_marks(conn, &src_ids).await?;
    let mut copied = copy_into(conn, ctx, target, &srcs, &series, n).await?;
    copied.counts.shared_series = shared;

    sqlx::query(
        "update launches set merged_into = $2, debug = true, updated_at = now() where id = any($1)",
    )
    .bind(&src_ids)
    .bind(target.id)
    .execute(&mut *conn)
    .await?;
    let fingerprints = repoint_fingerprints(conn, ctx.id, &srcs, target).await?;
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
        Utc::now() + FINAL_GRACE
    } else {
        Utc::now() + opts.undo_window
    };
    let window = srcs
        .iter()
        .map(|s| s.at)
        .chain([target.created_at])
        .min()
        .unwrap_or(target.created_at);
    let seqs: Vec<i64> = sources.iter().map(|r| r.attempt_seq).collect();
    let debug = rows[0].debug;
    sqlx::query(
        "insert into launch_merges (id, project_id, target_id, target_created, sources, source_seqs, log_marks, \
           debug, final, counts, created_by, created_by_key, undo_until, fingerprints, window_start) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(id)
    .bind(ctx.id)
    .bind(target.id)
    .bind(target_row.is_none())
    .bind(&src_ids)
    .bind(&seqs)
    .bind(&marks)
    .bind(debug)
    .bind(req.final_merge)
    .bind(serde_json::to_value(&copied.counts).map_err(anyhow::Error::from)?)
    .bind(no_nul(&caller.name))
    .bind(no_nul(&caller.key))
    .bind(undo_until)
    .bind(fingerprints)
    .bind(window)
    .execute(&mut *conn)
    .await?;
    if !debug {
        // The statistics hold a character per source that ran the test, and one of the target
        // if it ran the test before: they make way for the counted results.
        let work: Vec<(Uuid, i32)> = series_counts
            .iter()
            .map(|(s, c)| (*s, *c - if had.contains(s) { 0 } else { 1 }))
            .collect();
        queue_work(conn, ctx.id, id, &work).await?;
    } else {
        let work: Vec<(Uuid, i32)> = series.iter().map(|s| (*s, 0)).collect();
        queue_work(conn, ctx.id, id, &work).await?;
    }
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
    Ok((id, true))
}

async fn undo_in(
    conn: &mut PgConnection,
    ctx: &Ctx,
    id: Uuid,
    caller: &MergeCaller,
) -> Result<(), MergeError> {
    sqlx::query(LOCK_TIMEOUT).execute(&mut *conn).await?;
    if !try_project_lock(conn, ctx.id).await? {
        return Err(busy());
    }
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
    let rows = read_launches(conn, ctx.id, &ids, true).await?;
    let Some(t) = rows.iter().find(|r| r.id == m.target_id).cloned() else {
        return Err(conflict(
            "merge_target_gone",
            "the merged launch was deleted",
            serde_json::json!({}),
        ));
    };
    if rows.len() != m.sources.len() + 1 {
        let missing: Vec<Uuid> = m
            .sources
            .iter()
            .filter(|s| !rows.iter().any(|r| r.id == **s))
            .copied()
            .collect();
        return Err(conflict(
            "merge_source_gone",
            "a merged launch was deleted; the merge cannot be undone",
            serde_json::json!({"launchIds": missing}),
        ));
    }
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
    let srcs: Vec<Src> = sources.iter().map(|r| r.src()).collect();
    let src_ids: Vec<Uuid> = srcs.iter().map(|s| s.id).collect();
    if m.target_created {
        let own: bool = sqlx::query_scalar(
            "select exists (select 1 from attempts where launch_id = $1 and launch_created_at = $2 \
               and merged_from is null) or exists (select 1 from uploads where launch_id = $1) \
               or exists (select 1 from attempt_log_chunks where launch_id = $1 and merged_from is null)",
        )
        .bind(t.id)
        .bind(t.created_at)
        .fetch_one(&mut *conn)
        .await?;
        if own {
            return Err(conflict(
                "merge_target_changed",
                "results or logs were added to the merged launch after the merge; delete it instead",
                serde_json::json!({"targetId": t.id}),
            ));
        }
        let triaged: bool = sqlx::query_scalar(
            "select exists (select 1 from results where launch_id = $1 and launch_created_at = $2 \
               and defect ->> 'source' in ('user', 'ai') and (defect ->> 'at')::timestamptz > $3)",
        )
        .bind(t.id)
        .bind(t.created_at)
        .bind(m.created_at)
        .fetch_one(&mut *conn)
        .await?;
        if triaged && !caller.can_triage {
            return Err(MergeError::Forbidden {
                code: "merge_target_triaged",
                message: "results of the merged launch were triaged after the merge; undoing it \
                    deletes that work and needs results:triage"
                    .into(),
            });
        }
    }
    let series_counts = source_series(conn, &srcs).await?;
    let pending: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "select series_id from launch_merge_work where merge_id = $1",
    )
    .bind(m.id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    sqlx::query(
        "update launches set merged_into = null, debug = $2, rolled_up_at = case when $2 then rolled_up_at end, \
           updated_at = now() where id = any($1)",
    )
    .bind(&src_ids)
    .bind(m.debug)
    .execute(&mut *conn)
    .await?;
    restore_fingerprints(conn, ctx.id, m.fingerprints.as_ref(), t.id).await?;
    let mut work: Vec<(Uuid, i32)> = Vec::with_capacity(series_counts.len());
    if m.target_created {
        crate::purge::Purge::new(&mut *conn, ctx.purge_batch, crate::purge::Mode::Atomic)
            .launches(&[t.id])
            .await?;
        crate::events::emit(
            &mut *conn,
            "launch.deleted",
            Some(&ctx.key),
            serde_json::json!({"id": t.id, "launchId": t.id, "projectId": ctx.id}),
        )
        .await?;
        for (s, c) in &series_counts {
            work.push((*s, if pending.contains(s) { 0 } else { 1 - c }));
        }
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
        // A person's decision the merge copied from a source result leaves with it.
        let (ids, ats, _) = src_arrays(&srcs);
        sqlx::query(
            "update results r set defect = null, updated_at = now() \
             from unnest($3::uuid[], $4::timestamptz[]) as s(id, at) \
             join results sr on sr.launch_id = s.id and sr.launch_created_at = s.at \
             where r.launch_id = $1 and r.launch_created_at = $2 and r.series_id = sr.series_id \
               and r.series_id = any($5) and r.defect ->> 'source' in ('user', 'ai') and r.defect = sr.defect",
        )
        .bind(t.id)
        .bind(t.created_at)
        .bind(&ids)
        .bind(&ats)
        .bind(&affected)
        .execute(&mut *conn)
        .await?;
        let kept: HashSet<Uuid> = recompute_results(conn, target, &affected, &HashMap::new())
            .await?
            .into_iter()
            .collect();
        crate::ingest::recount(conn, t.id, t.created_at, None).await?;
        crate::events::notify_launch(&mut *conn, &ctx.key, t.id).await?;
        for (s, c) in &series_counts {
            let lost = affected.contains(s) && !kept.contains(s);
            work.push((
                *s,
                if pending.contains(s) {
                    0
                } else {
                    lost as i32 - c
                },
            ));
        }
    }
    sqlx::query("update launch_merge_work set delta = 0 where merge_id = $1")
        .bind(m.id)
        .execute(&mut *conn)
        .await?;
    queue_work(conn, ctx.id, m.id, &work).await?;
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
    .bind(no_nul(&caller.name))
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
        assert_eq!((s.as_str(), n), ("", 5));
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
