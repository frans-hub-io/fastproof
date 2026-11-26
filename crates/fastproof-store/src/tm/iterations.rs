//! Iterations of parameterized run items (§7.5, night 2).
//!
//! A case with parameters becomes one run item with one iteration per enabled row of the
//! pinned version (`run_iterations`, keyed by the item, its execution and the row id). Each
//! iteration has its own status, comment, executor, times and step results
//! (`run_iteration_step_results`), and its own series: the manual identity of the case with
//! the row's values as Allure parameters, so a final iteration projects one `results` row
//! (`projection.rs`) that sits next to an automated test with the same `@Parameter`s. The item
//! status is derived from the iterations ([`derive_status`]) and the item itself is not
//! projected.
//!
//! Locks: as for run items (`runs.rs`): an iteration change locks the item row, then (through
//! the projection) the iteration's result row, and the launch row last.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use fastproof_model::identity::{parameters_hash, retry_hash};
use sqlx::PgConnection;
use uuid::Uuid;

use super::identity::manual_hash;
use super::lookup::case_version_snapshot;
use super::parameters::{allure_parameters, display_parameters, enabled_rows, row_values};
use super::projection::{CountDelta, Projected};
use super::runs::{
    attachment_refs, check_text, closed, executed_snapshot, finish_launch, item_brief,
    item_changed, lock_item, manual_changed, open_launch, path_key, pins_of, step_at, step_paths,
    suggested_status, valid_step_path,
};
use super::{
    CaseParameters, ManualState, RunIteration, RunIterationPatch, RunIterationStepAck, RunStatus,
    RunStepResult, RunStepWrite, StepStatus, TmError, TmLimits, TmResult, UntestedStepsTo,
};
use crate::{Project, Store, json_no_nul, no_nul};

/// Cases whose parameters are read per statement while creating iterations.
const CHUNK: usize = 500;
/// Bytes of iteration values one call of [`create_iterations`] writes, at most.
pub const MAX_ITERATION_BYTES: usize = 64 * 1024 * 1024;

/// The status of a parameterized item from the statuses of its iterations (§7.5): `failed` if
/// any, else `blocked` if any, else `untested` when all are untested, `skipped` when all are
/// skipped, `in_progress` while some are started or final and others not, else `passed`.
pub fn derive_status(statuses: &[RunStatus]) -> RunStatus {
    if statuses.contains(&RunStatus::Failed) {
        return RunStatus::Failed;
    }
    if statuses.contains(&RunStatus::Blocked) {
        return RunStatus::Blocked;
    }
    if statuses.iter().all(|s| *s == RunStatus::Untested) {
        return RunStatus::Untested;
    }
    if statuses.iter().all(|s| *s == RunStatus::Skipped) {
        return RunStatus::Skipped;
    }
    if statuses.iter().any(|s| !s.is_final()) {
        return RunStatus::InProgress;
    }
    RunStatus::Passed
}

/// [`derive_status`] in SQL over the iterations of the current execution of item `ri`.
pub const DERIVED_STATUS_SQL: &str = "(select case when bool_or(x.status = 'failed') then 'failed' \
    when bool_or(x.status = 'blocked') then 'blocked' when bool_and(x.status = 'untested') then 'untested' \
    when bool_and(x.status = 'skipped') then 'skipped' \
    when bool_or(x.status in ('untested', 'in_progress')) then 'in_progress' else 'passed' end \
    from run_iterations x where x.run_item_id = ri.id and x.execution_no = ri.execution_no)";

/// The client key of the attempt of one iteration of one execution.
pub fn client_key(item: Uuid, execution_no: i32, row_id: &str) -> String {
    format!("manual:{item}:{execution_no}:{row_id}")
}

/// The series key of a case run with these values: the manual identity with the values as
/// Allure parameters (`retry_hash(manual_hash(case), parameters_hash(values), None)`).
pub fn series_key(
    case_id: Uuid,
    columns: &[String],
    values: &BTreeMap<String, String>,
) -> (String, String) {
    let ph = parameters_hash(&allure_parameters(columns, values));
    (retry_hash(&manual_hash(case_id), &ph, None), ph)
}

