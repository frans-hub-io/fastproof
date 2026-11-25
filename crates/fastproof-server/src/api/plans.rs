//! Managed test plans: list, get, change (with `If-Match`), archive, items, filter sync,
//! `testplan.json` of a launch, and launches started from a plan. `POST /test-plans` and
//! `GET /test-plans/{id}/testplan.json` are shared with adhoc plans (`api/analysis.rs`).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_store::analysis::plans::TestPlanJson;
use fastproof_store::launches::LaunchStart;
use fastproof_store::plan_dispatches::{DispatchRefusal, NewPlanDispatch};
use fastproof_store::tm::plans::{PlanItemFilter, PlanListFilter, TestPlanPatch, shell_quote};
use fastproof_store::tm::{
    PlanCheck, PlanCi, PlanDispatch, PlanDispatchList, PlanLaunch, PlanSyncDiff, StartPlanLaunch,
    TestPlan, TestPlanItem, TestPlanItemsChange, TestPlanWrite, TmError,
};
use fastproof_store::{Attribute, Launch, LaunchEnvironment, Page, Project};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, page_limit, tm_error};
use crate::integrations::rerun::{self, Job, PlanExtras, generic_ci_ref, generic_error};
use crate::integrations::settings::IntegrationSettings;
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
        .routes(routes!(start_plan_launch, list_plan_launches))
        .routes(routes!(list_plan_dispatches))
        .routes(routes!(launch_plan_check))
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
    let _permit = if matches!(&w.filter, Some(Some(_))) || matches!(&w.query, Some(Some(_))) {
        crate::tm_rate::admit(s, auth)?;
        Some(s.guards.heavy(auth).await?)
    } else {
        None
    };
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
            "cases": plan.counts.total, "filter": plan.filter.is_some(),
            "query": w.query.as_ref().is_some_and(Option::is_some)}),
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
    headers: axum::http::HeaderMap,
    P(key): P<String>,
    Q(q): Q<PlanListQuery>,
) -> ApiResult<Json<Page<TestPlan>>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    crate::tm_people::note(&s, &auth, &headers);
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
    let _permit = if matches!(&body.filter, Some(Some(_))) || matches!(&body.query, Some(Some(_))) {
        crate::tm_rate::admit(&s, &auth)?;
        Some(s.guards.heavy(&auth).await?)
    } else {
        None
    };
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
            "filter": body.filter.as_ref().map(|f| f.is_some()),
            "query": body.query.as_ref().map(|q| q.is_some())}),
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
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
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
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
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

/// Query of `GET /test-plans/{id}/launches`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlanLaunchesQuery {
    /// Launch number to continue below (the `next` of the previous page).
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}/launches", tag = "plans", operation_id = "list_plan_launches",
    description = "Launches started from a managed plan, newest first, with their `manual` progress.",
    params(("key" = String, Path), ("id" = Uuid, Path), PlanLaunchesQuery),
    responses((status = 200, body = Page<Launch>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_plan_launches(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<PlanLaunchesQuery>,
) -> ApiResult<Json<Page<Launch>>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let before = match q.before.as_deref() {
        None => None,
        Some(b) => Some(
            b.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid cursor"))?,
        ),
    };
    Ok(Json(
        s.store
            .plan_launches(p.id, id, before, page_limit(q.limit)?)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/test-plans/{id}/launches", tag = "plans", operation_id = "start_plan_launch",
    description = "Starts a launch from a managed plan: run items for its manual cases (assigned per `assign`), \
        `manual.state = open`; automated cases get no run items, CI runs them from `testplanUrl` (see `ciHint`) and \
        uploads into the same launch (`externalId`; generated when absent). An `externalId` of a launch already \
        started from this plan returns that launch (200). With `ci.dispatch` (needs `launches:rerun` and the project's `rerun` settings) \
        the start makes one launch per `ci.environments` entry (at most 10; none given: one) and triggers the CI once per launch \
        with the plan, the ref and the variables allowed by `rerun.variables` and `rerun.refs`; the answer is 201 with every dispatch's \
        status, and a CI refusal keeps the launches. Counts against the 60 CI triggers per project and hour shared with reruns, and \
        a plan can be started in CI once per 10 seconds.",
    request_body = StartPlanLaunch,
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 201, body = PlanLaunch), (status = 200, body = PlanLaunch), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody, description = "CI start limits (`too_many_requests`), or the caller's rate of plan launches (`rate_limited`; a CI start counts once per environment)"),
        (status = 409, body = ErrorBody, description = "`launch_exists`, `plan_archived`, `plan_not_managed`, `limit_reached`, `plan_has_no_automated_cases` and `conflict` (no `rerun` settings) with `ci`")))]
