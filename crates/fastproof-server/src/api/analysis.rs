//! Analysis API (stage C): defect types, triage, clusters, similar failures, rules, flaky
//! tests, settings, launch changes, comparison, dashboard, test plans, tasks, retention preview.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
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
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = ClusterList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn clusters(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<ClusterList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let l = s.launch_of(&p, id).await?;
    Ok(Json(ClusterList {
        items: s.store.clusters(&l).await?,
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
        (status = 404, body = ErrorBody)))]
pub async fn test_rule(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<RuleTest>,
) -> ApiResult<Json<DryRun>> {
    let p = s.project_for(&auth, &key, &s.perms.rules_write).await?;
    let r = body.rule;
    validate_rule(&r.name, &r.matcher, &r.action).map_err(ApiError::bad_request)?;
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
    match s.store.rule_dry_run(p.id, body.launch_id, rule).await? {
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
    params(("key" = String, Path)),
    responses((status = 200, body = ProjectSettings), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_settings(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<ProjectSettings>> {
    let p = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    Ok(Json(s.store.project_settings(p.id).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/settings", tag = "analysis", operation_id = "analysis_put_settings", request_body = ProjectSettings,
    params(("key" = String, Path)),
    responses((status = 200, body = ProjectSettings), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn put_settings(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<ProjectSettings>,
) -> ApiResult<Json<ProjectSettings>> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    body.validate().map_err(ApiError::bad_request)?;
    validate_integration_settings(&body)?;
    s.store.put_project_settings(p.id, &body).await?;
    s.audit(
        &auth,
        "project.settings",
        Some(&p.key),
        serde_json::json!({"flaky": body.flaky, "retention": body.retention, "rollup": body.rollup,
            "gates": body.gates.len(), "notifications": body.notifications.len(),
            "issueTrackers": body.issue_trackers.len(), "fastmock": body.fastmock.is_some()}),
    )
    .await;
    Ok(Json(s.store.project_settings(p.id).await?))
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

fn parse_day(s: &str, what: &str) -> ApiResult<NaiveDate> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d);
    }
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc).date_naive())
        .map_err(|_| ApiError::bad_request(format!("{what} must be YYYY-MM-DD or RFC 3339")))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/dashboard", tag = "analysis", operation_id = "analysis_dashboard",
    params(("key" = String, Path), DashboardParams),
    responses((status = 200, body = Dashboard), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
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
        None => to - chrono::Duration::days(29),
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
    let settings = s.store.project_settings(p.id).await?;
    let dq = DashboardQuery {
        branch: q.branch.filter(|b| !b.is_empty()),
        environment: q.environment.filter(|e| !e.is_empty()),
        from,
        to,
        bucket,
    };
    Ok(Json(s.store.dashboard(p.id, &settings, &dq).await?))
}

// Test plans.

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans", tag = "analysis", operation_id = "analysis_create_test_plan", request_body = NewTestPlan,
    params(("key" = String, Path)),
    responses((status = 201, body = TestPlanCreated), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn create_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<NewTestPlan>,
) -> ApiResult<(StatusCode, Json<TestPlanCreated>)> {
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
    };
    s.audit(
        &auth,
        "testPlan.create",
        Some(&p.key),
        serde_json::json!({"id": created.id, "name": body.name, "fromLaunchId": body.from_launch_id,
            "tests": created.test_count}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}/testplan.json", tag = "analysis", operation_id = "analysis_testplan_json",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TestPlanJson), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn testplan_json(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let plan = s
        .store
        .test_plan_json(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("test plan"))?;
    let mut r = Json(plan).into_response();
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"testplan.json\""),
    );
    Ok(r)
}
