//! Test-management tools of the MCP endpoint (`41-tm-interfaces.md` MCP, `40` §5). Rules for
//! AI writes are enforced here, not left to the agent: cases an agent creates are always
//! `draft`, `origin = ai`, `aiUnreviewed`; an agent never changes a case directly (only
//! `propose_case_update`, which stores a pending suggestion a person accepts or rejects);
//! every write is audited with `source: ai` and the agent's name. Reviewing, activating and
//! accepting are refused to the principal that created the draft or suggestion, on REST too
//! (`Store` checks). `trace_actions` returns action names from fixed lists only, never
//! titles, the values of `fill`, `type` or `press` actions, nor any parameter values; it
//! takes a heavy slot and reads trace lines with a bounded buffer.

use std::collections::BTreeMap;
use std::io::Read;

use fastproof_store::tm::cases::{CaseCtx, CaseQuery, HistoryQuery};
use fastproof_store::tm::plans::{PlanItemFilter, PlanListFilter};
use fastproof_store::tm::runs::RunItemFilter;
use fastproof_store::tm::{
    CaseBody, CaseFilter, CaseLink, CaseLinkType, CasePatch, CaseProposal, CaseStatus, CaseStep,
    CaseWrite, ResultSource, RunStatus, StepStatus, TestPlanItemsChange, TmError,
};
use franshub_auth::Auth;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::mcp::{ToolError, ToolResult, ToolSpec, args, clip, limit, schema};
use crate::state::AppState;

