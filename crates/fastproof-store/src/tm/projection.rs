//! Projection of run items into `results` and `attempts`, in the transaction that changed the
//! items (the caller holds the item rows).
//!
//! A final status (`passed`, `failed`, `blocked`, `skipped`) upserts one result on the series
//! of the case's manual identity (`run_item_id`; labels `manual=true`, `case=TC-<n>`,
//! `executedBy`, `manualStatus=<original>`; `blocked` is stored as `broken`) and one attempt
//! per execution (`client_key = 'manual:<item>:<execution_no>'`, `steps_z` in the shape the
//! UI's step tree reads). Going back to `untested`/`in_progress` removes the attempt of the
//! current execution; the result goes with its last attempt, or shows the previous execution.
//! Then the analysis hook runs on the affected series (manual series are never flaky).
//!
//! The projection does not touch the launch row: it returns what the launch row needs (the
//! change of its counters, the attempts waiting for a `seq`, the rule hits), and the caller
//! applies it with [`apply_launch_changes`] as the last step of its transaction, so the
//! launch row is locked only from that update to the commit.
//!
//! Lock order of manual writes: the run item, then its result row, then the launch row, then
//! rule rows. Ingest locks the launch row first and never touches manual rows, and
//! `recount_muted` locks results before launches, so neither waits in a cycle with them.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use fastproof_model::{
    AttachmentId, AttachmentKind, AttachmentRef as ModelAttachment, Status, StatusDetails, Step,
    StepKind,
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::runs::{FOLDER_PATH_SQL, SnapshotKey, attachment_refs, executed_snapshot, snapshot_key};
use super::{AttachmentRef, CaseSnapshot, CaseStep, RunStatus, StepStatus};
use crate::ingest::LaunchRef;
use crate::{codec, json_no_nul, no_nul};

/// Characters of a projected message, at most.
const MESSAGE_CHARS: usize = 4000;
/// Characters of a projected step name, at most.
const STEP_NAME_CHARS: usize = 1000;

/// The status of a projected result: `blocked` is stored as `broken` (the case could not be
/// executed; gates count it as a failure).
pub fn result_status(s: RunStatus) -> Option<Status> {
    match s {
        RunStatus::Passed => Some(Status::Passed),
        RunStatus::Failed => Some(Status::Failed),
        RunStatus::Blocked => Some(Status::Broken),
        RunStatus::Skipped => Some(Status::Skipped),
        RunStatus::Untested | RunStatus::InProgress => None,
    }
}

fn step_status(s: StepStatus) -> Status {
    match s {
        StepStatus::Passed => Status::Passed,
        StepStatus::Failed => Status::Failed,
        StepStatus::Blocked => Status::Broken,
        StepStatus::Skipped => Status::Skipped,
        StepStatus::Untested => Status::Unknown,
    }
}

fn cut(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The client key of the attempt of one execution.
pub fn client_key(item: Uuid, execution_no: i32) -> String {
    format!("manual:{item}:{execution_no}")
}

/// The labels of a projected result.
pub fn manual_labels(number: i64, executed_by: Option<&str>, status: &str) -> serde_json::Value {
    let mut v = vec![
        serde_json::json!({"name": "manual", "value": "true"}),
        serde_json::json!({"name": "case", "value": format!("TC-{number}")}),
    ];
    if let Some(by) = executed_by {
        v.push(serde_json::json!({"name": "executedBy", "value": by}));
    }
    v.push(serde_json::json!({"name": "manualStatus", "value": status}));
    json_no_nul(serde_json::Value::Array(v))
}

/// One step result as the projection reads it.
#[derive(Clone, Debug, Default)]
struct StepRes {
    status: Option<StepStatus>,
    actual: String,
    comment: String,
    attachments: Vec<Uuid>,
}

fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

/// The executed steps in the shape of `attempts.steps_z` (`Vec<Step>`).
fn build_steps(
    steps: &[CaseStep],
    prefix: &str,
    results: &HashMap<String, StepRes>,
    effective: &HashMap<String, StepStatus>,
    atts: &HashMap<Uuid, AttachmentRef>,
) -> Vec<Step> {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let path = if prefix.is_empty() {
                format!("{}", i + 1)
            } else {
                format!("{prefix}.{}", i + 1)
            };
            let r = results.get(&path).cloned().unwrap_or_default();
            Step {
                name: cut(s.action.trim(), STEP_NAME_CHARS),
                status: step_status(
                    effective
                        .get(&path)
                        .copied()
                        .or(r.status)
                        .unwrap_or(StepStatus::Untested),
                ),
                status_details: StatusDetails {
                    message: non_empty(&r.comment),
                    actual: non_empty(&r.actual),
                    expected: non_empty(&s.expected),
                    ..Default::default()
                },
                attachments: r
                    .attachments
                    .iter()
                    .filter_map(|id| atts.get(id))
                    .map(|a| ModelAttachment {
                        name: a.name.clone(),
                        source: a.name.clone(),
                        content_type: Some(a.content_type.clone()),
                        size: Some(a.size.max(0) as u64),
                        kind: AttachmentKind::infer(Some(&a.content_type), &a.name),
                        id: Some(AttachmentId(a.id.to_string())),
                        resolved_path: None,
                        missing: false,
                    })
                    .collect(),
                steps: build_steps(&s.steps, &path, results, effective, atts),
                kind: StepKind::Step,
                ..Default::default()
            }
        })
        .collect()
}

