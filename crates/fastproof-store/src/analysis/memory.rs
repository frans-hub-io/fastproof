//! Triage memory (contract §7.2, §7.3): the fold of people's journal rows into `triage_memory`
//! (one row per project, canonical fingerprint and defect type) and `triage_memory_events` (the
//! history the backtest replays), the seed from triage made before the journal existed, and
//! the memory reads and decision-event export.
//!
//! Only decisions of people teach (`source = user`): rules, AI agents and auto-apply never do.
//! An undo teaches nothing and takes back what the undone batch taught. The row arithmetic is
//! [`fastproof_analysis::backtest::fold_row`], the same code the backtest folds with.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_analysis::backtest::{fold_row, new_row};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::decisions::triage_state;
use super::types::{
    DecisionEvent, DecisionSample, MEMORY_TEST_CASES, MESSAGE_NORM_CHARS, MemoryEntry, MemoryRow,
};
use crate::{Defect, Store, no_nul};

/// Journal rows read per fold transaction, at most.
pub const FOLD_BATCH: i64 = 2000;
/// Launches read per seed step.
pub const SEED_LAUNCHES: i64 = 20;
/// Results learned per seed step, about.
pub const SEED_RESULTS: usize = 1000;
/// Memory entries listed for one fingerprint, at most.
pub const MAX_MEMORY_ENTRIES: i64 = 50;
/// Decision events of one export page, at most.
pub const MAX_EVENTS_PAGE: i64 = 10_000;
/// The `analysis_state` name prefix of the per-project seed marker.
pub const SEED_MARKER: &str = "memory.seed:";
/// The `analysis_state` name prefix of the per-project secret the export hashes authors with.
pub const EXPORT_KEY: &str = "events.key:";

type JournalRow = (
    i64,
    Uuid,
    Uuid,
    DateTime<Utc>,
    Uuid,
    Option<String>,
    Uuid,
    String,
    String,
    DateTime<Utc>,
    Value,
    Value,
    Option<Uuid>,
    Option<Uuid>,
);

/// A launch the seed reads: id, creation time, environment, branch.
type SeedLaunch = (Uuid, DateTime<Utc>, Option<String>, Option<String>);
/// A person's decision the seed learns: result, test case, defect.
type SeedResult = (Uuid, Uuid, Defect);

/// What one group of decisions teaches.
#[derive(Clone, Debug)]
struct Learned {
    project_id: Uuid,
    batch_id: Uuid,
    via: String,
    fingerprint: String,
    defect_type_id: Uuid,
    group: String,
    message_norm: String,
    frames: Vec<String>,
    test_case_ids: Vec<Uuid>,
    env_key: Option<String>,
    branch: Option<String>,
    comment: Option<String>,
    issue_url: Option<String>,
    issue_key: Option<String>,
    sample_result_id: Uuid,
    sample_launch_id: Uuid,
    at: DateTime<Utc>,
    by: String,
    previous_type_id: Option<Uuid>,
    suggested_type_id: Option<Uuid>,
    suggested_band: Option<String>,
    results: i32,
}

fn cut(s: &str, chars: usize) -> String {
    match s.char_indices().nth(chars) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// The canonical fingerprint (`alias_of` when set, one level) and the catalog message of
/// fingerprints of one project.
async fn canonical(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<HashMap<String, (String, String)>> {
    if fps.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "select f.fingerprint, coalesce(f.alias_of, f.fingerprint), \
           coalesce((select c.message from failure_fingerprints c \
             where c.project_id = f.project_id and c.fingerprint = f.alias_of), f.message) \
         from failure_fingerprints f where f.project_id = $1 and f.fingerprint = any($2)",
    )
    .bind(project_id)
    .bind(fps)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(f, c, m)| (f, (c, cut(&m, MESSAGE_NORM_CHARS))))
        .collect())
}

/// Frames of the newest attempt of results (by id and launch time).
pub(crate) async fn frames_of_results(
    conn: &mut PgConnection,
    keys: &[(Uuid, DateTime<Utc>)],
) -> anyhow::Result<HashMap<Uuid, Vec<String>>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        "select distinct on (a.result_id) a.result_id, left(a.trace, 65536) from attempts a \
         join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
           on a.result_id = t.i and a.launch_created_at = t.c \
         order by a.result_id, a.seq desc",
    )
    .bind(keys.iter().map(|k| k.0).collect::<Vec<_>>())
    .bind(keys.iter().map(|k| k.1).collect::<Vec<_>>())
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(r, t)| {
            (
                r,
                t.map(|t| fastproof_analysis::frames(&t))
                    .unwrap_or_default(),
            )
        })
        .collect())
}

