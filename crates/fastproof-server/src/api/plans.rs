//! Managed test plans: list, get, change (with `If-Match`), archive, items, filter sync,
//! `testplan.json` of a launch, and launches started from a plan. `POST /test-plans` and
//! `GET /test-plans/{id}/testplan.json` are shared with adhoc plans (`api/analysis.rs`).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_store::analysis::plans::TestPlanJson;
use fastproof_store::launches::LaunchStart;
use fastproof_store::tm::plans::{PlanItemFilter, PlanListFilter, TestPlanPatch, shell_quote};
use fastproof_store::tm::{
    PlanLaunch, PlanSyncDiff, StartPlanLaunch, TestPlan, TestPlanItem, TestPlanItemsChange,
    TestPlanWrite,
};
use fastproof_store::{Attribute, LaunchEnvironment, Page, Project};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::state::AppState;

/// Entries of `environment` when starting a launch from a plan, at most.
pub const MAX_ENVIRONMENT_VALUES: usize = 50;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_managed_plans))
        .routes(routes!(get_test_plan, update_test_plan, archive_test_plan))
        .routes(routes!(list_test_plan_items, change_test_plan_items))
        .routes(routes!(sync_test_plan))
        .routes(routes!(start_plan_launch))
        .routes(routes!(launch_testplan_json))
}

/// `ETag: "r<revision>"`.
pub fn etag(revision: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"r{revision}\"")).unwrap_or(HeaderValue::from_static("\"\""))
}

pub use super::if_match;

fn with_etag<T: serde::Serialize>(revision: i64, body: T) -> Response {
    let mut r = Json(body).into_response();
    r.headers_mut().insert(header::ETAG, etag(revision));
    r
}

/// Answers a managed plan create from `POST /test-plans` (`kind: managed`).
pub async fn create_managed(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    w: &TestPlanWrite,
) -> ApiResult<TestPlan> {
    let plan = s
        .store
        .create_managed_plan(p.id, w, &auth.principal.name, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        auth,
        "testPlan.create",
        Some(&p.key),
        serde_json::json!({"id": plan.id, "kind": "managed", "name": plan.name,
            "cases": plan.counts.total, "filter": plan.filter.is_some()}),
    )
    .await;
    Ok(plan)
}

/// Query of `GET /test-plans`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlanListQuery {
    /// `managed` (the only kind listed; adhoc plans are reached by id).
    pub kind: Option<String>,
    /// Substring of the name.
    pub q: Option<String>,
    /// `true`: archived plans only.
    pub archived: Option<bool>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans", tag = "plans", operation_id = "list_managed_plans",
    description = "Managed plans, newest first, with their case counts.",
    params(("key" = String, Path), PlanListQuery),
    responses((status = 200, body = Page<TestPlan>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_managed_plans(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<PlanListQuery>,
) -> ApiResult<Json<Page<TestPlan>>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    if q.kind.as_deref().is_some_and(|k| k != "managed") {
        return Err(ApiError::bad_request("kind must be managed"));
    }
    if q.q.as_ref().is_some_and(|v| v.len() > 200) {
        return Err(ApiError::bad_request("q has at most 200 bytes"));
    }
    let f = PlanListFilter {
        q: q.q,
        archived: q.archived.unwrap_or(false),
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store.managed_plans(p.id, &f).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}", tag = "plans", operation_id = "get_test_plan",
    description = "A plan (adhoc or managed) with `ETag: \"r<revision>\"`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TestPlan), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let plan = s
        .store
        .test_plan(p.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("test plan"))?;
    Ok(with_etag(plan.revision, plan))
}

#[utoipa::path(patch, path = "/api/v1/projects/{key}/test-plans/{id}", tag = "plans", operation_id = "update_test_plan",
    description = "Changes the name, description or filter of a managed plan. Needs `If-Match`.",
    request_body = TestPlanPatch,
    params(("key" = String, Path), ("id" = Uuid, Path), ("If-Match" = String, Header, description = "ETag of the plan")),
    responses((status = 200, body = TestPlan), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`plan_not_managed` or `plan_archived`"),
        (status = 412, body = ErrorBody), (status = 428, body = ErrorBody)))]
