//! Routes of signals about test attempts (tag `signals`, contract §7.7, §8): grids, CI systems
//! and Kubernetes event exporters push what happened to an attempt (a crashed browser session,
//! an evicted pod); the result page lists the signals of a result. Signals never change a
//! defect: they add to suggestions, flaky-cause hints and rule conditions (`match.signals`).

use std::sync::LazyLock;
use std::time::Instant;

use axum::Json;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use chrono::Utc;
use fastproof_analysis::model::{MAX_SIGNALS_BODY_BYTES, MAX_SIGNALS_PER_REQUEST};
use fastproof_store::Project;
use fastproof_store::analysis::types::{AttemptSignals, SignalPush, SignalsAccepted};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{ErrorBody, P, tm_error};
use crate::state::AppState;
use crate::tm_rate::WriteRate;

/// Pushed signals per project and replica (`FASTPROOF_ANALYSIS_SIGNALS_PER_MINUTE`).
static RATE: LazyLock<WriteRate> = LazyLock::new(WriteRate::default);

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(push))
        .layer(DefaultBodyLimit::max(MAX_SIGNALS_BODY_BYTES))
        .merge(OpenApiRouter::new().routes(routes!(of_result)))
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

/// Takes `n` signals of the project's per-minute budget. A push larger than the whole budget
/// takes all of it, so one push of up to 1000 signals still passes when the budget is full.
fn admit(s: &AppState, p: &Project, n: usize) -> ApiResult<()> {
    let per_minute = s.cfg.analysis.limits.signals_per_minute.max(1);
    let n = (n as u32).clamp(1, per_minute);
    RATE.take_n(&p.id.to_string(), per_minute, n, Instant::now())
        .map_err(|wait| {
            metrics::counter!("fastproof_guard_rejections_total", "guard" => "signals")
                .increment(1);
            metrics::counter!("fastproof_analysis_budget_exhausted_total", "kind" => "signals")
                .increment(1);
            let mut e = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!("at most {per_minute} signals a minute per project"),
            );
            if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
                e = e.with_header(RETRY_AFTER, v);
            }
            e
        })
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/signals", tag = "signals",
    operation_id = "signals_push", request_body = SignalPush,
    description = "Pushes signals about test attempts from a browser grid (`source: grid`: `session_crashed`, \
        `browser_oom`, `startup_timeout`, `node_lost`, `evicted`), a CI system (`ci`: `runner_oom`, `pod_evicted`, \
        `job_cancelled`) or Kubernetes (`k8s`: `pod_evicted`, `oom_killed`, `node_not_ready`). Each item names its \
        attempt by `traceId` (32 hex characters, the trace id of the attempt's `traceparent`; optionally with \
        `launchId`) or by `launchId` + `resultClientId` (the client id the reporter gave the attempt). A signal is \
        matched to its attempt at once when the attempt exists, otherwise later (it waits at most two days). \
        1 to 1000 items, body at most 1 MiB, `at` within the last 7 days, `detail` a flat object of strings, \
        numbers and booleans of at most 4 KiB, shown as text; at most 50 signals per attempt; duplicates (same \
        source, kind, `at` and addressing) are refused per item. Refused items are listed with their index and \
        code (`unknown_kind`, `bad_address`, `too_old`, `detail_too_large`, `attempt_full`, `duplicate`). \
        Signals never change a defect. 409 `analysis_disabled` while Analysis is off; 429 `rate_limited` beyond \
        `FASTPROOF_ANALYSIS_SIGNALS_PER_MINUTE` signals a minute per project. Needs `launches:write`.",
    params(("key" = String, Path)),
    responses((status = 200, body = SignalsAccepted), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 413, body = ErrorBody), (status = 429, body = ErrorBody)))]
pub async fn push(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    ApiJson(body): ApiJson<SignalPush>,
) -> ApiResult<Json<SignalsAccepted>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_write).await?;
    require_enabled(&s, &p).await?;
    if body.items.is_empty() || body.items.len() > MAX_SIGNALS_PER_REQUEST {
        return Err(ApiError::bad_request(format!(
            "items must hold 1 to {MAX_SIGNALS_PER_REQUEST} signals"
        )));
    }
    admit(&s, &p, body.items.len())?;
    Ok(Json(
        s.store
            .insert_signals(p.id, &body.items, Utc::now())
            .await
            .map_err(tm_error)?,
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}/signals", tag = "signals",
    operation_id = "signals_of_result",
    description = "The signals of a result's attempts by attempt and time (at most 500): pushed grid, CI and \
        Kubernetes signals and the FastMock signals the analysis wrote (`unmatched`, `stub_error`, `slow`, \
        `unexpected_calls`). `confidence` is `exact` when the signal names the attempt (trace id or client id) and \
        `window` when only the attempt's time window matched. `detail` is data from the sender, shown as text. \
        Needs `launches:read`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = AttemptSignals), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn of_result(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<AttemptSignals>> {
    let p = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    Ok(Json(
        s.store.result_signals(p.id, id).await.map_err(tm_error)?,
    ))
}
