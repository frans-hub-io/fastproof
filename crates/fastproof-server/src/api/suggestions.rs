//! Routes of triage suggestions, memory, backtest and the decision-event export (contract
//! §8 S rows). Suggestions are a transparent nearest-decision search over what people decided
//! before (see `fastproof_analysis::score`), not a trained model.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use fastproof_store::Project;
use fastproof_store::analysis::backtest::MAX_DAYS;
use fastproof_store::analysis::gate;
use fastproof_store::analysis::memory::{MAX_EVENTS_PAGE, hashed_by};
use fastproof_store::analysis::rules::Rule;
use fastproof_store::analysis::tasks::Task;
use fastproof_store::analysis::types::{
    AcceptActor, AnalysisLaunchState, AnalysisLaunchStatus, BacktestReport, BacktestRequest,
    LaunchSuggestions, LaunchSuggestionsState, MemoryList, QueueReason, Suggestion,
    SuggestionAccept, SuggestionAccepted, SuggestionReject,
};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, Q, tm_error};
use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(launch_suggestions))
        .routes(routes!(recompute))
        .routes(routes!(accept))
        .routes(routes!(reject))
        .routes(routes!(rule))
        .routes(routes!(memory))
        .routes(routes!(backtest))
        .routes(routes!(backtest_latest))
        .routes(routes!(decision_events))
}

/// Events of one export page by default.
const DEFAULT_EVENTS_PAGE: i64 = 1000;
/// The header naming the `after` of the next export page (absent on the last page).
pub const NEXT_HEADER: &str = "x-fastproof-next-after";
/// Bytes of one export page, at most (a page then ends early and names the next one).
pub const MAX_EXPORT_BYTES: usize = 8 << 20;

/// One export page: the events as NDJSON with their author hashed, at most `max_bytes` (at
/// least one event), with how many it holds and the `after` of the next page (`None` when
/// `events` held the last ones: fewer than `limit` and none left out).
pub fn ndjson_page(
    events: Vec<(i64, fastproof_store::analysis::types::DecisionEvent)>,
    key: &str,
    limit: i64,
    max_bytes: usize,
) -> anyhow::Result<(String, usize, Option<i64>)> {
    let total = events.len();
    let mut body = String::new();
    let mut last: Option<i64> = None;
    let mut n = 0usize;
    for (id, mut e) in events {
        e.by = hashed_by(key, &e.by);
        let line = serde_json::to_string(&e)?;
        if n > 0 && body.len() + line.len() + 1 > max_bytes {
            break;
        }
        body.push_str(&line);
        body.push('\n');
        last = Some(id);
        n += 1;
    }
    let next = if n < total || total as i64 == limit {
        last
    } else {
        None
    };
    Ok((body, n, next))
}

fn disabled() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "analysis_disabled",
        "FastProof Analysis is off for this instance or project",
    )
}

