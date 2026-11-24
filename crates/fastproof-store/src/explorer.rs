//! Explorer data kept beside the results: `results.label_values` (each label name with its
//! values, read by the tree instead of walking `labels`), `results.suite_tree` and
//! `suite_titled` (the suite path of the result in the tree), a cache of the explorer aggregates
//! of launches that do not run, and the background job that fills `label_values` and
//! `attachment_kinds` for results stored before they existed.
//!
//! The cache is checked against `explorer_gen`, a counter per launch that every transaction
//! writing the launch's results raises when it commits (the deferred trigger
//! `results_explorer_gen`, so writers of earlier versions and hand-written SQL count too; the
//! counter row is locked last, so it adds no lock cycle; [`touch`] for changes without a results
//! write, such as dropped partitions). A reader takes the
//! counter and the data in one snapshot and stores the counter with its payload, so an entry
//! built before a write commits is never served after it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_model::{AttachmentRef, Step};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::ingest::AttemptDetail;

/// The `label_values` of a result: each label name with its values in their order (the SQL
/// function `fastproof_label_values` computes the same from `labels`).
pub fn label_values(labels: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for e in labels.as_array().into_iter().flatten() {
        let Some(name) = e.get("name").and_then(Value::as_str) else {
            continue;
        };
        let value = e.get("value").cloned().unwrap_or_else(|| json!(""));
        if let Value::Array(a) = m
            .entry(name.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            a.push(value);
        }
    }
    Value::Object(m)
}

/// Marks the explorer data of the launches changed, for changes the results triggers do not
/// see (a dropped partition). The counter rows stay locked until the commit.
pub async fn touch(conn: &mut PgConnection, launches: &[Uuid]) -> sqlx::Result<()> {
    if launches.is_empty() {
        return Ok(());
    }
    let mut ids = launches.to_vec();
    ids.sort();
    ids.dedup();
    sqlx::query(
        "insert into explorer_gen (launch_id) select l.id from launches l where l.id = any($1) \
         order by l.id on conflict (launch_id) do update set gen = explorer_gen.gen + 1",
    )
    .bind(&ids)
    .execute(conn)
    .await?;
    Ok(())
}

/// Marks the caller's transaction as one that writes `label_values` and `suite_tree` itself:
/// the trigger `results_explorer_keys` recomputes or clears them only for writers that do not
/// (replicas of earlier versions, hand-written SQL).
pub async fn app_writes_keys(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("select set_config('fastproof.explorer_keys', 'app', true)")
        .execute(conn)
        .await?;
    Ok(())
}

/// The counter of a launch as the caller's snapshot sees it.
pub(crate) async fn gen_of(conn: &mut PgConnection, launch: Uuid) -> sqlx::Result<i64> {
    Ok(
        sqlx::query_scalar("select gen from explorer_gen where launch_id = $1")
            .bind(launch)
            .fetch_optional(conn)
            .await?
            .unwrap_or(0),
    )
}

/// A cached payload that is still valid for the counter `generation`.
pub(crate) async fn cache_get(
    conn: &mut PgConnection,
    launch: Uuid,
    key: &str,
    generation: i64,
) -> sqlx::Result<Option<Value>> {
    sqlx::query_scalar(
        "select payload from explorer_cache where launch_id = $1 and key = $2 and gen = $3",
    )
    .bind(launch)
    .bind(key)
    .bind(generation)
    .fetch_optional(conn)
    .await
}

/// Cached entries per launch, at most (the oldest go first).
pub const MAX_CACHE_ENTRIES: i64 = 32;
/// Bytes of one cached payload, at most (larger ones are not cached).
pub const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

impl Store {
    /// Stores a payload computed at the counter `generation` (a newer entry is kept). Best
    /// effort: a failure is logged and counted, and the caller still answers with its value.
    pub(crate) async fn cache_put(
        &self,
        launch: Uuid,
        key: &str,
        generation: i64,
        payload: &Value,
    ) {
        if let Err(e) = self.try_cache_put(launch, key, generation, payload).await {
            metrics::counter!("fastproof_explorer_cache_write_errors_total").increment(1);
            tracing::warn!(launch = %launch, error = %format!("{e:#}"), "storing explorer aggregates failed");
        }
    }

