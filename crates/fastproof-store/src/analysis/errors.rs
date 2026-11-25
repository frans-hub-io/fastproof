//! Unique errors (contract `76` §7.4): the `errors` step of a finished launch (branches,
//! states, commits) under the project's errors lock, aliases, the list and the detail of the
//! errors page, the PostgreSQL timeline, the `newErrors` gate count and the throttled
//! `errorRegressed` deliveries.
//!
//! An error is a canonical fingerprint: the catalog row's `alias_of` when set (one level),
//! itself otherwise. `fingerprint_branches` keeps one row per raw fingerprint and branch (the
//! newest [`TRACKED_BRANCHES`] branches of the project and the gate baselines by name, the rest
//! as [`OTHER_BRANCH`]); its `occurrences` counts launches. States are computed by
//! [`fastproof_analysis::lifecycle::state`] from facts that depend only on the set of finished
//! launches, so analysing launches out of order gives the same states.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use fastproof_analysis::lifecycle::{self, BranchCount, ErrorFacts};
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::types::{
    AnalysisResult, AnalyticsBackend, AnalyticsWindow, CommitRange, DecisionSample, ErrorBranch,
    ErrorDetail, ErrorIssue, ErrorIssueState, ErrorList, ErrorPatch, ErrorPatchState, ErrorQuery,
    ErrorSort, ErrorState, ErrorStateChange, ErrorSummary, ErrorTest, ErrorTimelinePoint,
    LaunchCtx, MemoryEntry, NEW_ERRORS_DAYS, OTHER_BRANCH, TRACKED_BRANCHES,
};
use crate::tm::TmError;
use crate::{Launch, Store, no_nul};

/// Class of the per-project advisory lock of the errors step (`pg_advisory_xact_lock(class,
/// hashtext(project id))`); patches of errors take it too.
pub const ERRORS_LOCK: i32 = 0x4650_4552;
/// Newest launches of a project the tracked branches are taken from.
pub const TRACKED_SCAN: i64 = 5000;
/// Errors re-checked by one step beyond the launch's own (absent ones, out-of-order ones).
pub const MAX_RECHECK: i64 = 5000;
/// Launches counted per branch in `launchesSince`, at most.
pub const LAUNCHES_SINCE_CAP: i64 = 1000;
/// Days of the sparkline of the errors list.
pub const SPARK_DAYS: i64 = 14;
/// Days the `tests` count of the list and the test list of the detail look back.
pub const TESTS_DAYS: i64 = 30;
/// Results read per fingerprint for the sparkline, the tests and the test list, newest first,
/// at most.
pub const ENRICH_ROWS: i64 = 20_000;
/// Results one list or detail request reads for those, at most: shared by the fingerprints of
/// the page (errors and their aliases), each reading at least one.
pub const ENRICH_BUDGET: i64 = 200_000;
/// Least time between two `errorRegressed` deliveries of one channel of a project.
pub const EVENT_THROTTLE: std::time::Duration = std::time::Duration::from_secs(600);
/// Errors listed in one `errorRegressed` delivery.
pub const EVENT_ITEMS: usize = 10;
/// Bytes of the `q` search of the errors list, at most.
pub const MAX_QUERY_BYTES: usize = 200;
/// Open errors per project re-checked by the hourly job, at most.
pub const RECHECK_PER_PROJECT: i64 = 2000;

const FAILING: &str = "('failed', 'broken')";

/// What the `errors` step did for a launch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErrorStep {
    /// Why the launch was not processed (debug, not finished, merged).
    pub skipped: Option<&'static str>,
    pub changes: Vec<ErrorStateChange>,
    /// Canonical errors of the launch.
    pub errors: usize,
    /// Errors whose state was computed (the launch's and the re-checked ones).
    pub evaluated: usize,
    /// The launch had more errors than `max_clusters` (the largest were processed).
    pub truncated: bool,
    /// The branch the launch counts on (`*` for an untracked branch).
    pub branch: String,
}

/// The branch a launch counts on.
fn bucket(branch: &str, tracked: &BTreeSet<String>) -> String {
    if tracked.contains(branch) {
        branch.to_string()
    } else {
        OTHER_BRANCH.to_string()
    }
}

/// Gate baseline branches of the project settings (`gates[].rules[].baseline`).
fn baselines(gates: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for g in gates {
        for r in g
            .get("rules")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(b) = r.get("baseline").and_then(Value::as_str)
                && !b.is_empty()
                && b.len() <= 256
            {
                out.push(b.to_string());
            }
        }
    }
    out
}

/// The tracked branches: the newest [`TRACKED_BRANCHES`] branches among the project's last
/// [`TRACKED_SCAN`] launches (by their newest launch) and the gate baselines.
async fn tracked_branches(
    conn: &mut PgConnection,
    project_id: Uuid,
    gates: &[Value],
) -> anyhow::Result<BTreeSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "select b from (select coalesce(branch, '') as b, max(number) as m from \
           (select branch, number from launches where project_id = $1 and not debug \
            order by number desc limit $2) x group by 1) y order by m desc limit $3",
    )
    .bind(project_id)
    .bind(TRACKED_SCAN)
    .bind(TRACKED_BRANCHES as i64)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: BTreeSet<String> = rows.into_iter().collect();
    out.extend(baselines(gates));
    out.remove(OTHER_BRANCH);
    Ok(out)
}

async fn lock(conn: &mut PgConnection, project_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("select pg_advisory_xact_lock($1, hashtext($2::text))")
        .bind(ERRORS_LOCK)
        .bind(project_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// A launch as the lifecycle reads it.
#[derive(Clone, Debug)]
struct LaunchFact {
    number: i64,
    commit: Option<String>,
}

async fn launch_facts(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> anyhow::Result<HashMap<Uuid, LaunchFact>> {
    let rows: Vec<(Uuid, i64, Option<String>)> = sqlx::query_as(
        "select id, number, nullif(vcs ->> 'commit', '') \
         from launches where project_id = $1 and id = any($2)",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, number, commit)| (id, LaunchFact { number, commit }))
        .collect())
}

/// The number of the newest launch of the project created at or before `at` (0 when none).
async fn number_at(
    conn: &mut PgConnection,
    project_id: Uuid,
    at: DateTime<Utc>,
) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        "select number from launches where project_id = $1 and created_at <= $2 \
         order by created_at desc, number desc limit 1",
    )
    .bind(project_id)
    .bind(at)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(0))
}

/// The number of a launch: from the loaded facts, else from its creation time.
async fn number_for(
    conn: &mut PgConnection,
    project_id: Uuid,
    launches: &HashMap<Uuid, LaunchFact>,
    cache: &mut HashMap<DateTime<Utc>, i64>,
    id: Option<Uuid>,
    at: DateTime<Utc>,
) -> anyhow::Result<i64> {
    if let Some(l) = id.and_then(|i| launches.get(&i)) {
        return Ok(l.number);
    }
    if let Some(n) = cache.get(&at) {
        return Ok(*n);
    }
    let n = number_at(conn, project_id, at).await?;
    cache.insert(at, n);
    Ok(n)
}

/// One count of finished, non-debug, unmerged launches of a branch with a number in
/// `(after, before)`, capped.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CountKey {
    branch: String,
    after: i64,
    before: Option<i64>,
}

/// The finished launches of each key, at most `cap` each.
async fn count_launches(
    conn: &mut PgConnection,
    project_id: Uuid,
    tracked: &BTreeSet<String>,
    keys: &BTreeSet<CountKey>,
    cap: i64,
) -> anyhow::Result<HashMap<CountKey, i64>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let list: Vec<&CountKey> = keys.iter().collect();
    let idx: Vec<i32> = (0..list.len() as i32).collect();
    let branches: Vec<String> = list.iter().map(|k| k.branch.clone()).collect();
    let after: Vec<i64> = list.iter().map(|k| k.after).collect();
    let before: Vec<i64> = list.iter().map(|k| k.before.unwrap_or(i64::MAX)).collect();
    let tracked: Vec<String> = tracked.iter().cloned().collect();
    let rows: Vec<(i32, i64)> = sqlx::query_as(
        "select k.i, (select count(*) from ( \
             (select 1 from launches l where k.b <> '' and k.b <> '*' and l.project_id = $1 \
                and l.branch = k.b and l.number > k.a and l.number < k.z \
                and l.status = 'finished' and not l.debug and l.merged_into is null limit $6) \
             union all \
             (select 1 from launches l where (k.b = '' or k.b = '*') and l.project_id = $1 \
                and l.number > k.a and l.number < k.z \
                and l.status = 'finished' and not l.debug and l.merged_into is null \
                and case when k.b = '' then coalesce(l.branch, '') = '' \
                  else not (coalesce(l.branch, '') = any($7)) end limit $6) \
           ) c)::bigint \
         from unnest($2::int[], $3::text[], $4::bigint[], $5::bigint[]) as k(i, b, a, z)",
    )
    .bind(project_id)
    .bind(&idx)
    .bind(&branches)
    .bind(&after)
    .bind(&before)
    .bind(cap)
    .bind(&tracked)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(i, n)| (list[i as usize].clone(), n.min(cap)))
        .collect())
}

/// The canonical error of every given fingerprint (itself when it has no alias).
async fn canonical_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select fingerprint, coalesce(alias_of, fingerprint) from failure_fingerprints \
         where project_id = $1 and fingerprint = any($2)",
    )
    .bind(project_id)
    .bind(fps)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<String, String> = rows.into_iter().collect();
    for f in fps {
        out.entry(f.clone()).or_insert_with(|| f.clone());
    }
    Ok(out)
}

