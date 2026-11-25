//! Configurable case workflow (§7.2): `cases.status` stays the category, `cases.state` is the
//! key of a configured status. People move a case through the transitions of its flow (manual
//! or automated) and their guards; code sync, imports and creates are system actions that set
//! the initial state or the first state of the category without a check. Removing a state that
//! cases use goes through the `workflow_remap` job.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::cases::{CaseCtx, Commit, case_event, commit, lock_case_by_number};
use super::fields::field_defs;
use super::lookup::{CASE_COLS, case_row};
use super::versions::snapshot_of;
use super::{
    AllowedTransition, CaseReviewSettings, CaseRow, CaseStatus, CaseWorkflowFlow, CaseWorkflowPut,
    CaseWorkflowSaved, CaseWorkflowSettings, CaseWorkflowStatus, CaseWorkflowView, ImportReport,
    ReviewState, TmError, TmJobKind, TmJobProgress, TmJobStatus, TmResult, VersionSource,
    WorkflowGuard, WorkflowPermission,
};
use crate::analysis::settings::{MAX_SETTINGS_BYTES, ProjectSettings, check_case_workflow};
use crate::events::{LiveEvent, notify};
use crate::{Store, no_nul};

/// Entries of a remap (old state key to new key), at most.
pub const MAX_REMAP: usize = 50;
/// Cases the remap job moves per transaction.
pub const REMAP_BATCH: i64 = 500;
/// Used states listed in a `workflow_status_in_use` answer, at most.
const MAX_IN_USE: i64 = 20;

/// What a transition misses: no rule, the permission, or a guard.
pub const MISSING_TRANSITION: &str = "transition";
pub const MISSING_PERMISSION: &str = "permission";