/// A `triage_memory` row as read ([`MEMORY_COLUMNS`]).
#[derive(sqlx::FromRow)]
pub(crate) struct MemoryDb {
    fingerprint: String,
    defect_type_id: Uuid,
    grp: String,
    message_norm: String,
    frames: Vec<String>,
    test_case_ids: Vec<Uuid>,
    env_keys: Vec<String>,
    branches: Vec<String>,
    comment: Option<String>,
    issue_url: Option<String>,
    issue_key: Option<String>,
    sample_result_id: Option<Uuid>,
    sample_launch_id: Option<Uuid>,
    confirmed: i32,
    overridden: i32,
    first_decided_at: DateTime<Utc>,
    last_decided_at: DateTime<Utc>,
    last_decided_by: String,
}

/// The columns of a [`MemoryDb`].
pub(crate) const MEMORY_COLUMNS: &str = "fingerprint, defect_type_id, grp, message_norm, frames, test_case_ids, env_keys, \
     branches, comment, issue_url, issue_key, sample_result_id, sample_launch_id, confirmed, overridden, \
     first_decided_at, last_decided_at, last_decided_by";

pub(crate) fn memory_row(r: MemoryDb) -> MemoryRow {
    MemoryRow {
        fingerprint: r.fingerprint,
        defect_type_id: r.defect_type_id,
        group: r.grp,
        message_norm: r.message_norm,
        frames: r.frames,
        test_case_ids: r.test_case_ids,
        env_keys: r.env_keys,
        branches: r.branches,
        comment: r.comment,
        issue_url: r.issue_url,
        issue_key: r.issue_key,
        sample_result_id: r.sample_result_id,
        sample_launch_id: r.sample_launch_id,
        confirmed: r.confirmed,
        overridden: r.overridden,
        first_decided_at: r.first_decided_at,
        last_decided_at: r.last_decided_at,
        last_decided_by: r.last_decided_by,
    }
}

/// Memory rows of a project by fingerprint (any of `fps`).
pub(crate) async fn memory_rows(
    conn: &mut PgConnection,
    project_id: Uuid,
    fps: &[String],
) -> anyhow::Result<Vec<MemoryRow>> {
    let rows: Vec<MemoryDb> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {MEMORY_COLUMNS} from triage_memory where project_id = $1 and fingerprint = any($2)"
    )))
    .bind(project_id)
    .bind(fps)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(memory_row).collect())
}