/// Actions `trace_actions` returns, at most.
pub const MAX_TRACE_ACTIONS: usize = 500;
/// Bytes of a trace archive `trace_actions` reads.
pub const MAX_TRACE_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes of a rationale or an agent name.
const MAX_RATIONALE: usize = 4000;
const MAX_AGENT: usize = 100;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchCasesArgs {
    project: String,
    /// Words of the name, description or steps, or a number (`TC-12`).
    q: Option<String>,
    folder_id: Option<Uuid>,
    /// `draft`, `active`, `outdated`.
    statuses: Option<Vec<String>>,
    /// `manual` or `automated`.
    automation: Option<String>,
    tags: Option<Vec<String>>,
    /// Field key → accepted values.
    fields: Option<BTreeMap<String, Vec<String>>>,
    ai_unreviewed: Option<bool>,
    /// 1 to 100 (default 20).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseArgs {
    project: String,
    /// Case number (`12` for TC-12).
    number: i64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseHistoryArgs {
    project: String,
    number: i64,
    branch: Option<String>,
    environment: Option<String>,
    /// `manual` or `automated`.
    source: Option<String>,
    /// 1 to 100 (default 30).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListPlansArgs {
    project: String,
    q: Option<String>,
    /// 1 to 100 (default 20).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanArgs {
    project: String,
    plan_id: Uuid,
    /// Items listed, 1 to 500 (default 100).
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunProgressArgs {
    project: String,
    launch_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceArgs {
    project: String,
    result_id: Uuid,
    /// Attempt number, 1-based (default: the last attempt).
    attempt: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftStep {
    action: String,
    #[serde(default)]
    expected: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    steps: Vec<DraftStep>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftFromResultArgs {
    project: String,
    result_id: Uuid,
    attempt: Option<i32>,
    /// Replaces the name taken from the result.
    name: Option<String>,
    /// Markdown added to the description the server builds.
    notes: Option<String>,
    /// Replaces the steps taken from the result's step tree.
    steps: Option<Vec<DraftStep>>,
    folder_id: Option<Uuid>,
    tags: Option<Vec<String>>,
    /// Name of the agent (recorded in the audit log).
    agent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDraftArgs {
    project: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    preconditions: String,
    #[serde(default)]
    steps: Vec<DraftStep>,
    folder_id: Option<Uuid>,
    tags: Option<Vec<String>>,
    /// Field key → value (select and text fields: a string).
    fields: Option<BTreeMap<String, Value>>,
    agent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposeArgs {
    project: String,
    number: i64,
    /// The revision the proposal is based on (`revision` of `get_case`).
    base_revision: i64,
    name: Option<String>,
    description: Option<String>,
    preconditions: Option<String>,
    /// Replaces all steps.
    steps: Option<Vec<DraftStep>>,
    /// `draft`, `active` or `outdated`.
    status: Option<String>,
    tags: Option<Vec<String>>,
    fields: Option<BTreeMap<String, Value>>,
    /// Why the change is proposed.
    rationale: String,
    agent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddToPlanArgs {
    project: String,
    plan_id: Uuid,
    /// Case numbers (at most 1000).
    numbers: Vec<i64>,
    agent: Option<String>,
}

/// The test-management tools.
pub(crate) const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "search_cases",
        description: "Test cases matching filters (text, folder, status, automation, tags, field values), at most 100.",
        schema: schema::<SearchCasesArgs>,
        write: false,
    },
    ToolSpec {
        name: "get_case",
        description: "A test case with its steps (Markdown), fields, tags, keys, code identities and latest results.",
        schema: schema::<CaseArgs>,
        write: false,
    },
    ToolSpec {
        name: "case_history",
        description: "Results of a test case over launches, manual and automated, newest first.",
        schema: schema::<CaseHistoryArgs>,
        write: false,
    },
    ToolSpec {
        name: "list_test_plans",
        description: "Managed test plans of a project.",
        schema: schema::<ListPlansArgs>,
        write: false,
    },
    ToolSpec {
        name: "get_test_plan",
        description: "A test plan with its cases.",
        schema: schema::<PlanArgs>,
        write: false,
    },
    ToolSpec {
        name: "manual_run_progress",
        description: "Progress of the manual run of a launch and its failed or blocked items with the failed steps and their actual results.",
        schema: schema::<RunProgressArgs>,
        write: false,
    },
    ToolSpec {
        name: "trace_actions",
        description: "Actions of the Playwright trace of an attempt (at most 500): apiName, selector, url, status, error, durationMs. Typed values are never returned.",
        schema: schema::<TraceArgs>,
        write: false,
    },
    ToolSpec {
        name: "draft_case_from_result",
        description: "Create a draft manual test case from a result: the server takes the name, preconditions from fixtures, steps from the step tree and the error; the agent may add notes or replace steps. The case is a draft marked as AI-made for review. Needs cases:write or ai:propose (the agent role).",
        schema: schema::<DraftFromResultArgs>,
        write: true,
    },
    ToolSpec {
        name: "create_case_draft",
        description: "Create a draft test case from the agent's text (e.g. from a trace or a requirement), marked as AI-made for review. Needs cases:write or ai:propose (the agent role).",
        schema: schema::<CreateDraftArgs>,
        write: true,
    },
    ToolSpec {
        name: "propose_case_update",
        description: "Propose changes to an existing case against its revision; stored as a pending suggestion that a person accepts or rejects. Nothing is applied. Needs cases:write or ai:propose (the agent role).",
        schema: schema::<ProposeArgs>,
        write: true,
    },
    ToolSpec {
        name: "add_cases_to_plan",
        description: "Add cases to a managed test plan. Needs plans:write.",
        schema: schema::<AddToPlanArgs>,
        write: true,
    },
];

fn status_of(v: &str) -> Result<CaseStatus, ToolError> {
    CaseStatus::parse(v).ok_or_else(|| ToolError("status must be draft, active or outdated".into()))
}

fn source_of(v: &Option<String>) -> Result<Option<ResultSource>, ToolError> {
    v.as_deref()
        .map(|x| {
            ResultSource::parse(x).ok_or_else(|| ToolError("must be manual or automated".into()))
        })
        .transpose()
}

fn tm(e: TmError) -> ToolError {
    ToolError::from(crate::api::tm_error(e))
}

fn agent_of(a: &Option<String>) -> Result<String, ToolError> {
    let a = a.as_deref().unwrap_or("mcp").trim();
    if a.is_empty() || a.len() > MAX_AGENT || a.chars().any(char::is_control) {
        return Err(ToolError(format!("agent must have 1 to {MAX_AGENT} bytes")));
    }
    Ok(a.to_string())
}

fn steps_of(s: &[DraftStep], depth: usize) -> Vec<CaseStep> {
    if depth > 16 {
        return Vec::new();
    }
    s.iter()
        .take(10_000)
        .map(|d| CaseStep {
            id: String::new(),
            action: d.action.clone(),
            expected: d.expected.clone(),
            data: d.data.clone(),
            attachments: Vec::new(),
            steps: steps_of(&d.steps, depth + 1),
        })
        .collect()
}

fn field_values(
    f: &Option<BTreeMap<String, Value>>,
) -> Result<Option<BTreeMap<String, fastproof_store::tm::FieldValue>>, ToolError> {
    let Some(f) = f else { return Ok(None) };
    if f.len() > 200 {
        return Err(ToolError("at most 200 fields".into()));
    }
    let mut out = BTreeMap::new();
    for (k, v) in f {
        let v: fastproof_store::tm::FieldValue = serde_json::from_value(v.clone())
            .map_err(|_| ToolError(format!("fields.{k}: a string, list of strings or number")))?;
        out.insert(k.clone(), v);
    }
    Ok(Some(out))
}

fn ctx<'a>(s: &'a AppState, p: &'a fastproof_store::Project, auth: &'a Auth) -> CaseCtx<'a> {
    CaseCtx {
        project_id: p.id,
        project_key: &p.key,
        by: &auth.principal.name,
        limits: &s.cfg.tm,
    }
}

async fn create_ai_case(
    s: &AppState,
    p: &fastproof_store::Project,
    auth: &Auth,
    w: CaseWrite,
) -> Result<fastproof_store::tm::CaseRow, ToolError> {
    s.store
        .create_ai_case(&ctx(s, p, auth), &w)
        .await
        .map_err(tm)
}

/// Runs a test-management tool; `None` when `name` is not one.
pub(crate) async fn run(s: &AppState, name: &str, a: &Value, auth: &Auth) -> Option<ToolResult> {
    if !TOOLS.iter().any(|t| t.name == name) {
        return None;
    }
    Some(run_tool(s, name, a, auth).await)
}

async fn project(
    s: &AppState,
    auth: &Auth,
    key: &str,
    perm: &franshub_access::Permission,
) -> Result<fastproof_store::Project, ToolError> {
    Ok(s.project_for(auth, key, perm).await?)
}

/// The project when the caller holds one of `perms` there; the denial names the first one.
pub(crate) async fn project_any(
    s: &AppState,
    auth: &Auth,
    key: &str,
    perms: &[&franshub_access::Permission],
) -> Result<fastproof_store::Project, ToolError> {
    let perm = perms
        .iter()
        .find(|p| auth.allowed(p, Some(key)))
        .or(perms.first())
        .ok_or_else(|| ToolError("forbidden".into()))?;
    project(s, auth, key, perm).await
}

/// Drafts and suggestions: `cases:write` or the agents' `ai:propose`.
async fn draft_project(
    s: &AppState,
    auth: &Auth,
    key: &str,
) -> Result<fastproof_store::Project, ToolError> {
    project_any(s, auth, key, &[&s.perms.cases_write, &s.perms.ai_propose]).await
}

async fn run_tool(s: &AppState, name: &str, a: &Value, auth: &Auth) -> ToolResult {
    match name {
        "search_cases" => {
            let x: SearchCasesArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.cases_read).await?;
            let q = CaseQuery {
                filter: CaseFilter {
                    folder_ids: x.folder_id.map(|f| vec![f]),
                    recursive: x.folder_id.map(|_| true),
                    statuses: x
                        .statuses
                        .as_ref()
                        .map(|v| {
                            v.iter()
                                .map(|x| status_of(x))
                                .collect::<Result<Vec<_>, _>>()
                        })
                        .transpose()?,
                    automation: source_of(&x.automation)?,
                    fields: x.fields,
                    tags: x.tags,
                    q: x.q,
                },
                ai_unreviewed: x.ai_unreviewed,
                limit: limit(x.limit, 20, 100)?,
                ..Default::default()
            };
            let l = s.store.list_cases(p.id, &q).await.map_err(tm)?;
            Ok(json!({"items": l.items, "more": l.next.is_some()}))
        }
        "get_case" => {
            let x: CaseArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.cases_read).await?;
            let c = s.store.case_view(p.id, x.number).await.map_err(tm)?;
            let (history, _) = s
                .store
                .case_history(
                    p.id,
                    x.number,
                    &HistoryQuery {
                        limit: 10,
                        ..Default::default()
                    },
                )
                .await
                .map_err(tm)?;
            let mut v = serde_json::to_value(&c).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.remove("html");
                o.insert("key".into(), json!(format!("TC-{}", c.number)));
                o.insert(
                    "latestResults".into(),
                    serde_json::to_value(history).unwrap_or_default(),
                );
            }
            Ok(v)
        }
        "case_history" => {
            let x: CaseHistoryArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.cases_read).await?;
            let q = HistoryQuery {
                branch: x.branch,
                environment: x.environment,
                source: source_of(&x.source)?,
                before: None,
                limit: limit(x.limit, 30, 100)?,
            };
            let (items, next) = s.store.case_history(p.id, x.number, &q).await.map_err(tm)?;
            Ok(json!({"items": items, "more": next.is_some()}))
        }
        "list_test_plans" => {
            let x: ListPlansArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.cases_read).await?;
            let f = PlanListFilter {
                q: x.q,
                archived: false,
                before: None,
                limit: limit(x.limit, 20, 100)?,
            };
            let page = s.store.managed_plans(p.id, &f).await.map_err(tm)?;
            Ok(json!({"items": page.items, "more": page.next.is_some()}))
        }
        "get_test_plan" => {
            let x: PlanArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.cases_read).await?;
            let plan = s
                .store
                .test_plan(p.id, x.plan_id)
                .await?
                .ok_or_else(|| ToolError("not_found: plan not found".into()))?;
            let items = if plan.kind == fastproof_store::tm::TestPlanKind::Managed {
                let f = PlanItemFilter {
                    limit: limit(x.limit, 100, 500)?,
                    ..Default::default()
                };
                let page = s.store.plan_items(p.id, x.plan_id, &f).await.map_err(tm)?;
                json!({"items": page.items, "more": page.next.is_some()})
            } else {
                json!({"items": [], "more": false})
            };
            Ok(json!({"plan": plan, "cases": items}))
        }
        "manual_run_progress" => {
            let x: RunProgressArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.launches_read).await?;
            let l = s.launch_of(&p, x.launch_id).await?;
            let Some(manual) = l.manual else {
                return Err(ToolError(
                    "no_manual_run: the launch has no manual run".into(),
                ));
            };
            let f = RunItemFilter {
                statuses: vec![RunStatus::Failed, RunStatus::Blocked],
                limit: 50,
                ..Default::default()
            };
            let page = s.store.run_items(p.id, l.id, &f).await.map_err(tm)?;
            let mut failed = Vec::new();
            for it in page.items.iter() {
                let item = s.store.run_item(p.id, it.id).await.map_err(tm)?;
                let steps: Vec<Value> = item
                    .step_results
                    .iter()
                    .filter(|r| matches!(r.status, StepStatus::Failed | StepStatus::Blocked))
                    .take(20)
                    .map(|r| {
                        json!({"path": r.path, "status": r.status, "actual": clip(&r.actual, 2000),
                        "comment": clip(&r.comment, 2000)})
                    })
                    .collect();
                failed.push(json!({"caseNumber": it.case_number, "name": it.name, "status": it.status,
                    "assignee": it.assignee, "comment": clip(&item.comment, 2000), "failedSteps": steps}));
            }
            Ok(
                json!({"launchId": l.id, "name": l.name, "manual": manual, "failedItems": failed,
                "moreFailed": page.next.is_some()}),
            )
        }
        "trace_actions" => {
            let x: TraceArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.launches_read).await?;
            let _permit = s.guards.heavy(auth).await?;
            trace_actions(s, &p, x.result_id, x.attempt).await
        }
        "draft_case_from_result" => {
            let x: DraftFromResultArgs = args(a)?;
            let p = draft_project(s, auth, &x.project).await?;
            franshub_auth::require(auth, &s.perms.launches_read, Some(&p.key))
                .map_err(ToolError::from)?;
            let agent = agent_of(&x.agent)?;
            let d = s
                .store
                .result_detail(p.id, x.result_id, s.history.as_ref())
                .await?
                .ok_or_else(|| ToolError("not_found: result not found".into()))?;
            let at = match x.attempt {
                None => d.attempts.last(),
                Some(n) => d.attempts.iter().find(|t| t.no == n),
            };
            let mut pre = Vec::new();
            let mut steps = Vec::new();
            if let Some(at) = at {
                for st in &at.steps {
                    if st.kind == "fixture" {
                        pre.push(format!("- {}", st.name));
                    } else {
                        steps.push(step_of(st, 1));
                    }
                }
            }
            let mut desc = vec![format!(
                "Drafted from result [{}](/p/{}/results/{}) of `{}`.",
                d.summary.name.replace(['[', ']'], ""),
                p.key,
                d.summary.id,
                d.summary.full_name.replace('`', "'")
            )];
            if let Some(e) = at
                .and_then(|t| t.error.as_ref())
                .and_then(|e| e.message.as_ref())
            {
                desc.push(format!(
                    "Failure:\n\n```\n{}\n```",
                    clip(e, 4000).replace("```", "'''")
                ));
            }
            if let Some(n) = &x.notes {
                desc.push(n.clone());
            }
            let body = CaseBody {
                description: desc.join("\n\n"),
                preconditions: pre.join("\n"),
                steps: match &x.steps {
                    Some(st) => steps_of(st, 1),
                    None => steps,
                },
                links: vec![CaseLink {
                    name: "Source result".into(),
                    url: format!("/p/{}/results/{}", p.key, d.summary.id),
                    link_type: CaseLinkType::Link,
                }],
            };
            let w = CaseWrite {
                name: x.name.clone().unwrap_or_else(|| d.summary.name.clone()),
                folder_id: x.folder_id,
                status: Some(CaseStatus::Draft),
                body: Some(body),
                fields: None,
                tags: x.tags.clone(),
            };
            let row = create_ai_case(s, &p, auth, w).await?;
            s.audit(
                auth,
                "case.create",
                Some(&p.key),
                json!({"number": row.number, "via": "mcp", "source": "ai",
                "agent": agent, "fromResult": x.result_id}),
            )
            .await;
            Ok(
                json!({"number": row.number, "key": row.key(), "status": row.status, "aiUnreviewed": true,
                "url": format!("/p/{}/cases/{}", p.key, row.number)}),
            )
        }
        "create_case_draft" => {
            let x: CreateDraftArgs = args(a)?;
            let p = draft_project(s, auth, &x.project).await?;
            let agent = agent_of(&x.agent)?;
            let w = CaseWrite {
                name: x.name.clone(),
                folder_id: x.folder_id,
                status: Some(CaseStatus::Draft),
                body: Some(CaseBody {
                    description: x.description.clone(),
                    preconditions: x.preconditions.clone(),
                    steps: steps_of(&x.steps, 1),
                    links: Vec::new(),
                }),
                fields: field_values(&x.fields)?,
                tags: x.tags.clone(),
            };
            let row = create_ai_case(s, &p, auth, w).await?;
            s.audit(
                auth,
                "case.create",
                Some(&p.key),
                json!({"number": row.number, "via": "mcp", "source": "ai",
                "agent": agent}),
            )
            .await;
            Ok(
                json!({"number": row.number, "key": row.key(), "status": row.status, "aiUnreviewed": true,
                "url": format!("/p/{}/cases/{}", p.key, row.number)}),
            )
        }
        "propose_case_update" => {
            let x: ProposeArgs = args(a)?;
            let p = draft_project(s, auth, &x.project).await?;
            let agent = agent_of(&x.agent)?;
            if x.rationale.trim().is_empty() || x.rationale.len() > MAX_RATIONALE {
                return Err(ToolError(format!(
                    "rationale must have 1 to {MAX_RATIONALE} bytes"
                )));
            }
            let cur = s
                .store
                .case_by_number(p.id, x.number)
                .await?
                .ok_or_else(|| ToolError("not_found: case not found".into()))?;
            if cur.archived_at.is_some() {
                return Err(ToolError("not_found: case not found".into()));
            }
            if cur.revision != x.base_revision {
                return Err(ToolError(format!(
                    "suggestion_stale: the case is at revision {}; read it again",
                    cur.revision
                )));
            }
            let body = if x.description.is_some() || x.preconditions.is_some() || x.steps.is_some()
            {
                Some(CaseBody {
                    description: x
                        .description
                        .clone()
                        .unwrap_or_else(|| cur.body.description.clone()),
                    preconditions: x
                        .preconditions
                        .clone()
                        .unwrap_or_else(|| cur.body.preconditions.clone()),
                    steps: match &x.steps {
                        Some(st) => steps_of(st, 1),
                        None => cur.body.steps.clone(),
                    },
                    links: cur.body.links.clone(),
                })
            } else {
                None
            };
            let proposal = CaseProposal {
                patch: CasePatch {
                    folder_id: None,
                    status: x.status.as_deref().map(status_of).transpose()?,
                    fields: field_values(&x.fields)?,
                    tags: x.tags.clone(),
                },
                name: x.name.clone(),
                body,
            };
            if proposal.name.is_none()
                && proposal.body.is_none()
                && proposal.patch == CasePatch::default()
            {
                return Err(ToolError("the proposal changes nothing".into()));
            }
            let id = s
                .store
                .create_case_suggestion(
                    &ctx(s, &p, auth),
                    &cur,
                    &proposal,
                    x.rationale.trim(),
                    &agent,
                )
                .await
                .map_err(tm)?;
            s.audit(
                auth,
                "caseSuggestion.create",
                Some(&p.key),
                json!({"number": cur.number, "suggestionId": id,
                "baseRevision": cur.revision, "via": "mcp", "source": "ai", "agent": agent}),
            )
            .await;
            Ok(
                json!({"suggestionId": id, "status": "pending", "caseNumber": cur.number, "baseRevision": cur.revision}),
            )
        }
        "add_cases_to_plan" => {
            let x: AddToPlanArgs = args(a)?;
            let p = project(s, auth, &x.project, &s.perms.plans_write).await?;
            let agent = agent_of(&x.agent)?;
            if x.numbers.is_empty() || x.numbers.len() > 1000 {
                return Err(ToolError("numbers: 1 to 1000 case numbers".into()));
            }
            let change = TestPlanItemsChange {
                add: Some(x.numbers.clone()),
                remove: None,
                assign: None,
            };
            let plan = s
                .store
                .change_plan_items(p.id, x.plan_id, &change, &auth.principal.name, &s.cfg.tm)
                .await
                .map_err(tm)?;
            s.audit(
                auth,
                "testPlan.items",
                Some(&p.key),
                json!({"planId": x.plan_id, "add": x.numbers, "via": "mcp",
                "source": "ai", "agent": agent}),
            )
            .await;
            Ok(json!({"plan": plan}))
        }
        other => Err(ToolError(format!("unknown tool {other:?}"))),
    }
}