    async fn try_cache_put(
        &self,
        launch: Uuid,
        key: &str,
        generation: i64,
        payload: &Value,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(payload)?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "insert into explorer_cache (launch_id, key, gen, payload) \
             select $1, $2, $3, $4 where exists (select 1 from launches where id = $1) \
             on conflict (launch_id, key) do update set gen = excluded.gen, payload = excluded.payload, \
               created_at = now(), warmed = false where explorer_cache.gen <= excluded.gen",
        )
        .bind(launch)
        .bind(key)
        .bind(generation)
        .bind(crate::json_no_nul(payload.clone()))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "delete from explorer_cache where launch_id = $1 and key in (select key from explorer_cache \
             where launch_id = $1 order by created_at desc, key offset $2)",
        )
        .bind(launch)
        .bind(MAX_CACHE_ENTRIES)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes cache entries older than `age`, and entries built ahead by
    /// [`Store::warm_explorer_trees`] that nobody read within [`WARMED_TTL`]; returns how many.
    pub async fn prune_explorer_cache(&self, age: Duration) -> anyhow::Result<u64> {
        let age = chrono::Duration::from_std(age).unwrap_or(chrono::Duration::days(7));
        let warmed = chrono::Duration::from_std(WARMED_TTL).unwrap_or(chrono::Duration::days(1));
        Ok(sqlx::query(
            "delete from explorer_cache where ctid = any(array(select ctid from explorer_cache \
             where created_at < $1 or (warmed and created_at < $2) limit 10000))",
        )
        .bind(Utc::now() - age)
        .bind(Utc::now() - warmed)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Marks a cache entry as read (a tree built ahead that someone opened is kept like any
    /// other). Best effort, and a no-op for entries not built ahead.
    pub(crate) async fn cache_read(&self, launch: Uuid, key: &str) {
        let _ = sqlx::query(
            "update explorer_cache set warmed = false where launch_id = $1 and key = $2 and warmed",
        )
        .bind(launch)
        .bind(key)
        .execute(&self.pool)
        .await;
    }
}

/// Where the backfill is, in `analysis_state`.
pub const BACKFILL_STATE: &str = "explorer.results.backfill";
/// The migration that added `results.attachment_kinds`: results of launches created before
/// it was applied may have 0 there although they have attachments.
pub const ATTACHMENT_KINDS_VERSION: i64 = 600;

/// What [`Store::backfill_explorer`] did in one call, and where the whole job is.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackfillProgress {
    /// Results looked at in this call.
    pub scanned: u64,
    /// Results changed in this call.
    pub updated: u64,
    /// Results looked at since the job started.
    pub total_scanned: u64,
    /// Results changed since the job started.
    pub total_updated: u64,
    pub done: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct BackfillState {
    cursor: Option<Uuid>,
    /// The walk by id reached the end.
    #[serde(default)]
    walked: bool,
    /// Rows another transaction held when the walk passed them.
    #[serde(default)]
    skipped: Vec<(Uuid, DateTime<Utc>)>,
    #[serde(default)]
    scanned: u64,
    #[serde(default)]
    updated: u64,
    #[serde(default)]
    done: bool,
}

/// The attachment bits of an attempt: its attachments and those of its steps (as ingest
/// computes them; fixtures of containers are not counted there either).
fn attempt_kinds(attachments: &[AttachmentRef], steps: &[Step]) -> i32 {
    fn walk(list: &[Step], m: &mut i32) {
        for s in list {
            for a in &s.attachments {
                *m |= crate::results::kind_bit(a.kind);
            }
            walk(&s.steps, m);
        }
    }
    let mut m = attachments
        .iter()
        .fold(0, |m, a| m | crate::results::kind_bit(a.kind));
    walk(steps, &mut m);
    m
}

/// Cursor of [`Store::warm_explorer_trees`] in `analysis_state`.
pub const WARM_STATE: &str = "explorer.warm";
/// How long a tree built ahead is kept when nobody opens it.
pub const WARMED_TTL: Duration = Duration::from_secs(24 * 3600);
/// Launches looked at per run of [`Store::warm_explorer_trees`], at most (the cursor moves
/// over every one, built or not).
const WARM_SCAN: i64 = 1000;
/// Launches waiting to be built (uploads still processing, a merge still analysing), at most.
const WARM_RETRY: usize = 500;
/// How long a launch waits in that list before it is given up.
const WARM_RETRY_FOR: chrono::Duration = chrono::Duration::hours(1);

/// Where the warming of finished launches is: the finish time and id of the last launch
/// looked at, the creation time of the last merge looked at, and launches to look at again.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WarmState {
    at: Option<DateTime<Utc>>,
    id: Option<Uuid>,
    #[serde(default)]
    merges_at: Option<DateTime<Utc>>,
    /// Launch, project, when it was first put here.
    #[serde(default)]
    retry: Vec<(Uuid, Uuid, DateTime<Utc>)>,
}

/// A launch as [`Store::warm_explorer_trees`] judges it.
#[derive(sqlx::FromRow)]
struct WarmRow {
    id: Uuid,
    project_id: Uuid,
    at: Option<DateTime<Utc>>,
    total: i32,
    /// Finished, not merged into another launch, of a live project.
    eligible: bool,
    /// Uploads still processing, or a merge into it still analysing.
    busy: bool,
}

const WARM_COLUMNS: &str = "l.id, l.project_id, l.finished_at as at, l.count_total as total, \
    (l.status <> 'running' and l.merged_into is null and p.deleted_at is null) as eligible, \
    (exists (select 1 from uploads u where u.launch_id = l.id and u.status in ('queued', 'processing')) \
     or exists (select 1 from launch_merges m join launch_merge_work w on w.merge_id = m.id \
       where m.target_id = l.id)) as busy \
    from launches l join projects p on p.id = l.project_id";

impl Store {
    /// Builds the unfiltered Suites tree (what a launch page shows first) of launches with at
    /// least `min_results` results, so the first person to open a large launch reads it from
    /// the cache.
    ///
    /// Walks launches that finished at least `settle` ago by finish time, and new merges by
    /// creation time (their target), from cursors in `analysis_state` ([`WARM_STATE`]; the
    /// first run starts at the present). The cursors move over every launch looked at, built
    /// or not, so the walk never repeats. A launch whose uploads are still processing, or
    /// whose merge is still analysing, waits in a list and is looked at again on later runs
    /// (up to an hour). At most `limit` trees or `budget` per call; a tree that fails is
    /// logged and skipped. Trees nobody opens go after [`WARMED_TTL`]. Returns how many trees
    /// were built.
    pub async fn warm_explorer_trees(
        &self,
        settle: Duration,
        min_results: i64,
        limit: i64,
        budget: Duration,
    ) -> anyhow::Result<u64> {
        let started = Instant::now();
        let raw: Option<Value> =
            sqlx::query_scalar("select value from analysis_state where name = $1")
                .bind(WARM_STATE)
                .fetch_optional(&self.pool)
                .await?;
        let parsed = raw
            .as_ref()
            .and_then(|v| serde_json::from_value::<WarmState>(v.clone()).ok());
        let Some(mut state) = parsed else {
            if raw.is_some() {
                tracing::warn!(
                    "the state of explorer.warm was unreadable; it starts again from now"
                );
            }
            sqlx::query(
                "insert into analysis_state (name, value) \
                 values ($1, jsonb_build_object('at', now() - make_interval(secs => $2), \
                   'merges_at', now() - make_interval(secs => $2))) \
                 on conflict (name) do update set value = excluded.value, updated_at = now()",
            )
            .bind(WARM_STATE)
            .bind(settle.as_secs_f64())
            .execute(&self.pool)
            .await?;
            return Ok(0);
        };
        let settle_s = settle.as_secs_f64();
        let now: DateTime<Utc> = sqlx::query_scalar("select now()")
            .fetch_one(&self.pool)
            .await?;
        // Finished launches after the cursor.
        let scanned: Vec<WarmRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {WARM_COLUMNS} where l.finished_at is not null \
               and l.finished_at <= now() - make_interval(secs => $1) \
               and (l.finished_at, l.id) > (coalesce($2, '-infinity'::timestamptz), \
                 coalesce($3, '00000000-0000-0000-0000-000000000000'::uuid)) \
             order by l.finished_at, l.id limit $4"
        )))
        .bind(settle_s)
        .bind(state.at)
        .bind(state.id)
        .bind(WARM_SCAN)
        .fetch_all(&self.pool)
        .await?;
        if let Some(last) = scanned.last() {
            state.at = last.at;
            state.id = Some(last.id);
        }
        // Targets of merges made since the last run join the list (their finish time is the
        // sources', usually behind the cursor).
        let merges: Vec<(Uuid, Uuid, DateTime<Utc>)> = sqlx::query_as(
            "select target_id, project_id, created_at from launch_merges \
             where created_at > coalesce($1, '-infinity'::timestamptz) \
               and created_at <= now() - make_interval(secs => $2) \
             order by created_at limit $3",
        )
        .bind(state.merges_at)
        .bind(settle_s)
        .bind(WARM_SCAN)
        .fetch_all(&self.pool)
        .await?;
        if let Some(last) = merges.last() {
            state.merges_at = Some(last.2);
        }
        let mut retry: Vec<(Uuid, Uuid, DateTime<Utc>)> = std::mem::take(&mut state.retry)
            .into_iter()
            .filter(|r| now - r.2 < WARM_RETRY_FOR)
            .collect();
        retry.extend(merges.iter().map(|m| (m.0, m.1, now)));
        let again: Vec<WarmRow> = if retry.is_empty() {
            Vec::new()
        } else {
            let ids: Vec<Uuid> = retry.iter().map(|r| r.0).collect();
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select {WARM_COLUMNS} where l.id = any($1)"
            )))
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?
        };
        let since: HashMap<Uuid, DateTime<Utc>> = retry.iter().map(|r| (r.0, r.2)).collect();
        let mut waiting: Vec<(Uuid, Uuid, DateTime<Utc>)> = Vec::new();
        let mut due: Vec<(Uuid, Uuid, DateTime<Utc>)> = Vec::new();
        let mut seen = HashSet::new();
        for r in again.iter().chain(scanned.iter()) {
            if !r.eligible || !seen.insert(r.id) {
                continue;
            }
            let first = since.get(&r.id).copied().unwrap_or(now);
            if r.busy {
                waiting.push((r.id, r.project_id, first));
            } else if r.total as i64 >= min_results {
                due.push((r.id, r.project_id, first));
            }
        }
        let mut built = 0;
        for (i, (id, project_id, _)) in due.iter().copied().enumerate() {
            if started.elapsed() > budget || built >= limit as u64 {
                waiting.extend(due[i..].iter().copied());
                break;
            }
            let Some(launch) = self.launch(project_id, id).await? else {
                continue;
            };
            let o = crate::results::TreeOptions {
                include_tests: true,
                ..crate::results::TreeOptions::groups(crate::results::GroupBy::Suite)
            };
            let t0: DateTime<Utc> = sqlx::query_scalar("select now()")
                .fetch_one(&self.pool)
                .await?;
            match self.tree_with(&launch, &o).await {
                Ok(_) => {
                    built += 1;
                    let _ = sqlx::query(
                        "update explorer_cache set warmed = true \
                         where launch_id = $1 and key = $2 and created_at >= $3",
                    )
                    .bind(id)
                    .bind(crate::results::default_tree_cache_key())
                    .bind(t0)
                    .execute(&self.pool)
                    .await;
                }
                Err(e) => {
                    tracing::warn!(launch = %id, error = %format!("{e:#}"), "building the tree of a finished launch failed")
                }
            }
        }
        waiting.sort_by_key(|w| std::cmp::Reverse(w.2));
        waiting.truncate(WARM_RETRY);
        state.retry = waiting;
        sqlx::query("update analysis_state set value = $2, updated_at = now() where name = $1")
            .bind(WARM_STATE)
            .bind(serde_json::to_value(&state)?)
            .execute(&self.pool)
            .await?;
        Ok(built)
    }
}