/// Folds one group: the memory row upserted (through the pure fold), the override counted on
/// the type it replaced, the event appended. Returns whether the row is new.
async fn learn(conn: &mut PgConnection, l: &Learned) -> anyhow::Result<bool> {
    let existing: Option<MemoryDb> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {MEMORY_COLUMNS} from triage_memory \
         where project_id = $1 and fingerprint = $2 and defect_type_id = $3 for update"
    )))
    .bind(l.project_id)
    .bind(&l.fingerprint)
    .bind(l.defect_type_id)
    .fetch_optional(&mut *conn)
    .await?;
    let ev = DecisionEvent {
        v: 1,
        at: l.at,
        fingerprint: l.fingerprint.clone(),
        message_norm: l.message_norm.clone(),
        frames: l.frames.clone(),
        test_case_id: l.test_case_ids.first().copied(),
        env_key: l.env_key.clone(),
        branch: l.branch.clone(),
        defect_type_id: l.defect_type_id,
        group: l.group.clone(),
        previous_type_id: l.previous_type_id,
        by: l.by.clone(),
        results: l.results.max(0) as u32,
    };
    let is_new = existing.is_none();
    let mut row = match existing {
        Some(r) => {
            let mut row = memory_row(r);
            fold_row(&mut row, &ev);
            row
        }
        None => new_row(&ev),
    };
    for t in l.test_case_ids.iter().skip(1).rev() {
        if !row.test_case_ids.contains(t) {
            row.test_case_ids.insert(1.min(row.test_case_ids.len()), *t);
        }
    }
    row.test_case_ids.truncate(MEMORY_TEST_CASES);
    if l.at >= row.last_decided_at {
        if l.comment.is_some() {
            row.comment = l.comment.clone();
        }
        if l.issue_url.is_some() {
            row.issue_url = l.issue_url.clone();
            row.issue_key = l.issue_key.clone();
        }
        row.sample_result_id = Some(l.sample_result_id);
        row.sample_launch_id = Some(l.sample_launch_id);
    } else if row.sample_result_id.is_none() {
        row.sample_result_id = Some(l.sample_result_id);
        row.sample_launch_id = Some(l.sample_launch_id);
    }
    sqlx::query(
        "insert into triage_memory (project_id, fingerprint, defect_type_id, grp, message_norm, frames, \
           test_case_ids, env_keys, branches, comment, issue_url, issue_key, sample_result_id, sample_launch_id, \
           confirmed, overridden, first_decided_at, last_decided_at, last_decided_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
         on conflict (project_id, fingerprint, defect_type_id) do update set grp = excluded.grp, \
           message_norm = excluded.message_norm, frames = excluded.frames, test_case_ids = excluded.test_case_ids, \
           env_keys = excluded.env_keys, branches = excluded.branches, comment = excluded.comment, \
           issue_url = excluded.issue_url, issue_key = excluded.issue_key, \
           sample_result_id = excluded.sample_result_id, sample_launch_id = excluded.sample_launch_id, \
           confirmed = excluded.confirmed, first_decided_at = excluded.first_decided_at, \
           last_decided_at = excluded.last_decided_at, last_decided_by = excluded.last_decided_by",
    )
    .bind(l.project_id)
    .bind(no_nul(&row.fingerprint))
    .bind(row.defect_type_id)
    .bind(&row.group)
    .bind(no_nul(&row.message_norm))
    .bind(row.frames.iter().map(|f| no_nul(f)).collect::<Vec<_>>())
    .bind(&row.test_case_ids)
    .bind(row.env_keys.iter().map(|e| no_nul(e)).collect::<Vec<_>>())
    .bind(row.branches.iter().map(|b| no_nul(b)).collect::<Vec<_>>())
    .bind(row.comment.as_deref().map(no_nul))
    .bind(row.issue_url.as_deref().map(no_nul))
    .bind(row.issue_key.as_deref().map(no_nul))
    .bind(row.sample_result_id)
    .bind(row.sample_launch_id)
    .bind(row.confirmed)
    .bind(row.overridden)
    .bind(row.first_decided_at)
    .bind(row.last_decided_at)
    .bind(no_nul(&row.last_decided_by))
    .execute(&mut *conn)
    .await?;
    if let Some(prev) = l.previous_type_id
        && prev != l.defect_type_id
    {
        sqlx::query(
            "update triage_memory set overridden = overridden + 1 \
             where project_id = $1 and fingerprint = $2 and defect_type_id = $3",
        )
        .bind(l.project_id)
        .bind(&l.fingerprint)
        .bind(prev)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query(
        "insert into triage_memory_events (project_id, decided_at, batch_id, fingerprint, defect_type_id, grp, \
           previous_type_id, message_norm, frames, test_case_id, env_key, branch, results, by, via, \
           suggested_type_id, suggested_band) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(l.project_id)
    .bind(l.at)
    .bind(l.batch_id)
    .bind(no_nul(&l.fingerprint))
    .bind(l.defect_type_id)
    .bind(&l.group)
    .bind(l.previous_type_id.filter(|p| *p != l.defect_type_id))
    .bind(no_nul(&l.message_norm))
    .bind(l.frames.iter().map(|f| no_nul(f)).collect::<Vec<_>>())
    .bind(l.test_case_ids.first().copied())
    .bind(l.env_key.as_deref().map(no_nul))
    .bind(l.branch.as_deref().map(no_nul))
    .bind(l.results)
    .bind(no_nul(&l.by))
    .bind(&l.via)
    .bind(l.suggested_type_id)
    .bind(&l.suggested_band)
    .execute(&mut *conn)
    .await?;
    Ok(is_new)
}

/// Takes back what a batch taught (it was undone): its events go, each confirmation and the
/// override it counted are subtracted, rows left without confirmations go.
async fn unlearn(conn: &mut PgConnection, project_id: Uuid, batch: Uuid) -> anyhow::Result<u64> {
    // The batch's events were decided within the times of its journal rows: the range keeps
    // the delete on the `(project_id, decided_at, id)` index instead of scanning every event.
    let (from, to): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "select min(at), max(at) from triage_decisions where project_id = $1 and batch_id = $2",
    )
    .bind(project_id)
    .bind(batch)
    .fetch_one(&mut *conn)
    .await?;
    let (Some(from), Some(to)) = (from, to) else {
        return Ok(0);
    };
    let gone: Vec<(String, Uuid, Option<Uuid>)> = sqlx::query_as(
        "delete from triage_memory_events where project_id = $1 and decided_at between $3 and $4 \
           and batch_id = $2 \
         returning fingerprint, defect_type_id, previous_type_id",
    )
    .bind(project_id)
    .bind(batch)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await?;
    for (fp, t, prev) in &gone {
        sqlx::query(
            "update triage_memory set confirmed = greatest(confirmed - 1, 0) \
             where project_id = $1 and fingerprint = $2 and defect_type_id = $3",
        )
        .bind(project_id)
        .bind(fp)
        .bind(t)
        .execute(&mut *conn)
        .await?;
        if let Some(p) = prev {
            sqlx::query(
                "update triage_memory set overridden = greatest(overridden - 1, 0) \
                 where project_id = $1 and fingerprint = $2 and defect_type_id = $3",
            )
            .bind(project_id)
            .bind(fp)
            .bind(p)
            .execute(&mut *conn)
            .await?;
        }
        sqlx::query(
            "delete from triage_memory where project_id = $1 and fingerprint = $2 and defect_type_id = $3 \
               and confirmed = 0",
        )
        .bind(project_id)
        .bind(fp)
        .bind(t)
        .execute(&mut *conn)
        .await?;
    }
    Ok(gone.len() as u64)
}