/// The fingerprints of each canonical error: itself and its aliases.
async fn members_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    canon: &[String],
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select alias_of, fingerprint from failure_fingerprints \
         where project_id = $1 and alias_of = any($2)",
    )
    .bind(project_id)
    .bind(canon)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: BTreeMap<String, Vec<String>> =
        canon.iter().map(|c| (c.clone(), vec![c.clone()])).collect();
    for (c, m) in rows {
        if let Some(v) = out.get_mut(&c) {
            v.push(m);
        }
    }
    Ok(out)
}

/// A branch of an error, its member rows merged.
#[derive(Clone, Debug)]
struct BranchAgg {
    first_seen_at: DateTime<Utc>,
    first_launch_id: Option<Uuid>,
    last_seen_at: DateTime<Utc>,
    last_launch_id: Option<Uuid>,
    occurrences: i64,
}

type BranchRow = (
    String,
    String,
    DateTime<Utc>,
    Option<Uuid>,
    DateTime<Utc>,
    Option<Uuid>,
    i64,
);

/// The branch rows of every member, merged per canonical error and branch.
async fn branch_aggs(
    conn: &mut PgConnection,
    project_id: Uuid,
    members: &BTreeMap<String, Vec<String>>,
) -> anyhow::Result<BTreeMap<String, BTreeMap<String, BranchAgg>>> {
    let mut canon_of: HashMap<&str, &str> = HashMap::new();
    let mut all: Vec<String> = Vec::new();
    for (c, ms) in members {
        for m in ms {
            canon_of.insert(m, c);
            all.push(m.clone());
        }
    }
    let rows: Vec<BranchRow> = sqlx::query_as(
        "select fingerprint, branch, first_seen_at, first_launch_id, last_seen_at, last_launch_id, occurrences \
         from fingerprint_branches where project_id = $1 and fingerprint = any($2)",
    )
    .bind(project_id)
    .bind(&all)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: BTreeMap<String, BTreeMap<String, BranchAgg>> = BTreeMap::new();
    for (fp, branch, fs, fl, ls, ll, n) in rows {
        let Some(c) = canon_of.get(fp.as_str()) else {
            continue;
        };
        let e = out.entry(c.to_string()).or_default();
        match e.get_mut(&branch) {
            None => {
                e.insert(
                    branch,
                    BranchAgg {
                        first_seen_at: fs,
                        first_launch_id: fl,
                        last_seen_at: ls,
                        last_launch_id: ll,
                        occurrences: n,
                    },
                );
            }
            Some(a) => {
                if fs < a.first_seen_at {
                    a.first_seen_at = fs;
                    a.first_launch_id = fl;
                }
                if ls > a.last_seen_at {
                    a.last_seen_at = ls;
                    a.last_launch_id = ll;
                }
                a.occurrences += n;
            }
        }
    }
    Ok(out)
}

/// The catalog part of an error the step reads and writes.
#[derive(Clone, Debug)]
struct CatalogRow {
    message: String,
    state: ErrorState,
    state_at: Option<DateTime<Utc>>,
    resolved_at: Option<DateTime<Utc>>,
    regressed_count: i32,
    first_commit: Option<String>,
    last_good_commit: Option<String>,
}

type CatalogTuple = (
    String,
    String,
    String,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    i32,
    Option<String>,
    Option<String>,
);

async fn catalog_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<HashMap<String, CatalogRow>> {
    let rows: Vec<CatalogTuple> = sqlx::query_as(
        "select fingerprint, message, state, state_at, resolved_at, regressed_count, first_commit, last_good_commit \
         from failure_fingerprints where project_id = $1 and fingerprint = any($2)",
    )
    .bind(project_id)
    .bind(fps)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(f, message, state, state_at, resolved_at, regressed_count, fc, lg)| {
                (
                    f,
                    CatalogRow {
                        message,
                        state: ErrorState::parse(&state).unwrap_or(ErrorState::Active),
                        state_at,
                        resolved_at,
                        regressed_count,
                        first_commit: fc,
                        last_good_commit: lg,
                    },
                )
            },
        )
        .collect())
}

/// The newest occurrence of any member on `branch` before `before` (created time, number).
async fn previous_occurrence(
    conn: &mut PgConnection,
    project_id: Uuid,
    members: &[String],
    branch: &str,
    before: DateTime<Utc>,
    tracked: &BTreeSet<String>,
) -> anyhow::Result<Option<(DateTime<Utc>, i64)>> {
    let tracked: Vec<String> = tracked.iter().cloned().collect();
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select x.created_at, x.number from unnest($2::text[]) as m(f) \
         cross join lateral (select l.created_at, l.number from results r join launches l on l.id = r.launch_id \
           where r.project_id = $1 and r.fingerprint = m.f and r.launch_created_at < $3 \
             and r.status in {FAILING} and l.status = 'finished' and not l.debug and l.merged_into is null \
             and case when $4 = '*' then not (coalesce(l.branch, '') = any($5)) \
               else coalesce(l.branch, '') = $4 end \
           order by r.launch_created_at desc limit 1) x \
         order by x.created_at desc limit 1"
    )))
    .bind(project_id)
    .bind(members)
    .bind(before)
    .bind(branch)
    .bind(&tracked)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Locks the catalog rows of these fingerprints in one fixed order (the order of the ingest
/// writers), so that two writers of the same rows wait for each other instead of deadlocking.
async fn lock_catalog_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<()> {
    if fps.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "select 1 from failure_fingerprints where project_id = $1 and fingerprint = any($2) \
         order by fingerprint collate \"C\" for update",
    )
    .bind(project_id)
    .bind(fps)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The newest finished, non-debug launch of a branch before a launch number: its commit.
async fn commit_before(
    conn: &mut PgConnection,
    project_id: Uuid,
    branch: &str,
    before: i64,
    tracked: &BTreeSet<String>,
) -> anyhow::Result<Option<String>> {
    let tracked: Vec<String> = tracked.iter().cloned().collect();
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "select nullif(vcs ->> 'commit', '') from launches l where l.project_id = $1 and l.number < $2 \
           and l.status = 'finished' and not l.debug and l.merged_into is null \
           and case when $3 = '*' then not (coalesce(l.branch, '') = any($4)) \
             else coalesce(l.branch, '') = $3 end \
         order by l.number desc limit 1",
    )
    .bind(project_id)
    .bind(before)
    .bind(branch)
    .bind(&tracked)
    .fetch_optional(&mut *conn)
    .await?
    .flatten())
}

/// The first finished, non-debug launch of a branch after a launch number: when it was created.
async fn first_after(
    conn: &mut PgConnection,
    project_id: Uuid,
    branch: &str,
    after: i64,
    tracked: &BTreeSet<String>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let tracked: Vec<String> = tracked.iter().cloned().collect();
    Ok(sqlx::query_scalar(
        "select l.created_at from launches l where l.project_id = $1 and l.number > $2 \
           and l.status = 'finished' and not l.debug and l.merged_into is null \
           and case when $3 = '*' then not (coalesce(l.branch, '') = any($4)) \
             else coalesce(l.branch, '') = $3 end \
         order by l.number limit 1",
    )
    .bind(project_id)
    .bind(after)
    .bind(branch)
    .bind(&tracked)
    .fetch_optional(&mut *conn)
    .await?)
}

/// When every issue linked to one of the fingerprints was closed: the latest close; `None`
/// when none is linked or one is still open.
async fn issues_closed(
    conn: &mut PgConnection,
    project_id: Uuid,
    members: &BTreeMap<String, Vec<String>>,
) -> anyhow::Result<HashMap<String, DateTime<Utc>>> {
    let mut canon: Vec<String> = Vec::new();
    let mut member: Vec<String> = Vec::new();
    for (c, ms) in members {
        for m in ms {
            canon.push(c.clone());
            member.push(m.clone());
        }
    }
    let rows: Vec<(String, Option<DateTime<Utc>>, bool)> = sqlx::query_as(
        "select m.c, max(i.closed_at), bool_and(i.category = 'closed' and i.closed_at is not null) \
         from unnest($2::text[], $3::text[]) as m(c, f) \
         join tracker_issue_links k on k.project_id = $1 and k.fingerprint = m.f \
         join tracker_issues i on i.id = k.issue_id group by m.c",
    )
    .bind(project_id)
    .bind(&canon)
    .bind(&member)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(c, at, all)| at.filter(|_| all).map(|a| (c, a)))
        .collect())
}

/// What the recompute of one error found.
struct Computed {
    canon: String,
    old: Option<ErrorState>,
    new: ErrorState,
    message: String,
}

