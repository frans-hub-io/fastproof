//! Rerun of a launch's tests in the CI that produced it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::analysis::plans::{NewTestPlan, PlanRefusal};
use fastproof_store::reruns::{MAX_RERUN_RESULTS, Rerun, RerunRefusal};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{ErrorBody, P};
use crate::integrations::rerun::{self, Job, generic_ci_ref, generic_error};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

/// What a rerun runs.
#[derive(Clone, Copy, Debug, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RerunMode {
    /// The tests of the launch with the given statuses (default `failed`, `broken`).
    Failed,
    /// The tests of the given results.
    Selected,
}

impl RerunMode {
    fn as_str(self) -> &'static str {
        match self {
            RerunMode::Failed => "failed",
            RerunMode::Selected => "selected",
        }
    }
}

/// Body of `POST .../launches/{id}/rerun`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RerunRequest {
    pub mode: RerunMode,
    /// `failed`: statuses taken from the launch (default `failed`, `broken`).
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    /// `selected`: results of this launch (1 to 1000).
    #[serde(default)]
    pub result_ids: Option<Vec<Uuid>>,
}

/// Answer of `GET .../launches/{id}/reruns`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RerunList {
    /// Whether the project settings have a `rerun` section.
    pub configured: bool,
    /// `github`, `gitlab` or `webhook` when configured.
    pub provider: Option<String>,
    /// Reruns requested from this launch, newest first.
    pub items: Vec<Rerun>,
    /// The rerun that started this launch, when it is the result of one.
    pub rerun_of: Option<Rerun>,
}

async fn settings_of(s: &AppState, project_id: Uuid) -> ApiResult<IntegrationSettings> {
    let doc = s.store.project_settings_json(project_id).await?;
    IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/reruns", tag = "integrations", operation_id = "integrations_reruns",
    description = "The reruns requested from the launch with the launches they started, and the rerun that started this launch. Callers without `settings:write` get generic error texts and only the pipeline id in `ciRef`, as the CI host and the secret names are settings details.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = RerunList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn list(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<RerunList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let settings = settings_of(&s, project.id).await.unwrap_or_default();
    let admin = can_see_ci(&s, &auth, &project.key);
    let hide = |mut r: Rerun| {
        if !admin {
            r.error = r.error.as_deref().map(generic_error);
            r.ci_ref = r.ci_ref.as_deref().map(generic_ci_ref);
        }
        r
    };
    Ok(Json(RerunList {
        configured: settings.rerun.is_some(),
        provider: settings.rerun.map(|r| r.provider),
        items: s
            .store
            .reruns_of(project.id, launch.id)
            .await?
            .into_iter()
            .map(hide)
            .collect(),
        rerun_of: s
            .store
            .rerun_that_started(project.id, launch.id)
            .await?
            .map(hide),
    }))
}

fn can_see_ci(s: &AppState, auth: &Auth, project: &str) -> bool {
    auth.principal
        .denial(&s.perms.settings_write, Some(project))
        .is_none()
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/rerun", tag = "integrations", operation_id = "integrations_rerun", request_body = RerunRequest,
    description = "Makes a test plan of the failed (or selected) tests and triggers the CI configured in the project settings (`rerun`) with the plan and the external id the new launch must use (`launches:rerun`, held by editor and admin, not by uploader). The CI is called once, without retries. The new launch is linked to this one when it starts with that external id. Every outcome, also a refusal, is written to the audit log (`launch.rerun`).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 201, body = Rerun), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "no `rerun` settings, or no tests to run", body = ErrorBody),
        (status = 429, description = "too many reruns", body = ErrorBody),
        (status = 502, description = "the CI refused or is unreachable (the rerun is kept with the reason)", body = ErrorBody)))]
pub async fn create(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<RerunRequest>,
) -> ApiResult<(StatusCode, Json<Rerun>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_rerun).await?;
    let mut audit = serde_json::json!({"launchId": id, "mode": body.mode.as_str()});
    let out = run(&s, &auth, &project, id, &body, &mut audit).await;
    match &out {
        Ok(_) => audit["ok"] = true.into(),
        Err(e) => {
            audit["ok"] = false.into();
            audit["status"] = e.status().as_u16().into();
            audit["error"] = serde_json::json!(e.body().message);
        }
    }
    s.audit(&auth, "launch.rerun", Some(&project.key), audit)
        .await;
    out.map(|r| (StatusCode::CREATED, Json(r)))
}