fn step_of(st: &fastproof_store::StepView, depth: usize) -> CaseStep {
    CaseStep {
        id: String::new(),
        action: st.name.clone(),
        expected: String::new(),
        data: String::new(),
        attachments: Vec::new(),
        steps: if depth < 3 {
            st.steps
                .iter()
                .filter(|c| c.kind != "fixture")
                .take(100)
                .map(|c| step_of(c, depth + 1))
                .collect()
        } else {
            Vec::new()
        },
    }
}

/// Objects and protocol classes an action name may start with.
const TRACE_OBJECTS: &[&str] = &[
    "page",
    "frame",
    "locator",
    "frameLocator",
    "elementHandle",
    "jsHandle",
    "context",
    "browserContext",
    "browser",
    "browserType",
    "keyboard",
    "mouse",
    "touchscreen",
    "request",
    "route",
    "dialog",
    "download",
    "worker",
    "tracing",
    "expect",
    "video",
    "webSocket",
    "clock",
    "Page",
    "Frame",
    "BrowserContext",
    "Browser",
    "BrowserType",
    "ElementHandle",
    "JSHandle",
    "Keyboard",
    "Mouse",
    "Touchscreen",
    "APIRequestContext",
    "Route",
    "Request",
    "Response",
    "Dialog",
    "Download",
    "Worker",
    "Tracing",
    "WebSocket",
    "Artifact",
    "CDPSession",
    "Clock",
    "Locator",
    "FrameLocator",
];