/// The message of a projected attempt: the item comment, else the first failed or blocked
/// step with its actual result.
fn message_of(comment: &str, steps: &HashMap<String, StepRes>) -> Option<String> {
    if let Some(c) = non_empty(comment) {
        return Some(cut(&c, MESSAGE_CHARS));
    }
    let mut bad: Vec<(&String, &StepRes)> = steps
        .iter()
        .filter(|(_, r)| matches!(r.status, Some(StepStatus::Failed | StepStatus::Blocked)))
        .collect();
    bad.sort_by_key(|(p, _)| {
        p.split('.')
            .filter_map(|x| x.parse::<u32>().ok())
            .collect::<Vec<_>>()
    });
    let (p, r) = bad.first()?;
    let what = non_empty(&r.actual).or_else(|| non_empty(&r.comment));
    Some(cut(
        &match what {
            Some(w) => format!("Step {p}: {w}"),
            None => format!("Step {p} {}", r.status.map(|s| s.as_str()).unwrap_or("")),
        },
        MESSAGE_CHARS,
    ))
}

/// A change of the launch counters (`launches.count_*`), applied by [`apply_launch_changes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountDelta {
    pub total: i32,
    pub passed: i32,
    pub failed: i32,
    pub broken: i32,
    pub skipped: i32,
    pub unknown: i32,
    pub flaky: i32,
    pub muted: i32,
    pub untested: i32,
    pub in_progress: i32,
}

/// The columns of a result row the launch counters count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountedResult {
    pub status: String,
    pub flaky_in_run: bool,
    pub muted: bool,
}

impl CountDelta {
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// Adds (`sign` 1) or removes (`sign` -1) one result row, as the recount counts it.
    pub fn add_result(&mut self, r: &CountedResult, sign: i32) {
        self.total += sign;
        match r.status.as_str() {
            "passed" => self.passed += sign,
            "failed" => self.failed += sign,
            "broken" => self.broken += sign,
            "skipped" => self.skipped += sign,
            "unknown" => self.unknown += sign,
            _ => {}
        }
        if r.flaky_in_run {
            self.flaky += sign;
        }
        if r.muted {
            self.muted += sign;
        }
    }

    /// Adds (`sign` 1) or removes (`sign` -1) one run item, as the recount counts it.
    pub fn add_item(&mut self, s: RunStatus, sign: i32) {
        match s {
            RunStatus::Untested => self.untested += sign,
            RunStatus::InProgress => self.in_progress += sign,
            _ => {}
        }
    }

    /// The change of a result row from `before` to `after` (`None` = no row).
    pub fn result_change(before: Option<&CountedResult>, after: Option<&CountedResult>) -> Self {
        let mut d = Self::default();
        if let Some(b) = before {
            d.add_result(b, -1);
        }
        if let Some(a) = after {
            d.add_result(a, 1);
        }
        d
    }

    /// The change of a run item's status.
    pub fn item_change(old: RunStatus, new: RunStatus) -> Self {
        let mut d = Self::default();
        d.add_item(old, -1);
        d.add_item(new, 1);
        d
    }

    pub fn add(&mut self, o: &Self) {
        self.total += o.total;
        self.passed += o.passed;
        self.failed += o.failed;
        self.broken += o.broken;
        self.skipped += o.skipped;
        self.unknown += o.unknown;
        self.flaky += o.flaky;
        self.muted += o.muted;
        self.untested += o.untested;
        self.in_progress += o.in_progress;
    }
}

/// What a projection leaves for the launch row (see [`apply_launch_changes`]).
#[derive(Clone, Debug, Default)]
pub struct Projected {
    /// The change of the counters from the projected result rows.
    pub delta: CountDelta,
    /// Attempts inserted with the provisional `seq` 0, in projection order.
    pub new_attempts: Vec<Uuid>,
    /// Rule hits of the analysis hook, not added yet.
    pub rule_hits: HashMap<Uuid, i64>,
}

impl Projected {
    pub fn merge(&mut self, o: Projected) {
        self.delta.add(&o.delta);
        self.new_attempts.extend(o.new_attempts);
        for (k, v) in o.rule_hits {
            *self.rule_hits.entry(k).or_default() += v;
        }
    }
}

/// Applies the launch side of manual writes with one update of the launch row: adds `delta`
/// to the counters and gives the new attempts their `seq` (in order, after every `seq` handed
/// out so far); then adds the rule hits. With `open_only`, nothing changes and `false` comes
/// back when the manual run is not open: the update waits for a concurrent close and then
/// sees its result, so the check has no race. Run it last before the commit: the launch row
/// stays locked until then, which also keeps `seq` in commit order.
pub async fn apply_launch_changes(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    delta: &CountDelta,
    new_attempts: &[Uuid],
    rule_hits: &HashMap<Uuid, i64>,
    open_only: bool,
) -> anyhow::Result<bool> {
    if !delta.is_zero() || !new_attempts.is_empty() {
        let n = new_attempts.len() as i64;
        let seq_end: Option<i64> = sqlx::query_scalar(
            "update launches set attempt_seq = attempt_seq + $2, count_total = count_total + $3, \
               count_passed = count_passed + $4, count_failed = count_failed + $5, \
               count_broken = count_broken + $6, count_skipped = count_skipped + $7, \
               count_unknown = count_unknown + $8, count_flaky = count_flaky + $9, \
               count_muted = count_muted + $10, count_untested = count_untested + $11, \
               count_in_progress = count_in_progress + $12, updated_at = now() \
             where id = $1 and (not $13 or manual_state = 'open') returning attempt_seq",
        )
        .bind(launch.id)
        .bind(n)
        .bind(delta.total)
        .bind(delta.passed)
        .bind(delta.failed)
        .bind(delta.broken)
        .bind(delta.skipped)
        .bind(delta.unknown)
        .bind(delta.flaky)
        .bind(delta.muted)
        .bind(delta.untested)
        .bind(delta.in_progress)
        .bind(open_only)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(seq_end) = seq_end else {
            return Ok(false);
        };
        if n > 0 {
            sqlx::query(
                "update attempts a set seq = $3 + t.o from unnest($1::uuid[]) with ordinality as t(i, o) \
                 where a.id = t.i and a.launch_created_at = $2",
            )
            .bind(new_attempts)
            .bind(launch.created_at)
            .bind(seq_end - n)
            .execute(&mut *conn)
            .await?;
        }
    }
    crate::analysis::rules::add_hits(conn, rule_hits).await?;
    Ok(true)
}

