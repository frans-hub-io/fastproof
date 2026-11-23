//! Launch merges: `POST /launches/merge`, `GET /merges/{id}`, `POST /merges/{id}/undo`,
//! `GET /launches/{id}/merges`.
//!
//! Permissions: `launches:write` merges launches the caller created itself (a CI token merging
//! its shards); merging launches of others also needs `results:triage`; a merge without undo
//! (`final`) needs `settings:write`, as deleting a launch does. Undo needs `launches:write`
//! and either `results:triage` or being the one who merged. "Created by the caller" compares
//! the stable caller key (`token:<id>` for managed tokens, else `<kind>:<name>`), never names
//! alone. Merges, undos and previews count as heavy requests of the caller.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::merges::{
    LaunchMerge, LaunchMergeRequest, MergeCaller, MergeError, MergeOptions,
};
use fastproof_store::{Launch, Project};
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P};
use crate::limits::caller_key;
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(merge))
        .routes(routes!(get_merge))
        .routes(routes!(undo))
        .routes(routes!(launch_merges))
}

/// Merges of a launch.
#[derive(Serialize, ToSchema)]
pub struct MergeList {
    pub items: Vec<LaunchMerge>,
}

/// The answer to a refused merge or undo.
pub fn merge_error(e: MergeError) -> ApiError {
    match e {
        MergeError::NotFound(what) => ApiError::not_found(what),
        MergeError::Invalid(m) => ApiError::bad_request(m),
        MergeError::Conflict {
            code,
            message,
            details,
        } => ApiError::new(StatusCode::CONFLICT, code, message).with_details(details),
        MergeError::Forbidden { code, message } => {
            ApiError::new(StatusCode::FORBIDDEN, code, message)
        }
        MergeError::TooLarge { what, value, limit } => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "merge_too_large",
            format!("the launches hold {value} {what}; a merge takes at most {limit}"),
        )
        .with_details(serde_json::json!({"limit": what, "value": value, "max": limit})),
        MergeError::Other(e) => ApiError::from(e),
    }
}

impl AppState {
    /// Refuses writes into a launch that was merged into another one (409 `launch_merged`,
    /// `details.mergedInto` names the launch to write to instead).
    pub fn refuse_merged(&self, launch: &Launch) -> ApiResult<()> {
        match launch.merged_into {
            Some(into) => Err(ApiError::new(
                StatusCode::CONFLICT,
                "launch_merged",
                "the launch was merged into another launch; write to that one",
            )
            .with_details(serde_json::json!({"mergedInto": into}))),
            None => Ok(()),
        }
    }

    /// [`AppState::refuse_merged`] for a triage request (a launch, or results by id).
    pub async fn refuse_merged_results(
        &self,
        project: &Project,
        launch: Option<Uuid>,
        results: Option<&[Uuid]>,
    ) -> ApiResult<()> {
        if let Some(l) = launch
            && let Some(l) = self.store.launch(project.id, l).await?
        {
            self.refuse_merged(&l)?;
        }
        if let Some(ids) = results
            && !ids.is_empty()
            && self
                .store
                .results_in_merged_launches(project.id, ids)
                .await?
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "launch_merged",
                "some of the results belong to a launch merged into another launch; triage them there",
            ));
        }
        Ok(())
    }

    fn merge_options(&self) -> MergeOptions {
        MergeOptions {
            undo_window: chrono::Duration::hours(self.cfg.merge_undo_hours),
            max_attempts: self.cfg.merge_max_attempts,
            max_log_bytes: self.cfg.log_limits.max_bytes_per_launch,
        }
    }

    fn merge_caller(&self, auth: &Auth, project: &Project) -> MergeCaller {
        MergeCaller {
            name: auth.principal.name.clone(),
            key: caller_key(auth),
            can_sync: auth
                .principal
                .denial(&self.perms.cases_sync, Some(&project.key))
                .is_none(),
            can_triage: auth
                .principal
                .denial(&self.perms.results_triage, Some(&project.key))
                .is_none(),
        }
    }
}