pub async fn start_plan_launch(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<StartPlanLaunch>,
) -> ApiResult<(StatusCode, Json<PlanLaunch>)> {
    let p = s.project_for(&auth, &key, &s.perms.runs_execute).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let permit = s.guards.heavy(&auth).await?;
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
    let ci = body.ci.as_ref().filter(|c| c.dispatch);
    if body.include_unapproved {
        franshub_auth::require(&auth, &s.perms.cases_review, Some(&p.key))?;
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
    if let Some(ci) = ci {
        return start_in_ci(&s, &auth, &p, id, &body, ci, start, permit).await;
    }
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
        audit_start(&s, &auth, &p, id, &launch, started.run_items, &body).await;
    }
    let ci_hint = hint_of(&p.key, &launch, &external_id, &body, started.automated);
    Ok((
        if started.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(PlanLaunch {
            launches: vec![launch.clone()],
            testplan_url: testplan_url_of(&s, &p.key, launch.id),
            launch,
            ci_hint,
            dispatches: Vec::new(),
            skips: started.skips,
        }),
    ))
}

async fn audit_start(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    plan_id: Uuid,
    launch: &Launch,
    run_items: i64,
    body: &StartPlanLaunch,
) {
    s.audit(
        auth,
        "run.start",
        Some(&p.key),
        serde_json::json!({"planId": plan_id, "launchId": launch.id, "number": launch.number,
            "externalId": launch.external_id, "runItems": run_items, "assign": body.assign}),
    )
    .await;
}

fn testplan_url_of(s: &AppState, project: &str, launch: Uuid) -> String {
    let path = format!("api/v1/projects/{project}/launches/{launch}/testplan.json");
    crate::integrations::dispatch::page_url(s, &path).unwrap_or_else(|| format!("/{path}"))
}

fn hint_of(
    project: &str,
    launch: &Launch,
    external_id: &str,
    body: &StartPlanLaunch,
    automated: i64,
) -> String {
    if body.include_automated && automated > 0 {
        ci_hint(
            project,
            launch.external_id.as_deref().unwrap_or(external_id),
        )
    } else {
        String::new()
    }
}

fn can_see_ci(s: &AppState, auth: &Auth, project: &str) -> bool {
    auth.principal
        .denial(&s.perms.settings_write, Some(project))
        .is_none()
}

/// What callers without `settings:write` may see of a dispatch: generic error texts and only the
/// pipeline id in `ciRef`, as the CI host and the secret names are settings details.
fn shown(mut d: PlanDispatch, admin: bool) -> PlanDispatch {
    if !admin {
        d.error = d.error.as_deref().map(generic_error);
        d.ci_ref = d.ci_ref.as_deref().map(generic_ci_ref);
    }
    d
}

fn refused(r: DispatchRefusal) -> ApiError {
    ApiError::too_many_requests(match r {
        DispatchRefusal::Cooldown => "this plan was started in CI less than 10 seconds ago",
        DispatchRefusal::TooManyInHour => "too many CI starts in this project in the last hour",
    })
}

/// Launch name for an environment: the name with the environment in brackets, within 300 bytes.
fn environment_name(name: &str, env: &str) -> String {
    let suffix = format!(" [{env}]");
    let mut room = 300usize.saturating_sub(suffix.len());
    while !name.is_char_boundary(room.min(name.len())) {
        room -= 1;
    }
    format!("{}{suffix}", &name[..room.min(name.len())])
}