/// The counted columns of result rows by id.
async fn counted_results(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, CountedResult>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, bool, bool)> = sqlx::query_as(
        "select id, status, flaky_in_run, muted from results where id = any($1) and launch_created_at = $2",
    )
    .bind(ids)
    .bind(launch.created_at)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, status, flaky_in_run, muted)| {
            (
                id,
                CountedResult {
                    status,
                    flaky_in_run,
                    muted,
                },
            )
        })
        .collect())
}

struct ItemRow {
    id: Uuid,
    case_id: Uuid,
    case_version: i32,
    series_id: Uuid,
    test_case_id: Uuid,
    status: RunStatus,
    execution_no: i32,
    executed_by: Option<String>,
    comment: String,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    defect: Option<serde_json::Value>,
    number: i64,
    folder_path: Vec<String>,
    shared_pins: Option<super::expand::SharedPins>,
    iterations: i32,
}

/// One projected unit: a plain run item, or one iteration of a parameterized item.
struct Unit<'a> {
    item: &'a ItemRow,
    /// The iteration's row id and values (`None`: a plain item).
    row: Option<(String, BTreeMap<String, String>)>,
    series_id: Uuid,
    status: RunStatus,
    executed_by: Option<String>,
    comment: String,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

impl Unit<'_> {
    fn key(&self) -> String {
        match &self.row {
            Some((r, _)) => super::iterations::client_key(self.item.id, self.item.execution_no, r),
            None => client_key(self.item.id, self.item.execution_no),
        }
    }

    fn step_key(&self) -> UnitKey {
        (self.item.id, self.row.as_ref().map(|r| r.0.clone()))
    }
}

/// Bytes the parameter values may add to the steps of one projected iteration, at most.
const SUBSTITUTION_BUDGET: usize = 1024 * 1024;

/// Iterations projected by one statement round, at most. Each holds the built steps of one
/// execution (up to the executed snapshot plus [`SUBSTITUTION_BUDGET`]) until the insert, so
/// a launch with `maxRunIterations` iterations is projected in bounded rounds.
pub const ITERATION_CHUNK: usize = 64;

/// Projects run items of one launch (the caller holds the item rows) and returns what the
/// launch row needs; the caller passes it to [`apply_launch_changes`]. A parameterized item
/// is projected through its iterations (of the current execution), never itself, in rounds
/// of at most [`ITERATION_CHUNK`] iterations.
pub async fn project_run_items(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    item_ids: &[Uuid],
) -> anyhow::Result<Projected> {
    if item_ids.is_empty() {
        return Ok(Projected::default());
    }
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select it.run_item_id, it.row_id from run_iterations it \
         join run_items ri on ri.id = it.run_item_id and ri.execution_no = it.execution_no \
         where ri.launch_id = $1 and ri.id = any($2) and ri.iterations > 0 \
         order by it.run_item_id, it.position, it.row_id",
    )
    .bind(launch.id)
    .bind(item_ids)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() <= ITERATION_CHUNK {
        return project(conn, launch, item_ids, None).await;
    }
    let parameterized: HashSet<Uuid> = rows.iter().map(|r| r.0).collect();
    let plain: Vec<Uuid> = item_ids
        .iter()
        .filter(|i| !parameterized.contains(i))
        .copied()
        .collect();
    let mut out = project(conn, launch, &plain, None).await?;
    for chunk in rows.chunks(ITERATION_CHUNK) {
        out.merge(project_iterations(conn, launch, chunk).await?);
    }
    Ok(out)
}

/// Projects some iterations `(item, row id)` of one launch (the caller holds the item rows).
pub async fn project_iterations(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    rows: &[(Uuid, String)],
) -> anyhow::Result<Projected> {
    let mut items: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    items.sort();
    items.dedup();
    project(conn, launch, &items, Some(rows)).await
}

