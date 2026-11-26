//! Manual runs: run items of a launch (one per case, executing a fixed case version), step
//! results, status changes with their projection into `results` (`projection.rs`), reruns,
//! closing and reopening the manual part of a launch, and the manual report.
//!
//! Locks: a change of a run item locks the item row, then (through the projection) its result
//! row, and touches the launch row only at the end, in one short update that applies the
//! change of the launch counters ([`super::projection::apply_launch_changes`]). A step write
//! of an item in progress does not touch the launch row at all, so testers of one launch wait
//! only for writers of the same item. Adding items locks the launch row first and only
//! inserts items; closing with `skipped` locks every item before the launch row; ingest locks
//! the launch row first and never touches manual rows. So no two of them wait in a cycle.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use fastproof_model::identity::{parameters_hash, retry_hash};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, QueryBuilder, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use super::expand::SharedPins;
use super::identity::manual_hash;
use super::lookup::case_version_snapshot;
use super::projection::{CountDelta, Projected};
use super::{
    AttachmentRef, CaseSnapshot, CaseStep, LaunchManual, ManualState, ManualUntestedTo, RunItem,
    RunItemExecution, RunItemPatch, RunItemSkip, RunItemSkipCode, RunItemSummary, RunItemsAdd,
    RunItemsAdded, RunStatus, RunStepAck, RunStepResult, RunStepWrite, StepStatus, TmError,
    TmLimits, TmResult, UntestedStepsTo,
};
use crate::ingest::LaunchRef;
use crate::{Page, Project, Store, no_nul};

/// Bytes of a step path (`1.2.3`), at most.
pub const MAX_STEP_PATH_BYTES: usize = 64;

/// A run item to insert.
#[derive(Clone, Debug)]
pub struct NewRunItem {
    pub case_id: Uuid,
    pub number: i64,
    pub name: String,
    pub version: i32,
    pub assignee: Option<String>,
}

/// A run item in "my queue", with its launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyRunItem {
    #[serde(flatten)]
    pub summary: RunItemSummary,
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub launch_name: String,
}

/// Filters of the run item list of a launch.
#[derive(Clone, Debug, Default)]
pub struct RunItemFilter {
    pub statuses: Vec<RunStatus>,
    /// `Some(None)`: unassigned items.
    pub assignee: Option<Option<String>>,
    /// Cases in this folder or its subfolders.
    pub folder: Option<Uuid>,
    pub before: Option<String>,
    pub limit: i64,
}

/// What a status change did, for the audit.
#[derive(Clone, Debug)]
pub struct RunItemChange {
    pub launch_id: Uuid,
    pub case_number: i64,
    pub old_status: RunStatus,
    pub new_status: RunStatus,
    pub assignee_changed: bool,
    pub revision: i64,
    /// The execution the change applied to (for a rerun: the one kept as history).
    pub execution_no: i32,
}

/// What closing a manual run did.
#[derive(Clone, Debug, Default)]
pub struct ManualCloseOutcome {
    /// Untested items set to `skipped`.
    pub skipped: i64,
    /// A purely manual launch became `finished`.
    pub launch_finished: bool,
    /// The gate of a finished launch was queued for evaluation again.
    pub gate_queued: bool,
}

/// The few fields of a run item that access checks need.
#[derive(Clone, Debug)]
pub struct RunItemBrief {
    pub id: Uuid,
    pub launch_id: Uuid,
    pub case_id: Uuid,
    pub case_number: i64,
    pub case_name: String,
    pub case_version: i32,
    pub status: RunStatus,
    pub assignee: Option<String>,
    pub executed_by: Option<String>,
    pub result_id: Option<Uuid>,
    pub manual_state: Option<ManualState>,
}

/// An iteration's projected result, the target of a defect.
#[derive(Clone, Debug)]
pub struct DefectTarget {
    pub row_id: String,
    pub result_id: Uuid,
    pub executed_by: Option<String>,
}

/// One step of the manual report.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualReportStep {
    pub path: String,
    pub action: String,
    pub expected: String,
    pub status: StepStatus,
    pub actual: String,
    pub comment: String,
}

/// One run item of the manual report.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualReportItem {
    pub case_number: i64,
    pub name: String,
    pub case_version: i32,
    pub status: RunStatus,
    pub execution_no: i32,
    pub assignee: Option<String>,
    pub executed_by: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub comment: String,
    pub defect: Option<serde_json::Value>,
    pub steps: Vec<ManualReportStep>,
    /// The parameter columns of a parameterized case, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameter_columns: Vec<String>,
    /// The iterations of a parameterized case (its `steps` stay empty: steps are recorded per
    /// iteration).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterations: Vec<ManualReportIteration>,
}

/// One iteration of a parameterized item in the manual report.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualReportIteration {
    pub row_id: String,
    pub position: i32,
    pub values: std::collections::BTreeMap<String, String>,
    pub status: RunStatus,
    pub comment: String,
    pub executed_by: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub defect: Option<serde_json::Value>,
    /// The recorded step results of the iteration, with the values put into the step texts.
    pub steps: Vec<ManualReportStep>,
    /// `true` when `steps` was left out because the item's report reached its size limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub steps_omitted: bool,
}

/// A step of an executed snapshot with its path (`1`, `1.2`, `1.2.3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepPath {
    pub path: String,
    pub leaf: bool,
    /// Paths of the enclosing steps, outermost first.
    pub ancestors: Vec<String>,
}

/// Every step of `steps` in document order.
pub fn step_paths(steps: &[CaseStep]) -> Vec<StepPath> {
    fn walk(steps: &[CaseStep], prefix: &str, anc: &[String], out: &mut Vec<StepPath>) {
        for (i, s) in steps.iter().enumerate() {
            let path = if prefix.is_empty() {
                format!("{}", i + 1)
            } else {
                format!("{prefix}.{}", i + 1)
            };
            out.push(StepPath {
                path: path.clone(),
                leaf: s.steps.is_empty(),
                ancestors: anc.to_vec(),
            });
            if !s.steps.is_empty() {
                let mut a = anc.to_vec();
                a.push(path.clone());
                walk(&s.steps, &path, &a, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(steps, "", &[], &mut out);
    out
}

/// The step at a path.
pub fn step_at<'a>(steps: &'a [CaseStep], path: &str) -> Option<&'a CaseStep> {
    let mut level = steps;
    let mut found = None;
    for part in path.split('.') {
        let i: usize = part.parse().ok()?;
        let s = level.get(i.checked_sub(1)?)?;
        found = Some(s);
        level = &s.steps;
    }
    found
}

/// A syntactically valid step path: `1`, `1.2`, ... (1-based, at most 10 levels).
pub fn valid_step_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_STEP_PATH_BYTES
        && p.split('.').count() <= 10
        && p.split('.').all(|x| {
            !x.is_empty()
                && x.len() <= 5
                && x.bytes().all(|b| b.is_ascii_digit())
                && !x.starts_with('0')
        })
}

/// The status the step results suggest for the item: `failed` when a step failed, else
/// `blocked` when one is blocked; `passed` (or `skipped` when nothing passed) once every leaf
/// step (or an enclosing step) has a status; `untested` with no results; else `in_progress`.
pub fn suggested_status(paths: &[StepPath], results: &HashMap<String, StepStatus>) -> RunStatus {
    let set = |p: &str| results.get(p).is_some_and(|s| *s != StepStatus::Untested);
    if results.values().any(|s| *s == StepStatus::Failed) {
        return RunStatus::Failed;
    }
    if results.values().any(|s| *s == StepStatus::Blocked) {
        return RunStatus::Blocked;
    }
    if !results.values().any(|s| *s != StepStatus::Untested) {
        return RunStatus::Untested;
    }
    let all = !paths.is_empty()
        && paths
            .iter()
            .filter(|p| p.leaf)
            .all(|p| set(&p.path) || p.ancestors.iter().any(|a| set(a)));
    if !all {
        return RunStatus::InProgress;
    }
    if results.values().any(|s| *s == StepStatus::Passed) {
        RunStatus::Passed
    } else {
        RunStatus::Skipped
    }
}

/// Severity of a step status for "the worst child": failed > blocked > skipped > passed;
/// `untested` counts only when every child is untested.
fn severity(s: StepStatus) -> u8 {
    match s {
        StepStatus::Untested => 0,
        StepStatus::Passed => 1,
        StepStatus::Skipped => 2,
        StepStatus::Blocked => 3,
        StepStatus::Failed => 4,
    }
}

/// The effective status of every step: a recorded status wins (a parent can be set by hand);
/// a parent without one takes the worst status of its children, ignoring untested children
/// while others have a status. The same rule as the runner's.
pub fn effective_statuses(
    steps: &[CaseStep],
    results: &HashMap<String, StepStatus>,
) -> HashMap<String, StepStatus> {
    fn walk(
        steps: &[CaseStep],
        prefix: &str,
        results: &HashMap<String, StepStatus>,
        out: &mut HashMap<String, StepStatus>,
    ) -> Vec<StepStatus> {
        let mut level = Vec::with_capacity(steps.len());
        for (i, s) in steps.iter().enumerate() {
            let path = if prefix.is_empty() {
                format!("{}", i + 1)
            } else {
                format!("{prefix}.{}", i + 1)
            };
            let children = walk(&s.steps, &path, results, out);
            let own = results
                .get(&path)
                .copied()
                .filter(|s| *s != StepStatus::Untested);
            let st = own.unwrap_or_else(|| {
                children
                    .iter()
                    .copied()
                    .max_by_key(|c| severity(*c))
                    .unwrap_or(StepStatus::Untested)
            });
            out.insert(path, st);
            level.push(st);
        }
        level
    }
    let mut out = HashMap::new();
    walk(steps, "", results, &mut out);
    out
}

/// Gets or creates the manual identities (test case and series) of many cases at once, in
/// hash order (the order ingest locks test cases in): case id → `(test_case_id, series_id)`.
/// Same rows as [`super::identity::manual_identity`].
pub async fn manual_identities(
    conn: &mut PgConnection,
    project_id: Uuid,
    cases: &[(Uuid, i64, String)],
) -> anyhow::Result<HashMap<Uuid, (Uuid, Uuid)>> {
    if cases.is_empty() {
        return Ok(HashMap::new());
    }
    let mut rows: Vec<(String, Uuid, i64, String)> = cases
        .iter()
        .map(|(id, n, name)| (manual_hash(*id), *id, *n, no_nul(name)))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.dedup_by(|a, b| a.0 == b.0);
    let ids: Vec<Uuid> = rows.iter().map(|_| Uuid::now_v7()).collect();
    let hashes: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let full: Vec<String> = rows.iter().map(|r| format!("TC-{}", r.2)).collect();
    let display: Vec<String> = rows.iter().map(|r| r.3.clone()).collect();
    let case_ids: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name, kind, case_id, \
           first_seen_at, last_seen_at) \
         select i, $1, h, f, d, 'manual', c, now(), now() \
         from unnest($2::uuid[], $3::text[], $4::text[], $5::text[], $6::uuid[]) as t(i, h, f, d, c) \
         on conflict (project_id, test_case_hash) do update set display_name = excluded.display_name, \
           case_id = excluded.case_id \
         where test_cases.display_name is distinct from excluded.display_name \
           or test_cases.case_id is distinct from excluded.case_id",
    )
    .bind(project_id)
    .bind(&ids)
    .bind(&hashes)
    .bind(&full)
    .bind(&display)
    .bind(&case_ids)
    .execute(&mut *conn)
    .await?;
    let tc: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "select test_case_hash, id from test_cases where project_id = $1 and test_case_hash = any($2)",
    )
    .bind(project_id)
    .bind(&hashes)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let params = parameters_hash(&[]);
    let mut keys = Vec::with_capacity(rows.len());
    let mut tcs = Vec::with_capacity(rows.len());
    for r in &rows {
        keys.push(retry_hash(&r.0, &params, None));
        tcs.push(
            *tc.get(&r.0)
                .ok_or_else(|| anyhow::anyhow!("manual identity row missing"))?,
        );
    }
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
    let s_ids: Vec<Uuid> = order.iter().map(|_| Uuid::now_v7()).collect();
    let s_tc: Vec<Uuid> = order.iter().map(|&i| tcs[i]).collect();
    let s_keys: Vec<String> = order.iter().map(|&i| keys[i].clone()).collect();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash, env_key, parameters, \
           first_seen_at, last_seen_at) \
         select i, $1, tc, k, $5, null, '[]', now(), now() \
         from unnest($2::uuid[], $3::uuid[], $4::text[]) as t(i, tc, k) \
         on conflict (project_id, series_key) do nothing",
    )
    .bind(project_id)
    .bind(&s_ids)
    .bind(&s_tc)
    .bind(&s_keys)
    .bind(&params)
    .execute(&mut *conn)
    .await?;
    let series: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "select series_key, id from series where project_id = $1 and series_key = any($2)",
    )
    .bind(project_id)
    .bind(&s_keys)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let mut out = HashMap::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let s = *series
            .get(&keys[i])
            .ok_or_else(|| anyhow::anyhow!("manual series row missing"))?;
        out.insert(r.1, (tcs[i], s));
    }
    Ok(out)
}