/// Parameters of the pinned versions `(case, version)`: the head version from `cases.body`,
/// others from their snapshot. Only parameterized cases are returned. Before anything large
/// is read, the head versions are sized in SQL: their enabled rows count against `room`
/// (iterations the launch may still take) and their parameters against
/// [`MAX_ITERATION_BYTES`]; other versions are charged as they are read. Either one exceeded:
/// 409 `limit_reached`, so a plan of many cases with large tables never loads them all.
async fn parameters_of(
    conn: &mut PgConnection,
    items: &[(Uuid, i32)],
    room: usize,
    max_iterations: usize,
) -> TmResult<HashMap<(Uuid, i32), CaseParameters>> {
    let too_many = || TmError::limit("maxRunIterations", max_iterations as u64);
    let too_big = || TmError::limit("maxIterationBytes", MAX_ITERATION_BYTES as u64);
    let ids: Vec<Uuid> = items.iter().map(|i| i.0).collect();
    let mut heads: HashMap<Uuid, (i32, i64, i64)> = HashMap::new();
    for chunk in ids.chunks(5000) {
        let rows: Vec<(Uuid, i32, i64, i64)> = sqlx::query_as(
            "select id, version, coalesce(pg_column_size(body->'parameters'), 0)::bigint, \
               (select count(*) from jsonb_array_elements(body->'parameters'->'rows') r \
                 where r->'enabled' = 'true'::jsonb)::bigint \
             from cases where id = any($1) and jsonb_typeof(body->'parameters'->'rows') = 'array'",
        )
        .bind(chunk)
        .fetch_all(&mut *conn)
        .await?;
        for (id, version, size, enabled) in rows {
            heads.insert(id, (version, size, enabled));
        }
    }
    let mut rows_left = room;
    let mut bytes_left = MAX_ITERATION_BYTES;
    let mut charge = |enabled: usize, size: usize| -> TmResult<()> {
        rows_left = rows_left.checked_sub(enabled).ok_or_else(too_many)?;
        bytes_left = bytes_left.checked_sub(size).ok_or_else(too_big)?;
        Ok(())
    };
    let mut load: Vec<Uuid> = Vec::new();
    for (case, version) in items {
        if let Some((v, size, enabled)) = heads.get(case)
            && v == version
            && *enabled > 0
        {
            charge(*enabled as usize, *size as usize)?;
            load.push(*case);
        }
    }
    let mut kept: HashMap<(Uuid, i32), CaseParameters> = HashMap::new();
    for chunk in load.chunks(CHUNK) {
        let rows: Vec<(Uuid, i32, Option<serde_json::Value>)> =
            sqlx::query_as("select id, version, body->'parameters' from cases where id = any($1)")
                .bind(chunk)
                .fetch_all(&mut *conn)
                .await?;
        for (id, version, p) in rows {
            if let Some(p) = p.and_then(|p| serde_json::from_value::<CaseParameters>(p).ok())
                && super::parameters::is_parameterized(Some(&p))
            {
                kept.insert((id, version), p);
            }
        }
    }
    for (case, version) in items {
        if heads.get(case).is_some_and(|h| h.0 == *version) {
            continue;
        }
        if let Some(s) = case_version_snapshot(conn, *case, *version).await?
            && super::parameters::is_parameterized(s.body.parameters.as_ref())
            && let Some(p) = s.body.parameters
        {
            let size = serde_json::to_vec(&p).map(|v| v.len()).unwrap_or(0);
            charge(enabled_rows(&p).count(), size)?;
            kept.insert((*case, *version), p);
        }
    }
    Ok(kept)
}

