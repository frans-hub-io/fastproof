//! Analysis API (stage C): defect types, triage, clusters, similar failures, rules, flaky
//! tests, settings, launch changes, comparison, dashboard, test plans, tasks, retention preview.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, NaiveDate, Utc};
use fastproof_store::analysis::clusters::Cluster;
use fastproof_store::analysis::compare::Comparison;
use fastproof_store::analysis::dashboard::{Bucket, Dashboard, DashboardQuery};
use fastproof_store::analysis::defects::{
    DefectType, DefectTypeChange, DefectTypePatch, GROUPS, NewDefectType, validate_fields,
};
use fastproof_store::analysis::flaky::{FlakyFilter, FlakyItem};
use fastproof_store::analysis::launches::LaunchPatch;
use fastproof_store::analysis::plans::{NewTestPlan, PlanRefusal, TestPlanCreated, TestPlanJson};
use fastproof_store::analysis::rollup::RetentionPreview;
use fastproof_store::analysis::rules::{NewRule, Rule, RuleChange, RulePatch, validate_rule};
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::tasks::{ApplyParams, DryRun, Task};
use fastproof_store::analysis::triage::{TriageOutcome, TriageRefusal, TriageRequest};
use fastproof_store::{Launch, Project};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{ErrorBody, P, Q};
use crate::state::AppState;

/// `{items}` of defect types.
#[derive(Serialize, ToSchema)]
pub struct DefectTypeList {
    pub items: Vec<DefectType>,
}

/// `{items}` of clusters.
#[derive(Serialize, ToSchema)]
pub struct ClusterList {
    pub items: Vec<Cluster>,
}

/// `{items}` of rules.
#[derive(Serialize, ToSchema)]
pub struct RuleList {
    pub items: Vec<Rule>,
}

/// `{items}` of flaky tests.
#[derive(Serialize, ToSchema)]
pub struct FlakyList {
    pub items: Vec<FlakyItem>,
}

async fn defect_type_ok(s: &AppState, project: &Project, id: Option<Uuid>) -> ApiResult<()> {
    if let Some(id) = id
        && s.store.defect_type(project.id, id).await?.is_none()
    {
        return Err(ApiError::bad_request("unknown defectTypeId"));
    }
    Ok(())
}

// Defect types.