/// Inserts run items (cases already in the launch are skipped) and the iterations of the
/// parameterized ones (within `max_run_iterations`); returns how many items were added.
pub async fn insert_run_items(
    conn: &mut PgConnection,
    project_id: Uuid,
    launch_id: Uuid,
    launch_created_at: DateTime<Utc>,
    items: &[NewRunItem],
    limits: &TmLimits,
) -> TmResult<u64> {
    if items.is_empty() {
        return Ok(0);
    }
    let cases: Vec<(Uuid, i64, String)> = items
        .iter()
        .map(|i| (i.case_id, i.number, i.name.clone()))
        .collect();
    let identities = manual_identities(conn, project_id, &cases).await?;
    let ids: Vec<Uuid> = items.iter().map(|_| Uuid::now_v7()).collect();
    let case_ids: Vec<Uuid> = items.iter().map(|i| i.case_id).collect();
    let versions: Vec<i32> = items.iter().map(|i| i.version).collect();
    let series: Vec<Uuid> = items
        .iter()
        .map(|i| identities.get(&i.case_id).map(|x| x.1))
        .collect::<Option<_>>()
        .ok_or_else(|| anyhow::anyhow!("manual series missing"))?;
    let assignees: Vec<Option<String>> = items
        .iter()
        .map(|i| i.assignee.as_deref().map(no_nul))
        .collect();
    let names: Vec<String> = items.iter().map(|i| no_nul(&i.name)).collect();
    let review = super::review::review_settings(conn, project_id).await?;
    let keys: Vec<(Uuid, i32)> = items.iter().map(|i| (i.case_id, i.version)).collect();
    let pins = super::expand::pins_for_items(conn, project_id, &keys, &review).await?;
    let pin_values: Vec<Option<serde_json::Value>> = items
        .iter()
        .map(|i| {
            pins.get(&i.case_id)
                .filter(|p| !p.is_empty())
                .map(serde_json::to_value)
                .transpose()
        })
        .collect::<Result<_, _>>()?;
    let inserted: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "insert into run_items (id, project_id, launch_id, launch_created_at, case_id, case_version, series_id, assignee, \
           case_name, shared_pins) \
         select i, $1, $2, $3, c, v, s, a, n, p \
         from unnest($4::uuid[], $5::uuid[], $6::int[], $7::uuid[], $8::text[], $9::text[], $10::jsonb[]) \
           as t(i, c, v, s, a, n, p) \
         on conflict (launch_id, case_id) do nothing returning id, case_id, case_version",
    )
    .bind(project_id)
    .bind(launch_id)
    .bind(launch_created_at)
    .bind(&ids)
    .bind(&case_ids)
    .bind(&versions)
    .bind(&series)
    .bind(&assignees)
    .bind(&names)
    .bind(&pin_values)
    .fetch_all(&mut *conn)
    .await?;
    if !inserted.is_empty() {
        super::iterations::create_iterations(conn, project_id, launch_id, &inserted, limits)
            .await?;
    }
    Ok(inserted.len() as u64)
}

/// Manual progress of a launch (`None` without a manual run).
pub async fn launch_manual(
    conn: &mut PgConnection,
    launch_id: Uuid,
) -> anyhow::Result<Option<LaunchManual>> {
    let state: Option<Option<String>> =
        sqlx::query_scalar("select manual_state from launches where id = $1")
            .bind(launch_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(state) = state.flatten().as_deref().and_then(ManualState::parse) else {
        return Ok(None);
    };
    type Counts = (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
    );
    let (total, untested, in_progress, passed, failed, blocked, skipped, started_at, finished_at): Counts =
        sqlx::query_as(
            "select count(*), count(*) filter (where status = 'untested'), \
               count(*) filter (where status = 'in_progress'), count(*) filter (where status = 'passed'), \
               count(*) filter (where status = 'failed'), count(*) filter (where status = 'blocked'), \
               count(*) filter (where status = 'skipped'), min(started_at), max(finished_at) \
             from run_items where launch_id = $1",
        )
        .bind(launch_id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(Some(LaunchManual {
        state,
        total,
        untested,
        in_progress,
        passed,
        failed,
        blocked,
        skipped,
        started_at,
        finished_at,
    }))
}

/// Emits `manualRun.updated {launchId, manual}` (outbox and live) and notifies the launch.
pub async fn manual_changed(
    conn: &mut PgConnection,
    project_key: &str,
    launch_id: Uuid,
) -> anyhow::Result<()> {
    let manual = launch_manual(conn, launch_id).await?;
    let payload = serde_json::json!({"launchId": launch_id, "manual": manual});
    crate::events::emit(
        conn,
        "manualRun.updated",
        Some(project_key),
        payload.clone(),
    )
    .await?;
    crate::events::notify(
        conn,
        &crate::events::LiveEvent {
            project: project_key.to_string(),
            kind: "manualRun.updated".into(),
            data: payload,
        },
    )
    .await?;
    crate::events::notify_launch(conn, project_key, launch_id).await
}

/// Emits `runItem.updated {launchId, id, status, revision}` (outbox and live).
pub async fn item_changed(
    conn: &mut PgConnection,
    project_key: &str,
    launch_id: Uuid,
    id: Uuid,
    status: RunStatus,
    revision: i64,
) -> anyhow::Result<()> {
    let payload = serde_json::json!({"launchId": launch_id, "id": id, "status": status, "revision": revision});
    crate::events::emit(conn, "runItem.updated", Some(project_key), payload.clone()).await?;
    crate::events::notify(
        conn,
        &crate::events::LiveEvent {
            project: project_key.to_string(),
            kind: "runItem.updated".into(),
            data: payload,
        },
    )
    .await
}

/// Folder names of a case's folder, outermost first (`c` = cases).
pub(crate) const FOLDER_PATH_SQL: &str = "coalesce((select array_agg(f2.name order by p.o) \
    from case_folders f cross join lateral unnest(f.path_ids) with ordinality as p(id, o) \
    join case_folders f2 on f2.id = p.id where f.id = c.folder_id), '{}'::text[])";

fn summary_select() -> String {
    format!(
        "select ri.id, c.number, coalesce(ri.case_name, c.name) as name, ri.status, ri.assignee, ri.execution_no, \
         ri.updated_at, ri.iterations, {FOLDER_PATH_SQL} as folder_path"
    )
}

fn summary_row(r: &PgRow) -> sqlx::Result<RunItemSummary> {
    let status: String = r.try_get("status")?;
    Ok(RunItemSummary {
        id: r.try_get("id")?,
        case_number: r.try_get("number")?,
        name: r.try_get("name")?,
        folder_path: r.try_get("folder_path")?,
        status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
        assignee: r.try_get("assignee")?,
        execution_no: r.try_get("execution_no")?,
        updated_at: r.try_get("updated_at")?,
        iteration_count: r.try_get("iterations")?,
    })
}

fn encode_cursor(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

/// The execution number of a `GET /run-items/{id}/executions` cursor.
pub fn decode_execution_cursor(c: &str) -> Option<i32> {
    decode_cursor(c).ok()?.parse().ok()
}

fn decode_cursor(c: &str) -> TmResult<String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .map_err(|_| TmError::bad_request("invalid cursor"))?;
    String::from_utf8(raw).map_err(|_| TmError::bad_request("invalid cursor"))
}

/// The executed snapshot of an item (falls back to the current case when the version row is
/// missing), with its `shared` steps expanded to the pinned versions when `pins` is given.
pub(crate) async fn executed_snapshot(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    version: i32,
    pins: Option<&SharedPins>,
) -> anyhow::Result<CaseSnapshot> {
    let mut snap = match case_version_snapshot(conn, case_id, version).await? {
        Some(s) => s,
        None => {
            let c = super::lookup::case_by_id(conn, project_id, case_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("case of a run item vanished"))?;
            CaseSnapshot {
                name: c.name,
                status: c.status,
                folder_id: c.folder_id,
                body: c.body,
                fields: c.fields,
                tags: c.tags,
            }
        }
    };
    if let Some(p) = pins {
        super::expand::expand_steps(conn, project_id, &mut snap.body.steps, p).await?;
    }
    Ok(snap)
}

/// `run_items.shared_pins` (or `run_item_executions.shared_pins`) as stored.
pub(crate) fn pins_of(v: Option<serde_json::Value>) -> Option<SharedPins> {
    v.and_then(|v| serde_json::from_value(v).ok())
}

/// A cache key of an executed snapshot: the case version and its pins.
pub(crate) type SnapshotKey = (Uuid, i32, Option<String>);

pub(crate) fn snapshot_key(case_id: Uuid, version: i32, pins: Option<&SharedPins>) -> SnapshotKey {
    (
        case_id,
        version,
        pins.map(|p| serde_json::to_string(p).unwrap_or_default()),
    )
}

/// Attachment references by id (attachments of the project).
pub(crate) async fn attachment_refs(
    conn: &mut PgConnection,
    project_id: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, AttachmentRef>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, i64, String, Option<String>)> = sqlx::query_as(
        "select id, sha256, size, content_type, original_name from attachments \
         where project_id = $1 and id = any($2)",
    )
    .bind(project_id)
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, sha256, size, content_type, name)| {
            let name =
                name.unwrap_or_else(|| format!("attachment-{}", &sha256[..12.min(sha256.len())]));
            (
                id,
                AttachmentRef {
                    id,
                    sha256,
                    size,
                    content_type,
                    name,
                },
            )
        })
        .collect())
}