async fn require_enabled(s: &AppState, p: &Project) -> ApiResult<()> {
    if !s.cfg.analysis.enabled || !s.store.project_settings(p.id).await?.analysis.enabled {
        return Err(disabled());
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/suggestions", tag = "analysis",
    operation_id = "analysis_launch_suggestions",
    description = "Triage suggestions of a launch by cluster (launch × canonical fingerprint): for each cluster its \
        decision state, why it has no suggestion, and up to three suggestions ranked by band (`same`, `high`, \
        `low`) and score. A suggestion is a transparent nearest-decision search over what people decided before \
        (each `why` item is one row of the weight table), not a trained model. `precision` is the band's precision \
        in the latest backtest when it had at least 50 decisions in that band. Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = LaunchSuggestions), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn launch_suggestions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<LaunchSuggestions>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let l = s.launch_of(&p, id).await?;
    if !s.cfg.analysis.enabled {
        return Ok(Json(LaunchSuggestions {
            launch_id: l.id,
            state: LaunchSuggestionsState::Disabled,
            computed_at: None,
            clusters: Vec::new(),
            truncated: false,
        }));
    }
    Ok(Json(
        s.store
            .launch_suggestions(p.id, &l)
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/launches/{id}/suggestions/recompute", tag = "analysis",
    operation_id = "analysis_recompute_suggestions",
    description = "Queues the analysis of a finished launch again (its suggestions are computed anew from the \
        current memory). A launch is recomputed at most once per `FASTPROOF_ANALYSIS_RECOMPUTE_SECONDS` (429 \
        `rate_limited` with `Retry-After`); a launch still running answers 409 `launch_running`. 409 \
        `analysis_disabled` while Analysis is off. Needs `results:triage`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 202, body = AnalysisLaunchStatus), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn recompute(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<(StatusCode, Json<AnalysisLaunchStatus>)> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    let l = s.launch_of(&p, id).await?;
    require_enabled(&s, &p).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let every = std::time::Duration::from_secs(s.cfg.analysis.limits.recompute_seconds);
    let status = s.store.analysis_launch_status(l.id).await?;
    if let Some(st) = &status {
        let recent = st.finished_at.map(|f| chrono::Utc::now() - f);
        if let Some(age) = recent
            && age < chrono::Duration::from_std(every).unwrap_or_default()
            && st.state != AnalysisLaunchState::Queued
        {
            let wait = (chrono::Duration::from_std(every).unwrap_or_default() - age)
                .num_seconds()
                .max(1);
            let mut e = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!("a launch is recomputed at most every {} s", every.as_secs()),
            );
            if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
                e = e.with_header(header::RETRY_AFTER, v);
            }
            return Err(e);
        }
    }
    if !s
        .store
        .enqueue_analysis_launch(p.id, l.id, QueueReason::Recompute, every)
        .await?
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "launch_running",
            "the launch is still running (or merged); it is analysed when it finishes",
        ));
    }
    let st = s
        .store
        .analysis_launch_status(l.id)
        .await?
        .unwrap_or(AnalysisLaunchStatus {
            state: AnalysisLaunchState::Queued,
            queued_at: None,
            finished_at: None,
            steps: Default::default(),
        });
    Ok((StatusCode::ACCEPTED, Json(st)))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/suggestions/{id}/accept", tag = "analysis",
    operation_id = "analysis_accept_suggestion", request_body = SuggestionAccept,
    description = "Accepts an open suggestion: its defect type (with an optional comment and issue) goes to the \
        cluster's results (`applyTo: cluster`, default) or to listed results of the cluster (`applyTo: results`, \
        at most 1000), leaving results a person or an agent already decided untouched. Journaled as one batch \
        with `via: suggestion` (undo takes it back). A suggestion that is not open answers 409 \
        `suggestion_closed {state}`. Needs `results:triage`; an AI agent accepts as `ai`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = SuggestionAccepted), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn accept(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(mut body): ApiJson<SuggestionAccept>,
) -> ApiResult<Json<SuggestionAccepted>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let Some((launch_id, fp, type_id, _)) = s.store.suggestion_target(p.id, id).await? else {
        return Err(ApiError::not_found("suggestion"));
    };
    s.refuse_merged_results(&p, Some(launch_id), body.result_ids.as_deref())
        .await?;
    if let Some(i) = &mut body.issue
        && i.url.is_none()
        && let Some(k) = i.key.as_deref()
    {
        i.url = s.store.project_settings(p.id).await?.issue_url(k);
    }
    let actor = AcceptActor {
        agent: crate::mcp_tm::is_agent(&s, &auth, &p.key),
        can_mute: auth.allowed(&s.perms.results_mute, Some(&p.key)),
    };
    let band = s.store.suggestion(p.id, id).await?.map(|x| x.band);
    let out = s
        .store
        .accept_suggestion(p.id, &p.key, id, &body, &auth.principal.name, &actor)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "suggestion.accept",
        Some(&p.key),
        serde_json::json!({"suggestionId": id, "launchId": launch_id, "fingerprint": fp, "band": band,
            "defectTypeId": type_id, "applyTo": body.apply_to, "updated": out.updated, "batchId": out.batch_id,
            "source": if actor.agent { "ai" } else { "user" }}),
    )
    .await;
    Ok(Json(out))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/suggestions/{id}/reject", tag = "analysis",
    operation_id = "analysis_reject_suggestion", request_body = SuggestionReject,
    description = "Rejects an open suggestion with an optional reason (at most 500 bytes). The memory is not \
        changed: only a person's different decision counts against a remembered one. Needs `results:triage`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = Suggestion), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 429, body = ErrorBody)))]