async fn run(
    s: &AppState,
    auth: &Auth,
    project: &fastproof_store::Project,
    id: Uuid,
    body: &RerunRequest,
    audit: &mut serde_json::Value,
) -> ApiResult<Rerun> {
    let admin = can_see_ci(s, auth, &project.key);
    let launch = s.launch_of(project, id).await?;
    let settings = settings_of(s, project.id).await?;
    let Some(cfg) = settings.rerun else {
        return Err(ApiError::conflict(
            "the project has no rerun settings (integrations: rerun)",
        ));
    };
    audit["target"] = rerun::target(&cfg);
    let mut plan_req = NewTestPlan {
        name: format!("Rerun of #{}", launch.number),
        from_launch_id: None,
        statuses: None,
        test_case_ids: None,
        kind: None,
        description: None,
        case_numbers: None,
        filter: None,
    };
    match body.mode {
        RerunMode::Failed => {
            if body.result_ids.is_some() {
                return Err(ApiError::bad_request("resultIds is for mode selected"));
            }
            plan_req.from_launch_id = Some(launch.id);
            plan_req.statuses = body.statuses.clone();
        }
        RerunMode::Selected => {
            if body.statuses.is_some() {
                return Err(ApiError::bad_request("statuses is for mode failed"));
            }
            let ids = body.result_ids.clone().unwrap_or_default();
            if ids.is_empty() || ids.len() > MAX_RERUN_RESULTS {
                return Err(ApiError::bad_request(format!(
                    "resultIds: 1 to {MAX_RERUN_RESULTS} results"
                )));
            }
            let cases = s.store.result_case_ids(project.id, launch.id, &ids).await?;
            if cases.is_empty() {
                return Err(ApiError::bad_request(
                    "none of the results belongs to this launch",
                ));
            }
            plan_req.test_case_ids = Some(cases);
        }
    }
    let statuses = plan_req.validate().map_err(ApiError::bad_request)?;
    let (rerun_id, external_id) = match s
        .store
        .rerun_begin(
            project.id,
            launch.id,
            &cfg.provider,
            body.mode.as_str(),
            &auth.principal.name,
        )
        .await?
    {
        Ok(v) => v,
        Err(RerunRefusal::UnknownLaunch) => return Err(ApiError::not_found("launch")),
        Err(RerunRefusal::Cooldown) => {
            return Err(ApiError::too_many_requests(
                "this launch was rerun less than 10 seconds ago",
            ));
        }
        Err(RerunRefusal::TooManyForLaunch) => {
            return Err(ApiError::too_many_requests(
                "this launch has been rerun too many times",
            ));
        }
        Err(RerunRefusal::TooManyInHour) => {
            return Err(ApiError::too_many_requests(
                "too many reruns in this project in the last hour",
            ));
        }
    };
    audit["rerunId"] = serde_json::json!(rerun_id);
    let fail = |msg: String| async {
        let _ = s.store.rerun_done(rerun_id, Some(&msg), None).await;
        msg
    };
    let created = match s
        .store
        .create_test_plan(project.id, &plan_req, &statuses, &auth.principal.name)
        .await?
    {
        Ok(c) => c,
        Err(PlanRefusal::UnknownLaunch) => return Err(ApiError::not_found("launch")),
        Err(PlanRefusal::TooManyTests) => {
            return Err(ApiError::conflict(
                fail("the plan would have too many tests".into()).await,
            ));
        }
        Err(PlanRefusal::TooManyPlans) => {
            return Err(ApiError::too_many_requests(
                fail("the project has too many test plans; old plans expire after 90 days".into())
                    .await,
            ));
        }
    };
    audit["planId"] = serde_json::json!(created.id);
    audit["tests"] = serde_json::json!(created.test_count);
    if created.test_count == 0 {
        return Err(ApiError::conflict(
            fail("no tests to run: the launch has no such results".into()).await,
        ));
    }
    s.store
        .rerun_plan(rerun_id, created.id, created.test_count as i32)
        .await?;
    let plan = s
        .store
        .plan_testplan(project.id, created.id)
        .await
        .map_err(super::tm_error)?
        .ok_or_else(ApiError::internal)?;
    let job = Job {
        project: &project.key,
        launch: &launch,
        rerun_id,
        external_id: &external_id,
        plan_id: created.id,
        mode: body.mode.as_str(),
        plan: &plan,
    };
    let outcome = rerun::dispatch(s, project.id, &cfg, &job).await;
    let (error, ci_ref) = match &outcome {
        Ok(r) => (None, r.as_deref()),
        Err(e) => (Some(e.as_str()), None),
    };
    s.store.rerun_done(rerun_id, error, ci_ref).await?;
    if let Some(e) = error {
        audit["detail"] = serde_json::json!(e);
        let shown = if admin {
            e.to_string()
        } else {
            generic_error(e)
        };
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "ci_failed", shown)
            .with_details(serde_json::json!({"rerunId": rerun_id})));
    }
    let mut rerun = s
        .store
        .reruns_of(project.id, launch.id)
        .await?
        .into_iter()
        .find(|r| r.id == rerun_id)
        .ok_or_else(ApiError::internal)?;
    if !admin {
        rerun.ci_ref = rerun.ci_ref.as_deref().map(generic_ci_ref);
    }
    Ok(rerun)
}