fn valid_key(k: &str) -> bool {
    let b = k.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// The flow of a case: automated cases follow `automated`, the others `manual`.
pub fn flow_of(w: &CaseWorkflowSettings, automated: bool) -> &CaseWorkflowFlow {
    if automated { &w.automated } else { &w.manual }
}

/// The status a category stands for: the one whose key is the category, else the first of
/// that category.
pub fn category_status(w: &CaseWorkflowSettings, c: CaseStatus) -> Option<&CaseWorkflowStatus> {
    w.statuses
        .iter()
        .find(|s| s.category == c && s.key == c.as_str())
        .or_else(|| w.statuses.iter().find(|s| s.category == c))
}

/// The state a case is in: its `state` when the workflow knows it with the case's category,
/// else the status its category stands for (cases written before the workflow existed).
pub fn current_key(w: &CaseWorkflowSettings, cur: &CaseRow) -> Option<String> {
    cur.state
        .as_deref()
        .and_then(|k| w.status(k))
        .filter(|s| s.category == cur.status)
        .or_else(|| category_status(w, cur.status))
        .map(|s| s.key.clone())
}

/// What the guards of a case look at, read once per check.
#[derive(Clone, Debug, Default)]
pub struct GuardFacts {
    /// The head version is approved (or needs no review).
    pub approved: bool,
    /// Every active required field has a value.
    pub required_ok: bool,
    /// Who made the head version.
    pub author: Option<String>,
}

/// Whether the head of a case counts as approved: review does not apply to it, it was never
/// edited under review, or its head is the approved version.
pub fn head_approved(policy: &CaseReviewSettings, cur: &CaseRow) -> bool {
    !policy.in_scope(cur.automated)
        || cur.review_state.is_none()
        || (cur.review_state == Some(ReviewState::Approved)
            && cur.approved_version == Some(cur.version))
}

/// The author of the head version of a case (`case_versions.created_by`).
pub async fn case_head_author(
    conn: &mut PgConnection,
    case_id: Uuid,
    version: i32,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("select created_by from case_versions where case_id = $1 and version = $2")
        .bind(case_id)
        .bind(version)
        .fetch_optional(&mut *conn)
        .await
}

/// Which guards the rules leaving `from` (towards `to`, or anywhere) use: `(requiredFields,
/// notAuthor)`, so only those facts are read.
pub fn guard_needs(flow: &CaseWorkflowFlow, from: &str, to: Option<&str>) -> (bool, bool) {
    let mut out = (false, false);
    for t in flow.transitions.iter().filter(|t| {
        t.from.iter().any(|f| f == "*" || f == from)
            && to.is_none_or(|to| t.to == "*" || t.to == to)
    }) {
        for g in t.requires.iter().flatten() {
            match g {
                WorkflowGuard::RequiredFields => out.0 = true,
                WorkflowGuard::NotAuthor => out.1 = true,
                WorkflowGuard::Approved => {}
            }
        }
    }
    out
}

/// Reads the [`GuardFacts`] of a case: the required fields and the head's author only when
/// asked (`required`, `author`); facts not read count as met.
pub async fn guard_facts(
    conn: &mut PgConnection,
    project_id: Uuid,
    cur: &CaseRow,
    policy: &CaseReviewSettings,
    required: bool,
    author: bool,
) -> TmResult<GuardFacts> {
    let required_ok = if required {
        let defs = field_defs(conn, project_id, false).await?;
        defs.iter()
            .filter(|d| d.required && !d.archived)
            .all(|d| cur.fields.get(&d.key).is_some_and(|v| !v.is_null()))
    } else {
        true
    };
    Ok(GuardFacts {
        approved: head_approved(policy, cur),
        required_ok,
        author: if author {
            case_head_author(conn, cur.id, cur.version).await?
        } else {
            None
        },
    })
}

/// What one transition rule misses for `by` (empty: it applies).
fn rule_missing(
    rule: &super::CaseWorkflowTransition,
    can_review: bool,
    by: &str,
    facts: &GuardFacts,
) -> Vec<String> {
    let mut out = Vec::new();
    if rule.permission == Some(WorkflowPermission::CasesReview) && !can_review {
        out.push(MISSING_PERMISSION.to_string());
    }
    let mut guards = rule.requires.clone().unwrap_or_default();
    guards.sort_by_key(|g| match g {
        WorkflowGuard::Approved => 0,
        WorkflowGuard::RequiredFields => 1,
        WorkflowGuard::NotAuthor => 2,
    });
    guards.dedup();
    for g in guards {
        let fails = match g {
            WorkflowGuard::Approved => !facts.approved,
            WorkflowGuard::RequiredFields => !facts.required_ok,
            WorkflowGuard::NotAuthor => facts
                .author
                .as_deref()
                .is_some_and(|a| super::review::same_principal(a, by)),
        };
        if fails {
            out.push(g.as_str().to_string());
        }
    }
    out
}

/// What the move `from` → `to` misses in a flow: `["transition"]` without a matching rule,
/// else the misses of the matching rule that misses least (empty: allowed).
pub fn missing(
    flow: &CaseWorkflowFlow,
    from: &str,
    to: &str,
    can_review: bool,
    by: &str,
    facts: &GuardFacts,
) -> Vec<String> {
    flow.transitions
        .iter()
        .filter(|t| t.from.iter().any(|f| f == "*" || f == from) && (t.to == "*" || t.to == to))
        .map(|t| rule_missing(t, can_review, by, facts))
        .min_by_key(Vec::len)
        .unwrap_or_else(|| vec![MISSING_TRANSITION.to_string()])
}

/// 409 `invalid_transition {from, to, missing}`.
pub fn invalid_transition(from: Option<&str>, to: &str, missing: Vec<String>) -> TmError {
    TmError::Conflict {
        code: "invalid_transition",
        message: format!(
            "the case cannot move from {} to {to}: {}",
            from.unwrap_or("its state"),
            missing.join(", ")
        ),
        details: Some(json!({"from": from, "to": to, "missing": missing})),
    }
}

async fn settings_of(conn: &mut PgConnection, project_id: Uuid) -> TmResult<ProjectSettings> {
    let v: Option<Value> = sqlx::query_scalar("select settings from projects where id = $1")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(ProjectSettings::from_value(v.unwrap_or(Value::Null)))
}

/// The state a case moves to when a person changes its status to `to` (a category): the
/// status whose key is the category, else the first status of that category the case may move
/// to; 409 `invalid_transition` when none is allowed. `Ok(None)` without a configured workflow
/// or when the category stays.
pub async fn check_transition(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &CaseRow,
    to: CaseStatus,
) -> TmResult<Option<String>> {
    check_transition_for(conn, ctx, cur, to, false).await
}

/// [`check_transition`] for a write that also changes content: when `needs_review` (the same
/// write makes a head that waits for review), the `approved` guard fails, as the version that
/// takes the new state is not approved.
pub async fn check_transition_for(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &CaseRow,
    to: CaseStatus,
    needs_review: bool,
) -> TmResult<Option<String>> {
    let s = settings_of(conn, ctx.project_id).await?;
    let Some(w) = s.case_workflow.as_ref() else {
        return Ok(None);
    };
    if to == cur.status {
        return Ok(None);
    }
    let candidates: Vec<&CaseWorkflowStatus> = match w
        .statuses
        .iter()
        .find(|s| s.category == to && s.key == to.as_str())
    {
        Some(st) => vec![st],
        None => w.statuses.iter().filter(|s| s.category == to).collect(),
    };
    let from = current_key(w, cur);
    let flow = flow_of(w, cur.automated);
    let (req, auth) = guard_needs(flow, from.as_deref().unwrap_or(""), None);
    let mut facts = guard_facts(conn, ctx.project_id, cur, &s.case_review, req, auth).await?;
    facts.approved &= !needs_review;
    let mut first: Option<(String, Vec<String>)> = None;
    for c in candidates {
        let m = missing(
            flow,
            from.as_deref().unwrap_or(""),
            &c.key,
            ctx.can_review,
            ctx.by,
            &facts,
        );
        if m.is_empty() {
            return Ok(Some(c.key.clone()));
        }
        first.get_or_insert((c.key.clone(), m));
    }
    let (to_key, m) = first.unwrap_or_else(|| {
        (
            to.as_str().to_string(),
            vec![MISSING_TRANSITION.to_string()],
        )
    });
    Err(invalid_transition(from.as_deref(), &to_key, m))
}

/// Checks that a person may move a case to the state `key` (bulk `setState` and `POST
/// /cases/{n}/transition`) and returns that state's category: 400 without a workflow or for
/// an unknown key, 409 `invalid_transition` when a rule, the permission or a guard is missing.
/// A state whose category is not `draft` also needs every required field.
pub async fn check_transition_to(
    conn: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    cur: &CaseRow,
    key: &str,
) -> TmResult<CaseStatus> {
    let s = settings_of(conn, ctx.project_id).await?;
    let w = s
        .case_workflow
        .as_ref()
        .ok_or_else(|| TmError::bad_request("the project has no case workflow"))?;
    let target = w.status(key).filter(|_| valid_key(key)).ok_or_else(|| {
        TmError::bad_request(if valid_key(key) {
            format!("unknown state {key:?}")
        } else {
            "unknown state".to_string()
        })
    })?;
    let from = current_key(w, cur);
    if from.as_deref() == Some(key) {
        return Ok(target.category);
    }
    let flow = flow_of(w, cur.automated);
    let (req, auth) = guard_needs(flow, from.as_deref().unwrap_or(""), Some(key));
    let facts = guard_facts(
        conn,
        ctx.project_id,
        cur,
        &s.case_review,
        req || target.category != CaseStatus::Draft,
        auth,
    )
    .await?;
    let mut m = missing(
        flow,
        from.as_deref().unwrap_or(""),
        key,
        ctx.can_review,
        ctx.by,
        &facts,
    );
    if target.category != CaseStatus::Draft
        && !facts.required_ok
        && !m
            .iter()
            .any(|x| x == WorkflowGuard::RequiredFields.as_str())
    {
        m.push(WorkflowGuard::RequiredFields.as_str().to_string());
    }
    if !m.is_empty() {
        return Err(invalid_transition(from.as_deref(), key, m));
    }
    Ok(target.category)
}

/// The transitions a person could take from the case's state: every state a rule of its flow
/// leads to, with what it still misses.
pub async fn allowed_transitions(
    conn: &mut PgConnection,
    project_id: Uuid,
    cur: &CaseRow,
    by: &str,
    can_review: bool,
) -> TmResult<Vec<AllowedTransition>> {
    let s = settings_of(conn, project_id).await?;
    let Some(w) = s.case_workflow.as_ref() else {
        return Ok(Vec::new());
    };
    let from = current_key(w, cur).unwrap_or_default();
    let flow = flow_of(w, cur.automated);
    let (_, auth) = guard_needs(flow, &from, None);
    let facts = guard_facts(conn, project_id, cur, &s.case_review, true, auth).await?;
    let mut out = Vec::new();
    for st in &w.statuses {
        if st.key == from {
            continue;
        }
        let mut m = missing(flow, &from, &st.key, can_review, by, &facts);
        if m.iter().any(|x| x == MISSING_TRANSITION) {
            continue;
        }
        if st.category != CaseStatus::Draft
            && !facts.required_ok
            && !m
                .iter()
                .any(|x| x == WorkflowGuard::RequiredFields.as_str())
        {
            m.push(WorkflowGuard::RequiredFields.as_str().to_string());
        }
        out.push(AllowedTransition {
            to: st.key.clone(),
            name: st.name.clone(),
            allowed: m.is_empty(),
            missing: m,
        });
    }
    Ok(out)
}

/// The state of a case that code sync, an import or a create gives it (no transition check):
/// the flow's `initial` when it has the category, else the status the category stands for.
pub async fn system_state(
    conn: &mut PgConnection,
    project_id: Uuid,
    automated: bool,
    status: CaseStatus,
) -> TmResult<Option<String>> {
    let s = settings_of(conn, project_id).await?;
    let Some(w) = s.case_workflow.as_ref() else {
        return Ok(None);
    };
    let initial = &flow_of(w, automated).initial;
    if let Some(st) = w.status(initial).filter(|st| st.category == status) {
        return Ok(Some(st.key.clone()));
    }
    Ok(category_status(w, status).map(|st| st.key.clone()))
}

/// States cases use that a new workflow does not keep with the same category (`None`: every
/// used state), with their case counts, most used first.
pub async fn states_in_use(
    conn: &mut PgConnection,
    project_id: Uuid,
    new: Option<&CaseWorkflowSettings>,
) -> TmResult<Vec<(String, i64)>> {
    let (keys, cats): (Vec<String>, Vec<String>) = new
        .map(|w| {
            w.statuses
                .iter()
                .map(|s| (s.key.clone(), s.category.as_str().to_string()))
                .unzip()
        })
        .unwrap_or_default();
    Ok(sqlx::query_as(
        "select c.state, count(*) from cases c where c.project_id = $1 and c.state is not null \
         and not exists (select 1 from unnest($2::text[], $3::text[]) as k(key, cat) \
           where k.key = c.state and k.cat = c.status) \
         group by c.state order by 2 desc, 1 limit $4",
    )
    .bind(project_id)
    .bind(&keys)
    .bind(&cats)
    .bind(MAX_IN_USE)
    .fetch_all(&mut *conn)
    .await?)
}

fn in_use_error(used: &[(String, i64)]) -> TmError {
    TmError::Conflict {
        code: "workflow_status_in_use",
        message: format!(
            "cases use states the new workflow removes or moves to another category: {}",
            used.iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        details: Some(json!({"statuses": used.iter()
            .map(|(k, n)| json!({"key": k, "cases": n})).collect::<Vec<_>>()})),
    }
}

/// Refuses a settings change that removes states cases still use, or moves them to another
/// category (409 `workflow_status_in_use {statuses: [{key, cases}]}`); `PUT /case-workflow`
/// with `remap` does such a change through a job.
pub async fn check_settings_change(
    conn: &mut PgConnection,
    project_id: Uuid,
    _old: Option<&CaseWorkflowSettings>,
    new: Option<&CaseWorkflowSettings>,
) -> TmResult<()> {
    let used = states_in_use(conn, project_id, new).await?;
    if used.is_empty() {
        Ok(())
    } else {
        Err(in_use_error(&used))
    }
}

/// Parameters of a `workflow_remap` job.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemapParams {
    /// Old state key to new key; a used state missing here (or every state without a
    /// workflow) is cleared.
    #[serde(default)]
    remap: BTreeMap<String, String>,
    #[serde(default)]
    done: i64,
}

impl Store {
    /// `GET /case-workflow`: the workflow (the suggested one when none is configured) and the
    /// review policy.
    pub async fn case_workflow(&self, project_id: Uuid) -> TmResult<CaseWorkflowView> {
        let mut conn = self.pool.acquire().await?;
        let s = settings_of(&mut conn, project_id).await?;
        Ok(CaseWorkflowView {
            configured: s.case_workflow.is_some(),
            workflow: s.case_workflow.unwrap_or_default(),
            review: s.case_review,
        })
    }

    /// `PUT /case-workflow`: replaces `caseWorkflow` (`null` removes it). States cases use that
    /// the new workflow drops, or keeps with another category, need `remap` (old key to a key of
    /// the new workflow; else 409 `workflow_status_in_use`); a `workflow_remap` job then moves
    /// those cases (and clears every state when the workflow is removed). Returns the workflow
    /// before and the answer.
    pub async fn put_case_workflow(
        &self,
        ctx: &CaseCtx<'_>,
        put: &CaseWorkflowPut,
    ) -> TmResult<(Option<CaseWorkflowSettings>, CaseWorkflowSaved)> {
        if let Some(w) = &put.workflow {
            check_case_workflow(w).map_err(TmError::bad_request)?;
        }
        let remap = put.remap.clone().unwrap_or_default();
        if remap.len() > MAX_REMAP {
            return Err(TmError::bad_request(format!(
                "remap: at most {MAX_REMAP} entries"
            )));
        }
        for (from, to) in &remap {
            if !valid_key(from) {
                return Err(TmError::bad_request(format!(
                    "remap: {from:?} is not a state key"
                )));
            }
            match &put.workflow {
                Some(w) if w.status(to).is_some() => {}
                Some(_) => {
                    return Err(TmError::bad_request(format!(
                        "remap.{from}: {to:?} names no status of the new workflow"
                    )));
                }
                None => {
                    return Err(TmError::bad_request(
                        "remap needs a workflow (without one every state is cleared)",
                    ));
                }
            }
        }
        let mut tx = self.pool.begin().await?;
        let doc: Value =
            sqlx::query_scalar("select settings from projects where id = $1 for update")
                .bind(ctx.project_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(TmError::NotFound("project"))?;
        let old = ProjectSettings::from_value(doc.clone()).case_workflow;
        let used = states_in_use(&mut tx, ctx.project_id, put.workflow.as_ref()).await?;
        if put.workflow.is_some() {
            let unmapped: Vec<(String, i64)> = used
                .iter()
                .filter(|(k, _)| !remap.contains_key(k))
                .cloned()
                .collect();
            if !unmapped.is_empty() {
                return Err(in_use_error(&unmapped));
            }
        }
        let mut new_doc = match doc {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        match &put.workflow {
            Some(w) => {
                new_doc.insert(
                    "caseWorkflow".into(),
                    serde_json::to_value(w).map_err(anyhow::Error::from)?,
                );
            }
            None => {
                new_doc.remove("caseWorkflow");
            }
        }
        let new_doc = Value::Object(new_doc);
        if serde_json::to_vec(&new_doc).map_or(0, |b| b.len()) > MAX_SETTINGS_BYTES {
            return Err(TmError::bad_request(format!(
                "the settings document would exceed {MAX_SETTINGS_BYTES} bytes"
            )));
        }
        sqlx::query("update projects set settings = $2, updated_at = now() where id = $1")
            .bind(ctx.project_id)
            .bind(crate::json_no_nul(new_doc))
            .execute(&mut *tx)
            .await?;
        let job_id = if used.is_empty() {
            None
        } else {
            let params = RemapParams { remap, done: 0 };
            Some(
                insert_remap_job(
                    &mut tx,
                    ctx,
                    serde_json::to_value(&params).map_err(anyhow::Error::from)?,
                )
                .await?,
            )
        };
        tx.commit().await?;
        let job = match job_id {
            Some(id) => self.tm_job(ctx.project_id, id).await?,
            None => None,
        };
        let s = settings_of(&mut *self.pool.acquire().await?, ctx.project_id).await?;
        Ok((
            old,
            CaseWorkflowSaved {
                view: CaseWorkflowView {
                    configured: s.case_workflow.is_some(),
                    workflow: s.case_workflow.unwrap_or_default(),
                    review: s.case_review,
                },
                job,
            },
        ))
    }

    /// Moves a case to the state `key` (`POST /cases/{n}/transition`) when its revision is
    /// `expected`: both `state` and `status` (the state's category), `revision + 1`,
    /// `case.updated`, and a version only when the category changes. Returns the case before
    /// and after; moving to the current state changes nothing.
    pub async fn transition_case(
        &self,
        ctx: &CaseCtx<'_>,
        n: i64,
        expected: i64,
        to: &str,
    ) -> TmResult<(CaseRow, CaseRow)> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_case_by_number(&mut tx, ctx.project_id, n, Some(expected)).await?;
        if cur.archived_at.is_some() {
            return Err(TmError::conflict("archived", "the case is archived"));
        }
        let category = check_transition_to(&mut tx, ctx, &cur, to).await?;
        if cur.state.as_deref() == Some(to) && cur.status == category {
            return Ok((cur.clone(), cur));
        }
        let activates = category == CaseStatus::Active && cur.ai_unreviewed;
        if activates {
            super::ai::check_ai_review(&cur, ctx.by)?;
        }
        let row = if category != cur.status {
            super::cases::write_state(&mut tx, cur.id, Some(to)).await?;
            let mut snap = snapshot_of(&cur);
            snap.status = category;
            let mut row = commit(
                &mut tx,
                ctx,
                &cur,
                snap,
                Commit {
                    source: VersionSource::Ui,
                    launch_id: None,
                    force_revision: false,
                },
            )
            .await?;
            if activates && row.ai_unreviewed {
                sqlx::query("update cases set ai_unreviewed = false where id = $1")
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
                row.ai_unreviewed = false;
            }
            row
        } else {
            let r = sqlx::query(sqlx::AssertSqlSafe(format!(
                "update cases set state = $3, revision = revision + 1, updated_by = $4, updated_at = now() \
                 where project_id = $1 and id = $2 returning {CASE_COLS}"
            )))
            .bind(ctx.project_id)
            .bind(cur.id)
            .bind(no_nul(to))
            .bind(no_nul(ctx.by))
            .fetch_one(&mut *tx)
            .await?;
            let row = case_row(&r)?;
            case_event(
                &mut tx,
                ctx.project_key,
                "case.updated",
                json!({"number": row.number, "revision": row.revision, "source": "ui"}),
            )
            .await?;
            row
        };
        tx.commit().await?;
        Ok((cur, row))
    }

    /// The `workflow_remap` job: moves cases of removed states to their new keys (and status to
    /// the new key's category, with a version `source = bulk`), clears states the remap does
    /// not name, in batches of [`REMAP_BATCH`] with the progress recorded after each, and ends
    /// `done` with the number of cases moved.
    pub async fn run_workflow_remap_job(&self, job_id: Uuid, owner: &str) -> TmResult<()> {
        let row: Option<(Uuid, String, Value, Option<String>)> = sqlx::query_as(
            "select j.project_id, p.key, j.params, j.created_by from tm_jobs j \
             join projects p on p.id = j.project_id \
             where j.id = $1 and j.kind = 'workflow_remap' and j.lease_owner = $2",
        )
        .bind(job_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        let Some((project_id, key, params, by)) = row else {
            return Ok(());
        };
        let by = by.unwrap_or_else(|| "system".into());
        let params: RemapParams = serde_json::from_value(params).unwrap_or_default();
        let mut done = params.done;
        loop {
            let mut tx = self.pool.begin().await?;
            let s = settings_of(&mut tx, project_id).await?;
            let w = s.case_workflow.clone();
            let (keys, cats): (Vec<String>, Vec<String>) = w
                .as_ref()
                .map(|w| {
                    w.statuses
                        .iter()
                        .map(|s| (s.key.clone(), s.category.as_str().to_string()))
                        .unzip()
                })
                .unwrap_or_default();
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "select {CASE_COLS} from cases c where c.project_id = $1 and c.state is not null \
                 and not exists (select 1 from unnest($2::text[], $3::text[]) as k(key, cat) \
                   where k.key = c.state and k.cat = c.status) \
                 order by c.id limit $4 for update"
            )))
            .bind(project_id)
            .bind(&keys)
            .bind(&cats)
            .bind(REMAP_BATCH)
            .fetch_all(&mut *tx)
            .await?;
            if rows.is_empty() {
                tx.rollback().await?;
                break;
            }
            let n = rows.len() as i64;
            let mut state_ids = Vec::new();
            let mut state_keys: Vec<Option<String>> = Vec::new();
            let mut moved: Vec<(CaseRow, String, CaseStatus)> = Vec::new();
            for r in &rows {
                let c = case_row(r)?;
                let target = w.as_ref().and_then(|w| {
                    let k = c.state.as_deref().and_then(|s| params.remap.get(s))?;
                    w.status(k).map(|st| (st.key.clone(), st.category))
                });
                match target {
                    Some((k, cat)) if cat != c.status => moved.push((c, k, cat)),
                    Some((k, _)) => {
                        state_ids.push(c.id);
                        state_keys.push(Some(k));
                    }
                    None => {
                        state_ids.push(c.id);
                        state_keys.push(None);
                    }
                }
            }
            if !state_ids.is_empty() {
                sqlx::query(
                    "update cases c set state = t.st, revision = c.revision + 1, updated_by = $2, \
                     updated_at = now() from unnest($3::uuid[], $4::text[]) as t(id, st) \
                     where c.project_id = $1 and c.id = t.id",
                )
                .bind(project_id)
                .bind(no_nul(&by))
                .bind(&state_ids)
                .bind(&state_keys)
                .execute(&mut *tx)
                .await?;
            }
            let ctx = CaseCtx {
                project_id,
                project_key: &key,
                by: &by,
                limits: &super::TmLimits::default(),
                can_review: false,
            };
            for (c, k, cat) in moved {
                super::cases::write_state(&mut tx, c.id, Some(&k)).await?;
                let mut snap = snapshot_of(&c);
                snap.status = cat;
                commit(
                    &mut tx,
                    &ctx,
                    &c,
                    snap,
                    Commit {
                        source: VersionSource::Bulk,
                        launch_id: None,
                        force_revision: false,
                    },
                )
                .await?;
            }
            done += n;
            notify(
                &mut tx,
                &LiveEvent {
                    project: key.clone(),
                    kind: "case.updated".into(),
                    data: json!({"source": "bulk", "count": n}),
                },
            )
            .await?;
            let kept = sqlx::query(
                "update tm_jobs set params = params || $3, progress = $4, \
                 lease_until = now() + make_interval(secs => $5) \
                 where id = $1 and lease_owner = $2 and status = 'running'",
            )
            .bind(job_id)
            .bind(owner)
            .bind(json!({"done": done}))
            .bind(
                serde_json::to_value(TmJobProgress { done, total: None })
                    .map_err(anyhow::Error::from)?,
            )
            .bind(super::import::LEASE_SECONDS as f64)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if kept == 0 {
                tx.rollback().await?;
                return Ok(());
            }
            tx.commit().await?;
            if n < REMAP_BATCH {
                break;
            }
        }
        let finished = self
            .finish_tm_job(
                job_id,
                owner,
                &key,
                TmJobStatus::Done,
                TmJobProgress {
                    done,
                    total: Some(done),
                },
                Some(&ImportReport {
                    update: done,
                    ..Default::default()
                }),
                None,
            )
            .await?;
        if !finished {
            return Err(TmError::Other(anyhow::anyhow!(
                "the workflow_remap job was taken over"
            )));
        }
        Ok(())
    }
}

async fn insert_remap_job(
    tx: &mut PgConnection,
    ctx: &CaseCtx<'_>,
    params: Value,
) -> TmResult<Uuid> {
    let busy: bool = sqlx::query_scalar(
        "select exists (select 1 from tm_jobs where project_id = $1 and kind = 'workflow_remap' \
         and status in ('queued', 'running'))",
    )
    .bind(ctx.project_id)
    .fetch_one(&mut *tx)
    .await?;
    if busy {
        return Err(TmError::conflict(
            "job_busy",
            "a workflow remap of this project is still running",
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into tm_jobs (id, project_id, kind, params, status, progress, created_by, owner_key) \
         values ($1, $2, $3, $4, 'queued', '{\"done\": 0}', $5, $5)",
    )
    .bind(id)
    .bind(ctx.project_id)
    .bind(TmJobKind::WorkflowRemap.as_str())
    .bind(crate::json_no_nul(params))
    .bind(no_nul(ctx.by))
    .execute(&mut *tx)
    .await?;
    case_event(
        tx,
        ctx.project_key,
        "tmJob.updated",
        json!({"id": id, "kind": TmJobKind::WorkflowRemap.as_str(), "status": "queued"}),
    )
    .await?;
    Ok(id)
}