/// Drops the oldest memory rows of a project beyond `cap` (by the last decision).
async fn cap_memory(conn: &mut PgConnection, project_id: Uuid, cap: u64) -> anyhow::Result<u64> {
    Ok(sqlx::query(
        "delete from triage_memory where (project_id, fingerprint, defect_type_id) in \
           (select project_id, fingerprint, defect_type_id from triage_memory where project_id = $1 \
            order by last_decided_at desc, fingerprint, defect_type_id offset $2 limit 10000)",
    )
    .bind(project_id)
    .bind(cap.min(i64::MAX as u64) as i64)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

fn defect_of(state: &Value) -> Option<Defect> {
    triage_state(state).defect
}

/// What a group was suggested: the batch's accepted suggestion, or the open rank-1
/// suggestion of the cluster at the time of the decision.
async fn suggested_for(
    conn: &mut PgConnection,
    suggestion: Option<Uuid>,
    launch: Uuid,
    fingerprint: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<Option<(Uuid, String)>> {
    if let Some(id) = suggestion {
        let r: Option<(Uuid, String)> =
            sqlx::query_as("select defect_type_id, band from triage_suggestions where id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        if r.is_some() {
            return Ok(r);
        }
    }
    Ok(sqlx::query_as(
        "select defect_type_id, band from triage_suggestions where launch_id = $1 and fingerprint = $2 \
           and rank = 1 and created_at <= $3 and (state = 'open' or decided_at >= $3) \
         order by created_at desc limit 1",
    )
    .bind(launch)
    .bind(fingerprint)
    .bind(at)
    .fetch_optional(&mut *conn)
    .await?)
}

impl Store {
    /// Job `analysis.memory.fold`: folds unfolded journal rows of people (`source = user`) in id
    /// order, up to `max` rows per transaction, until none is left or `budget` is spent;
    /// memory per project is capped at `memory_max_rows`. Returns the journal rows read. Every
    /// read row gets `folded_at` in the same transaction, so a row is folded once.
    pub async fn fold_decisions(
        &self,
        max: i64,
        budget: Duration,
        memory_max_rows: u64,
    ) -> anyhow::Result<u64> {
        let started = Instant::now();
        let max = max.clamp(1, FOLD_BATCH);
        let mut n = 0u64;
        loop {
            let read = self.fold_once(max, memory_max_rows).await?;
            n += read;
            if (read as i64) < max || started.elapsed() >= budget {
                return Ok(n);
            }
        }
    }

    async fn fold_once(&self, max: i64, memory_max_rows: u64) -> anyhow::Result<u64> {
        let t = Instant::now();
        let mut tx = self.pool.begin().await?;
        let rows: Vec<JournalRow> = sqlx::query_as(
            "select id, project_id, launch_id, launch_created_at, result_id, fingerprint, batch_id, via, by, at, \
               before, after, suggestion_id, reverts_batch \
             from triage_decisions where folded_at is null and source = 'user' \
             order by id limit $1 for update skip locked",
        )
        .bind(max)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            tx.commit().await?;
            return Ok(0);
        }
        let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
        // Canonical fingerprints and catalog messages per project.
        let mut fps: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
        for r in &rows {
            if let Some(f) = &r.5 {
                fps.entry(r.1).or_default().insert(f.clone());
            }
        }
        let mut canon: HashMap<(Uuid, String), (String, String)> = HashMap::new();
        for (p, set) in &fps {
            let list: Vec<String> = set.iter().cloned().collect();
            for (f, v) in canonical(&mut tx, *p, &list).await? {
                canon.insert((*p, f), v);
            }
        }
        // Work in id order: groups of teaching rows, and undone batches.
        enum Op {
            Teach(Vec<usize>),
            Undo(Uuid, Uuid),
        }
        let mut ops: Vec<(i64, Op)> = Vec::new();
        let mut group_of: HashMap<(Uuid, String, Uuid), usize> = HashMap::new();
        let mut undone: BTreeSet<(Uuid, Uuid)> = BTreeSet::new();
        for (i, r) in rows.iter().enumerate() {
            if r.7 == "undo" {
                if let Some(b) = r.13
                    && undone.insert((r.1, b))
                {
                    ops.push((r.0, Op::Undo(r.1, b)));
                }
                continue;
            }
            let Some(fp) = &r.5 else {
                continue;
            };
            let Some(after) = defect_of(&r.11) else {
                continue;
            };
            if after.group == super::defects::TO_INVESTIGATE {
                continue;
            }
            let before = defect_of(&r.10);
            if before
                .as_ref()
                .is_some_and(|b| b.type_id == after.type_id && b.source == "user")
            {
                continue;
            }
            let c = canon
                .get(&(r.1, fp.clone()))
                .map(|c| c.0.clone())
                .unwrap_or_else(|| fp.clone());
            let key = (r.6, c, after.type_id);
            match group_of.get(&key) {
                Some(g) => {
                    if let Op::Teach(v) = &mut ops[*g].1 {
                        v.push(i);
                    }
                }
                None => {
                    group_of.insert(key, ops.len());
                    ops.push((r.0, Op::Teach(vec![i])));
                }
            }
        }
        ops.sort_by_key(|o| o.0);
        // Facts of the results: test cases, launches' environment and branch, frames.
        let keys: Vec<(Uuid, DateTime<Utc>)> = rows.iter().map(|r| (r.4, r.3)).collect();
        let tests: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select r.id, r.test_case_id from results r \
             join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) on r.id = t.i and r.launch_created_at = t.c",
        )
        .bind(keys.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind(keys.iter().map(|k| k.1).collect::<Vec<_>>())
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let launch_ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.2)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let launches: HashMap<Uuid, (Option<String>, Option<String>)> =
            sqlx::query_as::<_, (Uuid, Option<String>, Option<String>)>(
                "select id, env_key, branch from launches where id = any($1)",
            )
            .bind(&launch_ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(i, e, b)| (i, (e, b)))
            .collect();
        let samples: Vec<(Uuid, DateTime<Utc>)> = ops
            .iter()
            .filter_map(|o| match &o.1 {
                Op::Teach(v) => Some((rows[v[0]].4, rows[v[0]].3)),
                Op::Undo(..) => None,
            })
            .collect();
        let frames = frames_of_results(&mut tx, &samples).await?;
        // A previous journal row of a result that put the type it had before (an accepted
        // suggestion counts as one the person overrides).
        let prior: HashMap<Uuid, (String, Value)> = sqlx::query_as::<_, (Uuid, String, Value)>(
            "select distinct on (d.result_id) d.result_id, d.via, d.after from triage_decisions d \
             where d.result_id = any($1) and d.id < $2 order by d.result_id, d.id desc",
        )
        .bind(rows.iter().map(|r| r.4).collect::<Vec<_>>())
        .bind(ids[0])
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(r, v, a)| (r, (v, a)))
        .collect();
        let mut touched: BTreeSet<Uuid> = BTreeSet::new();
        for (_, op) in &ops {
            match op {
                Op::Undo(p, b) => {
                    unlearn(&mut tx, *p, *b).await?;
                }
                Op::Teach(members) => {
                    let first = &rows[members[0]];
                    let after = defect_of(&first.11).expect("filtered");
                    let fp = first.5.clone().unwrap_or_default();
                    let (c, message) = canon
                        .get(&(first.1, fp.clone()))
                        .cloned()
                        .unwrap_or_else(|| (fp.clone(), String::new()));
                    let mut previous: Option<Uuid> = None;
                    for &m in members {
                        let r = &rows[m];
                        let Some(b) = defect_of(&r.10) else {
                            continue;
                        };
                        if b.type_id == after.type_id || b.group == super::defects::TO_INVESTIGATE {
                            continue;
                        }
                        let machine = matches!(b.source.as_str(), "rule" | "ai" | "auto")
                            && (b.source != "rule" || b.rule_id.is_some());
                        let accepted = prior
                            .get(&r.4)
                            .is_some_and(|(via, a)| via == "suggestion" && *a == r.10);
                        if machine || accepted {
                            previous = Some(b.type_id);
                            break;
                        }
                    }
                    let suggested = suggested_for(&mut tx, first.12, first.2, &c, first.9).await?;
                    if previous.is_none()
                        && let Some((t, _)) = &suggested
                        && *t != after.type_id
                    {
                        previous = Some(*t);
                    }
                    let mut test_case_ids: Vec<Uuid> = Vec::new();
                    for &m in members {
                        if let Some(t) = tests.get(&rows[m].4)
                            && !test_case_ids.contains(t)
                        {
                            test_case_ids.push(*t);
                        }
                    }
                    let (env_key, branch) = launches.get(&first.2).cloned().unwrap_or_default();
                    let learned = Learned {
                        project_id: first.1,
                        batch_id: first.6,
                        via: first.7.clone(),
                        fingerprint: c,
                        defect_type_id: after.type_id,
                        group: after.group.clone(),
                        message_norm: message,
                        frames: frames.get(&first.4).cloned().unwrap_or_default(),
                        test_case_ids,
                        env_key,
                        branch,
                        comment: after.comment.clone(),
                        issue_url: after.issue_url.clone(),
                        issue_key: after.issue_key.clone(),
                        sample_result_id: first.4,
                        sample_launch_id: first.2,
                        at: members.iter().map(|m| rows[*m].9).max().unwrap_or(first.9),
                        by: first.8.clone(),
                        previous_type_id: previous,
                        suggested_type_id: suggested.as_ref().map(|s| s.0),
                        suggested_band: suggested.map(|s| s.1),
                        results: members.len() as i32,
                    };
                    if learn(&mut tx, &learned).await? {
                        touched.insert(first.1);
                    }
                }
            }
        }
        for p in touched {
            cap_memory(&mut tx, p, memory_max_rows).await?;
        }
        sqlx::query("update triage_decisions set folded_at = now() where id = any($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        metrics::histogram!("fastproof_analysis_job_seconds", "job" => "memory.fold.batch")
            .record(t.elapsed().as_secs_f64());
        Ok(rows.len() as u64)
    }

    /// Projects with Analysis on whose memory was never seeded (no done marker), oldest
    /// first (projects with Analysis off do not hold the others back).
    pub async fn memory_seed_pending(&self, limit: i64) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select p.id from projects p where not exists (select 1 from analysis_state s \
               where s.name = $1 || p.id::text and (s.value->>'done')::boolean) \
               and (p.settings->'analysis'->'enabled') is distinct from 'false'::jsonb \
             order by p.created_at, p.id limit $2",
        )
        .bind(SEED_MARKER)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Job `analysis.memory.seed` for one project: results whose defect a person chose (and
    /// that the journal does not hold) become memory rows and events, launch by launch in
    /// creation order, from the cursor of the project's marker, until `budget` is spent.
    /// Returns whether the project is done (the marker then says so and it is never seeded
    /// again).
    pub async fn seed_triage_memory(
        &self,
        project_id: Uuid,
        limits: &super::types::AnalysisLimits,
        budget: Duration,
    ) -> anyhow::Result<bool> {
        let started = Instant::now();
        let name = format!("{SEED_MARKER}{project_id}");
        let marker: Option<Value> =
            sqlx::query_scalar("select value from analysis_state where name = $1")
                .bind(&name)
                .fetch_optional(&self.pool)
                .await?;
        let marker = marker.unwrap_or_else(|| serde_json::json!({}));
        if marker.get("done").and_then(Value::as_bool) == Some(true) {
            return Ok(true);
        }
        let mut cursor: Option<(DateTime<Utc>, Uuid)> = marker
            .get("cursor")
            .and_then(|c| serde_json::from_value(c.clone()).ok());
        loop {
            let launches: Vec<SeedLaunch> = sqlx::query_as(
                "select id, created_at, env_key, branch from launches where project_id = $1 \
                       and ($2::timestamptz is null or (created_at, id) > ($2, $3)) \
                     order by created_at, id limit $4",
            )
            .bind(project_id)
            .bind(cursor.map(|c| c.0))
            .bind(cursor.map(|c| c.1).unwrap_or_default())
            .bind(SEED_LAUNCHES)
            .fetch_all(&self.pool)
            .await?;
            let done = (launches.len() as i64) < SEED_LAUNCHES;
            let mut tx = self.pool.begin().await?;
            let mut learned = 0usize;
            for (lid, created, env, branch) in &launches {
                let results: Vec<(Uuid, Uuid, String, Value)> = sqlx::query_as(
                    "select r.id, r.test_case_id, r.fingerprint, r.defect from results r \
                     where r.launch_id = $1 and r.launch_created_at = $2 and r.fingerprint is not null \
                       and r.defect->>'source' = 'user' and r.defect->>'group' <> 'toInvestigate' \
                       and not exists (select 1 from triage_decisions d where d.result_id = r.id \
                         and d.source = 'user') \
                     order by r.id",
                )
                .bind(lid)
                .bind(created)
                .fetch_all(&mut *tx)
                .await?;
                if results.is_empty() {
                    continue;
                }
                let fps: Vec<String> = results
                    .iter()
                    .map(|r| r.2.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let canon = canonical(&mut tx, project_id, &fps).await?;
                let mut groups: BTreeMap<(String, Uuid), Vec<SeedResult>> = BTreeMap::new();
                for (rid, tc, fp, d) in results {
                    let Ok(d) = serde_json::from_value::<Defect>(d) else {
                        continue;
                    };
                    let c = canon.get(&fp).map(|c| c.0.clone()).unwrap_or(fp);
                    groups.entry((c, d.type_id)).or_default().push((rid, tc, d));
                }
                let samples: Vec<(Uuid, DateTime<Utc>)> =
                    groups.values().map(|v| (v[0].0, *created)).collect();
                let frames = frames_of_results(&mut tx, &samples).await?;
                for ((fp, t), members) in groups {
                    let newest = members
                        .iter()
                        .max_by_key(|m| m.2.at)
                        .map(|m| m.2.clone())
                        .expect("non-empty");
                    let message = canon
                        .values()
                        .find(|(c, _)| *c == fp)
                        .map(|(_, m)| m.clone())
                        .unwrap_or_default();
                    let mut tests: Vec<Uuid> = Vec::new();
                    for m in &members {
                        if !tests.contains(&m.1) {
                            tests.push(m.1);
                        }
                    }
                    let l = Learned {
                        project_id,
                        batch_id: Uuid::now_v7(),
                        via: "api".into(),
                        fingerprint: fp,
                        defect_type_id: t,
                        group: newest.group.clone(),
                        message_norm: message,
                        frames: frames.get(&members[0].0).cloned().unwrap_or_default(),
                        test_case_ids: tests,
                        env_key: env.clone(),
                        branch: branch.clone(),
                        comment: newest.comment.clone(),
                        issue_url: newest.issue_url.clone(),
                        issue_key: newest.issue_key.clone(),
                        sample_result_id: members[0].0,
                        sample_launch_id: *lid,
                        at: newest.at,
                        by: newest.by.clone(),
                        previous_type_id: None,
                        suggested_type_id: None,
                        suggested_band: None,
                        results: members.len() as i32,
                    };
                    learn(&mut tx, &l).await?;
                    learned += members.len();
                }
            }
            if learned > 0 {
                cap_memory(&mut tx, project_id, limits.memory_max_rows).await?;
            }
            if let Some(last) = launches.last() {
                cursor = Some((last.1, last.0));
            }
            sqlx::query(
                "insert into analysis_state (name, value, updated_at) values ($1, $2, now()) \
                 on conflict (name) do update set value = excluded.value, updated_at = now()",
            )
            .bind(&name)
            .bind(serde_json::json!({"cursor": cursor, "done": done}))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            if done {
                return Ok(true);
            }
            if started.elapsed() >= budget {
                return Ok(false);
            }
        }
    }

    /// Memory entries of a fingerprint (its canonical fingerprint and aliases), the most
    /// confirmed first.
    pub async fn memory_entries(
        &self,
        project_id: Uuid,
        fingerprint: &str,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut conn = self.pool.acquire().await?;
        let c = canonical(&mut conn, project_id, &[fingerprint.to_string()])
            .await?
            .remove(fingerprint)
            .map(|c| c.0)
            .unwrap_or_else(|| fingerprint.to_string());
        let mut fps: Vec<String> = sqlx::query_scalar(
            "select fingerprint from failure_fingerprints where project_id = $1 and alias_of = $2 limit 100",
        )
        .bind(project_id)
        .bind(&c)
        .fetch_all(&mut *conn)
        .await?;
        fps.push(c);
        fps.push(fingerprint.to_string());
        let mut rows = memory_rows(&mut conn, project_id, &fps).await?;
        rows.sort_by(|a, b| {
            b.confirmed
                .cmp(&a.confirmed)
                .then(b.last_decided_at.cmp(&a.last_decided_at))
                .then(a.defect_type_id.cmp(&b.defect_type_id))
        });
        rows.truncate(MAX_MEMORY_ENTRIES as usize);
        drop(conn);
        self.entries_of(project_id, rows).await
    }

    /// Memory rows as the API shows them (type names, samples with their launch number).
    pub(crate) async fn entries_of(
        &self,
        project_id: Uuid,
        rows: Vec<MemoryRow>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let names: HashMap<Uuid, String> = self
            .defect_types(project_id)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect();
        let launches: Vec<Uuid> = rows.iter().filter_map(|r| r.sample_launch_id).collect();
        let numbers: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
            "select id, number from launches where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(&launches)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        Ok(rows
            .into_iter()
            .map(|r| MemoryEntry {
                defect_name: names.get(&r.defect_type_id).cloned().unwrap_or_default(),
                sample: Some(DecisionSample {
                    result_id: r.sample_result_id,
                    launch_id: r.sample_launch_id,
                    launch_number: r.sample_launch_id.and_then(|l| numbers.get(&l).copied()),
                    decided_by: r.last_decided_by.clone(),
                    decided_at: r.last_decided_at,
                    confirmed: r.confirmed,
                    overridden: r.overridden,
                }),
                test_cases: r.test_case_ids.len() as i64,
                fingerprint: r.fingerprint,
                defect_type_id: r.defect_type_id,
                group: r.group,
                message_norm: r.message_norm,
                frames: r.frames,
                confirmed: r.confirmed,
                overridden: r.overridden,
                comment: r.comment,
                issue_url: r.issue_url,
                issue_key: r.issue_key,
                branches: r.branches,
                environments: r.env_keys,
                first_decided_at: r.first_decided_at,
                last_decided_at: r.last_decided_at,
                last_decided_by: r.last_decided_by,
            })
            .collect())
    }

    /// A page of a project's decision events in id order after `after` (the export; `by` is
    /// the stored author, hashed by the caller).
    pub async fn decision_events(
        &self,
        project_id: Uuid,
        after: Option<i64>,
        limit: i64,
    ) -> anyhow::Result<Vec<(i64, DecisionEvent)>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, decided_at, fingerprint, message_norm, frames, test_case_id, env_key, branch, \
               defect_type_id, grp, previous_type_id, by, results \
             from triage_memory_events where project_id = $1 and id > $2 order by id limit $3",
        )
        .bind(project_id)
        .bind(after.unwrap_or(0))
        .bind(limit.clamp(1, MAX_EVENTS_PAGE))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.0, event_of(r))).collect())
    }
}