type StepRow = (
    String,
    String,
    String,
    String,
    Vec<Uuid>,
    String,
    DateTime<Utc>,
);

/// A step row with its execution number.
type ExecStepRow = (
    i32,
    String,
    String,
    String,
    String,
    Vec<Uuid>,
    String,
    DateTime<Utc>,
);

/// Step results of the current execution of an item.
async fn step_rows(conn: &mut PgConnection, item_id: Uuid) -> sqlx::Result<Vec<StepRow>> {
    sqlx::query_as(
        "select s.step_path, s.status, s.actual, s.comment, s.attachment_ids, s.updated_by, s.updated_at \
         from run_step_results s join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
         where s.run_item_id = $1",
    )
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await
}

fn step_result_of(
    (path, status, actual, comment, ids, updated_by, updated_at): StepRow,
    atts: &HashMap<Uuid, AttachmentRef>,
) -> RunStepResult {
    RunStepResult {
        path,
        status: StepStatus::parse(&status).unwrap_or(StepStatus::Untested),
        actual,
        comment,
        attachments: ids.iter().filter_map(|i| atts.get(i).cloned()).collect(),
        updated_by,
        updated_at,
    }
}

/// Skipped cases listed in the answer of `add_run_items`, at most.
pub const MAX_SKIPS_LISTED: usize = 1000;

/// Tries of closing a manual run while items are being added to it.
const CLOSE_ATTEMPTS: usize = 5;

/// Earlier executions of a run item kept at most per read.
pub const MAX_EXECUTIONS_PAGE: i64 = 100;

pub(super) fn path_key(p: &str) -> Vec<u32> {
    p.split('.').filter_map(|x| x.parse().ok()).collect()
}

/// The brief of a run item of the project.
pub async fn item_brief(
    conn: &mut PgConnection,
    project_id: Uuid,
    item_id: Uuid,
) -> sqlx::Result<Option<RunItemBrief>> {
    let r = sqlx::query(
        "select ri.id, ri.launch_id, ri.case_id, c.number, coalesce(ri.case_name, c.name) as name, ri.case_version, \
           ri.status, ri.assignee, ri.executed_by, ri.result_id, l.manual_state \
         from run_items ri join cases c on c.id = ri.case_id join launches l on l.id = ri.launch_id \
         where ri.project_id = $1 and ri.id = $2",
    )
    .bind(project_id)
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(r) = r else {
        return Ok(None);
    };
    let status: String = r.try_get("status")?;
    let state: Option<String> = r.try_get("manual_state")?;
    Ok(Some(RunItemBrief {
        id: r.try_get("id")?,
        launch_id: r.try_get("launch_id")?,
        case_id: r.try_get("case_id")?,
        case_number: r.try_get("number")?,
        case_name: r.try_get("name")?,
        case_version: r.try_get("case_version")?,
        status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
        assignee: r.try_get("assignee")?,
        executed_by: r.try_get("executed_by")?,
        result_id: r.try_get("result_id")?,
        manual_state: state.as_deref().and_then(ManualState::parse),
    }))
}

/// Reads the launch of a run (`lock`: `for update`) and answers 409 `manual_run_closed` when
/// the manual run is closed. Returns the launch reference. Without `lock`, read it after
/// locking the item: a close that changed the item has committed by then; a change of the
/// counters re-checks the state race-free in `apply_launch_changes`.
pub(super) async fn open_launch(
    conn: &mut PgConnection,
    project: &Project,
    launch_id: Uuid,
    lock: bool,
) -> TmResult<LaunchRef> {
    let sql = if lock {
        "select created_at, manual_state from launches where id = $1 and project_id = $2 for update"
    } else {
        "select created_at, manual_state from launches where id = $1 and project_id = $2"
    };
    let row: Option<(DateTime<Utc>, Option<String>)> = sqlx::query_as(sql)
        .bind(launch_id)
        .bind(project.id)
        .fetch_optional(&mut *conn)
        .await?;
    let (created_at, state) = row.ok_or(TmError::NotFound("launch"))?;
    if state.as_deref() == Some("closed") {
        return Err(closed());
    }
    Ok(LaunchRef {
        id: launch_id,
        project_id: project.id,
        project_key: project.key.clone(),
        created_at,
    })
}

/// Applies the launch side of an item change last in its transaction (see
/// [`super::projection::apply_launch_changes`]); 409 `manual_run_closed` when the manual run
/// was closed meanwhile.
pub(super) async fn finish_launch(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    p: &Projected,
) -> TmResult<()> {
    if super::projection::apply_launch_changes(
        conn,
        launch,
        &p.delta,
        &p.new_attempts,
        &p.rule_hits,
        true,
    )
    .await?
    {
        Ok(())
    } else {
        Err(closed())
    }
}

/// 409 `parameterized_item`: steps and status of a parameterized item are written per
/// iteration.
fn parameterized() -> TmError {
    TmError::conflict(
        "parameterized_item",
        "this run item has iterations: write the steps and status of an iteration \
         (/run-items/{itemId}/iterations/{rowId}), or set allIterations to change every iteration",
    )
}

pub(super) fn closed() -> TmError {
    TmError::conflict(
        "manual_run_closed",
        "the manual run is closed; reopen it first",
    )
}

/// Which version of a case runs and which cases stay out (§7.3): the runnable version under
/// the project's review policy, and states that block runs. `includeUnapproved` (checked for
/// `cases:review` by the caller) runs the head and ignores both skips.
pub struct RunnableRule {
    /// SQL of the version over alias `c` (null: not runnable).
    pub version_sql: &'static str,
    /// Workflow states whose cases do not run.
    pub blocking: Vec<String>,
}

impl RunnableRule {
    /// The rule of a project.
    pub async fn of(
        conn: &mut PgConnection,
        project_id: Uuid,
        include_unapproved: bool,
    ) -> anyhow::Result<Self> {
        if include_unapproved {
            return Ok(Self {
                version_sql: "c.version",
                blocking: Vec::new(),
            });
        }
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select settings from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&mut *conn)
                .await?;
        let s = crate::analysis::settings::ProjectSettings::from_value(
            v.unwrap_or(serde_json::Value::Null),
        );
        Ok(Self {
            version_sql: super::review::runnable_version_sql(&s.case_review),
            blocking: s
                .case_workflow
                .as_ref()
                .map(|w| w.blocking_keys())
                .unwrap_or_default(),
        })
    }

    /// Why a case with this runnable version and state does not run, if it does not.
    pub fn skip(&self, runnable: Option<i32>, state: Option<&str>) -> Option<RunItemSkipCode> {
        if state.is_some_and(|st| self.blocking.iter().any(|b| b == st)) {
            return Some(RunItemSkipCode::BlockedByState);
        }
        if runnable.is_none() {
            return Some(RunItemSkipCode::NotApproved);
        }
        None
    }
}

/// The current row of a locked item.
pub(super) struct LockedItem {
    pub(super) case_id: Uuid,
    pub(super) case_version: i32,
    pub(super) status: RunStatus,
    pub(super) revision: i64,
    pub(super) executed_by: Option<String>,
    pub(super) number: i64,
    pub(super) assignee: Option<String>,
    pub(super) execution_no: i32,
    pub(super) comment: String,
    pub(super) started_at: Option<DateTime<Utc>>,
    pub(super) finished_at: Option<DateTime<Utc>>,
    pub(super) defect: Option<serde_json::Value>,
    pub(super) shared_pins: Option<serde_json::Value>,
    /// Iterations of a parameterized item (0: a plain item).
    pub(super) iterations: i32,
}

pub(super) async fn lock_item(conn: &mut PgConnection, item_id: Uuid) -> TmResult<LockedItem> {
    let r = sqlx::query(
        "select ri.case_id, ri.case_version, ri.status, ri.revision, ri.executed_by, ri.assignee, c.number, \
           ri.execution_no, ri.comment, ri.started_at, ri.finished_at, ri.defect, ri.shared_pins, ri.iterations \
         from run_items ri join cases c on c.id = ri.case_id where ri.id = $1 for update of ri",
    )
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("run item"))?;
    let status: String = r.try_get("status")?;
    Ok(LockedItem {
        case_id: r.try_get("case_id")?,
        case_version: r.try_get("case_version")?,
        status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
        revision: r.try_get("revision")?,
        executed_by: r.try_get("executed_by")?,
        number: r.try_get("number")?,
        assignee: r.try_get("assignee")?,
        execution_no: r.try_get("execution_no")?,
        comment: r.try_get("comment")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
        defect: r.try_get("defect")?,
        shared_pins: r.try_get("shared_pins")?,
        iterations: r.try_get("iterations")?,
    })
}

async fn step_statuses(
    conn: &mut PgConnection,
    item_id: Uuid,
) -> sqlx::Result<HashMap<String, StepStatus>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select s.step_path, s.status from run_step_results s \
         join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
         where s.run_item_id = $1",
    )
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(p, s)| StepStatus::parse(&s).map(|s| (p, s)))
        .collect())
}