pub async fn update_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<TestPlanPatch>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
    let revision = if_match(&headers)?;
    let plan = s
        .store
        .update_managed_plan(p.id, id, revision, &body, &auth.principal.name, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "testPlan.update",
        Some(&p.key),
        serde_json::json!({"id": id, "name": body.name, "description": body.description.is_some(),
            "filter": body.filter.as_ref().map(|f| f.is_some())}),
    )
    .await;
    Ok(with_etag(plan.revision, plan))
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/test-plans/{id}", tag = "plans", operation_id = "archive_test_plan",
    description = "Archives a managed plan (it stays readable; launches keep their link).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`plan_not_managed`")))]
pub async fn archive_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<StatusCode> {
    let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
    let plan = s
        .store
        .archive_managed_plan(p.id, id, &auth.principal.name)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "testPlan.archive",
        Some(&p.key),
        serde_json::json!({"id": id, "name": plan.name}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Query of `GET /test-plans/{id}/items`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlanItemsQuery {
    /// Cases in this folder or its subfolders.
    pub folder: Option<Uuid>,
    /// `me`, `none` (unassigned) or a name.
    pub assignee: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

/// `me` → the caller, `none` → unassigned, anything else → that name.
pub fn assignee_filter(v: Option<&str>, me: &str) -> ApiResult<Option<Option<String>>> {
    match v {
        None => Ok(None),
        Some("me") => Ok(Some(Some(me.to_string()))),
        Some("none") => Ok(Some(None)),
        Some(a) if a.is_empty() || a.len() > 200 => {
            Err(ApiError::bad_request("assignee has 1 to 200 bytes"))
        }
        Some(a) => Ok(Some(Some(a.to_string()))),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}/items", tag = "plans", operation_id = "list_test_plan_items",
    description = "The cases of a plan by position.",
    params(("key" = String, Path), ("id" = Uuid, Path), PlanItemsQuery),
    responses((status = 200, body = Page<TestPlanItem>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_test_plan_items(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<PlanItemsQuery>,
) -> ApiResult<Json<Page<TestPlanItem>>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let f = PlanItemFilter {
        folder: q.folder,
        assignee: assignee_filter(q.assignee.as_deref(), &auth.principal.name)?,
        before: q.before,
        limit: page_limit(q.limit)?,
    };
    Ok(Json(
        s.store.plan_items(p.id, id, &f).await.map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans/{id}/items", tag = "plans", operation_id = "change_test_plan_items",
    description = "Adds, removes and assigns cases of a managed plan (at most `maxBulk` numbers in all).",
    request_body = TestPlanItemsChange,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TestPlan), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody, description = "`limit_reached`, `plan_archived`")))]
pub async fn change_test_plan_items(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<TestPlanItemsChange>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
    let plan = s
        .store
        .change_plan_items(p.id, id, &body, &auth.principal.name, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "testPlan.items",
        Some(&p.key),
        serde_json::json!({"id": id, "add": body.add.as_ref().map(Vec::len),
            "remove": body.remove.as_ref().map(Vec::len), "assign": body.assign.as_ref().map(Vec::len)}),
    )
    .await;
    Ok(with_etag(plan.revision, plan))
}

/// Query of `POST /test-plans/{id}/sync`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct SyncQuery {
    /// `true`: only show the diff.
    pub dry_run: Option<bool>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans/{id}/sync", tag = "plans", operation_id = "sync_test_plan",
    description = "Brings the filter-added cases of a plan in line with its filter: `added` are cases the filter matches \
        that the plan lacks, `removed` cases the filter added earlier and no longer matches. `dryRun=true` only shows the diff.",
    params(("key" = String, Path), ("id" = Uuid, Path), SyncQuery),
    responses((status = 200, body = PlanSyncDiff), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
pub async fn sync_test_plan(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<SyncQuery>,
) -> ApiResult<Json<PlanSyncDiff>> {
    let p = s.project_for(&auth, &key, &s.perms.plans_write).await?;
    let dry = q.dry_run.unwrap_or(false);
    let diff = s
        .store
        .sync_plan(p.id, id, dry, &auth.principal.name, &s.cfg.tm)
        .await
        .map_err(tm_error)?;
    if !dry {
        s.audit(
            &auth,
            "testPlan.sync",
            Some(&p.key),
            serde_json::json!({"id": id, "added": diff.added.len(), "removed": diff.removed.len()}),
        )
        .await;
    }
    Ok(Json(diff))
}

/// The `fastproof testplan` command CI runs for a launch started from a plan.
pub fn ci_hint(project: &str, external_id: &str) -> String {
    let ext = shell_quote(external_id);
    format!(
        "export FASTPROOF_PROJECT={} FASTPROOF_LAUNCH={ext}\n\
         fastproof testplan --launch {ext} > testplan.json\n\
         export ALLURE_TESTPLAN_PATH=\"$PWD/testplan.json\"",
        shell_quote(project)
    )
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans/{id}/launches", tag = "plans", operation_id = "start_plan_launch",
    description = "Starts a launch from a managed plan: run items for its manual cases (assigned per `assign`), \
        `manual.state = open`; automated cases get no run items, CI runs them from `testplanUrl` (see `ciHint`) and \
        uploads into the same launch (`externalId`; generated when absent). An `externalId` of a launch already \
        started from this plan returns that launch (200).",
    request_body = StartPlanLaunch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 201, body = PlanLaunch), (status = 200, body = PlanLaunch), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`launch_exists`, `plan_archived`, `plan_not_managed`, `limit_reached`")))]
pub async fn start_plan_launch(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<StartPlanLaunch>,
) -> ApiResult<(StatusCode, Json<PlanLaunch>)> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    if let Some(env) = &body.environment
        && (env.len() > MAX_ENVIRONMENT_VALUES
            || env
                .iter()
                .any(|(k, v)| k.is_empty() || k.len() > 256 || v.len() > 1024))
    {
        return Err(ApiError::bad_request(format!(
            "environment has at most {MAX_ENVIRONMENT_VALUES} entries; keys 1 to 256 bytes, values at most 1024"
        )));
    }
    let external_id = body
        .external_id
        .clone()
        .unwrap_or_else(|| format!("plan-{}", Uuid::now_v7()));
    if external_id.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "externalId must not contain control characters",
        ));
    }
    let start = LaunchStart {
        external_id: Some(external_id.clone()),
        name: body.name.clone(),
        attributes: body.attributes.as_ref().map(|a| {
            a.iter()
                .map(|x| Attribute {
                    key: x.key.clone(),
                    value: x.value.clone(),
                })
                .collect()
        }),
        ci: None,
        vcs: None,
        environment: body.environment.as_ref().map(|e| LaunchEnvironment {
            key: None,
            values: e.clone(),
        }),
        debug: None,
    };
    super::launches::validate_start(&start)?;
    let can_sync = auth.principal.allowed(&s.perms.cases_sync, Some(&p.key));
    let started = s
        .store
        .start_plan_launch(
            &p,
            id,
            &body,
            &start,
            &auth.principal.name,
            can_sync,
            &s.cfg.tm,
        )
        .await
        .map_err(tm_error)?;
    let launch = started.launch;
    if started.created {
        s.audit(
            &auth,
            "run.start",
            Some(&p.key),
            serde_json::json!({"planId": id, "launchId": launch.id, "number": launch.number,
                "externalId": launch.external_id, "runItems": started.run_items, "assign": body.assign}),
        )
        .await;
    }
    let path = format!(
        "api/v1/projects/{}/launches/{}/testplan.json",
        p.key, launch.id
    );
    let testplan_url =
        crate::integrations::dispatch::page_url(&s, &path).unwrap_or_else(|| format!("/{path}"));
    let ext = launch.external_id.clone().unwrap_or(external_id);
    let ci_hint = if body.include_automated && started.automated > 0 {
        ci_hint(&p.key, &ext)
    } else {
        String::new()
    };
    Ok((
        if started.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(PlanLaunch {
            launch,
            testplan_url,
            ci_hint,
        }),
    ))
}

/// `testplan.json` as a download.
pub fn testplan_response(plan: TestPlanJson) -> Response {
    let mut r = Json(plan).into_response();
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"testplan.json\""),
    );
    r
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/testplan.json", tag = "plans", operation_id = "launch_testplan_json",
    description = "Allure `testplan.json` of a launch started from a managed plan, computed live from the plan's \
        automated cases (`id` = ALLURE_ID when the code has one, `selector` = full name; muted ones also in `mutedTests`).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = TestPlanJson), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody, description = "`plan_has_no_automated_cases`")))]
pub async fn launch_testplan_json(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let plan = s.store.launch_testplan(p.id, id).await.map_err(tm_error)?;
    Ok(testplan_response(plan))
}