/// Creates the iterations of newly inserted run items `(item, case, version)` (one per
/// enabled row of the version), their series, and `run_items.iterations`; returns how many it
/// created. The iterations of the launch stay within `max_run_iterations` (409
/// `limit_reached` before anything is written) and one call reads and writes at most
/// [`MAX_ITERATION_BYTES`] of parameters. Rows whose values give the same Allure parameters
/// as an earlier row of the item (one series) are left out.
pub async fn create_iterations(
    conn: &mut PgConnection,
    project_id: Uuid,
    launch_id: Uuid,
    items: &[(Uuid, Uuid, i32)],
    limits: &TmLimits,
) -> TmResult<u64> {
    if items.is_empty() {
        return Ok(0);
    }
    let existing: i64 = sqlx::query_scalar(
        "select coalesce(sum(iterations), 0)::bigint from run_items where launch_id = $1",
    )
    .bind(launch_id)
    .fetch_one(&mut *conn)
    .await?;
    let room = limits
        .max_run_iterations
        .saturating_sub(existing.max(0) as usize);
    let keys: Vec<(Uuid, i32)> = items.iter().map(|i| (i.1, i.2)).collect();
    let params = parameters_of(conn, &keys, room, limits.max_run_iterations).await?;
    if params.is_empty() {
        return Ok(0);
    }
    let wanted: usize = items
        .iter()
        .filter_map(|i| params.get(&(i.1, i.2)))
        .map(|p| enabled_rows(p).count())
        .sum();
    if wanted > room {
        return Err(TmError::limit(
            "maxRunIterations",
            limits.max_run_iterations as u64,
        ));
    }
    struct NewIteration {
        item: Uuid,
        row_id: String,
        position: i32,
        values: serde_json::Value,
        key: String,
    }
    let hashes: Vec<String> = items
        .iter()
        .filter(|i| params.contains_key(&(i.1, i.2)))
        .map(|i| manual_hash(i.1))
        .collect();
    let test_cases: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "select test_case_hash, id from test_cases where project_id = $1 and test_case_hash = any($2)",
    )
    .bind(project_id)
    .bind(&hashes)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let mut series: BTreeMap<String, (Uuid, String, serde_json::Value)> = BTreeMap::new();
    let mut rows: Vec<NewIteration> = Vec::with_capacity(wanted);
    let mut counts: Vec<(Uuid, i32)> = Vec::new();
    let mut bytes = 0usize;
    for (item, case, version) in items {
        let Some(p) = params.get(&(*case, *version)) else {
            continue;
        };
        let tc = *test_cases
            .get(&manual_hash(*case))
            .ok_or_else(|| anyhow::anyhow!("manual identity of a run item missing"))?;
        let mut n = 0;
        let mut keys_of_item: HashSet<String> = HashSet::new();
        for r in enabled_rows(p) {
            let values = row_values(&p.columns, r);
            bytes += values
                .iter()
                .map(|(k, v)| k.len() + v.len() + 8)
                .sum::<usize>();
            if bytes > MAX_ITERATION_BYTES {
                return Err(TmError::limit(
                    "maxIterationBytes",
                    MAX_ITERATION_BYTES as u64,
                ));
            }
            let (key, ph) = series_key(*case, &p.columns, &values);
            if !keys_of_item.insert(key.clone()) {
                continue;
            }
            series
                .entry(key.clone())
                .or_insert_with(|| (tc, ph, json_no_nul(display_parameters(&p.columns, &values))));
            n += 1;
            rows.push(NewIteration {
                item: *item,
                row_id: r.id.clone(),
                position: n,
                values: json_no_nul(serde_json::to_value(&values)?),
                key,
            });
        }
        counts.push((*item, n));
    }
    let s_keys: Vec<String> = series.keys().cloned().collect();
    let s_ids: Vec<Uuid> = s_keys.iter().map(|_| Uuid::now_v7()).collect();
    let s_tc: Vec<Uuid> = series.values().map(|v| v.0).collect();
    let s_ph: Vec<String> = series.values().map(|v| v.1.clone()).collect();
    let s_params: Vec<serde_json::Value> = series.values().map(|v| v.2.clone()).collect();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash, env_key, parameters, \
           first_seen_at, last_seen_at) \
         select i, $1, tc, k, ph, null, p, now(), now() \
         from unnest($2::uuid[], $3::uuid[], $4::text[], $5::text[], $6::jsonb[]) as t(i, tc, k, ph, p) \
         on conflict (project_id, series_key) do nothing",
    )
    .bind(project_id)
    .bind(&s_ids)
    .bind(&s_tc)
    .bind(&s_keys)
    .bind(&s_ph)
    .bind(&s_params)
    .execute(&mut *conn)
    .await?;
    let series_ids: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "select series_key, id from series where project_id = $1 and series_key = any($2)",
    )
    .bind(project_id)
    .bind(&s_keys)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let (c_items, c_counts): (Vec<Uuid>, Vec<i32>) = counts.into_iter().unzip();
    let execs: HashMap<Uuid, i32> = sqlx::query_as::<_, (Uuid, i32)>(
        "update run_items ri set iterations = t.n from unnest($1::uuid[], $2::int[]) as t(i, n) \
         where ri.id = t.i returning ri.id, ri.execution_no",
    )
    .bind(&c_items)
    .bind(&c_counts)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    for chunk in rows.chunks(5000) {
        let mut it = (
            Vec::with_capacity(chunk.len()),
            Vec::with_capacity(chunk.len()),
            Vec::with_capacity(chunk.len()),
            Vec::with_capacity(chunk.len()),
            Vec::with_capacity(chunk.len()),
            Vec::with_capacity(chunk.len()),
        );
        for r in chunk {
            it.0.push(r.item);
            it.1.push(execs.get(&r.item).copied().unwrap_or(1));
            it.2.push(no_nul(&r.row_id));
            it.3.push(r.position);
            it.4.push(r.values.clone());
            it.5.push(
                *series_ids
                    .get(&r.key)
                    .ok_or_else(|| anyhow::anyhow!("iteration series missing"))?,
            );
        }
        sqlx::query(
            "insert into run_iterations (run_item_id, execution_no, row_id, position, param_values, series_id) \
             select * from unnest($1::uuid[], $2::int[], $3::text[], $4::int[], $5::jsonb[], $6::uuid[]) \
             on conflict do nothing",
        )
        .bind(&it.0)
        .bind(&it.1)
        .bind(&it.2)
        .bind(&it.3)
        .bind(&it.4)
        .bind(&it.5)
        .execute(&mut *conn)
        .await?;
    }
    Ok(rows.len() as u64)
}