/// Computes and stores the state of canonical errors from their facts. Runs under the errors
/// lock of the project.
async fn recompute(
    conn: &mut PgConnection,
    project_id: Uuid,
    canon: &[String],
    tracked: &BTreeSet<String>,
    resolve_after: u32,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Computed>> {
    if canon.is_empty() {
        return Ok(Vec::new());
    }
    let k = i64::from(resolve_after.max(1));
    let catalog = catalog_rows(conn, project_id, canon).await?;
    let members = members_of(conn, project_id, canon).await?;
    let aggs = branch_aggs(conn, project_id, &members).await?;
    let closed = issues_closed(conn, project_id, &members).await?;
    let mut ids: BTreeSet<Uuid> = BTreeSet::new();
    for a in aggs.values().flat_map(|m| m.values()) {
        ids.extend(a.first_launch_id);
        ids.extend(a.last_launch_id);
    }
    let ids: Vec<Uuid> = ids.into_iter().collect();
    let launches = launch_facts(conn, project_id, &ids).await?;
    let mut numbers: HashMap<DateTime<Utc>, i64> = HashMap::new();

    struct Plan {
        canon: String,
        tail: Vec<(String, CountKey)>,
        gap: Vec<(String, CountKey)>,
        single: bool,
        latest: (DateTime<Utc>, i64, String, Option<Uuid>),
        first: (DateTime<Utc>, i64, String, Option<Uuid>),
        previous: Option<DateTime<Utc>>,
        closed: Option<(DateTime<Utc>, i64)>,
    }
    let mut plans: Vec<Plan> = Vec::new();
    let mut keys: BTreeSet<CountKey> = BTreeSet::new();
    for c in canon {
        let Some(rows) = aggs.get(c).filter(|r| !r.is_empty()) else {
            continue;
        };
        let (lb, la) = rows
            .iter()
            .max_by(|a, b| {
                a.1.last_seen_at
                    .cmp(&b.1.last_seen_at)
                    .then_with(|| b.0.cmp(a.0))
            })
            .map(|(b, a)| (b.clone(), a.clone()))
            .unwrap_or_else(|| unreachable!());
        let (fb, fa) = rows
            .iter()
            .min_by(|a, b| {
                a.1.first_seen_at
                    .cmp(&b.1.first_seen_at)
                    .then_with(|| a.0.cmp(b.0))
            })
            .map(|(b, a)| (b.clone(), a.clone()))
            .unwrap_or_else(|| unreachable!());
        let latest_n = number_for(
            conn,
            project_id,
            &launches,
            &mut numbers,
            la.last_launch_id,
            la.last_seen_at,
        )
        .await?;
        let first_n = number_for(
            conn,
            project_id,
            &launches,
            &mut numbers,
            fa.first_launch_id,
            fa.first_seen_at,
        )
        .await?;
        let single = la.last_launch_id.is_some() && fa.first_launch_id == la.last_launch_id;
        let mut tail = Vec::new();
        for (b, a) in rows {
            let after = number_for(
                conn,
                project_id,
                &launches,
                &mut numbers,
                a.last_launch_id,
                a.last_seen_at,
            )
            .await?;
            let key = CountKey {
                branch: b.clone(),
                after,
                before: None,
            };
            keys.insert(key.clone());
            tail.push((b.clone(), key));
        }
        let mut gap = Vec::new();
        let mut previous: Option<DateTime<Utc>> = None;
        if !single {
            for (b, a) in rows {
                if a.first_seen_at >= la.last_seen_at && *b == lb {
                    continue;
                }
                let before_at = if *b == lb {
                    let ms = members.get(c).cloned().unwrap_or_default();
                    match previous_occurrence(conn, project_id, &ms, b, la.last_seen_at, tracked)
                        .await?
                    {
                        Some(p) => Some(p),
                        None if a.first_seen_at < la.last_seen_at => Some((
                            a.first_seen_at,
                            number_for(
                                conn,
                                project_id,
                                &launches,
                                &mut numbers,
                                a.first_launch_id,
                                a.first_seen_at,
                            )
                            .await?,
                        )),
                        None => None,
                    }
                } else if a.last_seen_at < la.last_seen_at {
                    Some((
                        a.last_seen_at,
                        number_for(
                            conn,
                            project_id,
                            &launches,
                            &mut numbers,
                            a.last_launch_id,
                            a.last_seen_at,
                        )
                        .await?,
                    ))
                } else {
                    None
                };
                let Some((at, after)) = before_at else {
                    continue;
                };
                previous = Some(previous.map_or(at, |p| p.max(at)));
                let key = CountKey {
                    branch: b.clone(),
                    after,
                    before: Some(latest_n),
                };
                keys.insert(key.clone());
                gap.push((b.clone(), key));
            }
        }
        let closed_at = match closed.get(c) {
            Some(at) => Some((*at, number_at(conn, project_id, *at).await?)),
            None => None,
        };
        if let Some((_, n)) = closed_at {
            for b in rows.keys() {
                keys.insert(CountKey {
                    branch: b.clone(),
                    after: n,
                    before: None,
                });
                keys.insert(CountKey {
                    branch: b.clone(),
                    after: n,
                    before: Some(latest_n),
                });
            }
        }
        plans.push(Plan {
            canon: c.clone(),
            tail,
            gap,
            single,
            latest: (la.last_seen_at, latest_n, lb, la.last_launch_id),
            first: (fa.first_seen_at, first_n, fb, fa.first_launch_id),
            previous,
            closed: closed_at,
        });
    }
    let counts = count_launches(conn, project_id, tracked, &keys, k).await?;
    let count = |key: &CountKey| counts.get(key).copied().unwrap_or(0);

    let mut out = Vec::new();
    let mut writes: Vec<StateWrite> = Vec::new();
    for p in plans {
        let Some(row) = catalog.get(&p.canon) else {
            continue;
        };
        let old = row.state_at.map(|_| row.state);
        let branches: Vec<String> = p.tail.iter().map(|(b, _)| b.clone()).collect();
        let (issue_after, issue_in_gap) = match p.closed {
            Some((at, n)) => {
                let after = at > p.latest.0
                    && branches.iter().any(|b| {
                        count(&CountKey {
                            branch: b.clone(),
                            after: n,
                            before: None,
                        }) > 0
                    });
                let in_gap = !p.single
                    && at < p.latest.0
                    && p.previous.is_some_and(|prev| at > prev)
                    && branches.iter().any(|b| {
                        count(&CountKey {
                            branch: b.clone(),
                            after: n,
                            before: Some(p.latest.1),
                        }) > 0
                    });
                (after, in_gap)
            }
            None => (false, false),
        };
        let facts = ErrorFacts {
            ignored: row.state == ErrorState::Ignored,
            seen: true,
            single: p.single,
            tail: p
                .tail
                .iter()
                .map(|(b, key)| BranchCount {
                    branch: b.clone(),
                    launches: count(key) as u32,
                })
                .collect(),
            gap: p
                .gap
                .iter()
                .map(|(b, key)| BranchCount {
                    branch: b.clone(),
                    launches: count(key) as u32,
                })
                .collect(),
            issue_closed_after: issue_after,
            issue_closed_in_gap: issue_in_gap,
            resolve_after,
        };
        let Some(new) = lifecycle::state(&facts) else {
            continue;
        };
        if new == ErrorState::Ignored {
            continue;
        }
        let open = |s: Option<ErrorState>| {
            matches!(
                s,
                Some(ErrorState::New | ErrorState::Active | ErrorState::Regressed)
            )
        };
        let state_at = match new {
            ErrorState::New => p.first.0,
            ErrorState::Regressed if old == Some(ErrorState::Regressed) => {
                row.state_at.unwrap_or(p.latest.0)
            }
            ErrorState::Regressed => p.latest.0,
            ErrorState::Active | ErrorState::Resolved if open(old) => {
                row.state_at.unwrap_or(p.first.0)
            }
            ErrorState::Resolved if old == Some(ErrorState::Resolved) => {
                row.state_at.unwrap_or(p.first.0)
            }
            ErrorState::Active if old == Some(ErrorState::Resolved) => p.latest.0,
            _ => p.first.0,
        };
        let resolved_at = if new == ErrorState::Resolved
            && (old != Some(ErrorState::Resolved) || row.resolved_at.is_none())
        {
            let by_issue = p.closed.map(|c| c.0).filter(|_| issue_after);
            let by_launch = first_after(conn, project_id, &p.latest.2, p.latest.1, tracked)
                .await?
                .filter(|_| !issue_after);
            Some(by_issue.or(by_launch).unwrap_or(now))
        } else {
            row.resolved_at
        };
        let regressed_count = row.regressed_count
            + i32::from(new == ErrorState::Regressed && old != Some(ErrorState::Regressed));
        let (first_commit, last_good_commit) = match new {
            ErrorState::New if old != Some(ErrorState::New) || row.first_commit.is_none() => (
                p.first
                    .3
                    .and_then(|i| launches.get(&i))
                    .and_then(|l| l.commit.clone()),
                commit_before(conn, project_id, &p.first.2, p.first.1, tracked).await?,
            ),
            ErrorState::Regressed if old != Some(ErrorState::Regressed) => (
                p.latest
                    .3
                    .and_then(|i| launches.get(&i))
                    .and_then(|l| l.commit.clone()),
                commit_before(conn, project_id, &p.latest.2, p.latest.1, tracked).await?,
            ),
            _ => (row.first_commit.clone(), row.last_good_commit.clone()),
        };
        let unchanged = old == Some(new)
            && row.state_at == Some(state_at)
            && row.resolved_at == resolved_at
            && row.regressed_count == regressed_count
            && row.first_commit == first_commit
            && row.last_good_commit == last_good_commit;
        if !unchanged {
            writes.push(StateWrite {
                fingerprint: p.canon.clone(),
                state: new.as_str().to_string(),
                state_at,
                resolved_at,
                regressed_count,
                first_commit: first_commit.as_deref().map(no_nul),
                last_good_commit: last_good_commit.as_deref().map(no_nul),
            });
        }
        out.push(Computed {
            canon: p.canon,
            old,
            new,
            message: row.message.clone(),
        });
    }
    write_states(conn, project_id, writes).await?;
    Ok(out)
}

/// Locks the catalog rows a patch changes (`fps` and, with `aliases_of`, that error's aliases)
/// in the writers' order before it changes them.
async fn lock_patch_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
    aliases_of: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        "select 1 from failure_fingerprints where project_id = $1 \
           and (fingerprint = any($2) or alias_of = $3) \
         order by fingerprint collate \"C\" for update",
    )
    .bind(project_id)
    .bind(fps)
    .bind(aliases_of)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A computed state of a catalog row, written at the end of [`recompute`].
struct StateWrite {
    fingerprint: String,
    state: String,
    state_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    regressed_count: i32,
    first_commit: Option<String>,
    last_good_commit: Option<String>,
}