/// Method names an action name may end with (Playwright API and protocol).
const TRACE_METHODS: &[&str] = &[
    "goto",
    "reload",
    "goBack",
    "goForward",
    "waitForURL",
    "waitForLoadState",
    "waitForNavigation",
    "waitForSelector",
    "waitForTimeout",
    "waitForFunction",
    "waitForEvent",
    "waitForRequest",
    "waitForResponse",
    "click",
    "dblclick",
    "tap",
    "hover",
    "focus",
    "blur",
    "fill",
    "clear",
    "type",
    "press",
    "pressSequentially",
    "insertText",
    "down",
    "up",
    "move",
    "wheel",
    "check",
    "uncheck",
    "setChecked",
    "selectOption",
    "selectText",
    "setInputFiles",
    "dragTo",
    "dragAndDrop",
    "dispatchEvent",
    "scrollIntoViewIfNeeded",
    "screenshot",
    "pdf",
    "content",
    "setContent",
    "title",
    "textContent",
    "innerText",
    "innerHTML",
    "inputValue",
    "getAttribute",
    "isVisible",
    "isHidden",
    "isEnabled",
    "isDisabled",
    "isEditable",
    "isChecked",
    "count",
    "evaluate",
    "evaluateHandle",
    "evaluateExpression",
    "evaluateExpressionHandle",
    "expect",
    "newPage",
    "newContext",
    "close",
    "route",
    "unroute",
    "routeFromHAR",
    "setViewportSize",
    "addInitScript",
    "addScriptTag",
    "addStyleTag",
    "exposeFunction",
    "exposeBinding",
    "setExtraHTTPHeaders",
    "addCookies",
    "clearCookies",
    "cookies",
    "storageState",
    "grantPermissions",
    "clearPermissions",
    "setGeolocation",
    "setOffline",
    "fetch",
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "head",
    "dispose",
    "launch",
    "connect",
    "querySelector",
    "querySelectorAll",
    "boundingBox",
    "highlight",
    "ariaSnapshot",
    "emulateMedia",
    "bringToFront",
    "keyboardDown",
    "keyboardUp",
    "keyboardInsertText",
    "keyboardType",
    "keyboardPress",
    "mouseMove",
    "mouseDown",
    "mouseUp",
    "mouseClick",
    "mouseWheel",
    "touchscreenTap",
    "fulfill",
    "abort",
    "continue",
    "fallback",
    "accept",
    "dismiss",
    "saveAs",
    "path",
    "start",
    "stop",
    "startChunk",
    "stopChunk",
    "install",
    "fastForward",
    "runFor",
    "pauseAt",
    "resume",
    "setFixedTime",
    "setSystemTime",
    "toBeVisible",
    "toBeHidden",
    "toBeEnabled",
    "toBeDisabled",
    "toBeChecked",
    "toBeEditable",
    "toBeEmpty",
    "toBeFocused",
    "toBeAttached",
    "toBeInViewport",
    "toContainText",
    "toHaveText",
    "toHaveValue",
    "toHaveValues",
    "toHaveAttribute",
    "toHaveClass",
    "toContainClass",
    "toHaveCount",
    "toHaveCSS",
    "toHaveId",
    "toHaveJSProperty",
    "toHaveTitle",
    "toHaveURL",
    "toHaveScreenshot",
    "toMatchAriaSnapshot",
    "toHaveAccessibleName",
    "toHaveAccessibleDescription",
    "toHaveAccessibleErrorMessage",
    "toHaveRole",
    "toBeOK",
    "toPass",
];