impl Store {
    /// Where the explorer backfill is.
    pub async fn explorer_backfill_progress(&self) -> anyhow::Result<BackfillProgress> {
        let s = self.backfill_state().await?;
        Ok(BackfillProgress {
            total_scanned: s.scanned,
            total_updated: s.updated,
            done: s.done,
            ..BackfillProgress::default()
        })
    }

    async fn backfill_state(&self) -> anyhow::Result<BackfillState> {
        let v: Option<Value> =
            sqlx::query_scalar("select value from analysis_state where name = $1")
                .bind(BACKFILL_STATE)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    /// Fills `label_values`, `suite_tree` and, for results of launches created before migration 600,
    /// `attachment_kinds` of stored results: `batch` results per transaction in id order from
    /// a cursor kept in `analysis_state`, until `budget` is spent or every result was seen.
    /// Each transaction locks only the rows it changes, in id order, and skips rows other
    /// transactions hold (they are tried again after the walk). Idempotent and resumable.
    pub async fn backfill_explorer(
        &self,
        batch: i64,
        budget: Duration,
    ) -> anyhow::Result<BackfillProgress> {
        let started = Instant::now();
        let mut out = BackfillProgress::default();
        let cutoff: Option<DateTime<Utc>> = sqlx::query_scalar(
            "select installed_on from fastproof_schema_migrations where version = $1 and success",
        )
        .bind(ATTACHMENT_KINDS_VERSION)
        .fetch_optional(&self.pool)
        .await?;
        loop {
            let mut state = self.backfill_state().await?;
            if state.done {
                out.done = true;
                out.total_scanned = state.scanned;
                out.total_updated = state.updated;
                return Ok(out);
            }
            // The walk by id first, then the rows another transaction held when the walk passed.
            let retry = state.walked;
            let mut qb = sqlx::QueryBuilder::new(
                "select id, launch_created_at, launch_id, label_values is null, \
                   attachment_kinds = 0 and launch_created_at <= coalesce(",
            );
            qb.push_bind(cutoff)
                .push(", '-infinity'::timestamptz), suite_tree is null from results r where ");
            if retry {
                let (ids, ats): (Vec<Uuid>, Vec<DateTime<Utc>>) =
                    state.skipped.iter().take(batch as usize).copied().unzip();
                qb.push("(r.id, r.launch_created_at) in (select * from unnest(")
                    .push_bind(ids)
                    .push("::uuid[], ")
                    .push_bind(ats)
                    .push("::timestamptz[])) order by id");
            } else {
                qb.push("(")
                    .push_bind(state.cursor)
                    .push("::uuid is null or id > ")
                    .push_bind(state.cursor)
                    .push(") order by id limit ")
                    .push_bind(batch);
            }
            let rows: Vec<BackfillRow> = qb.build_query_as().fetch_all(&self.pool).await?;
            match self.backfill_batch(&rows).await {
                Ok((updated, skipped)) => {
                    if retry {
                        let n = state.skipped.len().min(batch as usize);
                        state.skipped.drain(..n);
                        state.skipped.extend(skipped);
                    } else {
                        state.scanned += rows.len() as u64;
                        match rows.last() {
                            Some(r) => state.cursor = Some(r.0),
                            None => state.walked = true,
                        }
                        let room = MAX_SKIPPED.saturating_sub(state.skipped.len());
                        state.skipped.extend(skipped.into_iter().take(room));
                    }
                    state.updated += updated;
                    state.done = state.walked && state.skipped.is_empty();
                    out.scanned += rows.len() as u64;
                    out.updated += updated;
                    metrics::counter!("fastproof_explorer_backfill_scanned_total")
                        .increment(rows.len() as u64);
                    metrics::counter!("fastproof_explorer_backfill_updated_total")
                        .increment(updated);
                }
                Err(e) if is_lock_timeout(&e) => {
                    metrics::counter!("fastproof_explorer_backfill_lock_timeouts_total")
                        .increment(1);
                    out.total_scanned = state.scanned;
                    out.total_updated = state.updated;
                    return Ok(out);
                }
                Err(e) => return Err(e),
            }
            sqlx::query(
                "insert into analysis_state (name, value, updated_at) values ($1, $2, now()) \
                 on conflict (name) do update set value = excluded.value, updated_at = now()",
            )
            .bind(BACKFILL_STATE)
            .bind(serde_json::to_value(&state)?)
            .execute(&self.pool)
            .await?;
            out.total_scanned = state.scanned;
            out.total_updated = state.updated;
            out.done = state.done;
            if state.done || started.elapsed() > budget {
                return Ok(out);
            }
        }
    }

    /// One transaction of the backfill: locks the rows that need work in id order, skipping
    /// those another transaction holds (returned to be tried again), and with a short
    /// `lock_timeout` for the rest, so the backfill never waits in a lock cycle with a writer.
    /// Returns how many rows changed and the rows skipped.
    async fn backfill_batch(
        &self,
        rows: &[BackfillRow],
    ) -> anyhow::Result<(u64, Vec<(Uuid, DateTime<Utc>)>)> {
        let need: Vec<&BackfillRow> = rows.iter().filter(|r| r.3 || r.4 || r.5).collect();
        if need.is_empty() {
            return Ok((0, Vec::new()));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("set local lock_timeout = '500ms'")
            .execute(&mut *tx)
            .await?;
        let (nid, nat): (Vec<Uuid>, Vec<DateTime<Utc>>) = need.iter().map(|r| (r.0, r.1)).unzip();
        let locked: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "select r.id from results r join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
               on r.id = t.i and r.launch_created_at = t.c \
             order by r.id for update of r skip locked",
        )
        .bind(&nid)
        .bind(&nat)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let skipped: Vec<(Uuid, DateTime<Utc>)> = need
            .iter()
            .filter(|r| !locked.contains(&r.0))
            .map(|r| (r.0, r.1))
            .collect();
        let mine = |f: fn(&BackfillRow) -> bool| {
            need.iter()
                .filter(|r| f(r) && locked.contains(&r.0))
                .map(|r| (r.0, r.1))
                .unzip::<Uuid, DateTime<Utc>, Vec<Uuid>, Vec<DateTime<Utc>>>()
        };
        let mut updated: BTreeSet<Uuid> = BTreeSet::new();
        let (lid, lat) = mine(|r| r.3);
        if !lid.is_empty() {
            let done: Vec<Uuid> = sqlx::query_scalar(
                "update results r set label_values = fastproof_label_values(r.labels) \
                 from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                 where r.id = t.i and r.launch_created_at = t.c and r.label_values is null \
                 returning r.id",
            )
            .bind(&lid)
            .bind(&lat)
            .fetch_all(&mut *tx)
            .await?;
            updated.extend(done);
        }
        let (sid, sat) = mine(|r| r.5);
        if !sid.is_empty() {
            // The rows are locked, so the values read here are those the update writes beside.
            type Suite = (Uuid, DateTime<Utc>, Vec<String>, Value, String);
            let found: Vec<Suite> = sqlx::query_as(
                "select r.id, r.launch_created_at, r.suite_path, r.labels, r.full_name from results r \
                 join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                   on r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&sid)
            .bind(&sat)
            .fetch_all(&mut *tx)
            .await?;
            let mut ids = Vec::with_capacity(found.len());
            let mut ats = Vec::with_capacity(found.len());
            let mut paths = Vec::with_capacity(found.len());
            let mut titled = Vec::with_capacity(found.len());
            for (id, at, suite_path, labels, full_name) in found {
                let (p, t) = crate::results::suite_place_json(&suite_path, &labels, &full_name);
                ids.push(id);
                ats.push(at);
                paths.push(crate::json_no_nul(serde_json::to_value(p)?));
                titled.push(t);
            }
            let done: Vec<Uuid> = sqlx::query_scalar(
                "update results r set suite_tree = array(select jsonb_array_elements_text(t.p)), \
                   suite_titled = t.s \
                 from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[], $4::bool[]) as t(i, c, p, s) \
                 where r.id = t.i and r.launch_created_at = t.c and r.suite_tree is null \
                 returning r.id",
            )
            .bind(&ids)
            .bind(&ats)
            .bind(&paths)
            .bind(&titled)
            .fetch_all(&mut *tx)
            .await?;
            updated.extend(done);
        }
        let (kid, kat) = mine(|r| r.4);
        if !kid.is_empty() {
            type Attempt = (Uuid, Value, Option<Vec<u8>>);
            let attempts: Vec<Attempt> = sqlx::query_as(
                "select a.result_id, a.detail, a.steps_z from attempts a \
                 join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                   on a.result_id = t.i and a.launch_created_at = t.c",
            )
            .bind(&kid)
            .bind(&kat)
            .fetch_all(&mut *tx)
            .await?;
            let mut kinds: HashMap<Uuid, i32> = HashMap::new();
            for (result, detail, steps) in attempts {
                let d: AttemptDetail = serde_json::from_value(detail).unwrap_or_default();
                let steps: Vec<Step> = match steps {
                    Some(z) => crate::codec::decode(&z).unwrap_or_default(),
                    None => Vec::new(),
                };
                let m = attempt_kinds(&d.attachments, &steps);
                if m != 0 {
                    *kinds.entry(result).or_default() |= m;
                }
            }
            let at: HashMap<Uuid, DateTime<Utc>> = kid.iter().copied().zip(kat).collect();
            let mut ids = Vec::with_capacity(kinds.len());
            let mut ats = Vec::with_capacity(kinds.len());
            let mut masks = Vec::with_capacity(kinds.len());
            let mut sorted: Vec<(Uuid, i32)> = kinds.into_iter().collect();
            sorted.sort();
            for (id, m) in sorted {
                ids.push(id);
                ats.push(at[&id]);
                masks.push(m);
            }
            if !ids.is_empty() {
                let done: Vec<Uuid> = sqlx::query_scalar(
                    "update results r set attachment_kinds = r.attachment_kinds | t.k \
                     from unnest($1::uuid[], $2::timestamptz[], $3::int[]) as t(i, c, k) \
                     where r.id = t.i and r.launch_created_at = t.c \
                       and r.attachment_kinds | t.k <> r.attachment_kinds \
                     returning r.id",
                )
                .bind(&ids)
                .bind(&ats)
                .bind(&masks)
                .fetch_all(&mut *tx)
                .await?;
                updated.extend(done);
            }
        }
        tx.commit().await?;
        Ok((updated.len() as u64, skipped))
    }
}

/// A result the backfill looks at: id, launch time, launch, and whether `label_values`,
/// `attachment_kinds` and `suite_tree` need filling.
type BackfillRow = (Uuid, DateTime<Utc>, Uuid, bool, bool, bool);

/// Rows a transaction held while the backfill walked past them, kept to be tried again.
pub const MAX_SKIPPED: usize = 10_000;

fn is_lock_timeout(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<sqlx::Error>()
            .and_then(|e| e.as_database_error())
            .and_then(|d| d.code())
            .is_some_and(|c| c == "55P03")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_keep_order_and_empty_values() {
        let v = label_values(&json!([
            {"name": "tag", "value": "b"},
            {"name": "owner", "value": "ann"},
            {"name": "tag", "value": ""},
            {"name": "tag", "value": "a"},
            {"value": "no name"},
            {"name": "severity"}
        ]));
        assert_eq!(
            v,
            json!({"tag": ["b", "", "a"], "owner": ["ann"], "severity": [""]})
        );
        assert_eq!(label_values(&json!("not a list")), json!({}));
    }
}