/// Writes the computed states in one statement. The catalog rows are read without row locks
/// (their state columns change only under the errors lock) and locked here, at the end of the
/// transaction, in the writers' order: ingest updates the same rows (occurrences, last seen)
/// and must not wait for the whole computation, nor deadlock with it.
async fn write_states(
    conn: &mut PgConnection,
    project_id: Uuid,
    mut writes: Vec<StateWrite>,
) -> anyhow::Result<()> {
    if writes.is_empty() {
        return Ok(());
    }
    writes.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    let fps: Vec<String> = writes.iter().map(|w| w.fingerprint.clone()).collect();
    lock_catalog_rows(conn, project_id, &fps).await?;
    let states: Vec<String> = writes.iter().map(|w| w.state.clone()).collect();
    let state_at: Vec<DateTime<Utc>> = writes.iter().map(|w| w.state_at).collect();
    let resolved_at: Vec<Option<DateTime<Utc>>> = writes.iter().map(|w| w.resolved_at).collect();
    let regressed: Vec<i32> = writes.iter().map(|w| w.regressed_count).collect();
    let first: Vec<Option<String>> = writes.iter().map(|w| w.first_commit.clone()).collect();
    let good: Vec<Option<String>> = writes.iter().map(|w| w.last_good_commit.clone()).collect();
    sqlx::query(
        "update failure_fingerprints f set state = u.s, state_at = u.sa, resolved_at = u.ra, \
           regressed_count = u.rc, first_commit = u.fc, last_good_commit = u.lg \
         from unnest($2::text[], $3::text[], $4::timestamptz[], $5::timestamptz[], $6::int[], \
           $7::text[], $8::text[]) as u(f, s, sa, ra, rc, fc, lg) \
         where f.project_id = $1 and f.fingerprint = u.f",
    )
    .bind(project_id)
    .bind(&fps)
    .bind(&states)
    .bind(&state_at)
    .bind(&resolved_at)
    .bind(&regressed)
    .bind(&first)
    .bind(&good)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Facts of a list or detail row, merged over the members of each error.
#[derive(Clone, Debug, Default)]
struct Enrich {
    tests: i64,
    spark: Vec<i64>,
}

/// Results read per fingerprint when `members` fingerprints share [`ENRICH_BUDGET`].
pub fn enrich_rows_per_member(members: usize) -> i64 {
    (ENRICH_BUDGET / members.max(1) as i64).clamp(1, ENRICH_ROWS)
}

fn spark_days(today: NaiveDate) -> Vec<NaiveDate> {
    (0..SPARK_DAYS)
        .rev()
        .map(|i| today - Duration::days(i))
        .collect()
}

fn sanitize_commit(c: &str) -> Option<&str> {
    (!c.is_empty()
        && c.len() <= 64
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    .then_some(c)
}

/// The compare page of a commit range on GitHub or GitLab: `vcs` is the project's `vcs`
/// settings (`{provider, url, repository}`).
pub fn compare_url(vcs: &Value, last_good: &str, first: &str) -> Option<String> {
    let last_good = sanitize_commit(last_good)?;
    let first = sanitize_commit(first)?;
    let provider = vcs.get("provider")?.as_str()?;
    let repo = vcs.get("repository")?.as_str()?.trim_matches('/');
    if repo.is_empty()
        || !repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        || repo
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    // Only the origin and the path of the configured URL: credentials, a query or a fragment
    // in it never reach the readers of the error.
    let mut parsed = url::Url::parse(vcs.get("url")?.as_str()?.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return None;
    }
    parsed.set_username("").ok()?;
    parsed.set_password(None).ok()?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    let full = parsed.to_string();
    let url = full.trim_end_matches('/');
    match provider {
        "gitlab" => Some(format!("{url}/{repo}/-/compare/{last_good}...{first}")),
        "github" => {
            let web = if url == "https://api.github.com" {
                "https://github.com".to_string()
            } else {
                url.strip_suffix("/api/v3").unwrap_or(url).to_string()
            };
            Some(format!("{web}/{repo}/compare/{last_good}...{first}"))
        }
        _ => None,
    }
}

/// Per error: aliases, their occurrences, their newest and their oldest occurrence.
type AliasExtra = (String, i64, i64, DateTime<Utc>, DateTime<Utc>);

type SummaryTuple = (
    String,
    String,
    String,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    DateTime<Utc>,
    i64,
    i32,
    Option<String>,
    Option<String>,
    Option<String>,
);

const SUMMARY_COLUMNS: &str = "f.fingerprint, f.message, f.state, f.state_at, f.resolved_at, f.first_seen_at, \
    f.last_seen_at, f.occurrences, f.regressed_count, f.alias_of, f.first_commit, f.last_good_commit";

fn parse_cursor(before: &str) -> AnalysisResult<(i64, String)> {
    let bad = || TmError::bad_request("before is not a cursor of this list");
    if before.len() > 64 {
        return Err(bad());
    }
    let (k, f) = before.split_once('~').ok_or_else(bad)?;
    let k: i64 = k.parse().map_err(|_| bad())?;
    if !super::rules::valid_fingerprint(f) {
        return Err(bad());
    }
    Ok((k, f.to_string()))
}

impl Store {
    /// The `errors` step of a finished launch (contract §7.4): branches of its errors, then the
    /// state of its errors and of the errors it may have resolved or split, under the errors
    /// lock of the project.
    pub async fn update_error_states(
        &self,
        ctx: &LaunchCtx<'_>,
    ) -> anyhow::Result<Vec<ErrorStateChange>> {
        Ok(self.error_step(ctx).await?.changes)
    }

    /// [`Store::update_error_states`] with what it did.
    pub async fn error_step(&self, ctx: &LaunchCtx<'_>) -> anyhow::Result<ErrorStep> {
        let l = ctx.launch;
        let mut step = ErrorStep::default();
        if l.debug {
            step.skipped = Some("debug launch");
            return Ok(step);
        }
        if l.merged_into.is_some() {
            step.skipped = Some("merged launch");
            return Ok(step);
        }
        let status: Option<String> =
            sqlx::query_scalar("select status from launches where id = $1 and project_id = $2")
                .bind(l.id)
                .bind(ctx.project_id)
                .fetch_optional(&self.pool)
                .await?;
        if status.as_deref() != Some("finished") {
            step.skipped = Some("launch not finished");
            return Ok(step);
        }
        let k = ctx.settings.analysis.resolve_after_launches;
        let max = ctx.limits.max_clusters.max(1) as usize;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx, ctx.project_id).await?;
        let tracked = tracked_branches(&mut tx, ctx.project_id, &ctx.settings.gates).await?;
        let own = l
            .vcs
            .as_ref()
            .and_then(|v| v.branch.clone())
            .unwrap_or_default();
        let b = bucket(&own, &tracked);
        step.branch = b.clone();
        let rows: Vec<(String, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select coalesce(f.alias_of, r.fingerprint), r.fingerprint, count(*)::bigint from results r \
             left join failure_fingerprints f on f.project_id = r.project_id and f.fingerprint = r.fingerprint \
             where r.launch_id = $1 and r.launch_created_at = $2 and r.project_id = $3 \
               and r.status in {FAILING} and r.fingerprint is not null group by 1, 2"
        )))
        .bind(l.id)
        .bind(l.created_at)
        .bind(ctx.project_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut per_canon: BTreeMap<String, (i64, Vec<String>)> = BTreeMap::new();
        for (c, f, n) in rows {
            let e = per_canon.entry(c).or_default();
            e.0 += n;
            e.1.push(f);
        }
        let mut ordered: Vec<(String, (i64, Vec<String>))> = per_canon.into_iter().collect();
        ordered.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        if ordered.len() > max {
            ordered.truncate(max);
            step.truncated = true;
            metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "clusters")
                .increment(1);
        }
        step.errors = ordered.len();
        let mut raw: Vec<String> = ordered.iter().flat_map(|(_, (_, f))| f.clone()).collect();
        raw.sort();
        let done_before: bool = sqlx::query_scalar(
            "select coalesce((select steps -> 'errors' ->> 'status' = 'done' from analysis_launches \
               where launch_id = $1), false)",
        )
        .bind(l.id)
        .fetch_one(&mut *tx)
        .await?;
        if !raw.is_empty() {
            sqlx::query(
                "insert into fingerprint_branches as fb (project_id, fingerprint, branch, first_seen_at, \
                   first_launch_id, last_seen_at, last_launch_id, occurrences) \
                 select $1, f, $2, $3, $4, $3, $4, 1 from unnest($5::text[]) as t(f) \
                 on conflict (project_id, fingerprint, branch) do update set \
                   occurrences = fb.occurrences + case when $6 or fb.first_launch_id = excluded.first_launch_id \
                     or fb.last_launch_id = excluded.last_launch_id then 0 else 1 end, \
                   first_launch_id = case when excluded.first_seen_at < fb.first_seen_at \
                     then excluded.first_launch_id else fb.first_launch_id end, \
                   first_seen_at = least(fb.first_seen_at, excluded.first_seen_at), \
                   last_launch_id = case when excluded.last_seen_at >= fb.last_seen_at \
                     then excluded.last_launch_id else fb.last_launch_id end, \
                   last_seen_at = greatest(fb.last_seen_at, excluded.last_seen_at)",
            )
            .bind(ctx.project_id)
            .bind(no_nul(&b))
            .bind(l.created_at)
            .bind(l.id)
            .bind(&raw)
            .bind(done_before)
            .execute(&mut *tx)
            .await?;
        }
        let mine: BTreeSet<String> = ordered.iter().map(|(c, _)| c.clone()).collect();
        let k_th: Option<DateTime<Utc>> = sqlx::query_scalar(
            "select created_at from launches l where l.project_id = $1 \
               and l.status = 'finished' and not l.debug and l.merged_into is null \
               and case when $2 = '*' then not (coalesce(l.branch, '') = any($3)) \
                 else coalesce(l.branch, '') = $2 end \
             order by l.number desc offset $4 limit 1",
        )
        .bind(ctx.project_id)
        .bind(&b)
        .bind(tracked.iter().cloned().collect::<Vec<_>>())
        .bind(i64::from(k.max(1)) - 1)
        .fetch_optional(&mut *tx)
        .await?;
        let recheck: Vec<String> = sqlx::query_scalar(
            "with open as (select fingerprint from failure_fingerprints where project_id = $1 \
                 and alias_of is null and state in ('new', 'active', 'regressed')), \
               mem as (select fingerprint as canon, fingerprint as m from open \
                 union all select a.alias_of, a.fingerprint from failure_fingerprints a \
                 where a.project_id = $1 and a.alias_of in (select fingerprint from open)) \
             select m.canon from mem m join fingerprint_branches fb \
               on fb.project_id = $1 and fb.fingerprint = m.m \
             group by m.canon \
             having bool_or(fb.branch = $2 and $3::timestamptz is not null and fb.last_seen_at < $3) \
               or (bool_or(fb.branch = $2) and max(fb.last_seen_at) > $4) \
             limit $5",
        )
        .bind(ctx.project_id)
        .bind(&b)
        .bind(k_th)
        .bind(l.created_at)
        .bind(MAX_RECHECK)
        .fetch_all(&mut *tx)
        .await?;
        let mut all: BTreeSet<String> = mine.clone();
        all.extend(recheck);
        let all: Vec<String> = all.into_iter().collect();
        step.evaluated = all.len();
        let computed = recompute(&mut tx, ctx.project_id, &all, &tracked, k, ctx.now).await?;
        tx.commit().await?;
        for c in computed {
            if c.old == Some(c.new) {
                continue;
            }
            metrics::counter!("fastproof_analysis_error_transitions_total", "to" => c.new.as_str())
                .increment(1);
            step.changes.push(ErrorStateChange {
                fingerprint: c.canon,
                from: c.old,
                to: c.new,
                branch: own.clone(),
                message: c.message,
            });
        }
        Ok(step)
    }

    /// Recomputes the state of errors (the hourly job and after a patch): `fps` are mapped to
    /// their canonical errors. Returns the changes.
    pub async fn recompute_error_states(
        &self,
        project_id: Uuid,
        fps: &[String],
        resolve_after: u32,
    ) -> anyhow::Result<Vec<ErrorStateChange>> {
        let settings = self.project_settings(project_id).await?;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx, project_id).await?;
        let tracked = tracked_branches(&mut tx, project_id, &settings.gates).await?;
        let canon = canonical_of(&mut tx, project_id, fps).await?;
        let mut list: Vec<String> = canon.into_values().collect();
        list.sort();
        list.dedup();
        let computed = recompute(
            &mut tx,
            project_id,
            &list,
            &tracked,
            resolve_after,
            Utc::now(),
        )
        .await?;
        tx.commit().await?;
        Ok(computed
            .into_iter()
            .filter(|c| c.old != Some(c.new))
            .map(|c| ErrorStateChange {
                fingerprint: c.canon,
                from: c.old,
                to: c.new,
                branch: String::new(),
                message: c.message,
            })
            .collect())
    }

    /// Open errors the hourly job re-checks, per project: those linked to a closed issue and
    /// those not seen for a day (at most [`RECHECK_PER_PROJECT`] each, oldest first).
    pub async fn error_recheck_targets(
        &self,
        project_ids: &[Uuid],
    ) -> anyhow::Result<Vec<(Uuid, Vec<String>)>> {
        let mut out = Vec::new();
        for p in project_ids {
            let mut fps: Vec<String> = sqlx::query_scalar(
                "select distinct coalesce(f.alias_of, f.fingerprint) from tracker_issue_links k \
                 join tracker_issues i on i.id = k.issue_id \
                 join failure_fingerprints f on f.project_id = k.project_id and f.fingerprint = k.fingerprint \
                 where k.project_id = $1 and k.fingerprint is not null and i.category = 'closed' limit $2",
            )
            .bind(p)
            .bind(RECHECK_PER_PROJECT)
            .fetch_all(&self.pool)
            .await?;
            let stale: Vec<String> = sqlx::query_scalar(
                "select fingerprint from failure_fingerprints where project_id = $1 and alias_of is null \
                   and state in ('new', 'active', 'regressed') and state_at is not null \
                   and last_seen_at < now() - interval '1 day' \
                 order by last_seen_at limit $2",
            )
            .bind(p)
            .bind(RECHECK_PER_PROJECT)
            .fetch_all(&self.pool)
            .await?;
            fps.extend(stale);
            fps.sort();
            fps.dedup();
            if !fps.is_empty() {
                out.push((*p, fps));
            }
        }
        Ok(out)
    }

    /// Whether the project has this error (any fingerprint of its catalog).
    pub async fn error_exists(&self, project_id: Uuid, fingerprint: &str) -> anyhow::Result<bool> {
        if !super::rules::valid_fingerprint(fingerprint) {
            return Ok(false);
        }
        Ok(sqlx::query_scalar(
            "select exists(select 1 from failure_fingerprints where project_id = $1 and fingerprint = $2)",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The canonical error of every fingerprint (itself when it has no alias or is unknown).
    pub async fn canonical_fingerprints(
        &self,
        project_id: Uuid,
        fps: &[String],
    ) -> anyhow::Result<HashMap<String, String>> {
        let mut conn = self.pool.acquire().await?;
        canonical_of(&mut conn, project_id, fps).await
    }

    /// New errors of a launch for the gate metric `newErrors`: its canonical errors that are
    /// not `ignored` and did not occur on the baseline branch (the gate rule's `baseline`, else
    /// the launch's own branch) in the [`NEW_ERRORS_DAYS`] before the launch. Read from the
    /// results, so it needs neither the `errors` step nor Analysis.
    pub async fn new_error_count(
        &self,
        project_id: Uuid,
        launch: &Launch,
        baseline: Option<&str>,
    ) -> anyhow::Result<i64> {
        let branch = baseline
            .map(str::to_string)
            .or_else(|| launch.vcs.as_ref().and_then(|v| v.branch.clone()))
            .unwrap_or_default();
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "with mine as (select distinct coalesce(f.alias_of, r.fingerprint) as canon from results r \
               left join failure_fingerprints f on f.project_id = r.project_id and f.fingerprint = r.fingerprint \
               where r.launch_id = $1 and r.launch_created_at = $2 and r.project_id = $3 \
                 and r.status in {FAILING} and r.fingerprint is not null), \
             mem as (select canon, canon as m from mine union all \
               select a.alias_of, a.fingerprint from failure_fingerprints a \
               where a.project_id = $3 and a.alias_of in (select canon from mine)) \
             select count(*) from mine c \
             where not exists (select 1 from failure_fingerprints f where f.project_id = $3 \
                 and f.fingerprint = c.canon and f.state = 'ignored') \
               and not exists (select 1 from mem m join results r on r.project_id = $3 and r.fingerprint = m.m \
                 join launches l on l.id = r.launch_id \
                 where m.canon = c.canon and r.launch_created_at >= $2 - make_interval(days => $5) \
                   and r.launch_created_at < $2 and r.launch_id <> $1 and r.status in {FAILING} \
                   and not l.debug and coalesce(l.branch, '') = $4)"
        )))
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(project_id)
        .bind(&branch)
        .bind(NEW_ERRORS_DAYS as i32)
        .fetch_one(&self.pool)
        .await?)
    }

    /// `GET .../errors`: canonical errors with filters, sorted and paged by a keyset cursor.
    pub async fn errors(&self, project_id: Uuid, q: &ErrorQuery) -> AnalysisResult<ErrorList> {
        let limit = q.limit.clamp(1, 500);
        let search = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) if s.chars().count() < 3 => {
                return Err(TmError::bad_request("q needs at least 3 characters"));
            }
            Some(s) if s.len() > MAX_QUERY_BYTES => {
                return Err(TmError::bad_request("q is longer than 200 bytes"));
            }
            Some(s) => Some(format!("%{}%", crate::launches::like_escape(s))),
            None => None,
        };
        if q.branch.as_ref().is_some_and(|b| b.len() > 256) {
            return Err(TmError::bad_request("branch is longer than 256 bytes"));
        }
        let cursor = q.before.as_deref().map(parse_cursor).transpose()?;
        let key = match q.sort {
            ErrorSort::LastSeen => "(extract(epoch from f.last_seen_at) * 1000000)::bigint",
            ErrorSort::FirstSeen => "(extract(epoch from f.first_seen_at) * 1000000)::bigint",
            ErrorSort::Occurrences => "f.occurrences",
            ErrorSort::Launches => {
                "coalesce((select sum(b.occurrences) from fingerprint_branches b \
                 where b.project_id = f.project_id and b.fingerprint = f.fingerprint), 0)::bigint"
            }
        };
        let order = match q.sort {
            ErrorSort::LastSeen => "f.last_seen_at desc",
            ErrorSort::FirstSeen => "f.first_seen_at desc",
            _ => "k desc",
        };
        let keyset = match q.sort {
            ErrorSort::LastSeen => {
                "(f.last_seen_at, f.fingerprint collate \"C\") < ($6::timestamptz, $7)"
            }
            ErrorSort::FirstSeen => {
                "(f.first_seen_at, f.fingerprint collate \"C\") < ($6::timestamptz, $7)"
            }
            _ => "(k, fingerprint collate \"C\") < ($8::bigint, $7)",
        };
        let inner = format!(
            "select {SUMMARY_COLUMNS}, {key} as k from failure_fingerprints f \
             where f.project_id = $1 and f.alias_of is null \
               and ($2::text is null or f.state = $2) \
               and ($3::text is null or f.message ilike $3) \
               and ($4::text is null or exists (select 1 from fingerprint_branches b \
                   where b.project_id = $1 and b.fingerprint = f.fingerprint and b.branch = $4) \
                 or exists (select 1 from failure_fingerprints a join fingerprint_branches b \
                   on b.project_id = a.project_id and b.fingerprint = a.fingerprint and b.branch = $4 \
                   where a.project_id = $1 and a.alias_of = f.fingerprint))"
        );
        let sql = match q.sort {
            ErrorSort::LastSeen | ErrorSort::FirstSeen => format!(
                "{inner} and ($7::text is null or {keyset}) \
                 order by {order}, f.fingerprint collate \"C\" desc limit $5"
            ),
            _ => format!(
                "select * from ({inner}) x where ($7::text is null or {keyset}) \
                 order by {order}, fingerprint collate \"C\" desc limit $5"
            ),
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(project_id)
            .bind(q.state.map(|s| s.as_str()))
            .bind(search)
            .bind(q.branch.as_deref().map(no_nul))
            .bind(limit + 1)
            .bind(
                cursor
                    .as_ref()
                    .and_then(|c| DateTime::<Utc>::from_timestamp_micros(c.0)),
            )
            .bind(cursor.as_ref().map(|c| c.1.clone()))
            .bind(cursor.as_ref().map(|c| c.0))
            .fetch_all(&self.pool)
            .await?;
        let mut tuples: Vec<(SummaryTuple, i64)> = Vec::with_capacity(rows.len());
        for r in &rows {
            tuples.push((
                (
                    r.try_get(0)?,
                    r.try_get(1)?,
                    r.try_get(2)?,
                    r.try_get(3)?,
                    r.try_get(4)?,
                    r.try_get(5)?,
                    r.try_get(6)?,
                    r.try_get(7)?,
                    r.try_get(8)?,
                    r.try_get(9)?,
                    r.try_get(10)?,
                    r.try_get(11)?,
                ),
                r.try_get::<i64, _>("k")?,
            ));
        }
        let more = tuples.len() as i64 > limit;
        tuples.truncate(limit as usize);
        let next = if more {
            tuples.last().map(|(t, k)| format!("{k}~{}", t.0))
        } else {
            None
        };
        let summaries = self
            .error_summaries(project_id, tuples.into_iter().map(|(t, _)| t).collect())
            .await?;
        Ok(ErrorList {
            items: summaries.into_iter().map(|(s, _)| s).collect(),
            next,
        })
    }

    /// Fills the list fields of catalog rows (branches, launches, tests, aliases, issue, group,
    /// sparkline); also returns each row's members.
    async fn error_summaries(
        &self,
        project_id: Uuid,
        rows: Vec<SummaryTuple>,
    ) -> anyhow::Result<Vec<(ErrorSummary, Vec<String>)>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.pool.acquire().await?;
        let canon: Vec<String> = rows
            .iter()
            .filter(|r| r.9.is_none())
            .map(|r| r.0.clone())
            .collect();
        let mut members = members_of(&mut conn, project_id, &canon).await?;
        for r in rows.iter().filter(|r| r.9.is_some()) {
            members.insert(r.0.clone(), vec![r.0.clone()]);
        }
        let aggs = branch_aggs(&mut conn, project_id, &members).await?;
        let mut pc: Vec<String> = Vec::new();
        let mut pm: Vec<String> = Vec::new();
        for (c, ms) in &members {
            for m in ms {
                pc.push(c.clone());
                pm.push(m.clone());
            }
        }
        let extra: Vec<AliasExtra> = sqlx::query_as(
            "select m.c, count(*)::bigint, sum(f.occurrences)::bigint, max(f.last_seen_at), min(f.first_seen_at) \
             from unnest($2::text[], $3::text[]) as m(c, f) \
             join failure_fingerprints f on f.project_id = $1 and f.fingerprint = m.f \
             where m.f <> m.c group by m.c",
        )
        .bind(project_id)
        .bind(&pc)
        .bind(&pm)
        .fetch_all(&mut *conn)
        .await?;
        let extra: HashMap<String, (i64, i64, DateTime<Utc>, DateTime<Utc>)> = extra
            .into_iter()
            .map(|(c, n, o, l, f)| (c, (n, o, l, f)))
            .collect();
        let today = Utc::now().date_naive();
        let days = spark_days(today);
        let since_spark = days[0].and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
        let since_tests = Utc::now() - Duration::days(TESTS_DAYS);
        let enrich_rows: Vec<(String, Option<NaiveDate>, i64, i64, i32)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select x.c, x.d, count(*)::bigint, count(distinct x.t)::bigint, grouping(x.d)::int from \
                   (select m.c, case when y.at >= $5 then (y.at at time zone 'UTC')::date end as d, y.t \
                    from unnest($2::text[], $3::text[]) as m(c, f) \
                    cross join lateral (select r.launch_created_at as at, r.test_case_id as t from results r \
                      where r.project_id = $1 and r.fingerprint = m.f and r.launch_created_at >= $4 \
                        and r.status in {FAILING} order by r.launch_created_at desc limit $6) y) x \
                 group by grouping sets ((x.c, x.d), (x.c))"
            )))
            .bind(project_id)
            .bind(&pc)
            .bind(&pm)
            .bind(since_tests)
            .bind(since_spark)
            .bind(enrich_rows_per_member(pm.len()))
            .fetch_all(&mut *conn)
            .await?;
        let mut enrich: HashMap<String, Enrich> = HashMap::new();
        for (c, day, n, tests, total) in enrich_rows {
            let e = enrich.entry(c).or_default();
            if total == 1 {
                e.tests = tests;
                continue;
            }
            let Some(d) = day else {
                continue;
            };
            if e.spark.is_empty() {
                e.spark = vec![0; days.len()];
            }
            if let Some(i) = days.iter().position(|x| *x == d) {
                e.spark[i] += n;
            }
        }
        let issues: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
            "select distinct on (m.c) m.c, coalesce(i.url, ''), i.issue_key, i.category \
             from unnest($2::text[], $3::text[]) as m(c, f) \
             join tracker_issue_links k on k.project_id = $1 and k.fingerprint = m.f \
             join tracker_issues i on i.id = k.issue_id \
             order by m.c, k.created_at desc, k.id",
        )
        .bind(project_id)
        .bind(&pc)
        .bind(&pm)
        .fetch_all(&mut *conn)
        .await?;
        let issues: HashMap<String, ErrorIssue> = issues
            .into_iter()
            .filter(|(_, url, _, _)| !url.is_empty())
            .map(|(c, url, key, cat)| {
                (
                    c,
                    ErrorIssue {
                        url,
                        key,
                        state: match cat.as_str() {
                            "closed" => ErrorIssueState::Closed,
                            "open" => ErrorIssueState::Open,
                            _ => ErrorIssueState::Creating,
                        },
                    },
                )
            })
            .collect();
        let groups: Vec<(String, String)> = sqlx::query_as(
            "select distinct on (m.c) m.c, t.grp from unnest($2::text[], $3::text[]) as m(c, f) \
             join triage_memory t on t.project_id = $1 and t.fingerprint = m.f \
             order by m.c, t.confirmed desc, t.last_decided_at desc",
        )
        .bind(project_id)
        .bind(&pc)
        .bind(&pm)
        .fetch_all(&mut *conn)
        .await?;
        let groups: HashMap<String, String> = groups.into_iter().collect();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let (
                fp,
                message,
                state,
                state_at,
                resolved_at,
                first,
                last,
                occ,
                regressed,
                alias,
                _,
                _,
            ) = r;
            let state = ErrorState::parse(&state).unwrap_or(ErrorState::Active);
            let ex = extra.get(&fp);
            let branch_rows = aggs.get(&fp);
            let mut branches: Vec<(DateTime<Utc>, String)> = branch_rows
                .map(|m| m.iter().map(|(b, a)| (a.last_seen_at, b.clone())).collect())
                .unwrap_or_default();
            branches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let launches: i64 = branch_rows
                .map(|m| m.values().map(|a| a.occurrences).sum())
                .unwrap_or(0);
            let e = enrich.remove(&fp).unwrap_or_default();
            let summary = ErrorSummary {
                fingerprint: fp.clone(),
                message,
                state,
                state_at: if state == ErrorState::Resolved {
                    resolved_at.or(state_at)
                } else {
                    state_at
                },
                first_seen_at: ex.map_or(first, |x| x.3.min(first)),
                last_seen_at: ex.map_or(last, |x| x.2.max(last)),
                occurrences: occ + ex.map_or(0, |x| x.1),
                tests: e.tests,
                launches,
                branches: branches.into_iter().map(|b| b.1).take(10).collect(),
                regressed_count: regressed,
                alias_of: alias,
                aliases: ex.map_or(0, |x| x.0),
                issue: issues.get(&fp).cloned(),
                group: groups.get(&fp).cloned(),
                spark: if e.spark.is_empty() {
                    vec![0; days.len()]
                } else {
                    e.spark
                },
            };
            let ms = members.remove(&fp).unwrap_or_else(|| vec![fp.clone()]);
            out.push((summary, ms));
        }
        Ok(out)
    }

    /// `GET .../errors/{fingerprint}` (the PostgreSQL timeline of the last 30 days; the server
    /// replaces it with ClickHouse's when it has one, and fills `commits.compareUrl`).
    pub async fn error_detail(
        &self,
        project_id: Uuid,
        fingerprint: &str,
    ) -> AnalysisResult<ErrorDetail> {
        if !super::rules::valid_fingerprint(fingerprint) {
            return Err(TmError::NotFound("error"));
        }
        let row: Option<SummaryTuple> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {SUMMARY_COLUMNS} from failure_fingerprints f where f.project_id = $1 and f.fingerprint = $2"
        )))
        .bind(project_id)
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(TmError::NotFound("error"));
        };
        let (first_commit, last_good) = (row.10.clone(), row.11.clone());
        let state = ErrorState::parse(&row.2).unwrap_or(ErrorState::Active);
        let episode = row.3;
        let resolved_at = row.4;
        let (summary, members) = self
            .error_summaries(project_id, vec![row])
            .await?
            .into_iter()
            .next()
            .ok_or(TmError::NotFound("error"))?;
        let mut conn = self.pool.acquire().await?;
        let alias_list: Vec<String> = sqlx::query_scalar(
            "select fingerprint from failure_fingerprints where project_id = $1 and alias_of = $2 \
             order by last_seen_at desc limit 1000",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_all(&mut *conn)
        .await?;
        let settings = self.project_settings(project_id).await?;
        let tracked = tracked_branches(&mut conn, project_id, &settings.gates).await?;
        let m: BTreeMap<String, Vec<String>> = [(fingerprint.to_string(), members.clone())].into();
        let aggs = branch_aggs(&mut conn, project_id, &m)
            .await?
            .remove(fingerprint)
            .unwrap_or_default();
        let ids: Vec<Uuid> = aggs.values().filter_map(|a| a.last_launch_id).collect();
        let lf = launch_facts(&mut conn, project_id, &ids).await?;
        let mut keys = BTreeSet::new();
        let mut branch_keys = Vec::new();
        for (b, a) in &aggs {
            let after = match a.last_launch_id.and_then(|i| lf.get(&i)) {
                Some(l) => l.number,
                None => number_at(&mut conn, project_id, a.last_seen_at).await?,
            };
            let key = CountKey {
                branch: b.clone(),
                after,
                before: None,
            };
            keys.insert(key.clone());
            branch_keys.push((b.clone(), a.clone(), key));
        }
        let counts =
            count_launches(&mut conn, project_id, &tracked, &keys, LAUNCHES_SINCE_CAP).await?;
        let mut branch_list: Vec<ErrorBranch> = branch_keys
            .into_iter()
            .map(|(b, a, key)| ErrorBranch {
                branch: b,
                first_seen_at: a.first_seen_at,
                last_seen_at: a.last_seen_at,
                last_launch_id: a.last_launch_id,
                occurrences: a.occurrences,
                launches_since: counts.get(&key).copied().unwrap_or(0),
            })
            .collect();
        branch_list.sort_by(|a, b| {
            b.last_seen_at
                .cmp(&a.last_seen_at)
                .then_with(|| a.branch.cmp(&b.branch))
        });
        let tests: Vec<(Uuid, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select x.t, (array_agg(x.n order by x.at desc))[1], count(*)::bigint from \
               unnest($2::text[]) as m(f) cross join lateral (select r.test_case_id as t, r.name as n, \
                 r.launch_created_at as at from results r where r.project_id = $1 and r.fingerprint = m.f \
                 and r.launch_created_at >= $3 and r.status in {FAILING} \
                 order by r.launch_created_at desc limit $4) x \
             group by x.t order by 3 desc, 1 limit 20"
        )))
        .bind(project_id)
        .bind(&members)
        .bind(Utc::now() - Duration::days(TESTS_DAYS))
        .bind(enrich_rows_per_member(members.len()))
        .fetch_all(&mut *conn)
        .await?;
        let mut test_list: Vec<ErrorTest> = tests
            .into_iter()
            .map(|(t, n, c)| ErrorTest {
                test_case_id: t,
                name: n,
                occurrences: c,
            })
            .collect();
        test_list.sort_by(|a, b| {
            b.occurrences
                .cmp(&a.occurrences)
                .then_with(|| a.test_case_id.cmp(&b.test_case_id))
        });
        let decisions = memory_of(&mut conn, project_id, &members).await?;
        drop(conn);
        let to = Utc::now();
        let from = to - Duration::days(fastproof_analysis::ANALYTICS_PG_DAYS);
        let (window, timeline) = self
            .error_timeline_pg(project_id, fingerprint, from, to)
            .await?;
        let mttr_seconds = (state == ErrorState::Resolved)
            .then(|| {
                resolved_at.map(|r| (r - episode.unwrap_or(summary.first_seen_at)).num_seconds())
            })
            .flatten()
            .map(|s| s.max(0));
        let has_commits = first_commit.is_some() || last_good.is_some();
        let commits = has_commits.then_some(CommitRange {
            last_good,
            first: first_commit,
            compare_url: None,
        });
        Ok(ErrorDetail {
            summary,
            alias_list,
            branch_list,
            commits,
            mttr_seconds,
            decisions,
            test_list,
            timeline,
            window,
        })
    }

    /// The daily occurrences and launches of an error on PostgreSQL between `from` and `to`
    /// (at most [`fastproof_analysis::ANALYTICS_PG_DAYS`] days back from `to`; `partial` when
    /// the window was cut), every day listed, oldest first.
    pub async fn error_timeline_pg(
        &self,
        project_id: Uuid,
        fingerprint: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> AnalysisResult<(AnalyticsWindow, Vec<ErrorTimelinePoint>)> {
        if !super::rules::valid_fingerprint(fingerprint) {
            return Err(TmError::NotFound("error"));
        }
        if from >= to {
            return Err(TmError::bad_request("from must be before to"));
        }
        let floor = to - Duration::days(fastproof_analysis::ANALYTICS_PG_DAYS);
        let partial = from < floor;
        let from = from.max(floor);
        let mut conn = self.pool.acquire().await?;
        let exists: bool = sqlx::query_scalar(
            "select exists(select 1 from failure_fingerprints where project_id = $1 and fingerprint = $2)",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_one(&mut *conn)
        .await?;
        if !exists {
            return Err(TmError::NotFound("error"));
        }
        let members = members_of(&mut conn, project_id, &[fingerprint.to_string()])
            .await?
            .remove(fingerprint)
            .unwrap_or_default();
        let rows: Vec<(NaiveDate, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select (r.launch_created_at at time zone 'UTC')::date, count(*)::bigint, \
               count(distinct r.launch_id)::bigint from results r \
             where r.project_id = $1 and r.fingerprint = any($2) and r.launch_created_at >= $3 \
               and r.launch_created_at < $4 and r.status in {FAILING} group by 1"
        )))
        .bind(project_id)
        .bind(&members)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await?;
        let by_day: HashMap<NaiveDate, (i64, i64)> =
            rows.into_iter().map(|(d, o, l)| (d, (o, l))).collect();
        let mut items = Vec::new();
        let mut d = from.date_naive();
        let last = to.date_naive();
        while d <= last {
            let (occurrences, launches) = by_day.get(&d).copied().unwrap_or((0, 0));
            items.push(ErrorTimelinePoint {
                day: d,
                occurrences,
                launches,
            });
            d += Duration::days(1);
        }
        Ok((
            AnalyticsWindow {
                backend: AnalyticsBackend::Postgres,
                from,
                to,
                partial,
                reason: partial.then(|| {
                    "PostgreSQL keeps the last 30 days here; longer history needs ClickHouse".into()
                }),
            },
            items,
        ))
    }

    /// `PATCH .../errors/{fingerprint}`: ignore or activate an error, or make it an alias of
    /// another one (`null` splits it), under the errors lock; the states of the touched errors
    /// are computed again.
    pub async fn patch_error(
        &self,
        project_id: Uuid,
        fingerprint: &str,
        patch: &ErrorPatch,
        by: &str,
    ) -> AnalysisResult<ErrorDetail> {
        let _ = by;
        if !super::rules::valid_fingerprint(fingerprint) {
            return Err(TmError::NotFound("error"));
        }
        if patch.state.is_some() && matches!(patch.alias_of, Some(Some(_))) {
            return Err(TmError::bad_request(
                "give state or aliasOf: an alias has no state of its own",
            ));
        }
        let settings = self.project_settings(project_id).await?;
        let k = settings.analysis.resolve_after_launches;
        let mut tx = self.pool.begin().await?;
        lock(&mut tx, project_id).await?;
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "select state, alias_of from failure_fingerprints where project_id = $1 and fingerprint = $2",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((state, alias_of)) = row else {
            return Err(TmError::NotFound("error"));
        };
        let mut touched: Vec<String> = Vec::new();
        match &patch.alias_of {
            Some(Some(target)) => {
                if !super::rules::valid_fingerprint(target) {
                    return Err(TmError::bad_request("aliasOf must be 16 hex digits"));
                }
                let t: Option<Option<String>> = sqlx::query_scalar(
                    "select alias_of from failure_fingerprints where project_id = $1 and fingerprint = $2",
                )
                .bind(project_id)
                .bind(target)
                .fetch_optional(&mut *tx)
                .await?;
                let Some(t_alias) = t else {
                    return Err(TmError::conflict(
                        "alias_cycle",
                        "aliasOf is not an error of this project",
                    ));
                };
                let canon = t_alias.unwrap_or_else(|| target.clone());
                if canon == fingerprint {
                    return Err(TmError::conflict(
                        "alias_cycle",
                        "aliasOf would make the error an alias of itself",
                    ));
                }
                let mut rows = vec![fingerprint.to_string(), canon.clone()];
                rows.extend(alias_of.clone());
                lock_patch_rows(&mut tx, project_id, &rows, Some(fingerprint)).await?;
                sqlx::query(
                    "update failure_fingerprints set alias_of = $3 where project_id = $1 \
                       and (fingerprint = $2 or alias_of = $2)",
                )
                .bind(project_id)
                .bind(fingerprint)
                .bind(&canon)
                .execute(&mut *tx)
                .await?;
                touched.push(canon);
                if let Some(old) = alias_of {
                    touched.push(old);
                }
            }
            Some(None) => {
                if let Some(old) = alias_of {
                    lock_patch_rows(
                        &mut tx,
                        project_id,
                        &[fingerprint.to_string(), old.clone()],
                        None,
                    )
                    .await?;
                    sqlx::query(
                        "update failure_fingerprints set alias_of = null, state = 'active', state_at = null \
                         where project_id = $1 and fingerprint = $2",
                    )
                    .bind(project_id)
                    .bind(fingerprint)
                    .execute(&mut *tx)
                    .await?;
                    touched.push(old);
                    touched.push(fingerprint.to_string());
                }
            }
            None => {}
        }
        if let Some(s) = patch.state {
            let is_alias: bool = sqlx::query_scalar(
                "select alias_of is not null from failure_fingerprints where project_id = $1 and fingerprint = $2",
            )
            .bind(project_id)
            .bind(fingerprint)
            .fetch_one(&mut *tx)
            .await?;
            if is_alias {
                return Err(TmError::bad_request(
                    "an alias has no state of its own: change its canonical error",
                ));
            }
            lock_patch_rows(&mut tx, project_id, &[fingerprint.to_string()], None).await?;
            match s {
                ErrorPatchState::Ignored => {
                    sqlx::query(
                        "update failure_fingerprints set state = 'ignored', state_at = now() \
                         where project_id = $1 and fingerprint = $2",
                    )
                    .bind(project_id)
                    .bind(fingerprint)
                    .execute(&mut *tx)
                    .await?;
                }
                ErrorPatchState::Active => {
                    if state == "ignored" {
                        sqlx::query(
                            "update failure_fingerprints set state = 'active', state_at = null \
                             where project_id = $1 and fingerprint = $2",
                        )
                        .bind(project_id)
                        .bind(fingerprint)
                        .execute(&mut *tx)
                        .await?;
                    }
                    touched.push(fingerprint.to_string());
                }
            }
        }
        if !touched.is_empty() {
            touched.sort();
            touched.dedup();
            let tracked = tracked_branches(&mut tx, project_id, &settings.gates).await?;
            recompute(&mut tx, project_id, &touched, &tracked, k, Utc::now()).await?;
        }
        tx.commit().await?;
        self.error_detail(project_id, fingerprint).await
    }

    /// Queues an `errorRegressed` delivery unless the channel got one within `throttle`
    /// (checked and written under a per-channel lock). Returns whether it was queued.
    pub async fn queue_error_delivery(
        &self,
        d: &ErrorDelivery<'_>,
        throttle: std::time::Duration,
    ) -> anyhow::Result<bool> {
        let ErrorDelivery {
            project_id,
            launch_id,
            channel,
            target_id,
            payload,
            max_attempts,
        } = d;
        let (project_id, launch_id, max_attempts) = (*project_id, *launch_id, *max_attempts);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "select pg_advisory_xact_lock(hashtextextended('fastproof:error-notify:' || $1::text || ':' || $2, 0))",
        )
        .bind(project_id)
        .bind(no_nul(target_id))
        .execute(&mut *tx)
        .await?;
        let recent: bool = sqlx::query_scalar(
            "select exists (select 1 from integration_deliveries where project_id = $1 \
             and target_id = $2 and events @> array['errorRegressed'] \
             and created_at > now() - make_interval(secs => $3))",
        )
        .bind(project_id)
        .bind(no_nul(target_id))
        .bind(throttle.as_secs_f64())
        .fetch_one(&mut *tx)
        .await?;
        if recent {
            return Ok(false);
        }
        sqlx::query(
            "insert into integration_deliveries (id, project_id, launch_id, channel, target_id, events, payload, max_attempts) \
             values ($1, $2, $3, $4, $5, array['errorRegressed'], $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(launch_id)
        .bind(no_nul(channel))
        .bind(no_nul(target_id))
        .bind(crate::json_no_nul((*payload).clone()))
        .bind(max_attempts.max(1))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// An `errorRegressed` delivery to queue.
#[derive(Clone, Debug)]
pub struct ErrorDelivery<'a> {
    pub project_id: Uuid,
    pub launch_id: Uuid,
    pub channel: &'a str,
    pub target_id: &'a str,
    pub payload: &'a Value,
    pub max_attempts: i32,
}