pub(super) fn check_text(what: &str, v: Option<&str>, max: usize) -> TmResult<()> {
    if let Some(v) = v
        && v.len() > max
    {
        return Err(TmError::Invalid {
            code: "invalid_markdown",
            message: format!("{what} has at most {max} bytes"),
            details: Some(serde_json::json!({"path": what, "max": max})),
        });
    }
    Ok(())
}

/// [`Store::patch_run_item`] of a parameterized item (locked, revision checked): a status or
/// `untestedStepsTo` applies to every iteration and only with `allIterations`; the item takes
/// the derived status; comment and assignee are the item's own.
#[allow(clippy::too_many_arguments)]
async fn patch_parameterized(
    tx: &mut PgConnection,
    project: &Project,
    launch: &LaunchRef,
    item_id: Uuid,
    item: &LockedItem,
    p: &RunItemPatch,
    assignee: Option<Option<String>>,
    by: &str,
) -> TmResult<RunItemChange> {
    if (p.status.is_some() || p.untested_steps_to.is_some()) && p.all_iterations != Some(true) {
        return Err(parameterized());
    }
    let rows: Vec<String> = sqlx::query_scalar(
        "select row_id from run_iterations where run_item_id = $1 and execution_no = $2 order by position, row_id",
    )
    .bind(item_id)
    .bind(item.execution_no)
    .fetch_all(&mut *tx)
    .await?;
    if p.untested_steps_to == Some(UntestedStepsTo::Skipped) {
        let snapshot = executed_snapshot(
            tx,
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
        super::iterations::skip_untested_steps(tx, item_id, item.execution_no, &rows, &leaves, by)
            .await?;
    }
    let changed = match p.status {
        Some(st) => {
            super::iterations::set_statuses(tx, item_id, item.execution_no, None, st, None, by)
                .await?
        }
        None => Vec::new(),
    };
    let (new_status, revision) =
        super::iterations::apply_derived(tx, item_id, item.execution_no, item.status, by).await?;
    let assignee_now: Option<String> = sqlx::query_scalar(
        "update run_items set comment = coalesce($2, comment), \
           assignee = case when $3 then $4 else assignee end where id = $1 returning assignee",
    )
    .bind(item_id)
    .bind(p.comment.as_deref().map(no_nul))
    .bind(assignee.is_some())
    .bind(assignee.clone().flatten())
    .fetch_one(&mut *tx)
    .await?;
    let mut projected = Projected::default();
    if changed
        .iter()
        .any(|(_, a, b)| a != b && (a.is_final() || b.is_final()))
    {
        projected = super::projection::project_run_items(tx, launch, &[item_id]).await?;
    }
    projected
        .delta
        .add(&CountDelta::item_change(item.status, new_status));
    if new_status != item.status {
        manual_changed(tx, &project.key, launch.id).await?;
    }
    for (row, a, b) in &changed {
        if a != b {
            super::iterations::iteration_changed(
                tx,
                &project.key,
                launch.id,
                item_id,
                row,
                *b,
                revision,
            )
            .await?;
        }
    }
    item_changed(tx, &project.key, launch.id, item_id, new_status, revision).await?;
    finish_launch(tx, launch, &projected).await?;
    Ok(RunItemChange {
        launch_id: launch.id,
        case_number: item.number,
        old_status: item.status,
        new_status,
        assignee_changed: assignee.is_some() && assignee_now != item.assignee,
        revision,
        execution_no: item.execution_no,
    })
}

impl Store {
    /// Adds run items to a launch from case numbers or a managed plan (its manual cases), and
    /// opens its manual run. Cases already in the launch are skipped.
    pub async fn add_run_items(
        &self,
        project: &Project,
        launch_id: Uuid,
        add: &RunItemsAdd,
        limits: &TmLimits,
    ) -> TmResult<RunItemsAdded> {
        let numbers = add.case_numbers.clone().unwrap_or_default();
        if numbers.is_empty() && add.plan_id.is_none() {
            return Err(TmError::bad_request("give caseNumbers or planId"));
        }
        if numbers.len() > limits.max_bulk {
            return Err(TmError::limit("maxBulk", limits.max_bulk as u64));
        }
        let assignee = match &add.assignee {
            Some(Some(a)) => Some(super::plans::check_assignee(a)?),
            _ => None,
        };
        let mut tx = self.pool.begin().await?;
        crate::ingest::lock_launch_exclusive(&mut tx, launch_id).await?;
        let launch = open_launch(&mut tx, project, launch_id, true).await?;
        let mut wanted: BTreeMap<Uuid, Option<String>> = BTreeMap::new();
        let mut skips: Vec<RunItemSkip> = Vec::new();
        let mut skipped_automated = 0i64;
        let named = super::plans::resolve_numbers(&mut tx, project.id, &numbers).await?;
        let automated: BTreeMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
            "select id, number from cases where id = any($1) and automated order by number",
        )
        .bind(&named)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        for c in named {
            match automated.get(&c) {
                Some(n) => {
                    skipped_automated += 1;
                    if skips.len() < MAX_SKIPS_LISTED {
                        skips.push(RunItemSkip {
                            case_number: *n,
                            code: RunItemSkipCode::Automated,
                        });
                    }
                }
                None => {
                    wanted.insert(c, assignee.clone());
                }
            }
        }
        if let Some(plan_id) = add.plan_id {
            let kind: Option<String> =
                sqlx::query_scalar("select kind from test_plans where project_id = $1 and id = $2")
                    .bind(project.id)
                    .bind(plan_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            match kind.as_deref() {
                None => return Err(TmError::NotFound("test plan")),
                Some("managed") => {}
                Some(_) => {
                    return Err(TmError::conflict(
                        "plan_not_managed",
                        "only managed plans have cases",
                    ));
                }
            }
            let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
                "select c.id, i.assignee from test_plan_items i join cases c on c.id = i.case_id \
                 where i.plan_id = $1 and c.archived_at is null and not c.automated \
                 order by i.position, c.number limit $2",
            )
            .bind(plan_id)
            .bind(limits.max_run_items as i64 + 1)
            .fetch_all(&mut *tx)
            .await?;
            for (c, a) in rows {
                let who = if add.assignee.is_some() {
                    assignee.clone()
                } else {
                    a
                };
                wanted.entry(c).or_insert(who);
            }
        }
        let existing: i64 =
            sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
                .bind(launch_id)
                .fetch_one(&mut *tx)
                .await?;
        let ids: Vec<Uuid> = wanted.keys().copied().collect();
        let present_rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "select ri.case_id, c.number from run_items ri join cases c on c.id = ri.case_id \
             where ri.launch_id = $1 and ri.case_id = any($2) order by c.number",
        )
        .bind(launch_id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        for (_, n) in &present_rows {
            if skips.len() < MAX_SKIPS_LISTED {
                skips.push(RunItemSkip {
                    case_number: *n,
                    code: RunItemSkipCode::InLaunch,
                });
            }
        }
        let present: BTreeSet<Uuid> = present_rows.into_iter().map(|r| r.0).collect();
        let fresh: Vec<Uuid> = ids
            .iter()
            .filter(|c| !present.contains(c))
            .copied()
            .collect();
        if existing as usize + fresh.len() > limits.max_run_items {
            return Err(TmError::limit("maxRunItems", limits.max_run_items as u64));
        }
        let rule = RunnableRule::of(&mut tx, project.id, add.include_unapproved).await?;
        type Candidate = (Uuid, i64, String, Option<i32>, Option<String>);
        let candidates: Vec<Candidate> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select c.id, c.number, c.name, {} as runnable, c.state from cases c \
                 where c.id = any($1) order by c.number",
            rule.version_sql
        )))
        .bind(&fresh)
        .fetch_all(&mut *tx)
        .await?;
        let mut rows: Vec<(Uuid, i64, String, i32)> = Vec::with_capacity(candidates.len());
        for (id, number, name, runnable, state) in candidates {
            match rule.skip(runnable, state.as_deref()) {
                Some(code) => {
                    if skips.len() < MAX_SKIPS_LISTED {
                        skips.push(RunItemSkip {
                            case_number: number,
                            code,
                        });
                    }
                }
                None => rows.push((id, number, name, runnable.unwrap_or(1))),
            }
        }
        let items: Vec<NewRunItem> = rows
            .into_iter()
            .map(|(case_id, number, name, version)| NewRunItem {
                case_id,
                number,
                name,
                version,
                assignee: wanted.get(&case_id).cloned().flatten(),
            })
            .collect();
        let added = insert_run_items(
            &mut tx,
            project.id,
            launch_id,
            launch.created_at,
            &items,
            limits,
        )
        .await?;
        sqlx::query(
            "update launches set manual_state = 'open' where id = $1 and manual_state is null",
        )
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        Store::recount_launch(&mut tx, launch_id).await?;
        manual_changed(&mut tx, &project.key, launch_id).await?;
        tx.commit().await?;
        Ok(RunItemsAdded {
            added: added as i64,
            skipped: ids.len() as i64 - added as i64 + skipped_automated,
            skips,
        })
    }

    /// Run items of a launch by case number.
    pub async fn run_items(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        f: &RunItemFilter,
    ) -> TmResult<Page<RunItemSummary>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(summary_select());
        qb.push(" from run_items ri join cases c on c.id = ri.case_id where ri.project_id = ")
            .push_bind(project_id)
            .push(" and ri.launch_id = ")
            .push_bind(launch_id);
        if !f.statuses.is_empty() {
            let st: Vec<String> = f.statuses.iter().map(|s| s.as_str().to_string()).collect();
            qb.push(" and ri.status = any(").push_bind(st).push(")");
        }
        match &f.assignee {
            Some(Some(a)) => {
                qb.push(" and ri.assignee = ").push_bind(no_nul(a));
            }
            Some(None) => {
                qb.push(" and ri.assignee is null");
            }
            None => {}
        }
        if let Some(folder) = f.folder {
            qb.push(
                " and c.folder_id in (select fo.id from case_folders fo where fo.project_id = ",
            )
            .push_bind(project_id)
            .push(" and fo.path_ids @> array[")
            .push_bind(folder)
            .push("]::uuid[])");
        }
        if let Some(b) = &f.before {
            let n: i64 = decode_cursor(b)?
                .parse()
                .map_err(|_| TmError::bad_request("invalid cursor"))?;
            qb.push(" and c.number > ").push_bind(n);
        }
        qb.push(" order by c.number limit ").push_bind(f.limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut items: Vec<RunItemSummary> =
            rows.iter().map(summary_row).collect::<Result<_, _>>()?;
        let next = if items.len() as i64 > f.limit {
            items.truncate(f.limit as usize);
            items
                .last()
                .map(|i| encode_cursor(&i.case_number.to_string()))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// Run items assigned to `me` in open manual runs of the project.
    pub async fn my_run_items(
        &self,
        project_id: Uuid,
        me: &str,
        statuses: &[RunStatus],
        before: Option<&str>,
        limit: i64,
    ) -> TmResult<Page<MyRunItem>> {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(summary_select());
        qb.push(
            ", l.id as launch_id, l.number as launch_number, l.name as launch_name \
             from run_items ri join cases c on c.id = ri.case_id join launches l on l.id = ri.launch_id \
             where ri.project_id = ",
        )
        .push_bind(project_id)
        .push(" and l.manual_state = 'open' and ri.assignee = ")
        .push_bind(no_nul(me));
        let st: Vec<String> = statuses.iter().map(|s| s.as_str().to_string()).collect();
        qb.push(" and ri.status = any(").push_bind(st).push(")");
        if let Some(b) = before {
            let id: Uuid = decode_cursor(b)?
                .parse()
                .map_err(|_| TmError::bad_request("invalid cursor"))?;
            qb.push(" and ri.id > ").push_bind(id);
        }
        qb.push(" order by ri.id limit ").push_bind(limit + 1);
        let rows = qb.build().fetch_all(&self.pool).await?;
        let mut items = Vec::with_capacity(rows.len());
        for r in &rows {
            items.push(MyRunItem {
                summary: summary_row(r)?,
                launch_id: r.try_get("launch_id")?,
                launch_number: r.try_get("launch_number")?,
                launch_name: r.try_get("launch_name")?,
            });
        }
        let next = if items.len() as i64 > limit {
            items.truncate(limit as usize);
            items
                .last()
                .map(|i| encode_cursor(&i.summary.id.to_string()))
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// The brief of a run item of the project.
    pub async fn run_item_brief(
        &self,
        project_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<Option<RunItemBrief>> {
        let mut conn = self.pool.acquire().await?;
        Ok(item_brief(&mut conn, project_id, item_id).await?)
    }

    /// A run item with the executed snapshot of its case (Markdown; the HTML fields are left
    /// empty for the caller to render).
    pub async fn run_item(&self, project_id: Uuid, item_id: Uuid) -> TmResult<RunItem> {
        let mut conn = self.pool.acquire().await?;
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(summary_select());
        qb.push(
            ", ri.launch_id, ri.case_id, ri.case_version, ri.comment, ri.defect, ri.result_id, ri.executed_by, \
             ri.started_at, ri.finished_at, ri.revision, ri.shared_pins \
             from run_items ri join cases c on c.id = ri.case_id where ri.project_id = ",
        )
        .push_bind(project_id)
        .push(" and ri.id = ")
        .push_bind(item_id);
        let r = qb
            .build()
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(TmError::NotFound("run item"))?;
        let summary = summary_row(&r)?;
        let case_id: Uuid = r.try_get("case_id")?;
        let case_version: i32 = r.try_get("case_version")?;
        let pins = pins_of(r.try_get("shared_pins")?);
        let snapshot =
            executed_snapshot(&mut conn, project_id, case_id, case_version, pins.as_ref()).await?;
        let shared_pins = match pins.as_ref().filter(|p| !p.is_empty()) {
            Some(p) => {
                let review = super::review::review_settings(&mut conn, project_id).await?;
                super::expand::pin_views(&mut conn, project_id, p, &review).await?
            }
            None => Vec::new(),
        };
        let (iterations, parameter_columns) = if summary.iteration_count > 0 {
            let mut budget = super::iterations::ITEM_VIEW_STEP_BYTES;
            (
                super::iterations::views(
                    &mut conn,
                    project_id,
                    item_id,
                    summary.execution_no,
                    super::iterations::ViewScope::Item {
                        budget: &mut budget,
                    },
                )
                .await?,
                snapshot
                    .body
                    .parameters
                    .as_ref()
                    .map(|p| p.columns.clone())
                    .unwrap_or_default(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let mut steps = step_rows(&mut conn, item_id).await?;
        steps.sort_by_key(|s| path_key(&s.0));
        let att_ids: Vec<Uuid> = steps.iter().flat_map(|s| s.4.iter().copied()).collect();
        let atts = attachment_refs(&mut conn, project_id, &att_ids).await?;
        let step_results = steps
            .into_iter()
            .map(|s| step_result_of(s, &atts))
            .collect();
        Ok(RunItem {
            summary,
            launch_id: r.try_get("launch_id")?,
            case_version,
            preconditions: snapshot.body.preconditions,
            preconditions_html: String::new(),
            steps: snapshot.body.steps,
            steps_html: Vec::new(),
            step_results,
            comment: r.try_get("comment")?,
            defect: r.try_get("defect")?,
            result_id: r.try_get("result_id")?,
            executed_by: r.try_get("executed_by")?,
            started_at: r.try_get("started_at")?,
            finished_at: r.try_get("finished_at")?,
            revision: r.try_get("revision")?,
            shared_pins,
            iterations,
            parameter_columns,
        })
    }

    /// Writes the result of one step (last writer wins; writing the same value again changes
    /// nothing but `updatedAt`). An untested item becomes `in_progress`; an item with a final
    /// status is projected again so its steps stay current. Only the item row is locked; the
    /// launch row is updated (at the end) only when the launch counters change: the first step
    /// of an execution, or a projection that changes the counted columns of the result.
    #[allow(clippy::too_many_arguments)]
    pub async fn put_run_step(
        &self,
        project: &Project,
        item_id: Uuid,
        path: &str,
        w: &RunStepWrite,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<RunStepAck> {
        if !valid_step_path(path) {
            return Err(TmError::NotFound("step"));
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
        if item.iterations > 0 {
            return Err(parameterized());
        }
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
            "insert into run_step_results (run_item_id, execution_no, step_path, status, actual, comment, \
               attachment_ids, updated_by, updated_at) \
             values ($1, $8, $2, $3, coalesce($4, ''), coalesce($5, ''), coalesce($6, '{}'::uuid[]), $7, now()) \
             on conflict (run_item_id, execution_no, step_path) do update set status = excluded.status, \
               actual = coalesce($4, run_step_results.actual), comment = coalesce($5, run_step_results.comment), \
               attachment_ids = coalesce($6, run_step_results.attachment_ids), \
               updated_by = excluded.updated_by, updated_at = now()",
        )
        .bind(item_id)
        .bind(path)
        .bind(w.status.as_str())
        .bind(w.actual.as_deref().map(no_nul))
        .bind(w.comment.as_deref().map(no_nul))
        .bind(attachments.as_deref())
        .bind(no_nul(by))
        .bind(item.execution_no)
        .execute(&mut *tx)
        .await?;
        let starts = item.status == RunStatus::Untested;
        let (status, revision): (String, i64) = sqlx::query_as(
            "update run_items set status = case when $2 then 'in_progress' else status end, \
               started_at = case when $2 then coalesce(started_at, now()) else started_at end, \
               executed_by = case when $2 then $3 else executed_by end, \
               revision = revision + 1, updated_at = now() \
             where id = $1 returning status, revision",
        )
        .bind(item_id)
        .bind(starts)
        .bind(no_nul(by))
        .fetch_one(&mut *tx)
        .await?;
        let status = RunStatus::parse(&status).unwrap_or(RunStatus::InProgress);
        let mut projected = Projected::default();
        if item.status.is_final() {
            projected = super::projection::project_run_items(&mut tx, &launch, &[item_id]).await?;
        } else if starts {
            projected.delta = CountDelta::item_change(item.status, status);
            manual_changed(&mut tx, &project.key, launch.id).await?;
        }
        let paths = step_paths(&snapshot.body.steps);
        let results = step_statuses(&mut tx, item_id).await?;
        let suggested = suggested_status(&paths, &results);
        item_changed(&mut tx, &project.key, launch.id, item_id, status, revision).await?;
        finish_launch(&mut tx, &launch, &projected).await?;
        tx.commit().await?;
        Ok(RunStepAck {
            item_status: status,
            suggested_status: suggested,
            revision,
        })
    }

    /// Changes the status, comment or assignee of an item at `revision`; a final status is
    /// projected into `results`, a change back to `untested`/`in_progress` removes the
    /// projected attempt.
    pub async fn patch_run_item(
        &self,
        project: &Project,
        item_id: Uuid,
        revision: i64,
        p: &RunItemPatch,
        by: &str,
        limits: &TmLimits,
    ) -> TmResult<RunItemChange> {
        check_text("comment", p.comment.as_deref(), limits.max_markdown_bytes)?;
        let assignee = match &p.assignee {
            Some(Some(a)) => Some(Some(super::plans::check_assignee(a)?)),
            Some(None) => Some(None),
            None => None,
        };
        if p.status.is_none()
            && p.comment.is_none()
            && p.assignee.is_none()
            && p.untested_steps_to.is_none()
        {
            return Err(TmError::bad_request(
                "nothing to change: give status, comment, assignee or untestedStepsTo",
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
        if item.revision != revision {
            return Err(TmError::Stale {
                current_revision: item.revision,
                changed_by: item.executed_by.clone(),
                source: Some("ui".into()),
            });
        }
        if item.iterations > 0 {
            let change =
                patch_parameterized(&mut tx, project, &launch, item_id, &item, p, assignee, by)
                    .await?;
            tx.commit().await?;
            return Ok(change);
        }
        if p.untested_steps_to == Some(UntestedStepsTo::Skipped) {
            let snapshot = executed_snapshot(
                &mut tx,
                project.id,
                item.case_id,
                item.case_version,
                pins_of(item.shared_pins.clone()).as_ref(),
            )
            .await?;
            let paths: Vec<String> = step_paths(&snapshot.body.steps)
                .into_iter()
                .filter(|p| p.leaf)
                .map(|p| p.path)
                .collect();
            if !paths.is_empty() {
                sqlx::query(
                    "insert into run_step_results (run_item_id, execution_no, step_path, status, updated_by) \
                     select $1, $4, p, 'skipped', $3 from unnest($2::text[]) as p \
                     on conflict (run_item_id, execution_no, step_path) do update set status = 'skipped', \
                       updated_by = excluded.updated_by, updated_at = now() \
                     where run_step_results.status = 'untested'",
                )
                .bind(item_id)
                .bind(&paths)
                .bind(no_nul(by))
                .bind(item.execution_no)
                .execute(&mut *tx)
                .await?;
            }
        }
        let new_status = p.status.unwrap_or(item.status);
        let status_changed = new_status != item.status;
        let (revision, assignee_now): (i64, Option<String>) = sqlx::query_as(
            "update run_items set status = $2, \
               comment = coalesce($3, comment), \
               assignee = case when $4 then $5 else assignee end, \
               started_at = case when $2 = 'untested' then null \
                 when $6 then coalesce(started_at, now()) else started_at end, \
               finished_at = case when $7 then now() when $2 in ('untested', 'in_progress') then null \
                 else finished_at end, \
               executed_by = case when $6 then $8 else executed_by end, \
               revision = revision + 1, updated_at = now() \
             where id = $1 returning revision, assignee",
        )
        .bind(item_id)
        .bind(new_status.as_str())
        .bind(p.comment.as_deref().map(no_nul))
        .bind(assignee.is_some())
        .bind(assignee.clone().flatten())
        .bind(status_changed && new_status != RunStatus::Untested)
        .bind(status_changed && new_status.is_final())
        .bind(no_nul(by))
        .fetch_one(&mut *tx)
        .await?;
        let mut projected = Projected::default();
        if item.status.is_final() || new_status.is_final() {
            projected = super::projection::project_run_items(&mut tx, &launch, &[item_id]).await?;
        }
        projected
            .delta
            .add(&CountDelta::item_change(item.status, new_status));
        if status_changed {
            manual_changed(&mut tx, &project.key, launch.id).await?;
        }
        item_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            new_status,
            revision,
        )
        .await?;
        finish_launch(&mut tx, &launch, &projected).await?;
        tx.commit().await?;
        Ok(RunItemChange {
            launch_id: launch.id,
            case_number: item.number,
            old_status: item.status,
            new_status,
            assignee_changed: assignee.is_some() && assignee_now != item.assignee,
            revision,
            execution_no: item.execution_no,
        })
    }

    /// Starts a new execution of an item at `revision`: `executionNo + 1`, status `untested`,
    /// no step results. The earlier execution (status, comment, executor, times, defect and its
    /// step results) is kept in `run_item_executions` and `run_step_results` outside the
    /// partitions, so it survives retention; its attempt stays in `attempts` (and its result
    /// until the new execution gets a final status).
    pub async fn rerun_run_item(
        &self,
        project: &Project,
        item_id: Uuid,
        revision: i64,
        by: &str,
    ) -> TmResult<RunItemChange> {
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
        if item.revision != revision {
            return Err(TmError::Stale {
                current_revision: item.revision,
                changed_by: item.executed_by.clone(),
                source: Some("ui".into()),
            });
        }
        sqlx::query(
            "insert into run_item_executions (run_item_id, execution_no, status, case_version, comment, executed_by, \
               started_at, finished_at, defect, rerun_by, shared_pins) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) on conflict (run_item_id, execution_no) do nothing",
        )
        .bind(item_id)
        .bind(item.execution_no)
        .bind(item.status.as_str())
        .bind(item.case_version)
        .bind(&item.comment)
        .bind(item.executed_by.as_deref())
        .bind(item.started_at)
        .bind(item.finished_at)
        .bind(item.defect.as_ref())
        .bind(no_nul(by))
        .bind(item.shared_pins.as_ref())
        .execute(&mut *tx)
        .await?;
        let review = super::review::review_settings(&mut tx, project.id).await?;
        super::expand::repin_item(&mut tx, project.id, item_id, &review).await?;
        let revision: i64 = sqlx::query_scalar(
            "update run_items set execution_no = execution_no + 1, status = 'untested', started_at = null, \
               finished_at = null, executed_by = null, comment = '', revision = revision + 1, updated_at = now() \
             where id = $1 returning revision",
        )
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        if item.iterations > 0 {
            super::iterations::copy_for_rerun(
                &mut tx,
                item_id,
                item.execution_no,
                item.execution_no + 1,
            )
            .await?;
        }
        manual_changed(&mut tx, &project.key, launch.id).await?;
        item_changed(
            &mut tx,
            &project.key,
            launch.id,
            item_id,
            RunStatus::Untested,
            revision,
        )
        .await?;
        let projected = Projected {
            delta: CountDelta::item_change(item.status, RunStatus::Untested),
            ..Default::default()
        };
        finish_launch(&mut tx, &launch, &projected).await?;
        tx.commit().await?;
        Ok(RunItemChange {
            launch_id: launch.id,
            case_number: item.number,
            old_status: item.status,
            new_status: RunStatus::Untested,
            assignee_changed: false,
            revision,
            execution_no: item.execution_no,
        })
    }

    /// Earlier executions of a run item of the project, newest first: executions before
    /// `before` (an execution number), at most `limit`.
    pub async fn run_item_executions(
        &self,
        project_id: Uuid,
        item_id: Uuid,
        before: Option<i32>,
        limit: i64,
    ) -> TmResult<Page<RunItemExecution>> {
        let mut conn = self.pool.acquire().await?;
        item_brief(&mut conn, project_id, item_id)
            .await?
            .ok_or(TmError::NotFound("run item"))?;
        let limit = limit.clamp(1, MAX_EXECUTIONS_PAGE);
        let rows = sqlx::query(
            "select execution_no, status, case_version, comment, executed_by, started_at, finished_at, defect, \
               rerun_by, rerun_at from run_item_executions \
             where run_item_id = $1 and ($2::int is null or execution_no < $2) \
             order by execution_no desc limit $3",
        )
        .bind(item_id)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
        let mut execs: Vec<(i32, PgRow)> = Vec::with_capacity(rows.len());
        for r in rows {
            execs.push((r.try_get("execution_no")?, r));
        }
        let next = if execs.len() as i64 > limit {
            execs.truncate(limit as usize);
            execs.last().map(|e| encode_cursor(&e.0.to_string()))
        } else {
            None
        };
        let nos: Vec<i32> = execs.iter().map(|e| e.0).collect();
        let srows: Vec<ExecStepRow> =
            sqlx::query_as(
                "select execution_no, step_path, status, actual, comment, attachment_ids, updated_by, updated_at \
                 from run_step_results where run_item_id = $1 and execution_no = any($2)",
            )
            .bind(item_id)
            .bind(&nos)
            .fetch_all(&mut *conn)
            .await?;
        let att_ids: Vec<Uuid> = srows.iter().flat_map(|s| s.5.iter().copied()).collect();
        let atts = attachment_refs(&mut conn, project_id, &att_ids).await?;
        let mut by_exec: HashMap<i32, Vec<StepRow>> = HashMap::new();
        for (no, p, st, a, c, ids, ub, ua) in srows {
            by_exec
                .entry(no)
                .or_default()
                .push((p, st, a, c, ids, ub, ua));
        }
        let counts: HashMap<i32, i64> = sqlx::query_as::<_, (i32, i64)>(
            "select execution_no, count(*) from run_iterations where run_item_id = $1 \
               and execution_no = any($2) group by execution_no",
        )
        .bind(item_id)
        .bind(&nos)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let mut case_id: Option<Uuid> = None;
        let mut budget = super::iterations::EXECUTIONS_VIEW_BYTES;
        let mut items = Vec::with_capacity(execs.len());
        for (no, r) in execs {
            let iteration_count = counts.get(&no).copied().unwrap_or(0) as i32;
            let (iterations, parameter_columns) = if iteration_count > 0 {
                let version: i32 = r.try_get("case_version")?;
                let case_id = match case_id {
                    Some(c) => c,
                    None => {
                        let c: Uuid =
                            sqlx::query_scalar("select case_id from run_items where id = $1")
                                .bind(item_id)
                                .fetch_one(&mut *conn)
                                .await?;
                        *case_id.insert(c)
                    }
                };
                let named = case_version_snapshot(&mut conn, case_id, version)
                    .await?
                    .and_then(|s| s.body.parameters)
                    .map(|p| p.columns);
                let its = super::iterations::views(
                    &mut conn,
                    project_id,
                    item_id,
                    no,
                    super::iterations::ViewScope::Page {
                        budget: &mut budget,
                    },
                )
                .await?;
                let cols = named.unwrap_or_else(|| {
                    its.first()
                        .map(|i| i.values.keys().cloned().collect())
                        .unwrap_or_default()
                });
                (its, cols)
            } else {
                (Vec::new(), Vec::new())
            };
            let mut steps = by_exec.remove(&no).unwrap_or_default();
            steps.sort_by_key(|s| path_key(&s.0));
            let status: String = r.try_get("status")?;
            items.push(RunItemExecution {
                execution_no: no,
                status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
                case_version: r.try_get("case_version")?,
                comment: r.try_get("comment")?,
                executed_by: r.try_get("executed_by")?,
                started_at: r.try_get("started_at")?,
                finished_at: r.try_get("finished_at")?,
                defect: r.try_get("defect")?,
                step_results: steps
                    .into_iter()
                    .map(|s| step_result_of(s, &atts))
                    .collect(),
                rerun_by: r.try_get("rerun_by")?,
                rerun_at: r.try_get("rerun_at")?,
                iterations,
                iteration_count,
                parameter_columns,
            });
        }
        Ok(Page { items, next })
    }

    /// Names seen as assignees or executors of run items and as plan assignees of the
    /// project, by name; with `q`, only names containing it (case-insensitive); at most `limit`.
    pub async fn seen_principals(
        &self,
        project_id: Uuid,
        q: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select n from (\
               select assignee as n from run_items where project_id = $1 and assignee is not null \
               union select executed_by from run_items where project_id = $1 and executed_by is not null \
               union select i.assignee from test_plan_items i join test_plans p on p.id = i.plan_id \
                 where p.project_id = $1 and i.assignee is not null) t \
             where $2::text is null or strpos(lower(n), lower($2)) > 0 order by n limit $3",
        )
        .bind(project_id)
        .bind(q.map(no_nul))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Copies the defect of an item's projected result (set by triage) onto the item. A
    /// parameterized item takes the defect of its first iteration (by position) that has one.
    pub async fn sync_run_item_defect(&self, project: &Project, item_id: Uuid) -> TmResult<i64> {
        let mut tx = self.pool.begin().await?;
        let row: Option<(Uuid, String, i64)> = sqlx::query_as(
            "update run_items ri set defect = case when ri.iterations > 0 then \
               (select r.defect from run_iterations it \
                  join results r on r.id = it.result_id and r.launch_created_at = ri.launch_created_at \
                where it.run_item_id = ri.id and it.execution_no = ri.execution_no and r.defect is not null \
                order by it.position limit 1) \
               else (select r.defect from results r where r.id = ri.result_id \
                  and r.launch_created_at = ri.launch_created_at) end, \
               revision = ri.revision + 1, updated_at = now() \
             where ri.project_id = $1 and ri.id = $2 \
             returning ri.launch_id, ri.status, ri.revision",
        )
        .bind(project.id)
        .bind(item_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (launch_id, status, revision) = row.ok_or(TmError::NotFound("run item"))?;
        item_changed(
            &mut tx,
            &project.key,
            launch_id,
            item_id,
            RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
            revision,
        )
        .await?;
        tx.commit().await?;
        Ok(revision)
    }

    /// The projected results a defect of a parameterized item goes to: the iteration `row_id`
    /// (empty while it has no final status), or without it the failed and blocked iterations
    /// of the current execution (at most `max`). `None` for a plain item.
    pub async fn defect_targets(
        &self,
        project_id: Uuid,
        item_id: Uuid,
        row_id: Option<&str>,
        max: i64,
    ) -> TmResult<Option<Vec<DefectTarget>>> {
        let mut conn = self.pool.acquire().await?;
        item_brief(&mut conn, project_id, item_id)
            .await?
            .ok_or(TmError::NotFound("run item"))?;
        let (count, exec): (i32, i32) =
            sqlx::query_as("select iterations, execution_no from run_items where id = $1")
                .bind(item_id)
                .fetch_one(&mut *conn)
                .await?;
        if count == 0 {
            return Ok(None);
        }
        let rows: Vec<(String, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "select row_id, result_id, executed_by from run_iterations \
             where run_item_id = $1 and execution_no = $2 \
               and (($3::text is not null and row_id = $3) \
                 or ($3::text is null and status in ('failed', 'blocked'))) \
             order by position, row_id limit $4",
        )
        .bind(item_id)
        .bind(exec)
        .bind(row_id)
        .bind(max)
        .fetch_all(&mut *conn)
        .await?;
        if row_id.is_some() && rows.is_empty() {
            return Err(TmError::NotFound("iteration"));
        }
        Ok(Some(
            rows.into_iter()
                .filter_map(|(row_id, result_id, executed_by)| {
                    result_id.map(|result_id| DefectTarget {
                        row_id,
                        result_id,
                        executed_by,
                    })
                })
                .collect(),
        ))
    }

    /// Closes the manual run of a launch: untested items become `skipped` (and are projected)
    /// unless `leave`; a purely manual running launch (no automated results, no uploads)
    /// becomes `finished`, which queues its gate; the gate of an already finished launch is
    /// queued again.
    pub async fn close_manual_run(
        &self,
        project: &Project,
        launch_id: Uuid,
        untested_to: ManualUntestedTo,
    ) -> TmResult<ManualCloseOutcome> {
        for _ in 0..CLOSE_ATTEMPTS {
            if let Some(out) = self.close_once(project, launch_id, untested_to).await? {
                return Ok(out);
            }
        }
        Err(TmError::Other(anyhow::anyhow!(
            "items kept being added to the run while it was closed"
        )))
    }

    /// One try of [`Store::close_manual_run`]; `None` when items were added meanwhile (rolled
    /// back). With `skipped`, every item is locked first (item writers lock the item first
    /// and the launch row last), so the items to skip are the ones open after every running
    /// item change; then the launch row is locked, the state checked again, and items added
    /// in between (adding locks the launch row) make it start over.
    async fn close_once(
        &self,
        project: &Project,
        launch_id: Uuid,
        untested_to: ManualUntestedTo,
    ) -> TmResult<Option<ManualCloseOutcome>> {
        let mut tx = self.pool.begin().await?;
        crate::ingest::lock_launch_exclusive(&mut tx, launch_id).await?;
        let state_of = |state: Option<&str>| match state {
            Some("open") => Ok(()),
            Some(_) => Err(closed()),
            None => Err(TmError::conflict(
                "no_manual_run",
                "the launch has no manual run",
            )),
        };
        let row: Option<(DateTime<Utc>, Option<String>)> = sqlx::query_as(
            "select created_at, manual_state from launches where id = $1 and project_id = $2",
        )
        .bind(launch_id)
        .bind(project.id)
        .fetch_optional(&mut *tx)
        .await?;
        let (created_at, state) = row.ok_or(TmError::NotFound("launch"))?;
        state_of(state.as_deref())?;
        let launch = LaunchRef {
            id: launch_id,
            project_id: project.id,
            project_key: project.key.clone(),
            created_at,
        };
        let mut out = ManualCloseOutcome::default();
        let mut projected = Projected::default();
        let mut locked = None;
        if untested_to == ManualUntestedTo::Skipped {
            let all: Vec<Uuid> = sqlx::query_scalar(
                "select id from run_items where launch_id = $1 order by id for update",
            )
            .bind(launch_id)
            .fetch_all(&mut *tx)
            .await?;
            locked = Some(all.len() as i64);
            // Every open iteration is skipped, also those of an item that is already `failed`
            // or `blocked` through another iteration (its derived status stays).
            let settled: Vec<Uuid> = sqlx::query_scalar(
                "with sk as (update run_iterations it set status = 'skipped', \
                   started_at = coalesce(it.started_at, now()), finished_at = now(), updated_at = now() \
                 from run_items ri where ri.launch_id = $1 and ri.iterations > 0 \
                   and it.run_item_id = ri.id and it.execution_no = ri.execution_no \
                   and it.status in ('untested', 'in_progress') returning it.run_item_id) \
                 select distinct run_item_id from sk",
            )
            .bind(launch_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "update run_items ri set status = case when ri.iterations > 0 \
                   then coalesce({}, 'skipped') else 'skipped' end, \
                   started_at = coalesce(started_at, now()), \
                   finished_at = now(), revision = revision + 1, updated_at = now() \
                 where launch_id = $1 and status in ('untested', 'in_progress') returning id",
                super::iterations::DERIVED_STATUS_SQL
            )))
            .bind(launch_id)
            .fetch_all(&mut *tx)
            .await?;
            out.skipped = ids.len() as i64;
            let open: HashSet<Uuid> = ids.iter().copied().collect();
            let settled: Vec<Uuid> = settled.into_iter().filter(|i| !open.contains(i)).collect();
            if !settled.is_empty() {
                sqlx::query(
                    "update run_items set revision = revision + 1, updated_at = now() where id = any($1)",
                )
                .bind(&settled)
                .execute(&mut *tx)
                .await?;
                ids.extend(settled);
            }
            for chunk in ids.chunks(1000) {
                projected
                    .merge(super::projection::project_run_items(&mut tx, &launch, chunk).await?);
            }
        }
        let (state, status): (Option<String>, String) =
            sqlx::query_as("select manual_state, status from launches where id = $1 for update")
                .bind(launch_id)
                .fetch_one(&mut *tx)
                .await?;
        state_of(state.as_deref())?;
        if let Some(n) = locked {
            let now: i64 =
                sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
                    .bind(launch_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if now != n {
                tx.rollback().await?;
                return Ok(None);
            }
        }
        super::projection::apply_launch_changes(
            &mut tx,
            &launch,
            &CountDelta::default(),
            &projected.new_attempts,
            &projected.rule_hits,
            false,
        )
        .await?;
        sqlx::query(
            "update launches set manual_state = 'closed', updated_at = now() where id = $1",
        )
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        let automated: bool = sqlx::query_scalar(
            "select exists (select 1 from results where launch_id = $1 and launch_created_at = $2 \
               and run_item_id is null) or exists (select 1 from uploads where launch_id = $1)",
        )
        .bind(launch_id)
        .bind(created_at)
        .fetch_one(&mut *tx)
        .await?;
        if automated {
            super::sync::queue_launch(&mut tx, project.id, launch_id).await?;
        }
        if status == "running" {
            if !automated {
                sqlx::query(
                    "update launches set status = 'finished', finished_at = coalesce(finished_at, now()), \
                     updated_at = now() where id = $1",
                )
                .bind(launch_id)
                .execute(&mut *tx)
                .await?;
                crate::events::emit(
                    &mut tx,
                    "launch.finished",
                    Some(&project.key),
                    serde_json::json!({"id": launch_id, "status": "finished"}),
                )
                .await?;
                out.launch_finished = true;
            }
        } else {
            sqlx::query(
                "insert into launch_gates (launch_id, project_id) values ($1, $2) \
                 on conflict (launch_id) do update set state = 'waiting', queued_at = now()",
            )
            .bind(launch_id)
            .bind(project.id)
            .execute(&mut *tx)
            .await?;
            out.gate_queued = true;
        }
        Store::recount_launch(&mut tx, launch_id).await?;
        manual_changed(&mut tx, &project.key, launch_id).await?;
        tx.commit().await?;
        Ok(Some(out))
    }

    /// Reopens a closed manual run. A launch started from a plan counts again toward its
    /// creator's `maxOpenPlanLaunches`.
    pub async fn reopen_manual_run(
        &self,
        project: &Project,
        launch_id: Uuid,
        limits: &TmLimits,
    ) -> TmResult<()> {
        // Before the transaction: retention may have dropped the month of an old run, and
        // projecting its items again would put their results into the default partition.
        let created: Option<DateTime<Utc>> =
            sqlx::query_scalar("select created_at from launches where id = $1 and project_id = $2")
                .bind(launch_id)
                .bind(project.id)
                .fetch_optional(&self.pool)
                .await?;
        if let Some(at) = created {
            crate::ingest::ensure_launch_partition(&self.pool, at).await;
        }
        let mut tx = self.pool.begin().await?;
        crate::ingest::lock_launch_exclusive(&mut tx, launch_id).await?;
        let row: Option<(Option<String>, Option<Uuid>, Option<String>)> = sqlx::query_as(
            "select manual_state, test_plan_id, created_by from launches where id = $1 and project_id = $2 for update",
        )
        .bind(launch_id)
        .bind(project.id)
        .fetch_optional(&mut *tx)
        .await?;
        let (state, plan, creator) = row.ok_or(TmError::NotFound("launch"))?;
        if state.as_deref() == Some("closed")
            && plan.is_some()
            && let Some(c) = creator.as_deref()
        {
            super::plans::check_open_plan_launches(&mut tx, project.id, c, limits).await?;
        }
        match state.as_deref() {
            Some("closed") => {}
            Some(_) => {
                return Err(TmError::conflict(
                    "manual_run_open",
                    "the manual run is open",
                ));
            }
            None => {
                return Err(TmError::conflict(
                    "no_manual_run",
                    "the launch has no manual run",
                ));
            }
        }
        sqlx::query("update launches set manual_state = 'open', updated_at = now() where id = $1")
            .bind(launch_id)
            .execute(&mut *tx)
            .await?;
        manual_changed(&mut tx, &project.key, launch_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// A page of the manual report of a launch: items after case number `after`, with their
    /// steps from the executed snapshot and the step results. The page ends early after the
    /// item whose step text takes it over `max_bytes` (then `true`: more items may follow), so
    /// expanded shared steps never hold a page of `limit` large snapshots in memory.
    pub async fn manual_report_page(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        after: i64,
        limit: i64,
        max_bytes: usize,
    ) -> anyhow::Result<(Vec<ManualReportItem>, bool)> {
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(
            "select ri.id, ri.case_id, c.number, coalesce(ri.case_name, c.name) as name, ri.case_version, ri.status, \
               ri.execution_no, ri.assignee, ri.executed_by, ri.started_at, ri.finished_at, ri.comment, ri.defect, \
               ri.shared_pins, ri.iterations, ri.execution_no as cur_exec \
             from run_items ri join cases c on c.id = ri.case_id \
             where ri.project_id = $1 and ri.launch_id = $2 and c.number > $3 order by c.number limit $4",
        )
        .bind(project_id)
        .bind(launch_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
        let ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<Result<_, _>>()?;
        let mut steps: HashMap<Uuid, Vec<(String, String, String, String)>> = HashMap::new();
        let srows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
            "select s.run_item_id, s.step_path, s.status, s.actual, s.comment from run_step_results s \
             join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
             where s.run_item_id = any($1)",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        for (i, p, s, a, c) in srows {
            steps.entry(i).or_default().push((p, s, a, c));
        }
        let mut out = Vec::with_capacity(rows.len());
        let mut bytes = 0usize;
        for r in &rows {
            if bytes > max_bytes {
                return Ok((out, true));
            }
            let id: Uuid = r.try_get("id")?;
            let case_id: Uuid = r.try_get("case_id")?;
            let version: i32 = r.try_get("case_version")?;
            let pins = pins_of(r.try_get("shared_pins")?);
            let snap =
                &executed_snapshot(&mut conn, project_id, case_id, version, pins.as_ref()).await?;
            let results: HashMap<String, (String, String, String)> = steps
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .map(|(p, s, a, c)| (p, (s, a, c)))
                .collect();
            let recorded: HashMap<String, StepStatus> = results
                .iter()
                .filter_map(|(p, (s, _, _))| StepStatus::parse(s).map(|s| (p.clone(), s)))
                .collect();
            let effective = effective_statuses(&snap.body.steps, &recorded);
            let report_steps: Vec<ManualReportStep> = step_paths(&snap.body.steps)
                .into_iter()
                .map(|sp| {
                    let step = step_at(&snap.body.steps, &sp.path);
                    let (_, actual, comment) = results.get(&sp.path).cloned().unwrap_or_default();
                    ManualReportStep {
                        action: step.map(|s| s.action.clone()).unwrap_or_default(),
                        expected: step.map(|s| s.expected.clone()).unwrap_or_default(),
                        status: effective
                            .get(&sp.path)
                            .copied()
                            .unwrap_or(StepStatus::Untested),
                        actual,
                        comment,
                        path: sp.path,
                    }
                })
                .collect();
            let status: String = r.try_get("status")?;
            let iteration_count: i32 = r.try_get("iterations")?;
            let (report_steps, iterations, parameter_columns) = if iteration_count > 0 {
                let mut budget = REPORT_ITERATION_BYTES;
                let its = super::iterations::views(
                    &mut conn,
                    project_id,
                    id,
                    r.try_get("cur_exec")?,
                    super::iterations::ViewScope::Item {
                        budget: &mut budget,
                    },
                )
                .await?;
                let list: Vec<ManualReportIteration> = its
                    .into_iter()
                    .map(|it| report_iteration(&snap.body.steps, it))
                    .collect();
                bytes += list
                    .iter()
                    .flat_map(|i| i.steps.iter())
                    .map(|s| s.action.len() + s.expected.len() + s.actual.len() + s.comment.len())
                    .sum::<usize>()
                    + list.len() * 128;
                let cols = snap
                    .body
                    .parameters
                    .as_ref()
                    .map(|p| p.columns.clone())
                    .unwrap_or_default();
                (Vec::new(), list, cols)
            } else {
                (report_steps, Vec::new(), Vec::new())
            };
            bytes += report_steps
                .iter()
                .map(|s| {
                    s.action.len()
                        + s.expected.len()
                        + s.actual.len()
                        + s.comment.len()
                        + s.path.len()
                })
                .sum::<usize>();
            out.push(ManualReportItem {
                case_number: r.try_get("number")?,
                name: r.try_get("name")?,
                case_version: version,
                status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
                execution_no: r.try_get("execution_no")?,
                assignee: r.try_get("assignee")?,
                executed_by: r.try_get("executed_by")?,
                started_at: r.try_get("started_at")?,
                finished_at: r.try_get("finished_at")?,
                comment: r.try_get("comment")?,
                defect: r.try_get("defect")?,
                steps: report_steps,
                parameter_columns,
                iterations,
            });
        }
        Ok((out, false))
    }
}

/// Bytes of step results one parameterized item carries in the manual report.
const REPORT_ITERATION_BYTES: usize = 8 * 1024 * 1024;
/// Bytes the values may add to the step texts of one iteration in the report.
const REPORT_SUBSTITUTION_BYTES: usize = 64 * 1024;

fn report_iteration(steps: &[CaseStep], it: super::RunIteration) -> ManualReportIteration {
    let mut budget = REPORT_SUBSTITUTION_BYTES;
    let report_steps = it
        .step_results
        .iter()
        .map(|r| {
            let step = step_at(steps, &r.path);
            let text = |t: Option<&str>, budget: &mut usize| {
                super::parameters::substitute(t.unwrap_or_default(), &it.values, budget)
            };
            ManualReportStep {
                action: text(step.map(|s| s.action.as_str()), &mut budget),
                expected: text(step.map(|s| s.expected.as_str()), &mut budget),
                path: r.path.clone(),
                status: r.status,
                actual: r.actual.clone(),
                comment: r.comment.clone(),
            }
        })
        .collect();
    ManualReportIteration {
        row_id: it.row_id,
        position: it.position,
        values: it.values,
        status: it.status,
        comment: it.comment,
        executed_by: it.executed_by,
        started_at: it.started_at,
        finished_at: it.finished_at,
        defect: it.defect,
        steps: report_steps,
        steps_omitted: it.step_results_omitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(action: &str, children: Vec<CaseStep>) -> CaseStep {
        CaseStep {
            id: String::new(),
            action: action.into(),
            expected: String::new(),
            data: String::new(),
            attachments: Vec::new(),
            steps: children,
            shared: None,
        }
    }

    #[test]
    fn paths_and_lookup() {
        let steps = vec![
            step(
                "a",
                vec![step("a1", vec![]), step("a2", vec![step("a2x", vec![])])],
            ),
            step("b", vec![]),
        ];
        let p: Vec<String> = step_paths(&steps).into_iter().map(|p| p.path).collect();
        assert_eq!(p, ["1", "1.1", "1.2", "1.2.1", "2"]);
        assert_eq!(step_at(&steps, "1.2.1").unwrap().action, "a2x");
        assert!(step_at(&steps, "3").is_none());
        assert!(step_at(&steps, "1.0").is_none());
        assert!(valid_step_path("1.2.3"));
        for bad in [
            "",
            "0",
            "1.",
            ".1",
            "01",
            "1..2",
            "a",
            "1.2.3.4.5.6.7.8.9.10.11",
        ] {
            assert!(!valid_step_path(bad), "{bad}");
        }
    }

    #[test]
    fn parents_follow_the_worst_child() {
        let steps = vec![
            step("a", vec![step("a1", vec![]), step("a2", vec![])]),
            step("b", vec![]),
        ];
        let m = |v: &[(&str, StepStatus)]| -> HashMap<String, StepStatus> {
            v.iter().map(|(p, s)| (p.to_string(), *s)).collect()
        };
        let e = effective_statuses(
            &steps,
            &m(&[("1.1", StepStatus::Blocked), ("1.2", StepStatus::Skipped)]),
        );
        assert_eq!(
            e["1"],
            StepStatus::Blocked,
            "a blocked child wins over skipped"
        );
        let e = effective_statuses(
            &steps,
            &m(&[("1.1", StepStatus::Passed), ("1", StepStatus::Untested)]),
        );
        assert_eq!(e["1"], StepStatus::Passed, "untested children are ignored");
        assert_eq!(e["2"], StepStatus::Untested);
        let e = effective_statuses(
            &steps,
            &m(&[("1.1", StepStatus::Failed), ("1", StepStatus::Passed)]),
        );
        assert_eq!(e["1"], StepStatus::Passed, "a parent set by hand wins");
    }

    #[test]
    fn suggestion() {
        let steps = vec![
            step("a", vec![step("a1", vec![]), step("a2", vec![])]),
            step("b", vec![]),
        ];
        let paths = step_paths(&steps);
        let m = |v: &[(&str, StepStatus)]| -> HashMap<String, StepStatus> {
            v.iter().map(|(p, s)| (p.to_string(), *s)).collect()
        };
        assert_eq!(suggested_status(&paths, &m(&[])), RunStatus::Untested);
        assert_eq!(
            suggested_status(&paths, &m(&[("1.1", StepStatus::Passed)])),
            RunStatus::InProgress
        );
        assert_eq!(
            suggested_status(
                &paths,
                &m(&[("1", StepStatus::Passed), ("2", StepStatus::Passed)])
            ),
            RunStatus::Passed
        );
        assert_eq!(
            suggested_status(
                &paths,
                &m(&[
                    ("1.1", StepStatus::Passed),
                    ("1.2", StepStatus::Skipped),
                    ("2", StepStatus::Blocked)
                ])
            ),
            RunStatus::Blocked
        );
        assert_eq!(
            suggested_status(
                &paths,
                &m(&[("1.1", StepStatus::Failed), ("2", StepStatus::Blocked)])
            ),
            RunStatus::Failed
        );
        assert_eq!(
            suggested_status(
                &paths,
                &m(&[("1", StepStatus::Skipped), ("2", StepStatus::Skipped)])
            ),
            RunStatus::Skipped
        );
    }
}