/// Starts one launch per environment and dispatches each to the project's CI (§7.4). `permit`
/// (the heavy slot of the start) is given back before the CI is called, so a slow CI does not
/// hold the replica's heavy slots.
#[allow(clippy::too_many_arguments)]
async fn start_in_ci(
    s: &AppState,
    auth: &Auth,
    p: &Project,
    plan_id: Uuid,
    body: &StartPlanLaunch,
    ci: &PlanCi,
    start: LaunchStart,
    permit: crate::limits::HeavyPermit,
) -> ApiResult<(StatusCode, Json<PlanLaunch>)> {
    franshub_auth::require(auth, &s.perms.launches_rerun, Some(&p.key))?;
    let admin = can_see_ci(s, auth, &p.key);
    let plan = s
        .store
        .test_plan(p.id, plan_id)
        .await?
        .ok_or_else(|| ApiError::not_found("test plan"))?;
    if plan.kind != fastproof_store::tm::TestPlanKind::Managed {
        return Err(tm_error(TmError::conflict(
            "plan_not_managed",
            "only managed plans start launches",
        )));
    }
    if plan.archived {
        return Err(tm_error(TmError::conflict(
            "plan_archived",
            "the plan is archived",
        )));
    }
    if plan.counts.automated == 0 {
        return Err(tm_error(TmError::conflict(
            "plan_has_no_automated_cases",
            "the plan has no automated cases to run in CI",
        )));
    }
    let doc = s.store.project_settings_json(p.id).await?;
    let settings = IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))?;
    let Some(cfg) = settings.rerun else {
        return Err(ApiError::conflict(
            "the project has no rerun settings (integrations: rerun)",
        ));
    };
    let resolved = rerun::resolve_plan_ci(&cfg, ci)?;
    let several = resolved.environments.len() > 1;
    if several && body.external_id.is_some() {
        return Err(ApiError::bad_request(
            "externalId is for a start with one environment; with several each launch gets its own",
        ));
    }
    if body
        .external_id
        .as_deref()
        .is_some_and(|e| !rerun::valid_ci_external_id(e))
    {
        return Err(ApiError::bad_request(
            "externalId of a CI start has 1 to 128 letters, digits, . _ and -",
        ));
    }
    if let Some(extra) = resolved
        .environments
        .len()
        .checked_sub(1)
        .filter(|n| *n > 0)
    {
        crate::tm_rate::admit_n(s, auth, extra as u32)?;
    }
    if let Some(r) = s
        .store
        .plan_dispatch_check(p.id, plan_id, resolved.environments.len())
        .await?
    {
        return Err(refused(r));
    }
    let can_sync = auth.principal.allowed(&s.perms.cases_sync, Some(&p.key));
    let mut launches: Vec<Launch> = Vec::new();
    let mut first: Option<fastproof_store::tm::plans::StartedPlanLaunch> = None;
    let mut failure: Option<ApiError> = None;
    for (env, _) in &resolved.environments {
        let mut st = start.clone();
        if let Some(e) = env {
            st.name = environment_name(&start.name, e);
            st.environment = Some(LaunchEnvironment {
                key: Some(e.clone()),
                values: body.environment.clone().unwrap_or_default(),
            });
        }
        if several {
            st.external_id = Some(format!("plan-{}", Uuid::now_v7()));
        }
        if let Err(e) = super::launches::validate_start(&st) {
            failure = Some(e);
            break;
        }
        match s
            .store
            .start_plan_launch(
                p,
                plan_id,
                body,
                &st,
                &auth.principal.name,
                can_sync,
                &s.cfg.tm,
            )
            .await
        {
            Ok(started) if !started.created => {
                failure = Some(tm_error(TmError::conflict(
                    "launch_exists",
                    "a launch with this externalId was started from this plan already",
                )));
                break;
            }
            Ok(started) => {
                launches.push(started.launch.clone());
                audit_start(
                    s,
                    auth,
                    p,
                    plan_id,
                    &started.launch,
                    started.run_items,
                    body,
                )
                .await;
                first.get_or_insert(started);
            }
            Err(e) => {
                failure = Some(tm_error(e));
                break;
            }
        }
    }
    let undo = |launches: Vec<Launch>| async move {
        for l in launches {
            let _ = s.store.delete_launch(p.id, &p.key, l.id).await;
        }
    };
    if let Some(e) = failure {
        undo(launches).await;
        return Err(e);
    }
    let rows: Vec<NewPlanDispatch> = launches
        .iter()
        .zip(&resolved.environments)
        .map(|(l, (env, vars))| NewPlanDispatch {
            launch_id: l.id,
            environment: env.clone(),
            git_ref: resolved.git_ref.clone(),
            variables: vars.clone(),
        })
        .collect();
    let begun = match s
        .store
        .plan_dispatch_begin(
            &p.key,
            p.id,
            plan_id,
            &cfg.provider,
            &auth.principal.name,
            &rows,
        )
        .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(r)) => {
            undo(launches).await;
            return Err(refused(r));
        }
        Err(e) => {
            undo(launches).await;
            return Err(e.into());
        }
    };
    let first = first.ok_or_else(ApiError::internal)?;
    let testplan = s.store.launch_testplan(p.id, launches[0].id).await;
    drop(permit);
    let target = rerun::target(&cfg);
    let calls = launches.iter().zip(&begun).zip(&rows).map(|((l, d), row)| {
        let (cfg, target, testplan) = (&cfg, &target, &testplan);
        async move {
            let outcome = match testplan {
                Ok(t) => {
                    let job = Job {
                        project: &p.key,
                        launch: l,
                        rerun_id: d.id,
                        external_id: l.external_id.as_deref().unwrap_or_default(),
                        plan_id,
                        mode: "plan",
                        plan: t,
                    };
                    let extras = PlanExtras {
                        git_ref: &row.git_ref,
                        variables: &row.variables,
                        environment: row.environment.as_deref(),
                    };
                    rerun::dispatch_plan(s, p.id, cfg, &job, &extras).await
                }
                Err(e) => Err(format!("no tests to run: {e}")),
            };
            let (error, ci_ref) = match &outcome {
                Ok(r) => (None, r.as_deref()),
                Err(e) => (Some(e.as_str()), None),
            };
            let done = s
                .store
                .plan_dispatch_done(&p.key, d.id, error, ci_ref)
                .await;
            let mut audit = serde_json::json!({"planId": plan_id, "launchId": l.id,
                "environment": row.environment, "provider": cfg.provider, "ref": row.git_ref,
                "variables": row.variables.keys().collect::<Vec<_>>(), "target": target,
                "ok": error.is_none()});
            if let Some(e) = error {
                audit["detail"] = serde_json::json!(e);
            }
            s.audit(auth, "testPlan.dispatch", Some(&p.key), audit)
                .await;
            done
        }
    });
    let mut dispatches = Vec::with_capacity(begun.len());
    for d in futures::future::join_all(calls).await {
        dispatches.push(shown(d?, admin));
    }
    let ext = first.launch.external_id.clone().unwrap_or_default();
    let ci_hint = hint_of(&p.key, &first.launch, &ext, body, first.automated);
    Ok((
        StatusCode::CREATED,
        Json(PlanLaunch {
            testplan_url: testplan_url_of(s, &p.key, first.launch.id),
            launch: first.launch,
            launches,
            ci_hint,
            dispatches,
            skips: first.skips,
        }),
    ))
}

