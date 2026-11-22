//! Projection of run items into `results` and `attempts`, in the transaction that changed the
//! items (the caller holds the launch row lock).
//!
//! A final status (`passed`, `failed`, `blocked`, `skipped`) upserts one result on the series
//! of the case's manual identity (`run_item_id`; labels `manual=true`, `case=TC-<n>`,
//! `executedBy`, `manualStatus=<original>`; `blocked` is stored as `broken`) and one attempt
//! per execution (`client_key = 'manual:<item>:<execution_no>'`, `steps_z` in the shape the
//! UI's step tree reads). Going back to `untested`/`in_progress` removes the attempt of the
//! current execution; the result goes with its last attempt, or shows the previous execution.
//! Then the analysis hook runs on the affected series (manual series are never flaky) and the
//! launch counters are recounted.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use fastproof_model::{
    AttachmentId, AttachmentKind, AttachmentRef as ModelAttachment, Status, StatusDetails, Step,
    StepKind,
};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::runs::{FOLDER_PATH_SQL, attachment_refs, executed_snapshot};
use super::{AttachmentRef, CaseSnapshot, CaseStep, RunStatus, StepStatus};
use crate::ingest::{LaunchRef, recount};
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
}

/// Projects one run item (the caller holds the lock of its launch row).
pub async fn project_run_item(conn: &mut PgConnection, item_id: Uuid) -> anyhow::Result<()> {
    let row: Option<(Uuid, Uuid, DateTime<Utc>, String)> = sqlx::query_as(
        "select l.id, l.project_id, l.created_at, p.key from run_items ri join launches l on l.id = ri.launch_id \
         join projects p on p.id = l.project_id where ri.id = $1",
    )
    .bind(item_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((id, project_id, created_at, project_key)) = row else {
        return Ok(());
    };
    let launch = LaunchRef {
        id,
        project_id,
        project_key,
        created_at,
    };
    project_run_items(conn, &launch, &[item_id]).await
}

/// Projects run items of one launch (the caller holds the lock of the launch row), then
/// recounts the launch.
pub async fn project_run_items(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    item_ids: &[Uuid],
) -> anyhow::Result<()> {
    if item_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select ri.id, ri.case_id, ri.case_version, ri.series_id, s.test_case_id, ri.status, ri.execution_no, \
           ri.executed_by, ri.comment, ri.started_at, ri.finished_at, ri.defect, c.number, \
           {FOLDER_PATH_SQL} as folder_path \
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
        });
    }
    let (finals, opens): (Vec<&ItemRow>, Vec<&ItemRow>) =
        items.iter().partition(|i| i.status.is_final());

    let mut series_all: Vec<Uuid> = items.iter().map(|i| i.series_id).collect();
    series_all.sort();
    series_all.dedup();
    let existing: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select series_id, id from results where launch_id = $1 and launch_created_at = $2 and series_id = any($3)",
    )
    .bind(launch.id)
    .bind(launch.created_at)
    .bind(&series_all)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();

    let mut updated_events = Vec::new();
    let mut stored_events = Vec::new();
    let mut removed_events = Vec::new();
    let mut analyzed: Vec<Uuid> = Vec::new();
    let mut result_of: Vec<(Uuid, Option<Uuid>)> = Vec::new();

    if !finals.is_empty() {
        let ids: Vec<Uuid> = finals.iter().map(|i| i.id).collect();
        let step_rows: Vec<(Uuid, String, String, String, String, Vec<Uuid>)> = sqlx::query_as(
            "select s.run_item_id, s.step_path, s.status, s.actual, s.comment, s.attachment_ids \
             from run_step_results s join run_items ri on ri.id = s.run_item_id and ri.execution_no = s.execution_no \
             where s.run_item_id = any($1)",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut steps: HashMap<Uuid, HashMap<String, StepRes>> = HashMap::new();
        let mut att_ids: Vec<Uuid> = Vec::new();
        for (item, path, status, actual, comment, atts) in step_rows {
            att_ids.extend(atts.iter().copied());
            steps.entry(item).or_default().insert(
                path,
                StepRes {
                    status: StepStatus::parse(&status),
                    actual,
                    comment,
                    attachments: atts,
                },
            );
        }
        att_ids.sort();
        att_ids.dedup();
        let atts = attachment_refs(conn, launch.project_id, &att_ids).await?;
        let mut snapshots: HashMap<(Uuid, i32), CaseSnapshot> = HashMap::new();
        for i in &finals {
            if let std::collections::hash_map::Entry::Vacant(e) =
                snapshots.entry((i.case_id, i.case_version))
            {
                e.insert(
                    executed_snapshot(conn, launch.project_id, i.case_id, i.case_version).await?,
                );
            }
        }
        let n = finals.len() as i64;
        let seq_end: i64 = sqlx::query_scalar(
            "update launches set attempt_seq = attempt_seq + $2 where id = $1 returning attempt_seq",
        )
        .bind(launch.id)
        .bind(n)
        .fetch_one(&mut *conn)
        .await?;
        let seq0 = seq_end - n;

        let mut a = (
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<Uuid>::new(),
            Vec::<String>::new(),
            Vec::<i64>::new(),
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
        for (k, i) in finals.iter().enumerate() {
            let snap = &snapshots[&(i.case_id, i.case_version)];
            let empty = HashMap::new();
            let st = steps.get(&i.id).unwrap_or(&empty);
            let status = result_status(i.status).unwrap_or(Status::Unknown);
            let result_id = existing
                .get(&i.series_id)
                .copied()
                .unwrap_or_else(Uuid::now_v7);
            let recorded: HashMap<String, StepStatus> = st
                .iter()
                .filter_map(|(p, r)| r.status.map(|s| (p.clone(), s)))
                .collect();
            let effective = super::runs::effective_statuses(&snap.body.steps, &recorded);
            let built = build_steps(&snap.body.steps, "", st, &effective, &atts);
            let duration = match (i.started_at, i.finished_at) {
                (Some(a), Some(b)) if b >= a => Some((b - a).num_milliseconds()),
                _ => None,
            };
            let message = message_of(&i.comment, st);
            let links: Vec<serde_json::Value> = snap
                .body
                .links
                .iter()
                .map(|l| serde_json::json!({"name": l.name, "url": l.url, "type": l.link_type.as_str()}))
                .collect();
            let mut detail = serde_json::json!({"stage": "finished", "interrupted": false,
                "format": "fastproof-json", "attempt": i.execution_no, "links": links,
                "caseVersion": i.case_version, "executedBy": i.executed_by});
            if let Some(d) = non_empty(&snap.body.description) {
                detail["description"] = serde_json::json!({"text": d});
            }
            a.0.push(Uuid::now_v7());
            a.1.push(result_id);
            a.2.push(i.series_id);
            a.3.push(client_key(i.id, i.execution_no));
            a.4.push(seq0 + k as i64 + 1);
            a.5.push(status.as_str().to_string());
            a.6.push(i.status.as_str().to_string());
            t.0.push(i.started_at);
            t.1.push(i.finished_at);
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
            r.2.push(i.series_id);
            r.3.push(no_nul(&snap.name));
            r.4.push(format!("TC-{}", i.number));
            r.5.push(json_no_nul(serde_json::to_value(&i.folder_path)?));
            r.6.push(status.as_str().to_string());
            r.7.push(manual_labels(
                i.number,
                i.executed_by.as_deref(),
                i.status.as_str(),
            ));
            r.8.push(i.id);
            r.9.push(i.defect.clone().map(json_no_nul));
            result_of.push((i.id, Some(result_id)));
            analyzed.push(i.series_id);
            updated_events.push(serde_json::json!({"id": result_id, "launchId": launch.id,
                "seriesId": i.series_id, "testCaseId": i.test_case_id, "status": status.as_str(),
                "source": "manual", "runItemId": i.id}));
        }
        let stored: Vec<(Uuid, String, Uuid)> = sqlx::query_as(
            "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, upload_id, \
               client_key, seq, status, raw_status, started_at, stopped_at, duration_ms, message, detail, steps_z) \
             select i, $1, $2, $3, r, s, null, k, q, st, rs, sa, so, d, m, de, sz \
             from unnest($4::uuid[], $5::uuid[], $6::uuid[], $7::text[], $8::bigint[], $9::text[], $10::text[], \
               $11::timestamptz[], $12::timestamptz[], $13::bigint[], $14::text[], $15::jsonb[], $16::bytea[]) \
               as t(i, r, s, k, q, st, rs, sa, so, d, m, de, sz) \
             on conflict (launch_id, client_key, launch_created_at) do update set result_id = excluded.result_id, \
               series_id = excluded.series_id, status = excluded.status, raw_status = excluded.raw_status, \
               started_at = excluded.started_at, stopped_at = excluded.stopped_at, \
               duration_ms = excluded.duration_ms, message = excluded.message, detail = excluded.detail, \
               steps_z = excluded.steps_z \
             returning id, status, result_id",
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
        .bind(&a.6)
        .bind(&t.0)
        .bind(&t.1)
        .bind(&t.2)
        .bind(&t.3)
        .bind(&t.4)
        .bind(&t.5)
        .fetch_all(&mut *conn)
        .await?;
        for (id, status, result_id) in stored {
            stored_events.push(serde_json::json!({"id": id, "resultId": result_id,
                "launchId": launch.id, "status": status}));
        }
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
               message, labels, run_item_id, defect) \
             select i, $1, $2, $3, tc, se, n, f, array(select jsonb_array_elements_text(sp)), st, false, ac, sa, so, \
               d, m, lb, ri, df \
             from unnest($4::uuid[], $5::uuid[], $6::uuid[], $7::text[], $8::text[], $9::jsonb[], $10::text[], \
               $11::int[], $12::timestamptz[], $13::timestamptz[], $14::bigint[], $15::text[], $16::jsonb[], \
               $17::uuid[], $18::jsonb[]) as t(i, tc, se, n, f, sp, st, ac, sa, so, d, m, lb, ri, df) \
             on conflict (launch_id, series_id, launch_created_at) do update set name = excluded.name, \
               full_name = excluded.full_name, suite_path = excluded.suite_path, status = excluded.status, \
               flaky_in_run = false, attempt_count = excluded.attempt_count, started_at = excluded.started_at, \
               stopped_at = excluded.stopped_at, duration_ms = excluded.duration_ms, message = excluded.message, \
               labels = excluded.labels, run_item_id = excluded.run_item_id, \
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
        .execute(&mut *conn)
        .await?;
    }

    if !opens.is_empty() {
        let keys: Vec<String> = opens
            .iter()
            .map(|i| client_key(i.id, i.execution_no))
            .collect();
        sqlx::query(
            "delete from attempts where launch_id = $1 and launch_created_at = $2 and client_key = any($3)",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(&keys)
        .execute(&mut *conn)
        .await?;
        for i in &opens {
            let Some(result_id) = existing.get(&i.series_id).copied() else {
                result_of.push((i.id, None));
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
                        "seriesId": i.series_id, "removed": true}));
                    result_of.push((i.id, None));
                }
                Some(row) => {
                    let status: String = row.try_get("status")?;
                    let raw: Option<String> = row.try_get("raw_status")?;
                    let by: Option<String> = row.try_get("by")?;
                    let n: i32 = row.try_get("n")?;
                    sqlx::query(
                        "update results set status = $3, started_at = $4, stopped_at = $5, duration_ms = $6, \
                           message = $7, attempt_count = $8, labels = $9, updated_at = now() \
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
                    analyzed.push(i.series_id);
                    updated_events.push(serde_json::json!({"id": result_id, "launchId": launch.id,
                        "seriesId": i.series_id, "testCaseId": i.test_case_id, "status": status,
                        "source": "manual", "runItemId": i.id}));
                    result_of.push((i.id, Some(result_id)));
                }
            }
        }
    }

    let (ri, rr): (Vec<Uuid>, Vec<Option<Uuid>>) = result_of.into_iter().unzip();
    sqlx::query(
        "update run_items ri set result_id = t.r from unnest($1::uuid[], $2::uuid[]) as t(i, r) \
         where ri.id = t.i and ri.result_id is distinct from t.r",
    )
    .bind(&ri)
    .bind(&rr)
    .execute(&mut *conn)
    .await?;
    let mut seen = HashSet::new();
    analyzed.retain(|s| seen.insert(*s));
    if !analyzed.is_empty() {
        crate::analysis::after_persist(&mut *conn, launch, &[], &analyzed).await?;
    }
    recount(&mut *conn, launch.id, launch.created_at, None).await?;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