/// Methods whose parameters carry what was typed (lowercase).
const SECRET_METHODS: &[&str] = &[
    "fill",
    "type",
    "press",
    "presssequentially",
    "inserttext",
    "setinputfiles",
    "selectoption",
    "check",
    "keyboarddown",
    "keyboardup",
    "keyboardinserttext",
    "keyboardtype",
    "keyboardpress",
];

/// Bytes of one trace line `trace_actions` reads; longer lines are skipped unread.
pub const MAX_TRACE_LINE: usize = 1024 * 1024;

fn is_secret(name: &str) -> bool {
    let m = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    SECRET_METHODS.contains(&m.as_str())
}

/// The name of an action from fixed lists only (`Frame.fill`, `page.goto`): a recorded
/// `apiName` when both its parts are known, else `class.method` with unknown parts as
/// `Other`/`other`. Titles are never used: the runner's titles carry typed values.
fn action_name(class: &str, method: &str, api: Option<&str>) -> String {
    if let Some((o, m)) = api.and_then(|a| a.split_once('.'))
        && TRACE_OBJECTS.contains(&o)
        && TRACE_METHODS.contains(&m)
    {
        return format!("{o}.{m}");
    }
    let c = if TRACE_OBJECTS.contains(&class) {
        class
    } else {
        "Other"
    };
    let m = if TRACE_METHODS.contains(&method) {
        method
    } else {
        "other"
    };
    format!("{c}.{m}")
}