/// Starts the iterations of a new execution of an item from those of the previous one (all
/// `untested`, same rows, values and series).
pub async fn copy_for_rerun(
    conn: &mut PgConnection,
    item_id: Uuid,
    from: i32,
    to: i32,
) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "insert into run_iterations (run_item_id, execution_no, row_id, position, param_values, series_id) \
         select run_item_id, $3, row_id, position, param_values, series_id from run_iterations \
         where run_item_id = $1 and execution_no = $2 on conflict do nothing",
    )
    .bind(item_id)
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

/// The statuses of the iterations of an execution of an item.
async fn statuses(
    conn: &mut PgConnection,
    item_id: Uuid,
    execution_no: i32,
) -> sqlx::Result<Vec<RunStatus>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "select status from run_iterations where run_item_id = $1 and execution_no = $2",
    )
    .bind(item_id)
    .bind(execution_no)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|s| RunStatus::parse(s).unwrap_or(RunStatus::Untested))
        .collect())
}

/// Sets the derived status of a parameterized item (status times and executor as a status
/// change of the item sets them) and bumps its revision. Returns the status and revision.
pub(super) async fn apply_derived(
    conn: &mut PgConnection,
    item_id: Uuid,
    execution_no: i32,
    old: RunStatus,
    by: &str,
) -> sqlx::Result<(RunStatus, i64)> {
    let new = derive_status(&statuses(conn, item_id, execution_no).await?);
    let changed = new != old;
    let revision: i64 = sqlx::query_scalar(
        "update run_items set status = $2, \
           started_at = case when $2 = 'untested' then null \
             when $3 then coalesce(started_at, now()) else started_at end, \
           finished_at = case when $4 then now() when $2 in ('untested', 'in_progress') then null \
             else finished_at end, \
           executed_by = case when $3 then $5 else executed_by end, \
           revision = revision + 1, updated_at = now() \
         where id = $1 returning revision",
    )
    .bind(item_id)
    .bind(new.as_str())
    .bind(changed && new != RunStatus::Untested)
    .bind(changed && new.is_final())
    .bind(no_nul(by))
    .fetch_one(&mut *conn)
    .await?;
    Ok((new, revision))
}

/// Emits `runIteration.updated {launchId, itemId, rowId, status, revision}` (outbox and live).
pub async fn iteration_changed(
    conn: &mut PgConnection,
    project_key: &str,
    launch_id: Uuid,
    item_id: Uuid,
    row_id: &str,
    status: RunStatus,
    revision: i64,
) -> anyhow::Result<()> {
    let payload = serde_json::json!({"launchId": launch_id, "itemId": item_id, "rowId": row_id,
        "status": status, "revision": revision});
    crate::events::emit(
        conn,
        "runIteration.updated",
        Some(project_key),
        payload.clone(),
    )
    .await?;
    crate::events::notify(
        conn,
        &crate::events::LiveEvent {
            project: project_key.to_string(),
            kind: "runIteration.updated".into(),
            data: payload,
        },
    )
    .await
}