pub(crate) type EventRow = (
    i64,
    DateTime<Utc>,
    String,
    String,
    Vec<String>,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    Uuid,
    String,
    Option<Uuid>,
    String,
    i32,
);

pub(crate) fn event_of(r: EventRow) -> DecisionEvent {
    DecisionEvent {
        v: 1,
        at: r.1,
        fingerprint: r.2,
        message_norm: r.3,
        frames: r.4,
        test_case_id: r.5,
        env_key: r.6,
        branch: r.7,
        defect_type_id: r.8,
        group: r.9,
        previous_type_id: r.10,
        by: r.11,
        results: r.12.max(0) as u32,
    }
}

/// The exported author of a decision: the first 16 hex digits of sha256 of
/// `"<project secret>:<by>"` ([`Store::export_key`]). The same person has the same hash
/// within a project and none outside it, and nobody without the database can test a guessed
/// name against it (the project id alone, which every reader knows, would allow that).
pub fn hashed_by(key: &str, by: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(format!("{key}:{by}").as_bytes());
    hex::encode(&d[..8])
}

impl Store {
    /// The project's secret for [`hashed_by`]: 244 random bits made on first use and kept in
    /// `analysis_state`; never shown by the API.
    pub async fn export_key(&self, project_id: Uuid) -> anyhow::Result<String> {
        let name = format!("{EXPORT_KEY}{project_id}");
        let fresh = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sqlx::query(
            "insert into analysis_state (name, value, updated_at) values ($1, $2, now()) \
             on conflict (name) do nothing",
        )
        .bind(&name)
        .bind(serde_json::json!({ "key": fresh }))
        .execute(&self.pool)
        .await?;
        let v: Value = sqlx::query_scalar("select value from analysis_state where name = $1")
            .bind(&name)
            .fetch_one(&self.pool)
            .await?;
        v.get("key")
            .and_then(Value::as_str)
            .filter(|k| k.len() >= 32)
            .map(str::to_string)
            .ok_or_else(|| {
                anyhow::anyhow!("the export key of project {project_id} is not readable")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authors_are_hashed_per_project() {
        let k = "0123456789abcdef0123456789abcdef";
        let a = hashed_by(k, "ann");
        assert_eq!(a.len(), 16);
        assert_eq!(a, hashed_by(k, "ann"));
        assert_ne!(a, hashed_by("fedcba9876543210fedcba9876543210", "ann"));
        assert_ne!(a, hashed_by(k, "bob"));
        assert_eq!(cut("abcdef", 3), "abc");
    }
}