type IterationRow = (
    Uuid,
    String,
    serde_json::Value,
    Uuid,
    String,
    String,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

/// An item, or one iteration of it (`Some(row id)`).
type UnitKey = (Uuid, Option<String>);

type IterationStepRow = (Uuid, String, String, String, String, String, Vec<Uuid>);

async fn project(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    item_ids: &[Uuid],
    only: Option<&[(Uuid, String)]>,
) -> anyhow::Result<Projected> {
    let mut out = Projected::default();
    if item_ids.is_empty() {
        return Ok(out);
    }
    crate::explorer::app_writes_keys(&mut *conn).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select ri.id, ri.case_id, ri.case_version, ri.series_id, s.test_case_id, ri.status, ri.execution_no, \
           ri.executed_by, ri.comment, ri.started_at, ri.finished_at, ri.defect, c.number, ri.shared_pins, \
           ri.iterations, {FOLDER_PATH_SQL} as folder_path \
         from run_items ri join cases c on c.id = ri.case_id join series s on s.id = ri.series_id \
         where ri.launch_id = $1 and ri.id = any($2) order by ri.id"
    )))
    .bind(launch.id)
    .bind(item_ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let status: String = r.try_get("status")?;
        items.push(ItemRow {
            id: r.try_get("id")?,
            case_id: r.try_get("case_id")?,
            case_version: r.try_get("case_version")?,
            series_id: r.try_get("series_id")?,
            test_case_id: r.try_get("test_case_id")?,
            status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
            execution_no: r.try_get("execution_no")?,
            executed_by: r.try_get("executed_by")?,
            comment: r.try_get("comment")?,
            started_at: r.try_get("started_at")?,
            finished_at: r.try_get("finished_at")?,
            defect: r.try_get("defect")?,
            number: r.try_get("number")?,
            folder_path: r.try_get("folder_path")?,
            shared_pins: super::runs::pins_of(r.try_get("shared_pins")?),
            iterations: r.try_get("iterations")?,
        });
    }
    let by_id: HashMap<Uuid, &ItemRow> = items.iter().map(|i| (i.id, i)).collect();
    let (only_items, only_rows): (Option<Vec<Uuid>>, Option<Vec<String>>) = match only {
        Some(o) => {
            let (a, b) = o.iter().cloned().unzip();
            (Some(a), Some(b))
        }
        None => (None, None),
    };
    let mut units: Vec<Unit> = Vec::new();
    for i in items.iter().filter(|i| i.iterations == 0 && only.is_none()) {
        units.push(Unit {
            item: i,
            row: None,
            series_id: i.series_id,
            status: i.status,
            executed_by: i.executed_by.clone(),
            comment: i.comment.clone(),
            started_at: i.started_at,
            finished_at: i.finished_at,
        });
    }
    let parameterized: Vec<Uuid> = items
        .iter()
        .filter(|i| i.iterations > 0)
        .map(|i| i.id)
        .collect();
    if !parameterized.is_empty() {
        let wanted: Option<HashSet<(Uuid, &str)>> =
            only.map(|o| o.iter().map(|(i, r)| (*i, r.as_str())).collect());
        let its: Vec<IterationRow> = sqlx::query_as(
            "select it.run_item_id, it.row_id, it.param_values, it.series_id, it.status, it.comment, \
               it.executed_by, it.started_at, it.finished_at \
             from run_iterations it join run_items ri on ri.id = it.run_item_id and ri.execution_no = it.execution_no \
             where it.run_item_id = any($1) \
               and ($2::uuid[] is null or (it.run_item_id, it.row_id) in \
                 (select * from unnest($2::uuid[], $3::text[]))) \
             order by it.run_item_id, it.position, it.row_id",
        )
        .bind(&parameterized)
        .bind(only_items.as_deref())
        .bind(only_rows.as_deref())
        .fetch_all(&mut *conn)
        .await?;
        for (item, row_id, values, series_id, status, comment, by, started_at, finished_at) in its {
            if wanted
                .as_ref()
                .is_some_and(|w| !w.contains(&(item, row_id.as_str())))
            {
                continue;
            }
            let Some(i) = by_id.get(&item) else {
                continue;
            };
            units.push(Unit {
                item: i,
                row: Some((row_id, serde_json::from_value(values).unwrap_or_default())),
                series_id,
                status: RunStatus::parse(&status).unwrap_or(RunStatus::Untested),
                executed_by: by,
                comment,
                started_at,
                finished_at,
            });
        }
    }
    let (finals, opens): (Vec<&Unit>, Vec<&Unit>) = units.iter().partition(|u| u.status.is_final());

    let mut series_all: Vec<Uuid> = units.iter().map(|u| u.series_id).collect();
    series_all.sort();
    series_all.dedup();
    // Locked, so the counted columns read here are what this transaction changes them from
    // (a concurrent mute waits, and recounts after the commit).
    let existing_rows: Vec<(Uuid, Uuid, String, bool, bool)> = sqlx::query_as(
        "select series_id, id, status, flaky_in_run, muted from results \
         where launch_id = $1 and launch_created_at = $2 and series_id = any($3) order by id for update",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(&series_all)
    .fetch_all(&mut *conn)
    .await?;
    let mut existing: HashMap<Uuid, Uuid> = HashMap::new();
    let mut before: HashMap<Uuid, CountedResult> = HashMap::new();
    for (series_id, id, status, flaky_in_run, muted) in existing_rows {
        existing.insert(series_id, id);
        before.insert(
            id,
            CountedResult {
                status,
                flaky_in_run,
                muted,
            },
        );
    }
    let mut touched: Vec<Uuid> = before.keys().copied().collect();

    let mut updated_events = Vec::new();
    let mut stored_events = Vec::new();
    let mut removed_events = Vec::new();
    let mut analyzed: Vec<Uuid> = Vec::new();
    let mut result_of: Vec<(UnitKey, Option<Uuid>)> = Vec::new();

    if !finals.is_empty() {
        let mut steps: HashMap<UnitKey, HashMap<String, StepRes>> = HashMap::new();
        let mut att_ids: Vec<Uuid> = Vec::new();
        let plain: Vec<Uuid> = finals
            .iter()
            .filter(|u| u.row.is_none())
            .map(|u| u.item.id)
            .collect();
        if !plain.is_empty() {
            let step_rows: Vec<(Uuid, String, String, String, String, Vec<Uuid>)> = sqlx::query_as(
                "select s.run_item_id, s.step_path, s.status, s.actual, s.comment, s.attachment_ids \
                 from run_step_results s join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
                 where s.run_item_id = any($1)",
            )
            .bind(&plain)
            .fetch_all(&mut *conn)
            .await?;
            for (item, path, status, actual, comment, atts) in step_rows {
                att_ids.extend(atts.iter().copied());
                steps.entry((item, None)).or_default().insert(
                    path,
                    StepRes {
                        status: StepStatus::parse(&status),
                        actual,
                        comment,
                        attachments: atts,
                    },
                );
            }
        }
        let mut iter_items: Vec<Uuid> = finals
            .iter()
            .filter(|u| u.row.is_some())
            .map(|u| u.item.id)
            .collect();
        iter_items.sort();
        iter_items.dedup();
        if !iter_items.is_empty() {
            let (f_items, f_rows): (Vec<Uuid>, Vec<String>) = finals
                .iter()
                .filter_map(|u| u.row.as_ref().map(|r| (u.item.id, r.0.clone())))
                .unzip();
            let step_rows: Vec<IterationStepRow> = sqlx::query_as(
                    "select s.run_item_id, s.row_id, s.step_path, s.status, s.actual, s.comment, s.attachment_ids \
                     from run_iteration_step_results s \
                     join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
                     where s.run_item_id = any($1) \
                       and (s.run_item_id, s.row_id) in (select * from unnest($2::uuid[], $3::text[]))",
                )
                .bind(&iter_items)
                .bind(&f_items)
                .bind(&f_rows)
                .fetch_all(&mut *conn)
                .await?;
            let wanted: HashSet<UnitKey> = finals.iter().map(|u| u.step_key()).collect();
            for (item, row, path, status, actual, comment, atts) in step_rows {
                let key = (item, Some(row));
                if !wanted.contains(&key) {
                    continue;
                }
                att_ids.extend(atts.iter().copied());
                steps.entry(key).or_default().insert(
                    path,
                    StepRes {
                        status: StepStatus::parse(&status),
                        actual,
                        comment,
                        attachments: atts,
                    },
                );
            }
        }
        att_ids.sort();
        att_ids.dedup();
        let atts = attachment_refs(conn, launch.project_id, &att_ids).await?;
        let mut snapshots: HashMap<SnapshotKey, CaseSnapshot> = HashMap::new();
        for u in &finals {
            let i = u.item;
            if let std::collections::hash_map::Entry::Vacant(e) = snapshots.entry(snapshot_key(
                i.case_id,
                i.case_version,
                i.shared_pins.as_ref(),
            )) {
                e.insert(
                    executed_snapshot(
                        conn,
                        launch.project_id,
                        i.case_id,
                        i.case_version,
                        i.shared_pins.as_ref(),
                    )
                    .await?,
                );
            }
        }
        let mut a = (
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
        );
        let mut t = (
            Vec::<Option<DateTime<Utc>>>::new(),
            Vec::<Option<DateTime<Utc>>>::new(),
            Vec::<Option<i64>>::new(),
            Vec::<Option<String>>::new(),
            Vec::<serde_json::Value>::new(),
            Vec::<Option<Vec<u8>>>::new(),
        );
        let mut r = (
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<String>::new(),
            Vec::<String>::new(),
            Vec::<serde_json::Value>::new(),
            Vec::<String>::new(),
            Vec::<serde_json::Value>::new(),
            Vec::<Uuid>::new(),
            Vec::<Option<serde_json::Value>>::new(),
        );
        let mut suite_tree: (Vec<serde_json::Value>, Vec<bool>) = (Vec::new(), Vec::new());
        let empty = HashMap::new();
        for u in &finals {
            let i = u.item;
            let snap = &snapshots[&snapshot_key(i.case_id, i.case_version, i.shared_pins.as_ref())];
            let st = steps.get(&u.step_key()).unwrap_or(&empty);
            let status = result_status(u.status).unwrap_or(Status::Unknown);
            let result_id = existing
                .get(&u.series_id)
                .copied()
                .unwrap_or_else(Uuid::now_v7);
            let recorded: HashMap<String, StepStatus> = st
                .iter()
                .filter_map(|(p, r)| r.status.map(|s| (p.clone(), s)))
                .collect();
            let effective = super::runs::effective_statuses(&snap.body.steps, &recorded);
            let substituted;
            let case_steps = match &u.row {
                Some((_, values)) => {
                    let mut budget = SUBSTITUTION_BUDGET;
                    substituted =
                        super::parameters::substitute_steps(&snap.body.steps, values, &mut budget);
                    &substituted
                }
                None => &snap.body.steps,
            };
            let built = build_steps(case_steps, "", st, &effective, &atts);
            let duration = match (u.started_at, u.finished_at) {
                (Some(a), Some(b)) if b >= a => Some((b - a).num_milliseconds()),
                _ => None,
            };
            let message = message_of(&u.comment, st);
            let links: Vec<serde_json::Value> = snap
                .body
                .links
                .iter()
                .map(|l| serde_json::json!({"name": l.name, "url": l.url, "type": l.link_type.as_str()}))
                .collect();
            let mut detail = serde_json::json!({"stage": "finished", "interrupted": false,
                "format": "fastproof-json", "attempt": i.execution_no, "links": links,
                "caseVersion": i.case_version, "executedBy": u.executed_by});
            if let Some(d) = non_empty(&snap.body.description) {
                detail["description"] = serde_json::json!({"text": d});
            }
            if let Some((row_id, values)) = &u.row {
                let columns: Vec<String> = match snap.body.parameters.as_ref() {
                    Some(p) if !p.columns.is_empty() => p.columns.clone(),
                    _ => values.keys().cloned().collect(),
                };
                detail["parameters"] = super::parameters::display_parameters(&columns, values);
                detail["iteration"] = serde_json::json!(row_id);
            }
            a.0.push(Uuid::now_v7());
            a.1.push(result_id);
            a.2.push(u.series_id);
            a.3.push(u.key());
            a.4.push(status.as_str().to_string());
            a.5.push(u.status.as_str().to_string());
            t.0.push(u.started_at);
            t.1.push(u.finished_at);
            t.2.push(duration);
            t.3.push(message.as_deref().map(no_nul));
            t.4.push(json_no_nul(detail));
            t.5.push(if built.is_empty() {
                None
            } else {
                Some(codec::encode(&built)?)
            });
            r.0.push(result_id);
            r.1.push(i.test_case_id);
            r.2.push(u.series_id);
            r.3.push(no_nul(&snap.name));
            r.4.push(format!("TC-{}", i.number));
            r.5.push(json_no_nul(serde_json::to_value(&i.folder_path)?));
            r.6.push(status.as_str().to_string());
            let labels = manual_labels(i.number, u.executed_by.as_deref(), u.status.as_str());
            let folder: Vec<String> = i.folder_path.iter().map(|x| no_nul(x)).collect();
            let (tree, titled) =
                crate::results::suite_place_json(&folder, &labels, &format!("TC-{}", i.number));
            suite_tree.0.push(json_no_nul(serde_json::to_value(tree)?));
            suite_tree.1.push(titled);
            r.7.push(labels);
            r.8.push(i.id);
            r.9.push(if u.row.is_none() {
                i.defect.clone().map(json_no_nul)
            } else {
                None
            });
            result_of.push((u.step_key(), Some(result_id)));
            analyzed.push(u.series_id);
            let mut ev = serde_json::json!({"id": result_id, "launchId": launch.id,
                "seriesId": u.series_id, "testCaseId": i.test_case_id, "status": status.as_str(),
                "source": "manual", "runItemId": i.id});
            if let Some((row_id, _)) = &u.row {
                ev["rowId"] = serde_json::json!(row_id);
            }
            updated_events.push(ev);
        }
        // A new attempt gets `seq` 0 here and its real one from `apply_launch_changes`; an
        // attempt stored again keeps its `seq`.
        let stored: Vec<(Uuid, String, Uuid, i64)> = sqlx::query_as(
            "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, upload_id, \
               client_key, seq, status, raw_status, started_at, stopped_at, duration_ms, message, detail, steps_z) \
             select i, $1, $2, $3, r, s, null, k, 0, st, rs, sa, so, d, m, de, sz \
             from unnest($4::uuid[], $5::uuid[], $6::uuid[], $7::text[], $8::text[], $9::text[], \
               $10::timestamptz[], $11::timestamptz[], $12::bigint[], $13::text[], $14::jsonb[], $15::bytea[]) \
               as t(i, r, s, k, st, rs, sa, so, d, m, de, sz) \
             on conflict (launch_id, client_key, launch_created_at) do update set result_id = excluded.result_id, \
               series_id = excluded.series_id, status = excluded.status, raw_status = excluded.raw_status, \
               started_at = excluded.started_at, stopped_at = excluded.stopped_at, \
               duration_ms = excluded.duration_ms, message = excluded.message, detail = excluded.detail, \
               steps_z = excluded.steps_z \
             returning id, status, result_id, seq",
        )
        .bind(launch.created_at)
        .bind(launch.project_id)
        .bind(launch.id)
        .bind(&a.0)
        .bind(&a.1)
        .bind(&a.2)
        .bind(&a.3)
        .bind(&a.4)
        .bind(&a.5)
        .bind(&t.0)
        .bind(&t.1)
        .bind(&t.2)
        .bind(&t.3)
        .bind(&t.4)
        .bind(&t.5)
        .fetch_all(&mut *conn)
        .await?;
        let order: HashMap<Uuid, usize> = a.0.iter().enumerate().map(|(k, id)| (*id, k)).collect();
        let mut fresh: Vec<(usize, Uuid)> = Vec::new();
        for (id, status, result_id, seq) in stored {
            if seq == 0 {
                fresh.push((order.get(&id).copied().unwrap_or(usize::MAX), id));
            }
            stored_events.push(serde_json::json!({"id": id, "resultId": result_id,
                "launchId": launch.id, "status": status}));
        }
        fresh.sort();
        out.new_attempts = fresh.into_iter().map(|(_, id)| id).collect();
        touched.extend(r.0.iter().copied());
        let counts: HashMap<Uuid, i32> = sqlx::query_as::<_, (Uuid, i32)>(
            "select result_id, count(*)::int from attempts where launch_id = $1 and launch_created_at = $2 \
             and result_id = any($3) group by result_id",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(&r.0)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect();
        let attempt_counts: Vec<i32> =
            r.0.iter()
                .map(|id| counts.get(id).copied().unwrap_or(1))
                .collect();
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, suite_path, status, flaky_in_run, attempt_count, started_at, stopped_at, duration_ms, \
               message, labels, run_item_id, defect, label_values, suite_tree, suite_titled) \
             select i, $1, $2, $3, tc, se, n, f, array(select jsonb_array_elements_text(sp)), st, false, ac, sa, so, \
               d, m, lb, ri, df, fastproof_label_values(lb), array(select jsonb_array_elements_text(tr)), tt \
             from unnest($4::uuid[], $5::uuid[], $6::uuid[], $7::text[], $8::text[], $9::jsonb[], $10::text[], \
               $11::int[], $12::timestamptz[], $13::timestamptz[], $14::bigint[], $15::text[], $16::jsonb[], \
               $17::uuid[], $18::jsonb[], $19::jsonb[], $20::bool[]) \
               as t(i, tc, se, n, f, sp, st, ac, sa, so, d, m, lb, ri, df, tr, tt) \
             on conflict (launch_id, series_id, launch_created_at) do update set name = excluded.name, \
               full_name = excluded.full_name, suite_path = excluded.suite_path, status = excluded.status, \
               flaky_in_run = false, attempt_count = excluded.attempt_count, started_at = excluded.started_at, \
               stopped_at = excluded.stopped_at, duration_ms = excluded.duration_ms, message = excluded.message, \
               labels = excluded.labels, label_values = excluded.label_values, suite_tree = excluded.suite_tree, \
               suite_titled = excluded.suite_titled, run_item_id = excluded.run_item_id, \
               defect = coalesce(excluded.defect, results.defect), updated_at = now()",
        )
        .bind(launch.created_at)
        .bind(launch.project_id)
        .bind(launch.id)
        .bind(&r.0)
        .bind(&r.1)
        .bind(&r.2)
        .bind(&r.3)
        .bind(&r.4)
        .bind(&r.5)
        .bind(&r.6)
        .bind(&attempt_counts)
        .bind(&t.0)
        .bind(&t.1)
        .bind(&t.2)
        .bind(&t.3)
        .bind(&r.7)
        .bind(&r.8)
        .bind(&r.9)
        .bind(&suite_tree.0)
        .bind(&suite_tree.1)
        .execute(&mut *conn)
        .await?;
    }

    if !opens.is_empty() {
        let keys: Vec<String> = opens.iter().map(|u| u.key()).collect();
        sqlx::query(
            "delete from attempts where launch_id = $1 and launch_created_at = $2 and client_key = any($3)",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(&keys)
        .execute(&mut *conn)
        .await?;
        for u in &opens {
            let i = u.item;
            let Some(result_id) = existing.get(&u.series_id).copied() else {
                result_of.push((u.step_key(), None));
                continue;
            };
            let latest = sqlx::query(
                "select status, raw_status, started_at, stopped_at, duration_ms, message, detail->>'executedBy' as by, \
                   (select count(*)::int from attempts x where x.result_id = a.result_id \
                     and x.launch_created_at = a.launch_created_at) as n \
                 from attempts a where a.result_id = $1 and a.launch_created_at = $2 order by a.seq desc limit 1",
            )
            .bind(result_id)
            .bind(launch.created_at)
            .fetch_optional(&mut *conn)
            .await?;
            match latest {
                None => {
                    sqlx::query("delete from results where id = $1 and launch_created_at = $2")
                        .bind(result_id)
                        .bind(launch.created_at)
                        .execute(&mut *conn)
                        .await?;
                    removed_events.push(serde_json::json!({"id": result_id, "launchId": launch.id,
                        "seriesId": u.series_id, "removed": true}));
                    result_of.push((u.step_key(), None));
                }
                Some(row) => {
                    let status: String = row.try_get("status")?;
                    let raw: Option<String> = row.try_get("raw_status")?;
                    let by: Option<String> = row.try_get("by")?;
                    let n: i32 = row.try_get("n")?;
                    sqlx::query(
                        "update results set status = $3, started_at = $4, stopped_at = $5, duration_ms = $6, \
                           message = $7, attempt_count = $8, labels = $9, \
                           label_values = fastproof_label_values($9), updated_at = now() \
                         where id = $1 and launch_created_at = $2",
                    )
                    .bind(result_id)
                    .bind(launch.created_at)
                    .bind(&status)
                    .bind(row.try_get::<Option<DateTime<Utc>>, _>("started_at")?)
                    .bind(row.try_get::<Option<DateTime<Utc>>, _>("stopped_at")?)
                    .bind(row.try_get::<Option<i64>, _>("duration_ms")?)
                    .bind(row.try_get::<Option<String>, _>("message")?)
                    .bind(n)
                    .bind(manual_labels(
                        i.number,
                        by.as_deref(),
                        raw.as_deref().unwrap_or(&status),
                    ))
                    .execute(&mut *conn)
                    .await?;
                    analyzed.push(u.series_id);
                    let mut ev = serde_json::json!({"id": result_id, "launchId": launch.id,
                        "seriesId": u.series_id, "testCaseId": i.test_case_id, "status": status,
                        "source": "manual", "runItemId": i.id});
                    if let Some((row_id, _)) = &u.row {
                        ev["rowId"] = serde_json::json!(row_id);
                    }
                    updated_events.push(ev);
                    result_of.push((u.step_key(), Some(result_id)));
                }
            }
        }
    }

    let mut ri = Vec::new();
    let mut rr = Vec::new();
    let mut ii = Vec::new();
    let mut irow = Vec::new();
    let mut ir = Vec::new();
    let mut execs = Vec::new();
    for ((item, row), res) in result_of {
        match row {
            None => {
                ri.push(item);
                rr.push(res);
            }
            Some(row) => {
                execs.push(by_id.get(&item).map(|i| i.execution_no).unwrap_or(1));
                ii.push(item);
                irow.push(row);
                ir.push(res);
            }
        }
    }
    if !ri.is_empty() {
        sqlx::query(
            "update run_items ri set result_id = t.r from unnest($1::uuid[], $2::uuid[]) as t(i, r) \
             where ri.id = t.i and ri.result_id is distinct from t.r",
        )
        .bind(&ri)
        .bind(&rr)
        .execute(&mut *conn)
        .await?;
    }
    if !ii.is_empty() {
        sqlx::query(
            "update run_iterations it set result_id = t.r \
             from unnest($1::uuid[], $2::int[], $3::text[], $4::uuid[]) as t(i, e, w, r) \
             where it.run_item_id = t.i and it.execution_no = t.e and it.row_id = t.w \
               and it.result_id is distinct from t.r",
        )
        .bind(&ii)
        .bind(&execs)
        .bind(&irow)
        .bind(&ir)
        .execute(&mut *conn)
        .await?;
    }
    let mut case_ids: Vec<Uuid> = items.iter().map(|i| i.case_id).collect();
    case_ids.sort();
    case_ids.dedup();
    super::last_status::apply_manual(conn, &case_ids).await?;
    let mut seen = HashSet::new();
    analyzed.retain(|s| seen.insert(*s));
    if !analyzed.is_empty() {
        out.rule_hits =
            crate::analysis::after_persist_deferred(&mut *conn, launch, &[], &analyzed).await?;
    }
    touched.sort();
    touched.dedup();
    let after = counted_results(conn, launch, &touched).await?;
    for id in &touched {
        out.delta
            .add(&CountDelta::result_change(before.get(id), after.get(id)));
    }
    crate::events::emit_many(
        &mut *conn,
        "result.updated",
        Some(&launch.project_key),
        updated_events,
    )
    .await?;
    crate::events::emit_many(
        &mut *conn,
        "attempt.stored",
        Some(&launch.project_key),
        stored_events,
    )
    .await?;
    crate::events::emit_many(
        &mut *conn,
        "result.removed",
        Some(&launch.project_key),
        removed_events,
    )
    .await?;
    crate::events::notify_launch(&mut *conn, &launch.project_key, launch.id).await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(status: &str, flaky_in_run: bool, muted: bool) -> CountedResult {
        CountedResult {
            status: status.into(),
            flaky_in_run,
            muted,
        }
    }

    #[test]
    fn count_deltas() {
        let d = CountDelta::result_change(None, Some(&res("passed", false, true)));
        assert_eq!(
            d,
            CountDelta {
                total: 1,
                passed: 1,
                muted: 1,
                ..Default::default()
            }
        );
        let d = CountDelta::result_change(
            Some(&res("failed", true, false)),
            Some(&res("broken", false, true)),
        );
        assert_eq!(
            d,
            CountDelta {
                failed: -1,
                broken: 1,
                flaky: -1,
                muted: 1,
                ..Default::default()
            }
        );
        let d = CountDelta::result_change(Some(&res("unknown", false, false)), None);
        assert_eq!(
            d,
            CountDelta {
                total: -1,
                unknown: -1,
                ..Default::default()
            }
        );
        let d = CountDelta::result_change(Some(&res("weird", false, false)), None);
        assert_eq!(
            d,
            CountDelta {
                total: -1,
                ..Default::default()
            }
        );
        assert!(CountDelta::result_change(None, None).is_zero());
        assert!(
            CountDelta::result_change(
                Some(&res("skipped", false, true)),
                Some(&res("skipped", false, true))
            )
            .is_zero()
        );
        for a in RunStatus::ALL {
            for b in RunStatus::ALL {
                let d = CountDelta::item_change(*a, *b);
                let open = |s: RunStatus| (s == RunStatus::Untested, s == RunStatus::InProgress);
                assert_eq!(
                    (d.untested, d.in_progress),
                    (
                        open(*b).0 as i32 - open(*a).0 as i32,
                        open(*b).1 as i32 - open(*a).1 as i32
                    ),
                    "{a} -> {b}"
                );
                assert_eq!(d.total, 0);
            }
        }
        let mut p = Projected::default();
        p.merge(Projected {
            delta: CountDelta::item_change(RunStatus::Untested, RunStatus::Passed),
            new_attempts: vec![Uuid::nil()],
            rule_hits: HashMap::from([(Uuid::nil(), 2)]),
        });
        p.merge(Projected {
            delta: CountDelta::item_change(RunStatus::InProgress, RunStatus::Passed),
            new_attempts: vec![Uuid::max()],
            rule_hits: HashMap::from([(Uuid::nil(), 1)]),
        });
        assert_eq!((p.delta.untested, p.delta.in_progress), (-1, -1));
        assert_eq!(p.new_attempts, vec![Uuid::nil(), Uuid::max()]);
        assert_eq!(p.rule_hits[&Uuid::nil()], 3);
    }

    #[test]
    fn statuses_and_message() {
        assert_eq!(result_status(RunStatus::Blocked), Some(Status::Broken));
        assert_eq!(result_status(RunStatus::InProgress), None);
        let mut steps = HashMap::new();
        steps.insert(
            "2".to_string(),
            StepRes {
                status: Some(StepStatus::Failed),
                actual: "500 error".into(),
                ..Default::default()
            },
        );
        steps.insert(
            "10".to_string(),
            StepRes {
                status: Some(StepStatus::Blocked),
                ..Default::default()
            },
        );
        assert_eq!(message_of("", &steps).as_deref(), Some("Step 2: 500 error"));
        assert_eq!(message_of("see log", &steps).as_deref(), Some("see log"));
        let labels = manual_labels(7, Some("ann"), "blocked");
        assert_eq!(labels[1]["value"], "TC-7");
        assert_eq!(labels[3]["value"], "blocked");
    }
}