/// The iterations of an execution of an item with their step results.
pub async fn views(
    conn: &mut PgConnection,
    project_id: Uuid,
    item_id: Uuid,
    execution_no: i32,
) -> anyhow::Result<Vec<RunIteration>> {
    type Row = (
        String,
        i32,
        serde_json::Value,
        String,
        String,
        Option<Uuid>,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select row_id, position, param_values, status, comment, result_id, executed_by, started_at, finished_at \
         from run_iterations where run_item_id = $1 and execution_no = $2 order by position, row_id",
    )
    .bind(item_id)
    .bind(execution_no)
    .fetch_all(&mut *conn)
    .await?;
    type StepRow = (
        String,
        String,
        String,
        String,
        String,
        Vec<Uuid>,
        String,
        DateTime<Utc>,
    );
    let steps: Vec<StepRow> = sqlx::query_as(
        "select row_id, step_path, status, actual, comment, attachment_ids, updated_by, updated_at \
         from run_iteration_step_results where run_item_id = $1 and execution_no = $2",
    )
    .bind(item_id)
    .bind(execution_no)
    .fetch_all(&mut *conn)
    .await?;
    let att_ids: Vec<Uuid> = steps.iter().flat_map(|s| s.5.iter().copied()).collect();
    let atts = attachment_refs(conn, project_id, &att_ids).await?;
    let mut by_row: HashMap<String, Vec<RunStepResult>> = HashMap::new();
    for (row, path, status, actual, comment, ids, updated_by, updated_at) in steps {
        by_row.entry(row).or_default().push(RunStepResult {
            path,
            status: StepStatus::parse(&status).unwrap_or(StepStatus::Untested),
            actual,
            comment,
            attachments: ids.iter().filter_map(|i| atts.get(i).cloned()).collect(),
            updated_by,
            updated_at,
        });
    }
    Ok(rows
        .into_iter()
        .map(
            |(
                row_id,
                position,
                values,
                status,
                comment,
                result_id,
                executed_by,
                started_at,
                finished_at,
            )| {
                let mut step_results = by_row.remove(&row_id).unwrap_or_default();
                step_results.sort_by_key(|s| path_key(&s.path));
                RunIteration {
                    position,
                    values: serde_json::from_value(values).unwrap_or_default(),
                    status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
                    comment,
                    result_id,
                    executed_by,
                    started_at,
                    finished_at,
                    step_results,
                    row_id,
                }
            },
        )
        .collect())
}

/// Step statuses of one iteration.
async fn step_statuses(
    conn: &mut PgConnection,
    item_id: Uuid,
    execution_no: i32,
    row_id: &str,
) -> sqlx::Result<HashMap<String, StepStatus>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select step_path, status from run_iteration_step_results \
         where run_item_id = $1 and execution_no = $2 and row_id = $3",
    )
    .bind(item_id)
    .bind(execution_no)
    .bind(row_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(p, s)| StepStatus::parse(&s).map(|s| (p, s)))
        .collect())
}

/// Marks the untested leaf steps of iterations `skipped`.
pub(super) async fn skip_untested_steps(
    conn: &mut PgConnection,
    item_id: Uuid,
    execution_no: i32,
    rows: &[String],
    leaves: &[String],
    by: &str,
) -> sqlx::Result<()> {
    if rows.is_empty() || leaves.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "insert into run_iteration_step_results (run_item_id, execution_no, row_id, step_path, status, updated_by) \
         select $1, $2, r, p, 'skipped', $5 from unnest($3::text[]) as r cross join unnest($4::text[]) as p \
         on conflict (run_item_id, execution_no, row_id, step_path) do update set status = 'skipped', \
           updated_by = excluded.updated_by, updated_at = now() \
         where run_iteration_step_results.status = 'untested'",
    )
    .bind(item_id)
    .bind(execution_no)
    .bind(rows)
    .bind(leaves)
    .bind(no_nul(by))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Sets the status of iterations of an execution (status times and executor as for an item).