fn typed_secret(class: &str, method: &str, api: Option<&str>) -> bool {
    is_secret(method)
        || api.is_some_and(is_secret)
        || class.eq_ignore_ascii_case("keyboard")
        || api.is_some_and(|a| a.to_ascii_lowercase().starts_with("keyboard."))
}

/// String values of the parameters of a typing action (what an error may repeat), except
/// the selector.
fn param_strings(v: Option<&Value>, out: &mut Vec<String>, depth: usize) {
    let Some(v) = v else { return };
    if out.len() >= 50 || depth > 4 {
        return;
    }
    match v {
        Value::String(s) if !s.is_empty() => out.push(s.clone()),
        Value::Array(a) => a
            .iter()
            .take(50)
            .for_each(|x| param_strings(Some(x), out, depth + 1)),
        Value::Object(o) => o
            .iter()
            .filter(|(k, _)| k.as_str() != "selector")
            .take(50)
            .for_each(|(_, x)| param_strings(Some(x), out, depth + 1)),
        _ => {}
    }
}

/// The error of an action: for a typing action only its first line, with every parameter
/// value replaced (call logs repeat `fill("…")`).
fn error_text(msg: &str, secrets: &[String]) -> String {
    if secrets.is_empty() {
        return clip(msg, 2000);
    }
    let mut s = msg.lines().next().unwrap_or("").to_string();
    for v in secrets {
        s = s.replace(v.as_str(), "***");
    }
    clip(&s, 500)
}