/// Query of `GET /test-plans/{id}/dispatches`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlanDispatchesQuery {
    pub before: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/test-plans/{id}/dispatches", tag = "plans", operation_id = "list_plan_dispatches",
    description = "CI dispatches of the plan's launches, newest first. Callers without `settings:write` get generic error texts and only the pipeline id in `ciRef`.",
    params(("key" = String, Path), ("id" = Uuid, Path), PlanDispatchesQuery),
    responses((status = 200, body = PlanDispatchList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn list_plan_dispatches(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<PlanDispatchesQuery>,
) -> ApiResult<Json<PlanDispatchList>> {
    let p = s.project_for(&auth, &key, &s.perms.cases_read).await?;
    let admin = can_see_ci(&s, &auth, &p.key);
    let mut list = s
        .store
        .plan_dispatches(p.id, id, q.before.as_deref(), page_limit(q.limit)?)
        .await
        .map_err(tm_error)?;
    list.items = list.items.into_iter().map(|d| shown(d, admin)).collect();
    Ok(Json(list))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/plan-check", tag = "plans", operation_id = "launch_plan_check",
    description = "What a finished launch started from a plan ran compared to the plan: results that are not linked to an automated case of the plan (`extra`) and automated plan cases without a result (`missing`). 404 for a launch without a plan or one that is not finished.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = PlanCheck), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 429, body = ErrorBody, description = "too many concurrent heavy reads of the caller")))]
pub async fn launch_plan_check(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<PlanCheck>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&p, id).await?;
    let _permit = s.guards.heavy(&auth).await?;
    s.store
        .plan_check_of(launch.id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("plan check"))
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