/// Returns `(row_id, old status, new status)` of each changed iteration.
pub(super) async fn set_statuses(
    conn: &mut PgConnection,
    item_id: Uuid,
    execution_no: i32,
    rows: Option<&[String]>,
    status: RunStatus,
    comment: Option<&str>,
    by: &str,
) -> sqlx::Result<Vec<(String, RunStatus, RunStatus)>> {
    let out: Vec<(String, String, String)> = sqlx::query_as(
        "with old as (select row_id, status from run_iterations \
           where run_item_id = $1 and execution_no = $2 and ($3::text[] is null or row_id = any($3)) for update) \
         update run_iterations it set status = $4, comment = coalesce($5, it.comment), \
           started_at = case when $4 = 'untested' then null \
             when it.status <> $4 then coalesce(it.started_at, now()) else it.started_at end, \
           finished_at = case when it.status <> $4 and $4 in ('passed', 'failed', 'blocked', 'skipped') then now() \
             when $4 in ('untested', 'in_progress') then null else it.finished_at end, \
           executed_by = case when it.status <> $4 and $4 <> 'untested' then $6 else it.executed_by end, \
           updated_at = now() \
         from old where it.run_item_id = $1 and it.execution_no = $2 and it.row_id = old.row_id \
         returning it.row_id, old.status, it.status",
    )
    .bind(item_id)
    .bind(execution_no)
    .bind(rows)
    .bind(status.as_str())
    .bind(comment.map(no_nul))
    .bind(no_nul(by))
    .fetch_all(&mut *conn)
    .await?;
    Ok(out
        .into_iter()
        .map(|(r, a, b)| {
            (
                r,
                RunStatus::parse(&a).unwrap_or(RunStatus::Untested),
                RunStatus::parse(&b).unwrap_or(RunStatus::Untested),
            )
        })
        .collect())
}

/// What an iteration change did, for the audit.
#[derive(Clone, Debug)]
pub struct IterationChange {
    pub launch_id: Uuid,
    pub case_number: i64,
    pub row_id: String,
    pub old_status: RunStatus,
    pub new_status: RunStatus,
    pub item_status: RunStatus,
    pub revision: i64,
}