/// An action seen in a `before` event and waiting for its `after` event.
#[derive(Debug)]
pub struct OpenCall {
    index: usize,
    start: f64,
    secrets: Vec<String>,
}

/// Actions from the event lines of a trace: `before`/`after` pairs (Playwright 1.3x+) or
/// `action` records (older); only a name from fixed lists, the selector, the URL (not of
/// typing actions), status, error and duration. The test runner's events (`class: Test`)
/// are skipped: their titles carry typed values, and the library trace has the same calls.
pub fn actions_of(
    lines: &str,
    out: &mut Vec<Value>,
    open: &mut std::collections::HashMap<String, OpenCall>,
) {
    for line in lines.lines() {
        if out.len() >= MAX_TRACE_ACTIONS && open.is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let t = v.get("type").and_then(Value::as_str).unwrap_or("");
        match t {
            "before" => {
                let class = v.get("class").and_then(Value::as_str).unwrap_or("");
                if out.len() >= MAX_TRACE_ACTIONS || class == "Test" {
                    continue;
                }
                let method = v.get("method").and_then(Value::as_str).unwrap_or("");
                let api = v.get("apiName").and_then(Value::as_str);
                let params = v.get("params");
                let selector = params
                    .and_then(|p| p.get("selector"))
                    .and_then(Value::as_str);
                let url = params.and_then(|p| p.get("url")).and_then(Value::as_str);
                let secret = typed_secret(class, method, api);
                let mut secrets = Vec::new();
                if secret {
                    param_strings(params, &mut secrets, 0);
                }
                let start = v.get("startTime").and_then(Value::as_f64).unwrap_or(0.0);
                let call = v
                    .get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                out.push(json!({"apiName": action_name(class, method, api),
                    "selector": selector.map(|x| clip(x, 500)),
                    "url": if secret { None } else { url.map(|x| clip(x, 1000)) }, "status": "passed",
                    "error": Value::Null, "durationMs": Value::Null}));
                open.insert(
                    call,
                    OpenCall {
                        index: out.len() - 1,
                        start,
                        secrets,
                    },
                );
            }
            "after" => {
                let call = v.get("callId").and_then(Value::as_str).unwrap_or("");
                if let Some(c) = open.remove(call) {
                    let end = v.get("endTime").and_then(Value::as_f64).unwrap_or(c.start);
                    out[c.index]["durationMs"] = json!(((end - c.start).max(0.0)) as i64);
                    if let Some(e) = v
                        .get("error")
                        .and_then(|e| {
                            e.get("message")
                                .or_else(|| e.get("error").and_then(|x| x.get("message")))
                        })
                        .and_then(Value::as_str)
                    {
                        out[c.index]["status"] = json!("failed");
                        out[c.index]["error"] = json!(error_text(e, &c.secrets));
                    }
                }
            }
            "action" => {
                if out.len() >= MAX_TRACE_ACTIONS {
                    continue;
                }
                let m = v.get("metadata").unwrap_or(&Value::Null);
                let class = m.get("type").and_then(Value::as_str).unwrap_or("");
                if class == "Test" {
                    continue;
                }
                let api = m.get("apiName").and_then(Value::as_str);
                let method = m.get("method").and_then(Value::as_str).unwrap_or("");
                let params = m.get("params");
                let secret = typed_secret(class, method, api);
                let mut secrets = Vec::new();
                if secret {
                    param_strings(params, &mut secrets, 0);
                }
                let start = m.get("startTime").and_then(Value::as_f64).unwrap_or(0.0);
                let end = m.get("endTime").and_then(Value::as_f64).unwrap_or(start);
                let err = m
                    .get("error")
                    .and_then(|e| {
                        e.get("error")
                            .and_then(|x| x.get("message"))
                            .or_else(|| e.get("message"))
                    })
                    .and_then(Value::as_str);
                out.push(json!({"apiName": action_name(class, method, api),
                    "selector": params.and_then(|p| p.get("selector")).and_then(Value::as_str).map(|x| clip(x, 500)),
                    "url": if secret { None } else { params.and_then(|p| p.get("url")).and_then(Value::as_str).map(|x| clip(x, 1000)) },
                    "status": if err.is_some() { "failed" } else { "passed" },
                    "error": err.map(|e| error_text(e, &secrets)),
                    "durationMs": ((end - start).max(0.0)) as i64}));
            }
            _ => {}
        }
    }
}