pub async fn reject(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    ApiJson(body): ApiJson<SuggestionReject>,
) -> ApiResult<Json<Suggestion>> {
    let p = s.project_for(&auth, &key, &s.perms.results_triage).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let out = s
        .store
        .reject_suggestion(p.id, id, &body, &auth.principal.name)
        .await
        .map_err(tm_error)?;
    s.audit(
        &auth,
        "suggestion.reject",
        Some(&p.key),
        serde_json::json!({"suggestionId": id, "reason": body.reason}),
    )
    .await;
    Ok(Json(out))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/suggestions/{id}/rule", tag = "analysis",
    operation_id = "analysis_suggestion_rule",
    description = "Makes a disabled rule from a suggestion for review: `{name: \"Same as <fingerprint>\", match: \
        {fingerprint}, action: {defectTypeId}}`. Needs `rules:write` or `ai:propose` (an agent has at most 20 \
        disabled rules waiting for review: 409 `limit_reached`).",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 201, body = Rule), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn rule(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<(StatusCode, Json<Rule>)> {
    let perm = if auth.allowed(&s.perms.rules_write, Some(&key)) {
        &s.perms.rules_write
    } else if auth.allowed(&s.perms.ai_propose, Some(&key)) {
        &s.perms.ai_propose
    } else {
        &s.perms.rules_write
    };
    let p = s.project_for(&auth, &key, perm).await?;
    crate::tm_rate::admit(&s, &auth)?;
    let Some((_, fp, type_id, _)) = s.store.suggestion_target(p.id, id).await? else {
        return Err(ApiError::not_found("suggestion"));
    };
    if !auth.allowed(&s.perms.rules_write, Some(&p.key)) {
        let max = fastproof_store::tm::ai::MAX_RULE_DRAFTS;
        if s.store
            .disabled_rules_by(p.id, &auth.principal.name)
            .await?
            >= max
        {
            return Err(tm_error(fastproof_store::tm::TmError::limit(
                "maxRuleDrafts",
                max as u64,
            )));
        }
    }
    let draft = serde_json::json!({"name": format!("Same as {fp}"), "match": {"fingerprint": fp},
        "action": {"defectTypeId": type_id}});
    let r = super::analysis::create_rule_draft(&s, &p, &auth.principal.name, draft).await?;
    s.audit(
        &auth,
        "suggestion.rule",
        Some(&p.key),
        serde_json::json!({"suggestionId": id, "ruleId": r.id}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(r)))
}

/// Query of the memory of a fingerprint.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct MemoryQuery {
    /// A fingerprint (16 hex digits); its canonical fingerprint and aliases are included.
    pub fingerprint: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analysis/memory", tag = "analysis",
    operation_id = "analysis_memory",
    description = "What people decided for a fingerprint (its canonical fingerprint and aliases): one entry per \
        defect type with confirmations, overrides, the last comment and issue, the tests, branches and \
        environments and a sample decision. Needs `launches:read`.",
    params(("key" = String, Path), MemoryQuery),
    responses((status = 200, body = MemoryList), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn memory(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<MemoryQuery>,
) -> ApiResult<Json<MemoryList>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if !fastproof_store::analysis::rules::valid_fingerprint(&q.fingerprint) {
        return Err(ApiError::bad_request("fingerprint must be 16 hex digits"));
    }
    Ok(Json(MemoryList {
        items: s.store.memory_entries(p.id, &q.fingerprint).await?,
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/analysis/backtest", tag = "analysis",
    operation_id = "analysis_backtest", request_body = BacktestRequest,
    description = "Queues a backtest of the project's decision history of the last `days` (1 to 730, default \
        180): every person's decision is replayed in time order against the memory folded from the decisions \
        before it, with the live scoring, and the report gives the precision per band and the coverage \
        (`GET .../analysis/backtest/latest`). Answers the task (202). A backtest replays for at most 10 minutes \
        (the report is then `truncated`). 409 `analysis_disabled` while Analysis is off, 409 \
        `upgrade_in_progress` while older replicas run, 409 `limit_reached` while a backtest of the project is \
        queued or running, or with too many open tasks. Needs `settings:write`.",
    params(("key" = String, Path)),
    responses((status = 202, body = Task), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody),
        (status = 503, body = ErrorBody)))]
pub async fn backtest(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<BacktestRequest>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if body.days.is_some_and(|d| !(1..=MAX_DAYS).contains(&d)) {
        return Err(ApiError::bad_request(format!(
            "days must be 1..={MAX_DAYS}"
        )));
    }
    require_enabled(&s, &p).await?;
    {
        let mut conn = s
            .store
            .pool()
            .acquire()
            .await
            .map_err(anyhow::Error::from)?;
        gate::require(&mut conn, &s.cfg.analysis.limits, "backtest")
            .await
            .map_err(tm_error)?;
    }
    crate::tm_rate::admit(&s, &auth)?;
    let _permit = s.guards.heavy(&auth).await?;
    let max = fastproof_store::analysis::backtest::MAX_OPEN_BACKTESTS;
    if s.store.open_backtests(p.id).await? >= max {
        return Err(tm_error(fastproof_store::tm::TmError::limit(
            "openBacktests",
            max as u64,
        )));
    }
    let days = body
        .days
        .unwrap_or(fastproof_store::analysis::backtest::DEFAULT_DAYS);
    let Some(task) = s
        .store
        .enqueue_task(
            p.id,
            "backtest",
            serde_json::json!({"days": days}),
            &auth.principal.name,
        )
        .await?
    else {
        return Err(tm_error(fastproof_store::tm::TmError::limit(
            "openTasks",
            fastproof_store::analysis::tasks::MAX_OPEN_TASKS as u64,
        )));
    };
    s.audit(
        &auth,
        "analysis.backtest",
        Some(&p.key),
        serde_json::json!({"taskId": task.id, "days": days}),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analysis/backtest/latest", tag = "analysis",
    operation_id = "analysis_backtest_latest",
    description = "The newest backtest report of the project (404 without one). Needs `projects:read`.",
    params(("key" = String, Path)),
    responses((status = 200, body = BacktestReport), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn backtest_latest(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<BacktestReport>> {
    let p = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    s.store
        .latest_backtest(p.id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("backtest"))
}

/// Query of the decision-event export.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// The `x-fastproof-next-after` header of the previous page.
    pub after: Option<i64>,
    /// 1 to 10000, default 1000.
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/analysis/decision-events", tag = "analysis",
    operation_id = "analysis_decision_events",
    description = "Exports the project's decision events (what the memory learned, one JSON `DecisionEvent` per \
        line) for an offline backtest (`fastproof analysis backtest --events <file>`). The author is the first 16 \
        hex digits of sha256 of a random secret of the project and the author (stable within the project, not \
        testable without the server's database). A page holds at most `limit` events and 8 MiB; when more are \
        left it carries the `after` of the next one in `x-fastproof-next-after`. 409 `analysis_disabled` while Analysis is off. Needs `settings:write`.",
    params(("key" = String, Path), EventsQuery),
    responses((status = 200, content_type = "application/x-ndjson", body = String,
        description = "One DecisionEvent per line"),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn decision_events(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<EventsQuery>,
) -> ApiResult<Response> {
    let p = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    let limit = q.limit.unwrap_or(DEFAULT_EVENTS_PAGE);
    if !(1..=MAX_EVENTS_PAGE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be 1..={MAX_EVENTS_PAGE}"
        )));
    }
    if q.after.is_some_and(|a| a < 0) {
        return Err(ApiError::bad_request("after must not be negative"));
    }
    require_enabled(&s, &p).await?;
    let _permit = s.guards.heavy(&auth).await?;
    let events = s.store.decision_events(p.id, q.after, limit).await?;
    let key = s.store.export_key(p.id).await?;
    let (body, count, next) = ndjson_page(events, &key, limit, MAX_EXPORT_BYTES)?;
    s.audit(
        &auth,
        "analysis.events.export",
        Some(&p.key),
        serde_json::json!({"after": q.after, "count": count}),
    )
    .await;
    let mut r = Body::from(body).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    if let Some(n) = next
        && let Ok(v) = HeaderValue::from_str(&n.to_string())
    {
        h.insert(NEXT_HEADER, v);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastproof_store::analysis::types::DecisionEvent;

    fn ev(by: &str) -> DecisionEvent {
        DecisionEvent {
            v: 1,
            at: chrono::Utc::now(),
            fingerprint: "aaaaaaaaaaaaaaaa".into(),
            message_norm: "x".repeat(100),
            frames: Vec::new(),
            test_case_id: None,
            env_key: None,
            branch: None,
            defect_type_id: Uuid::nil(),
            group: "productBug".into(),
            previous_type_id: None,
            by: by.into(),
            results: 1,
        }
    }

    #[test]
    fn an_export_page_has_a_byte_budget_and_names_the_next_page() {
        let key = "0123456789abcdef0123456789abcdef";
        let evs: Vec<(i64, DecisionEvent)> = (1..=10).map(|i| (i, ev("ann"))).collect();
        let (body, n, next) = ndjson_page(evs.clone(), key, 1000, usize::MAX).unwrap();
        assert_eq!((n, next), (10, None), "the last page");
        assert!(!body.contains("ann"));
        assert!(body.contains(&hashed_by(key, "ann")));
        let (_, n, next) = ndjson_page(evs.clone(), key, 10, usize::MAX).unwrap();
        assert_eq!((n, next), (10, Some(10)), "a full page");
        let line = body.lines().next().unwrap().len() + 1;
        let (body, n, next) = ndjson_page(evs.clone(), key, 1000, line * 3 + 1).unwrap();
        assert_eq!(
            (n, next),
            (3, Some(3)),
            "cut by bytes, the rest on the next page"
        );
        assert!(body.len() <= line * 3 + 1);
        let (_, n, _) = ndjson_page(evs, key, 1000, 1).unwrap();
        assert_eq!(n, 1, "one event at least");
    }
}