impl Store {
    /// Writes the result of one step of an iteration (last writer wins, as for an item step).
    /// An untested iteration becomes `in_progress`; a final one is projected again. The item
    /// takes its derived status and a new revision.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_iteration_step(
        &self,
        project: &Project,
        item_id: Uuid,
        row_id: &str,
        path: &str,
        w: &RunStepWrite,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<RunIterationStepAck> {
        if !valid_step_path(path) {
            return Err(TmError::NotFound("step"));
        }
        if !super::parameters::valid_row_id(row_id) {
            return Err(TmError::NotFound("iteration"));
        }
        check_text("actual", w.actual.as_deref(), limits.max_markdown_bytes)?;
        check_text("comment", w.comment.as_deref(), limits.max_markdown_bytes)?;
        let attachments = match &w.attachment_ids {
            Some(ids) => {
                let mut v = ids.clone();
                v.sort();
                v.dedup();
                if v.len() > limits.max_step_attachments {
                    return Err(TmError::limit(
                        "maxStepAttachments",
                        limits.max_step_attachments as u64,
                    ));
                }
                Some(v)
            }
            None => None,
        };
        let mut conn = self.pool.acquire().await?;
        let brief = item_brief(&mut conn, project.id, item_id)
            .await?
            .ok_or(TmError::NotFound("run item"))?;
        drop(conn);
        if brief.manual_state == Some(ManualState::Closed) {
            return Err(closed());
        }
        let mut tx = self.pool.begin().await?;
        let item = lock_item(&mut tx, item_id).await?;
        let launch = open_launch(&mut tx, project, brief.launch_id, false).await?;
        let it: Option<String> = sqlx::query_scalar(
            "select status from run_iterations where run_item_id = $1 and execution_no = $2 and row_id = $3",
        )
        .bind(item_id)
        .bind(item.execution_no)
        .bind(row_id)
        .fetch_optional(&mut *tx)
        .await?;
        let it_status = RunStatus::parse(&it.ok_or(TmError::NotFound("iteration"))?)
            .unwrap_or(RunStatus::Untested);
        let snapshot = executed_snapshot(
            &mut tx,
            project.id,
            item.case_id,
            item.case_version,
            pins_of(item.shared_pins.clone()).as_ref(),
        )
        .await?;
        if step_at(&snapshot.body.steps, path).is_none() {
            return Err(TmError::NotFound("step"));
        }
        if let Some(ids) = &attachments
            && !ids.is_empty()
        {
            let n: i64 = sqlx::query_scalar(
                "select count(*) from attachments where project_id = $1 and id = any($2)",
            )
            .bind(project.id)
            .bind(ids)
            .fetch_one(&mut *tx)
            .await?;
            if n as usize != ids.len() {
                return Err(TmError::NotFound("attachment"));
            }
        }
        sqlx::query(
            "insert into run_iteration_step_results (run_item_id, execution_no, row_id, step_path, status, actual, \
               comment, attachment_ids, updated_by, updated_at) \
             values ($1, $2, $3, $4, $5, coalesce($6, ''), coalesce($7, ''), coalesce($8, '{}'::uuid[]), $9, now()) \
             on conflict (run_item_id, execution_no, row_id, step_path) do update set status = excluded.status, \
               actual = coalesce($6, run_iteration_step_results.actual), \
               comment = coalesce($7, run_iteration_step_results.comment), \
               attachment_ids = coalesce($8, run_iteration_step_results.attachment_ids), \
               updated_by = excluded.updated_by, updated_at = now()",
        )
        .bind(item_id)
        .bind(item.execution_no)
        .bind(row_id)
        .bind(path)
        .bind(w.status.as_str())
        .bind(w.actual.as_deref().map(no_nul))
        .bind(w.comment.as_deref().map(no_nul))
        .bind(attachments.as_deref())
        .bind(no_nul(by))
        .execute(&mut *tx)
        .await?;
        let iteration_status = if it_status == RunStatus::Untested {
            set_statuses(
                &mut tx,
                item_id,
                item.execution_no,
                Some(&[row_id.to_string()]),
                RunStatus::InProgress,
                None,
                by,
            )
            .await?;
            RunStatus::InProgress
        } else {
            sqlx::query(
                "update run_iterations set updated_at = now() \
                 where run_item_id = $1 and execution_no = $2 and row_id = $3",
            )
            .bind(item_id)
            .bind(item.execution_no)
            .bind(row_id)
            .execute(&mut *tx)
            .await?;
            it_status
        };
        let (item_status, revision) =
            apply_derived(&mut tx, item_id, item.execution_no, item.status, by).await?;
        let mut projected = Projected::default();
        if it_status.is_final() {
            projected = super::projection::project_iterations(
                &mut tx,
                &launch,
                &[(item_id, row_id.to_string())],
            )
            .await?;
        }
        projected
            .delta
            .add(&CountDelta::item_change(item.status, item_status));
        if item_status != item.status {
            manual_changed(&mut tx, &project.key, launch.id).await?;
        }
        let paths = step_paths(&snapshot.body.steps);
        let results = step_statuses(&mut tx, item_id, item.execution_no, row_id).await?;
        let suggested = suggested_status(&paths, &results);
        iteration_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            row_id,
            iteration_status,
            revision,
        )
        .await?;
        item_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            item_status,
            revision,
        )
        .await?;
        finish_launch(&mut tx, &launch, &projected).await?;
        tx.commit().await?;
        Ok(RunIterationStepAck {
            iteration_status,
            item_status,
            suggested_status: suggested,
            revision,
        })
    }

    /// Changes the status or comment of an iteration at the item's `revision` (`If-Match` of
    /// the item); a final status is projected into `results`, a change back to `untested` or
    /// `in_progress` removes the projected attempt. `untestedStepsTo: skipped` marks the
    /// iteration's untested steps skipped. The item takes its derived status.
    #[allow(clippy::too_many_arguments)]
    pub async fn patch_run_iteration(
        &self,
        project: &Project,
        item_id: Uuid,
        row_id: &str,
        revision: i64,
        p: &RunIterationPatch,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<IterationChange> {
        if !super::parameters::valid_row_id(row_id) {
            return Err(TmError::NotFound("iteration"));
        }
        check_text("comment", p.comment.as_deref(), limits.max_markdown_bytes)?;
        if p.status.is_none() && p.comment.is_none() && p.untested_steps_to.is_none() {
            return Err(TmError::bad_request(
                "nothing to change: give status, comment or untestedStepsTo",
            ));
        }
        let mut conn = self.pool.acquire().await?;
        let brief = item_brief(&mut conn, project.id, item_id)
            .await?
            .ok_or(TmError::NotFound("run item"))?;
        drop(conn);
        if brief.manual_state == Some(ManualState::Closed) {
            return Err(closed());
        }
        let mut tx = self.pool.begin().await?;
        let item = lock_item(&mut tx, item_id).await?;
        let launch = open_launch(&mut tx, project, brief.launch_id, false).await?;
        let cur: Option<String> = sqlx::query_scalar(
            "select status from run_iterations where run_item_id = $1 and execution_no = $2 and row_id = $3",
        )
        .bind(item_id)
        .bind(item.execution_no)
        .bind(row_id)
        .fetch_optional(&mut *tx)
        .await?;
        let old = RunStatus::parse(&cur.ok_or(TmError::NotFound("iteration"))?)
            .unwrap_or(RunStatus::Untested);
        if item.revision != revision {
            return Err(TmError::Stale {
                current_revision: item.revision,
                changed_by: item.executed_by.clone(),
                source: Some("ui".into()),
            });
        }
        let rows = [row_id.to_string()];
        if p.untested_steps_to == Some(UntestedStepsTo::Skipped) {
            let snapshot = executed_snapshot(
                &mut tx,
                project.id,
                item.case_id,
                item.case_version,
                pins_of(item.shared_pins.clone()).as_ref(),
            )
            .await?;
            let leaves: Vec<String> = step_paths(&snapshot.body.steps)
                .into_iter()
                .filter(|p| p.leaf)
                .map(|p| p.path)
                .collect();
            skip_untested_steps(&mut tx, item_id, item.execution_no, &rows, &leaves, by).await?;
        }
        let new = p.status.unwrap_or(old);
        set_statuses(
            &mut tx,
            item_id,
            item.execution_no,
            Some(&rows),
            new,
            p.comment.as_deref(),
            by,
        )
        .await?;
        let (item_status, revision) =
            apply_derived(&mut tx, item_id, item.execution_no, item.status, by).await?;
        let mut projected = Projected::default();
        if old.is_final() || new.is_final() {
            projected = super::projection::project_iterations(
                &mut tx,
                &launch,
                &[(item_id, row_id.to_string())],
            )
            .await?;
        }
        projected
            .delta
            .add(&CountDelta::item_change(item.status, item_status));
        if item_status != item.status {
            manual_changed(&mut tx, &project.key, launch.id).await?;
        }
        iteration_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            row_id,
            new,
            revision,
        )
        .await?;
        item_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            item_status,
            revision,
        )
        .await?;
        finish_launch(&mut tx, &launch, &projected).await?;
        tx.commit().await?;
        Ok(IterationChange {
            launch_id: launch.id,
            case_number: item.number,
            row_id: row_id.to_string(),
            old_status: old,
            new_status: new,
            item_status,
            revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RunStatus::*;

    #[test]
    fn derived_item_status() {
        let cases: &[(&[RunStatus], RunStatus)] = &[
            (&[Untested, Untested], Untested),
            (&[Skipped, Skipped], Skipped),
            (&[Passed, Passed], Passed),
            (&[Passed, Skipped], Passed),
            (&[Passed, Untested], InProgress),
            (&[InProgress, Untested], InProgress),
            (&[InProgress, InProgress], InProgress),
            (&[Skipped, Untested], InProgress),
            (&[Failed, Untested], Failed),
            (&[Blocked, Passed], Blocked),
            (&[Blocked, Failed, Passed], Failed),
            (&[Passed], Passed),
        ];
        for (input, want) in cases {
            assert_eq!(derive_status(input), *want, "{input:?}");
        }
    }

    #[test]
    fn keys() {
        let item = Uuid::nil();
        assert_eq!(client_key(item, 2, "r_1"), format!("manual:{item}:2:r_1"));
        let cols = vec!["b".to_string(), "a".to_string()];
        let v: BTreeMap<String, String> =
            [("a".into(), "1".into()), ("b".into(), "2".into())].into();
        let (k, ph) = series_key(item, &cols, &v);
        assert!(k.starts_with(&manual_hash(item)));
        assert!(k.ends_with(&ph));
        let (k2, _) = series_key(item, &["a".to_string(), "b".to_string()], &v);
        assert_eq!(k, k2, "column order does not change the series");
    }
}