#[utoipa::path(get, path = "/api/v1/projects/{key}/defect-types", tag = "analysis", operation_id = "analysis_defect_types",
    params(("key" = String, Path)),
    responses((status = 200, body = DefectTypeList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn defect_types(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<DefectTypeList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(DefectTypeList {
        items: s.store.defect_types(p.id).await?,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/defect-types", tag = "analysis", operation_id = "analysis_create_defect_type", request_body = NewDefectType,
    params(("key" = String, Path)),
    responses((status = 201, body = DefectType), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_defect_type(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<NewDefectType>,
) -> ApiResult<(StatusCode, Json<DefectType>)> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if !GROUPS.contains(&body.group.as_str()) {
        return Err(ApiError::bad_request(format!(
            "group must be one of {}",
            GROUPS.join(", ")
        )));
    }
    validate_fields(
        Some(&body.name),
        body.short_name.as_deref(),
        body.color.as_deref(),
    )
    .map_err(ApiError::bad_request)?;
    match s.store.create_defect_type(p.id, &body).await? {
        DefectTypeChange::Done(t) => {
            s.audit(
                &auth,
                "defectType.create",
                Some(&p.key),
                serde_json::json!({"id": t.id, "group": t.group, "name": t.name}),
            )
            .await;
            Ok((StatusCode::CREATED, Json(t)))
        }
        DefectTypeChange::Conflict(m) => Err(ApiError::conflict(m)),
        DefectTypeChange::NotFound => Err(ApiError::not_found("defect type")),
    }
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/defect-types/{id}", tag = "analysis", operation_id = "analysis_patch_defect_type", request_body = DefectTypePatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = DefectType), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn patch_defect_type(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<DefectTypePatch>,
) -> ApiResult<Json<DefectType>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    validate_fields(
        body.name.as_deref(),
        body.short_name.as_deref(),
        body.color.as_deref(),
    )
    .map_err(ApiError::bad_request)?;
    match s.store.update_defect_type(p.id, id, &body).await? {
        DefectTypeChange::Done(t) => {
            s.audit(
                &auth,
                "defectType.update",
                Some(&p.key),
                serde_json::json!({"id": t.id, "name": t.name}),
            )
            .await;
            Ok(Json(t))
        }
        DefectTypeChange::Conflict(m) => Err(ApiError::conflict(m)),
        DefectTypeChange::NotFound => Err(ApiError::not_found("defect type")),
    }
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/defect-types/{id}", tag = "analysis", operation_id = "analysis_delete_defect_type",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn delete_defect_type(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    match s.store.delete_defect_type(p.id, id).await? {
        DefectTypeChange::Done(t) => {
            s.audit(
                &auth,
                "defectType.delete",
                Some(&p.key),
                serde_json::json!({"id": t.id, "name": t.name}),
            )
            .await;
            Ok(StatusCode::NO_CONTENT)
        }
        DefectTypeChange::Conflict(m) => Err(ApiError::conflict(m)),
        DefectTypeChange::NotFound => Err(ApiError::not_found("defect type")),
    }
}

// Triage.

#[utoipa::path(post, path = "/api/v1/projects/{key}/triage", tag = "analysis", operation_id = "analysis_triage", request_body = TriageRequest,
    params(("key" = String, Path)),
    responses((status = 200, body = TriageOutcome), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn triage(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<TriageRequest>,
) -> ApiResult<Json<TriageOutcome>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    let mut body = body;
    crate::mcp_tm::check_mute(
        &s,
        &auth,
        &p.key,
        body.mute.is_some() || body.unmute == Some(true),
    )?;
    if crate::mcp_tm::is_agent(&s, &auth, &p.key) {
        body.source = Some("ai".into());
    }
    s.refuse_merged_results(&p, body.launch_id, body.result_ids.as_deref())
        .await?;
    let (out, body) = apply_triage(&s, &p, &auth.principal.name, body).await?;
    s.audit(
        &auth,
        "results.triage",
        Some(&p.key),
        serde_json::json!({"updated": out.updated, "resultIds": body.result_ids.as_ref().map(|v| v.len()),
            "launchId": body.launch_id, "fingerprint": body.fingerprint, "defectTypeId": body.defect_type_id,
            "comment": body.comment.is_some(), "issue": body.issue.as_ref().map(|i| &i.url),
            "mute": body.mute.as_ref().map(|m| serde_json::json!({"until": m.until, "reason": m.reason})),
            "unmute": body.unmute, "source": body.source}),
    )
    .await;
    Ok(Json(out))
}

/// Applies a triage request for a principal that holds `results:triage` (the HTTP handler and
/// the MCP tools call this): issue keys become URLs through the project's issue trackers, then
/// the request is validated and applied. Returns the outcome and the resolved request.
pub async fn apply_triage(
    s: &AppState,
    project: &Project,
    by: &str,
    mut req: TriageRequest,
) -> ApiResult<(TriageOutcome, TriageRequest)> {
    if let Some(i) = &mut req.issue
        && i.url.is_none()
        && let Some(k) = i.key.as_deref()
    {
        i.url = s.store.project_settings(project.id).await?.issue_url(k);
    }
    req.validate(Utc::now()).map_err(ApiError::bad_request)?;
    match s.store.triage(project.id, &project.key, &req, by).await? {
        Ok(o) => Ok((o, req)),
        Err(TriageRefusal::UnknownDefectType) => Err(ApiError::bad_request("unknown defectTypeId")),
        Err(TriageRefusal::UnknownLaunch) => Err(ApiError::not_found("launch")),
    }
}

/// Creates a disabled rule from a `NewRule` document (for review before it is enabled); the
/// rule-draft operation of the MCP tools calls this for a principal that holds `rules:write`.
pub async fn create_rule_draft(
    s: &AppState,
    project: &Project,
    by: &str,
    rule: serde_json::Value,
) -> ApiResult<Rule> {
    let mut r: NewRule = serde_json::from_value(rule)
        .map_err(|e| ApiError::bad_request(format!("invalid rule: {e}")))?;
    r.enabled = Some(false);
    validate_rule(&r.name, &r.matcher, &r.action).map_err(ApiError::bad_request)?;
    check_order(r.order)?;
    defect_type_ok(s, project, r.action.defect_type_id).await?;
    match s.store.create_rule(project.id, &r, by).await? {
        RuleChange::Done(r) => Ok(*r),
        RuleChange::Conflict(m) => Err(ApiError::conflict(m)),
        RuleChange::NotFound => Err(ApiError::not_found("rule")),
    }
}

// Clusters and similar failures.

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/clusters", tag = "analysis", operation_id = "analysis_clusters",
    params(("key" = String, Path), ("id" = Uuid, Path), super::results::EnvQuery),
    responses((status = 200, body = ClusterList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn clusters(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<super::results::EnvQuery>,
) -> ApiResult<Json<ClusterList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let l = s.launch_of(&p, id).await?;
    let env = super::results::env_filter(q.env.as_deref())?;
    Ok(Json(ClusterList {
        items: s.store.clusters(&l, env.as_ref()).await?,
    }))
}

// Rules.

#[utoipa::path(get, path = "/api/v1/projects/{key}/rules", tag = "analysis", operation_id = "analysis_rules",
    params(("key" = String, Path)),
    responses((status = 200, body = RuleList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn rules(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<RuleList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(RuleList {
        items: s.store.rules(p.id).await?,
    }))
}

fn check_order(o: Option<i32>) -> ApiResult<()> {
    if let Some(o) = o
        && !(-1_000_000..=1_000_000).contains(&o)
    {
        return Err(ApiError::bad_request("order must be -1000000..=1000000"));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/rules", tag = "analysis", operation_id = "analysis_create_rule", request_body = NewRule,
    params(("key" = String, Path)),
    responses((status = 201, body = Rule), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn create_rule(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<NewRule>,
) -> ApiResult<(StatusCode, Json<Rule>)> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    validate_rule(&body.name, &body.matcher, &body.action).map_err(ApiError::bad_request)?;
    check_order(body.order)?;
    defect_type_ok(&s, &p, body.action.defect_type_id).await?;
    match s
        .store
        .create_rule(p.id, &body, &auth.principal.name)
        .await?
    {
        RuleChange::Done(r) => {
            s.audit(
                &auth,
                "rule.create",
                Some(&p.key),
                serde_json::json!({"id": r.id, "name": r.name, "match": r.matcher, "action": r.action}),
            )
            .await;
            Ok((StatusCode::CREATED, Json(*r)))
        }
        RuleChange::Conflict(m) => Err(ApiError::conflict(m)),
        RuleChange::NotFound => Err(ApiError::not_found("rule")),
    }
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/rules/{id}", tag = "analysis", operation_id = "analysis_patch_rule", request_body = RulePatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Rule), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn patch_rule(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<RulePatch>,
) -> ApiResult<Json<Rule>> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    let mut rule = s
        .store
        .rule(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("rule"))?;
    check_order(body.order)?;
    if let Some(n) = body.name {
        rule.name = n;
    }
    if let Some(e) = body.enabled {
        rule.enabled = e;
    }
    if let Some(o) = body.order {
        rule.order = o;
    }
    if let Some(m) = body.matcher {
        rule.matcher = m;
    }
    if let Some(a) = body.action {
        rule.action = a;
    }
    validate_rule(&rule.name, &rule.matcher, &rule.action).map_err(ApiError::bad_request)?;
    defect_type_ok(&s, &p, rule.action.defect_type_id).await?;
    match s.store.update_rule(p.id, &rule).await? {
        RuleChange::Done(r) => {
            s.audit(
                &auth,
                "rule.update",
                Some(&p.key),
                serde_json::json!({"id": r.id, "name": r.name, "enabled": r.enabled, "match": r.matcher, "action": r.action}),
            )
            .await;
            Ok(Json(*r))
        }
        RuleChange::Conflict(m) => Err(ApiError::conflict(m)),
        RuleChange::NotFound => Err(ApiError::not_found("rule")),
    }
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/rules/{id}", tag = "analysis", operation_id = "analysis_delete_rule",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete_rule(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    if !s.store.delete_rule(p.id, id).await? {
        return Err(ApiError::not_found("rule"));
    }
    s.audit(
        &auth,
        "rule.delete",
        Some(&p.key),
        serde_json::json!({"id": id}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST .../rules/test`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleTest {
    pub rule: NewRule,
    pub launch_id: Uuid,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/rules/test", tag = "analysis", operation_id = "analysis_test_rule", request_body = RuleTest,
    params(("key" = String, Path)),
    responses((status = 200, body = DryRun), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn test_rule(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<RuleTest>,
) -> ApiResult<Json<DryRun>> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    let r = body.rule;
    validate_rule(&r.name, &r.matcher, &r.action).map_err(ApiError::bad_request)?;
    let _permit = s.guards.heavy(&auth).await?;
    let now = Utc::now();
    let rule = Rule {
        id: Uuid::nil(),
        name: r.name,
        enabled: true,
        order: r.order.unwrap_or(0),
        matcher: r.matcher,
        action: r.action,
        created_at: now,
        updated_at: now,
        hits: 0,
    };
    match s
        .store
        .rule_dry_run(p.id, body.launch_id, rule)
        .await
        .map_err(crate::limits::heavy_error)?
    {
        None => Err(ApiError::not_found("launch")),
        Some(Err(e)) => Err(ApiError::bad_request(e)),
        Some(Ok(d)) => Ok(Json(d)),
    }
}

/// Query of `POST .../rules/{id}/apply`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ApplyQuery {
    /// One launch; default: the launches of the last 30 days (at most 500).
    pub launch_id: Option<Uuid>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/rules/{id}/apply", tag = "analysis", operation_id = "analysis_apply_rule",
    params(("key" = String, Path), ("id" = Uuid, Path), ApplyQuery),
    responses((status = 202, body = Task), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn apply_rule(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<ApplyQuery>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    s.store
        .rule(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("rule"))?;
    let launches = s.store.apply_targets(p.id, q.launch_id).await?;
    if q.launch_id.is_some() && launches.is_empty() {
        return Err(ApiError::not_found("launch"));
    }
    let params = serde_json::to_value(ApplyParams {
        rule_id: id,
        launch_ids: launches.clone(),
    })
    .map_err(anyhow::Error::from)?;
    let task = s
        .store
        .enqueue_task(p.id, "rule.apply", params, &auth.principal.name)
        .await?
        .ok_or_else(|| ApiError::too_many_requests("too many queued tasks in this project"))?;
    s.audit(
        &auth,
        "rule.apply",
        Some(&p.key),
        serde_json::json!({"ruleId": id, "launches": launches.len(), "taskId": task.id}),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/tasks/{id}", tag = "analysis", operation_id = "analysis_task",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Task), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn task(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<Task>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(
        s.store
            .task(p.id, id)
            .await?
            .ok_or_else(|| ApiError::not_found("task"))?,
    ))
}

// Flaky tests and settings.

/// Query of the flaky list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FlakyQuery {
    pub branch: Option<String>,
    pub environment: Option<String>,
    /// 1..=500, default 50.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/flaky", tag = "analysis", operation_id = "analysis_flaky",
    params(("key" = String, Path), FlakyQuery),
    responses((status = 200, body = FlakyList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn flaky(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<FlakyQuery>,
) -> ApiResult<Json<FlakyList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let settings = s.store.project_settings(p.id).await?;
    let f = FlakyFilter {
        branch: q.branch.filter(|b| !b.is_empty()),
        environment: q.environment.filter(|e| !e.is_empty()),
        limit: super::page_limit(q.limit)?,
    };
    Ok(Json(FlakyList {
        items: s.store.flaky_tests(p.id, &settings, &f).await?,
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/settings", tag = "analysis", operation_id = "analysis_get_settings",
    description = "The settings document. Callers without `settings:write` get notification targets and the `vcs` and `fastmock` URLs emptied, with `redacted: true` on each.",
    params(("key" = String, Path)),
    responses((status = 200, body = ProjectSettings), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_settings(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<ProjectSettings>> {
    let p = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    let mut doc = s.store.project_settings(p.id).await?;
    if auth
        .principal
        .denial(&s.perms.settings_write, Some(&p.key))
        .is_some()
    {
        crate::integrations::settings::redact(&mut doc);
    }
    Ok(Json(doc))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/settings", tag = "analysis", operation_id = "analysis_put_settings",
    request_body(content = ProjectSettings, description = "The areas to change. Each top-level area present in the body \
        (`flaky`, `retention`, `rollup`, `gates`, `notifications`, `issueTrackers`, `fastmock`, `vcs`, `rerun`) replaces the stored \
        one as a whole; areas left out are kept; `null` resets an area to its default (removes it). `caseSync` is merged \
        by key instead (and its `fields` by field): keys left out keep their values, a `null` key returns to its default. \
        The answer is the whole stored document."),
    params(("key" = String, Path)),
    responses((status = 200, body = ProjectSettings), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn put_settings(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(patch): ApiJson<serde_json::Value>,
) -> ApiResult<Json<ProjectSettings>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let areas: Vec<String> = patch
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let body = merge_settings(s.store.project_settings(p.id).await?, patch)?;
    body.validate().map_err(ApiError::bad_request)?;
    validate_integration_settings(&body)?;
    let new_doc = serde_json::to_value(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    crate::integrations::settings::check_secret_uses(&s, p.id, &new_doc)
        .await
        .map_err(ApiError::bad_request)?;
    let old_doc = s.store.project_settings_json(p.id).await?;
    s.store.put_project_settings(p.id, &body).await?;
    let url_changes = crate::integrations::settings::url_changes(&old_doc, &new_doc);
    if !url_changes.is_empty() {
        s.audit(
            &auth,
            "project.settings.urls",
            Some(&p.key),
            serde_json::json!({"changes": url_changes}),
        )
        .await;
    }
    if old_doc.get("rerun") != new_doc.get("rerun") {
        s.audit(
            &auth,
            "project.settings.rerun",
            Some(&p.key),
            serde_json::json!({"old": old_doc.get("rerun"), "new": new_doc.get("rerun")}),
        )
        .await;
    }
    if areas.iter().any(|a| a == "caseSync") {
        s.audit(
            &auth,
            "caseSync.settings",
            Some(&p.key),
            serde_json::json!({"caseSync": body.case_sync}),
        )
        .await;
    }
    s.audit(
        &auth,
        "project.settings",
        Some(&p.key),
        serde_json::json!({"areas": areas, "flaky": body.flaky, "retention": body.retention, "rollup": body.rollup,
            "gates": body.gates.len(), "notifications": body.notifications.len(),
            "issueTrackers": body.issue_trackers.len(), "fastmock": body.fastmock.is_some()}),
    )
    .await;
    Ok(Json(s.store.project_settings(p.id).await?))
}

/// The settings document after a `PUT`: each top-level area present in `patch` replaces the
/// stored one, areas left out are kept, and `null` resets an area. Validation runs on the result.
fn merge_settings(
    current: ProjectSettings,
    patch: serde_json::Value,
) -> ApiResult<ProjectSettings> {
    let serde_json::Value::Object(patch) = patch else {
        return Err(ApiError::bad_request("settings must be a JSON object"));
    };
    let mut doc = match serde_json::to_value(&current).map_err(anyhow::Error::from)? {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    for (k, v) in patch {
        if v.is_null() {
            doc.remove(&k);
        } else if k == "caseSync" {
            let merged = merge_case_sync(doc.remove(&k), v);
            doc.insert(k, merged);
        } else {
            doc.insert(k, v);
        }
    }
    serde_json::from_value(serde_json::Value::Object(doc))
        .map_err(|e| ApiError::bad_request(format!("invalid settings: {e}")))
}

/// `caseSync` of a `PUT` merged into the stored one by key (and `fields` by field): keys left
/// out keep their values, a `null` key goes back to its default.
fn merge_case_sync(
    current: Option<serde_json::Value>,
    patch: serde_json::Value,
) -> serde_json::Value {
    let (Some(serde_json::Value::Object(mut cur)), serde_json::Value::Object(patch)) =
        (current, patch.clone())
    else {
        return patch;
    };
    for (k, v) in patch {
        if v.is_null() {
            cur.remove(&k);
            continue;
        }
        if k == "fields"
            && let (serde_json::Value::Object(f), Some(serde_json::Value::Object(cf))) =
                (&v, cur.get_mut(&k))
        {
            for (fk, fv) in f {
                if fv.is_null() {
                    cf.remove(fk);
                } else {
                    cf.insert(fk.clone(), fv.clone());
                }
            }
            continue;
        }
        cur.insert(k, v);
    }
    serde_json::Value::Object(cur)
}

/// Validation of the integration parts of a settings document (gates, notifications, vcs,
/// issueTrackers, fastmock); the integrations feature plugs its validator in here.
fn validate_integration_settings(doc: &ProjectSettings) -> ApiResult<()> {
    let v = serde_json::to_value(doc).map_err(|e| ApiError::bad_request(e.to_string()))?;
    crate::integrations::settings::validate(&v).map_err(ApiError::bad_request)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/retention/preview", tag = "analysis", operation_id = "analysis_retention_preview",
    params(("key" = String, Path)),
    responses((status = 200, body = RetentionPreview), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn retention_preview(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<RetentionPreview>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let settings = s.store.project_settings(p.id).await?;
    Ok(Json(s.store.retention_preview(p.id, &settings).await?))
}

// Launches.

#[utoipa::path(patch, path = "/api/v1/projects/{key}/launches/{id}", tag = "launches", operation_id = "launches_patch", request_body = LaunchPatch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Launch), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn patch_launch(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<LaunchPatch>,
) -> ApiResult<Json<Launch>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    s.refuse_merged(&s.launch_of(&p, id).await?)?;
    if let Some(n) = &body.name
        && (n.trim().is_empty() || n.len() > 300)
    {
        return Err(ApiError::bad_request("name must have 1 to 300 bytes"));
    }
    if let Some(a) = &body.attributes {
        super::launches::validate_start(&fastproof_store::launches::LaunchStart {
            name: "x".into(),
            attributes: Some(a.clone()),
            ..Default::default()
        })?;
    }
    let l = s
        .store
        .patch_launch(p.id, &p.key, id, &body)
        .await?
        .ok_or_else(|| ApiError::not_found("launch"))?;
    s.audit(
        &auth,
        "launch.update",
        Some(&p.key),
        serde_json::json!({"launchId": id, "number": l.number, "important": body.important,
            "name": body.name, "attributes": body.attributes.as_ref().map(|a| a.len())}),
    )
    .await;
    Ok(Json(l))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{a}/compare/{b}", tag = "analysis", operation_id = "analysis_compare",
    params(("key" = String, Path), ("a" = Uuid, Path, description = "base launch"), ("b" = Uuid, Path, description = "compared launch")),
    responses((status = 200, body = Comparison), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn compare(
    State(s): State<AppState>,
    auth: Auth,
    P((key, a, b)): P<(String, Uuid, Uuid)>,
) -> ApiResult<Json<Comparison>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let la = s.launch_of(&p, a).await?;
    let lb = s.launch_of(&p, b).await?;
    Ok(Json(s.store.compare(&la, &lb).await?))
}

// Dashboard.

/// Query of the dashboard.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DashboardParams {
    pub branch: Option<String>,
    pub environment: Option<String>,
    /// First day (`YYYY-MM-DD` or RFC 3339, UTC); default 30 days before `to`.
    pub from: Option<String>,
    /// Last day (inclusive); default today.
    pub to: Option<String>,
    /// `day` (default) or `week`.
    pub bucket: Option<String>,
}

/// Longest dashboard window, in days.
pub const MAX_DASHBOARD_DAYS: i64 = 3660;
/// Years a dashboard day may have.
pub const DASHBOARD_YEARS: std::ops::RangeInclusive<i32> = 1970..=9999;

fn parse_day(s: &str, what: &str) -> ApiResult<NaiveDate> {
    use chrono::Datelike;
    let d = match NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => DateTime::parse_from_rfc3339(s)
            .map(|t| t.with_timezone(&Utc).date_naive())
            .map_err(|_| ApiError::bad_request(format!("{what} must be YYYY-MM-DD or RFC 3339")))?,
    };
    if !DASHBOARD_YEARS.contains(&d.year()) {
        return Err(ApiError::bad_request(format!(
            "{what} must be a day of the years 1970 to 9999"
        )));
    }
    Ok(d)
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/dashboard", tag = "analysis", operation_id = "analysis_dashboard",
    params(("key" = String, Path), DashboardParams),
    responses((status = 200, body = Dashboard), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 429, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn dashboard(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<DashboardParams>,
) -> ApiResult<Json<Dashboard>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let to = match q.to.as_deref() {
        Some(t) => parse_day(t, "to")?,
        None => Utc::now().date_naive(),
    };
    let from = match q.from.as_deref() {
        Some(f) => parse_day(f, "from")?,
        None => to
            .checked_sub_days(chrono::Days::new(29))
            .unwrap_or(to)
            .max(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or(to)),
    };
    if from > to || (to - from).num_days() > MAX_DASHBOARD_DAYS {
        return Err(ApiError::bad_request(format!(
            "from must not be after to, and the window must be at most {MAX_DASHBOARD_DAYS} days"
        )));
    }
    let bucket = match q.bucket.as_deref().unwrap_or("day") {
        "day" => Bucket::Day,
        "week" => Bucket::Week,
        _ => return Err(ApiError::bad_request("bucket must be day or week")),
    };
    let _permit = s.guards.heavy(&auth).await?;
    let settings = s.store.project_settings(p.id).await?;
    let dq = DashboardQuery {
        branch: q.branch.filter(|b| !b.is_empty()),
        environment: q.environment.filter(|e| !e.is_empty()),
        from,
        to,
        bucket,
    };
    Ok(Json(
        s.store
            .dashboard(p.id, &settings, &dq)
            .await
            .map_err(crate::limits::heavy_error)?,
    ))
}

// Test plans.

/// What `POST /test-plans` answers: the managed plan, or the id and test count of an adhoc one.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum CreatedTestPlan {
    Managed(Box<fastproof_store::tm::TestPlan>),
    Adhoc(TestPlanCreated),
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans", tag = "analysis", operation_id = "analysis_create_test_plan", request_body = NewTestPlan,
    description = "Creates an adhoc plan (`launches:write`; expires after 90 days) or, with `kind: managed`, a managed plan \
        (`plans:write`; answers the `TestPlan`).",
    params(("key" = String, Path)),
    responses((status = 201, body = CreatedTestPlan), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn create_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<NewTestPlan>,
) -> ApiResult<(StatusCode, Json<CreatedTestPlan>)> {
    if body.is_managed() {
        let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
        let w = body.managed().map_err(ApiError::bad_request)?;
        let plan = super::plans::create_managed(&s, &auth, &p, &w).await?;
        return Ok((
            StatusCode::CREATED,
            Json(CreatedTestPlan::Managed(Box::new(plan))),
        ));
    }
    let p = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let statuses = body.validate().map_err(ApiError::bad_request)?;
    let created = match s
        .store
        .create_test_plan(p.id, &body, &statuses, &auth.principal.name)
        .await?
    {
        Ok(c) => c,
        Err(PlanRefusal::UnknownLaunch) => return Err(ApiError::not_found("launch")),
        Err(PlanRefusal::TooManyTests) => {
            return Err(ApiError::bad_request("the plan would have too many tests"));
        }
        Err(PlanRefusal::TooManyPlans) => {
            return Err(ApiError::too_many_requests(
                "the project has too many test plans; old plans expire after 90 days",
            ));
        }
    };
    s.audit(
        &auth,
        "testPlan.create",
        Some(&p.key),
        serde_json::json!({"id": created.id, "name": body.name, "fromLaunchId": body.from_launch_id,
            "tests": created.test_count}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(CreatedTestPlan::Adhoc(created))))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}/testplan.json", tag = "analysis", operation_id = "analysis_testplan_json",
    description = "Allure `testplan.json`: the stored tests of an adhoc plan, or the live automated cases of a managed plan \
        (409 `plan_has_no_automated_cases` when it has none: an empty file would run every test).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TestPlanJson), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody)))]
pub async fn testplan_json(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let plan = s
        .store
        .plan_testplan(p.id, id)
        .await
        .map_err(super::tm_error)?
        .ok_or_else(|| ApiError::not_found("test plan"))?;
    Ok(super::plans::testplan_response(plan))
}