/// The triage memory of an error's fingerprints (what people decided), most confirmed first.
async fn memory_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    members: &[String],
) -> anyhow::Result<Vec<MemoryEntry>> {
    let rows = sqlx::query(
        "select m.fingerprint, m.defect_type_id, m.grp, t.name, m.message_norm, m.frames, m.confirmed, \
           m.overridden, m.comment, m.issue_url, m.issue_key, cardinality(m.test_case_ids)::bigint, \
           m.branches, m.env_keys, m.first_decided_at, m.last_decided_at, m.last_decided_by, \
           m.sample_result_id, m.sample_launch_id, l.number \
         from triage_memory m left join defect_types t on t.id = m.defect_type_id \
         left join launches l on l.id = m.sample_launch_id \
         where m.project_id = $1 and m.fingerprint = any($2) \
         order by m.confirmed desc, m.last_decided_at desc limit 20",
    )
    .bind(project_id)
    .bind(members)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let confirmed: i32 = r.try_get(6)?;
        let overridden: i32 = r.try_get(7)?;
        let last_decided_at: DateTime<Utc> = r.try_get(15)?;
        let last_decided_by: String = r.try_get(16)?;
        out.push(MemoryEntry {
            fingerprint: r.try_get(0)?,
            defect_type_id: r.try_get(1)?,
            group: r.try_get(2)?,
            defect_name: r.try_get::<Option<String>, _>(3)?.unwrap_or_default(),
            message_norm: r.try_get(4)?,
            frames: r.try_get(5)?,
            confirmed,
            overridden,
            comment: r.try_get(8)?,
            issue_url: r.try_get(9)?,
            issue_key: r.try_get(10)?,
            test_cases: r.try_get(11)?,
            branches: r.try_get(12)?,
            environments: r.try_get(13)?,
            first_decided_at: r.try_get(14)?,
            last_decided_at,
            last_decided_by: last_decided_by.clone(),
            sample: Some(DecisionSample {
                result_id: r.try_get(17)?,
                launch_id: r.try_get(18)?,
                launch_number: r.try_get(19)?,
                decided_by: last_decided_by,
                decided_at: last_decided_at,
                confirmed,
                overridden,
            }),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_urls_for_github_and_gitlab() {
        let gl = serde_json::json!({"provider": "gitlab", "url": "https://gitlab.example.com/", "repository": "group/app"});
        assert_eq!(
            compare_url(&gl, "a1b2c3", "d4e5f6").as_deref(),
            Some("https://gitlab.example.com/group/app/-/compare/a1b2c3...d4e5f6")
        );
        let gh = serde_json::json!({"provider": "github", "url": "https://api.github.com", "repository": "owner/repo"});
        assert_eq!(
            compare_url(&gh, "a1b2c3", "d4e5f6").as_deref(),
            Some("https://github.com/owner/repo/compare/a1b2c3...d4e5f6")
        );
        let ghe = serde_json::json!({"provider": "github", "url": "https://github.example.com/api/v3", "repository": "o/r"});
        assert_eq!(
            compare_url(&ghe, "a", "b").as_deref(),
            Some("https://github.example.com/o/r/compare/a...b")
        );
        assert_eq!(compare_url(&gh, "a b", "c"), None);
        assert_eq!(compare_url(&gh, "a", "c?x=<script>"), None);
        let bad = serde_json::json!({"provider": "github", "url": "javascript:alert(1)", "repository": "o/r"});
        assert_eq!(compare_url(&bad, "a", "b"), None);
        let creds = serde_json::json!({"provider": "gitlab",
            "url": "https://oauth2:glpat-secret@gitlab.example.com/sub/?private_token=x#f", "repository": "g/app"});
        assert_eq!(
            compare_url(&creds, "a", "b").as_deref(),
            Some("https://gitlab.example.com/sub/g/app/-/compare/a...b")
        );
        let up = serde_json::json!({"provider": "gitlab", "url": "https://gitlab.example.com", "repository": "../../x"});
        assert_eq!(compare_url(&up, "a", "b"), None);
        let nohost =
            serde_json::json!({"provider": "gitlab", "url": "https://", "repository": "g/app"});
        assert_eq!(compare_url(&nohost, "a", "b"), None);
        assert_eq!(compare_url(&serde_json::json!({}), "a", "b"), None);
    }

    #[test]
    fn one_request_reads_a_bounded_number_of_results() {
        assert_eq!(enrich_rows_per_member(1), ENRICH_ROWS);
        assert_eq!(enrich_rows_per_member(0), ENRICH_ROWS);
        for n in [50usize, 500, 5_000, 200_000, 1_000_000] {
            let per = enrich_rows_per_member(n);
            assert!((1..=ENRICH_ROWS).contains(&per));
            assert!(
                per * n as i64 <= ENRICH_BUDGET.max(n as i64),
                "{n} fingerprints read {per} each"
            );
        }
    }

    #[test]
    fn buckets_and_baselines() {
        let t: BTreeSet<String> = ["main".to_string(), String::new()].into();
        assert_eq!(bucket("main", &t), "main");
        assert_eq!(bucket("", &t), "");
        assert_eq!(bucket("feature/x", &t), "*");
        let gates =
            vec![serde_json::json!({"rules": [{"baseline": "main"}, {"metric": "failed"}]})];
        assert_eq!(baselines(&gates), vec!["main".to_string()]);
        assert!(parse_cursor("12~0123456789abcdef").is_ok());
        assert!(parse_cursor("x~0123456789abcdef").is_err());
        assert!(parse_cursor("12~zz").is_err());
    }
}