/// `results:triage` unless the caller created every launch (by stable key).
async fn check_others(
    s: &AppState,
    auth: &Auth,
    project: &Project,
    caller: &MergeCaller,
    ids: &[Uuid],
) -> ApiResult<()> {
    if caller.can_triage {
        return Ok(());
    }
    let owners = s.store.launch_owner_keys(project.id, ids).await?;
    if owners
        .values()
        .all(|k| k.as_deref() == Some(caller.key.as_str()))
    {
        return Ok(());
    }
    require(auth, &s.perms.results_triage, Some(&project.key))?;
    Ok(())
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/merge", tag = "launches", operation_id = "launches_merge",
    description = "Merges launches (CI shards, re-runs) into a new launch or into one of them (`into`). A test run \
        in several of them becomes one result with more attempts (the latest is the result, mixed outcomes are flaky \
        in the run); counters, transitions, flakiness, defects and the series statistics are recomputed (the \
        analysis may finish shortly after the answer: `analysing`). The other launches are hidden (`mergedInto`) and \
        refuse further writes with 409 `launch_merged`; they come back on undo, possible until `undoUntil` \
        (`FASTPROOF_MERGE_UNDO_HOURS`), and are deleted afterwards (a merge with `final` keeps them a couple of \
        minutes for writes on their way). Repeating the same merge returns it (200) and copies what reached the \
        launches since. Launches to merge must be finished with no upload in progress (409 `launch_running`, \
        `launch_busy`); the target may run, but a finished target takes the merge only from its creator or a \
        caller with `cases:sync` (409 `launch_finished`). Also 409 `launch_merged`, `launch_manual`, \
        `launch_plan`, `merge_mixed` (debug or branch differ), `launch_rolled_up`, `merge_busy` (another merge \
        or undo of the project runs); 413 `merge_too_large` (attempts or log bytes). `dryRun` answers counts \
        (status `preview`) without merging and without locks.",
    request_body = LaunchMergeRequest,
    params(("key" = String, Path)),
    responses((status = 201, description = "merged", body = LaunchMerge),
        (status = 200, description = "a preview, or the same merge made before", body = LaunchMerge),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn merge(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<LaunchMergeRequest>,
) -> ApiResult<(StatusCode, Json<LaunchMerge>)> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    fastproof_store::merges::validate(&body).map_err(merge_error)?;
    if (body.final_merge || s.cfg.merge_undo_hours == 0) && !body.dry_run {
        require(&auth, &s.perms.settings_write, Some(&project.key))?;
    }
    let caller = s.merge_caller(&auth, &project);
    check_others(&s, &auth, &project, &caller, &body.launch_ids).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let mut req = body;
    if s.cfg.merge_undo_hours == 0 {
        req.final_merge = true;
    }
    let (m, created) = s
        .store
        .merge_launches(&project, &req, &caller, s.merge_options())
        .await
        .map_err(merge_error)?;
    if created {
        s.audit(
            &auth,
            "launch.merge",
            Some(&project.key),
            serde_json::json!({"mergeId": m.id, "targetId": m.target_id, "targetNumber": m.target_number,
                "createdTarget": m.created_target, "sources": m.sources.iter().map(|x| x.id).collect::<Vec<_>>(),
                "final": m.final_merge, "counts": m.counts}),
        )
        .await;
    }
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(m),
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/merges/{id}", tag = "launches", operation_id = "merges_get",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = LaunchMerge), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn get_merge(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<LaunchMerge>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let m = s
        .store
        .merge(project.id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("merge"))?;
    Ok(Json(m))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/merges", tag = "launches", operation_id = "launches_merges",
    description = "The merges the launch took part in, as the target or as a source, newest first (at most 50).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = MergeList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn launch_merges(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<MergeList>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    s.launch_of(&project, id).await?;
    Ok(Json(MergeList {
        items: s.store.launch_merges(project.id, id).await?,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/merges/{id}/undo", tag = "launches", operation_id = "merges_undo",
    description = "Undoes a merge before `undoUntil`: a launch the merge created is deleted, or the copied attempts \
        leave the launch merged into; the merged launches come back. 409 `merge_not_active`, `merge_final`, \
        `merge_expired`, `merge_target_changed` (results or logs were added to a created launch: delete it \
        instead), `merge_target_merged`, `merge_target_rolled_up`, `merge_target_gone`, `merge_source_gone`, \
        `merge_busy`; 403 `merge_target_triaged` when people triaged the created launch and the caller lacks \
        `results:triage`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = LaunchMerge), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn undo(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<LaunchMerge>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    let caller = s.merge_caller(&auth, &project);
    let owner = s.store.merge_owner(project.id, id).await?;
    if s.store.merge(project.id, id).await?.is_none() {
        return Err(ApiError::not_found("merge"));
    }
    if owner.as_deref() != Some(caller.key.as_str()) {
        require(&auth, &s.perms.results_triage, Some(&project.key))?;
    }
    let _permit = s.guards.heavy(&auth).await?;
    let m = s
        .store
        .undo_merge(&project, id, &caller)
        .await
        .map_err(merge_error)?;
    s.audit(
        &auth,
        "launch.merge.undo",
        Some(&project.key),
        serde_json::json!({"mergeId": id, "targetId": m.target_id, "createdTarget": m.created_target,
            "sources": m.sources.iter().map(|x| x.id).collect::<Vec<_>>()}),
    )
    .await;
    Ok(Json(m))
}