/// Reads one line of at most `max` bytes into `line` (without the newline) and never holds
/// more: `Ok(None)` at the end, `Ok(Some(false))` for a longer line, which is skipped.
pub fn read_bounded_line(
    r: &mut impl std::io::BufRead,
    max: usize,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(any.then_some(!too_long));
        }
        any = true;
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let content = &chunk[..if done { take - 1 } else { take }];
        if too_long || line.len() + content.len() > max {
            too_long = true;
            line.clear();
        } else {
            line.extend_from_slice(content);
        }
        r.consume(take);
        if done {
            return Ok(Some(!too_long));
        }
    }
}

async fn trace_actions(
    s: &AppState,
    p: &fastproof_store::Project,
    result_id: Uuid,
    attempt: Option<i32>,
) -> ToolResult {
    let d = s
        .store
        .result_detail(p.id, result_id, s.history.as_ref())
        .await?
        .ok_or_else(|| ToolError("not_found: result not found".into()))?;
    let at = match attempt {
        None => d.attempts.last(),
        Some(n) => d.attempts.iter().find(|t| t.no == n),
    }
    .ok_or_else(|| ToolError("not_found: attempt not found".into()))?;
    fn find(
        atts: &[fastproof_store::AttachmentView],
        steps: &[fastproof_store::StepView],
    ) -> Option<Uuid> {
        atts.iter()
            .find(|a| a.kind == "trace" && a.id.is_some())
            .and_then(|a| a.id)
            .or_else(|| steps.iter().find_map(|s| find(&s.attachments, &s.steps)))
    }
    let id = find(&at.attachments, &at.steps)
        .ok_or_else(|| ToolError("not_found: the attempt has no Playwright trace".into()))?;
    let stored = s
        .store
        .attachment(p.id, id)
        .await?
        .ok_or_else(|| ToolError("not_found: trace not found".into()))?;
    if stored.size as u64 > MAX_TRACE_BYTES {
        return Err(ToolError(format!(
            "the trace is larger than {MAX_TRACE_BYTES} bytes"
        )));
    }
    let dir = s.cfg.spool_dir.join("traces");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    let path = dir.join(Uuid::now_v7().to_string());
    let key = fastproof_blob::BlobKey::parse(&stored.blob_key)?;
    let got = s.blobs.get_to_file(&key, &path).await;
    let r = async {
        got?.ok_or_else(|| anyhow::anyhow!("the trace file is gone"))?;
        let pth = path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Value>> {
            let limits = fastproof_ingest::Limits {
                max_upload_bytes: MAX_TRACE_BYTES,
                max_total_uncompressed_bytes: 4 * MAX_TRACE_BYTES,
                max_entry_bytes: 2 * MAX_TRACE_BYTES,
                max_archive_entries: 100_000,
                ..Default::default()
            };
            let mut out = Vec::new();
            fastproof_ingest::for_each_entry(
                &pth,
                Some("trace.zip"),
                &limits,
                &mut |name, _, r| {
                    if !name.ends_with(".trace") || out.len() >= MAX_TRACE_ACTIONS {
                        return Ok(());
                    }
                    let mut open = std::collections::HashMap::new();
                    let mut buf = String::new();
                    let mut reader = std::io::BufReader::with_capacity(64 * 1024, r);
                    let mut line = Vec::new();
                    while let Some(fits) =
                        read_bounded_line(&mut reader, MAX_TRACE_LINE, &mut line)?
                    {
                        if !fits {
                            continue;
                        }
                        if let Ok(t) = std::str::from_utf8(&line) {
                            buf.push_str(t);
                            buf.push('\n');
                        }
                        if buf.len() > MAX_TRACE_LINE {
                            actions_of(&buf, &mut out, &mut open);
                            buf.clear();
                            if out.len() >= MAX_TRACE_ACTIONS && open.is_empty() {
                                break;
                            }
                        }
                    }
                    actions_of(&buf, &mut out, &mut open);
                    let _ = reader.read(&mut [0u8; 0]);
                    Ok(())
                },
            )
            .map_err(|e| anyhow::anyhow!("reading the trace: {e}"))?;
            Ok(out)
        })
        .await?
    }
    .await;
    let _ = tokio::fs::remove_file(&path).await;
    let actions = r?;
    let truncated = actions.len() >= MAX_TRACE_ACTIONS;
    Ok(
        json!({"resultId": d.summary.id, "attempt": at.no, "traceAttachmentId": id, "actions": actions,
        "truncated": truncated}),
    )
}
