//! Gates, project secrets, notification deliveries and FastMock correlation.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use fastproof_store::Page;
use fastproof_store::integrations::{DeliveryView, SecretMeta};
use franshub_auth::Auth;
use franshub_http::{ApiError, ApiJson, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{ErrorBody, P, Q, page_limit};
use crate::integrations::dispatch;
use crate::integrations::fastmock::{self, MockInteractions};
use crate::integrations::gates::{self, GateReport};
use crate::integrations::notify::{self, Outcome};
use crate::integrations::secrets::{
    MAX_SECRET_BYTES, SecretPurpose, check_teams_url, normalize_origin, origin_of, valid_name,
};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/gate", tag = "integrations", operation_id = "integrations_gate",
    description = "Evaluates the project's quality gates for the launch now. `pending` while the launch runs or has unprocessed uploads; `failed` when a rule of an enabled gate that applies to the launch branch does not hold. A rule holds when `actual op value`.",
    params(("key" = String, Path), ("id" = Uuid, Path)),
    responses((status = 200, body = GateReport), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, description = "the gate settings are invalid", body = ErrorBody)))]
pub async fn gate(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
) -> ApiResult<Json<GateReport>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    let launch = s.launch_of(&project, id).await?;
    let doc = s.store.project_settings_json(project.id).await?;
    let settings = IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))?;
    let ev = gates::evaluate_launch(&s, &project, &launch, &settings.gates).await?;
    Ok(Json(ev.report))
}

/// Names of the project's secrets.
#[derive(Serialize, ToSchema)]
pub struct SecretList {
    pub items: Vec<SecretMeta>,
}

/// Body of `PUT .../secrets/{name}`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretValue {
    /// At most 8 KiB. Never returned by the API.
    pub value: String,
    /// What the secret is for; it is refused for anything else.
    pub purpose: SecretPurpose,
    /// The only origin (`https://host[:port]`) the secret may be sent to. Required for `vcs`
    /// (the GitLab base URL or GitHub API URL) and `fastmock` (the admin URL); taken from the
    /// URL for `teams` (the value is the workflow URL, which must be `https`); optional for the
    /// others (for `slack` and a webhook URL it must then be the origin of the URL itself).
    /// Changing the URL in the settings needs the secret written again for the new origin.
    #[serde(default)]
    pub origin: Option<String>,
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/secrets", tag = "integrations", operation_id = "integrations_secrets",
    description = "Names, purposes and origins of the project's secrets; values are write-only.",
    params(("key" = String, Path)),
    responses((status = 200, body = SecretList), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn secrets(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
) -> ApiResult<Json<SecretList>> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    Ok(Json(SecretList {
        items: s.store.secret_names(project.id).await?,
    }))
}

#[utoipa::path(put, path = "/api/v1/projects/{key}/secrets/{name}", tag = "integrations", operation_id = "integrations_put_secret", request_body = SecretValue,
    description = "Creates or replaces a secret, encrypted at rest with `FASTPROOF_SECRETS_KEY_FILE` and bound to a purpose and, optionally, the origin it may be sent to. Settings refer to it by name (`secretRef`, `targetSecretRef`, `tokenSecretRef`); a reference with another purpose, or a URL on another origin, is refused.",
    params(("key" = String, Path), ("name" = String, Path)),
    responses((status = 201, description = "created"), (status = 204, description = "replaced"),
        (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 503, description = "FASTPROOF_SECRETS_KEY_FILE is not set", body = ErrorBody)))]
pub async fn put_secret(
    State(s): State<AppState>,
    auth: Auth,
    P((key, name)): P<(String, String)>,
    ApiJson(body): ApiJson<SecretValue>,
) -> ApiResult<StatusCode> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if !valid_name(&name) {
        return Err(ApiError::bad_request(
            "name must match ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$",
        ));
    }
    if body.value.is_empty() || body.value.len() > MAX_SECRET_BYTES || body.value.contains('\0') {
        return Err(ApiError::bad_request(format!(
            "value must have 1 to {MAX_SECRET_BYTES} bytes"
        )));
    }
    if body.purpose == SecretPurpose::TrackerHook && body.value.len() < 16 {
        return Err(ApiError::bad_request(
            "a tracker-webhook secret has at least 16 bytes",
        ));
    }
    let mut origin = body
        .origin
        .as_deref()
        .map(normalize_origin)
        .transpose()
        .map_err(ApiError::bad_request)?;
    if body.purpose == SecretPurpose::Teams {
        check_teams_url(&body.value).map_err(ApiError::bad_request)?;
        let o = origin_of(&body.value)
            .ok_or_else(|| ApiError::bad_request("a teams secret is the workflow URL"))?;
        if origin.as_ref().is_some_and(|x| *x != o) {
            return Err(ApiError::bad_request(
                "origin is not the origin of the URL in value",
            ));
        }
        origin = Some(o);
    }
    if body.purpose.needs_origin() && origin.is_none() {
        return Err(ApiError::bad_request(format!(
            "a {} secret needs origin, the URL it is sent to",
            body.purpose.as_str()
        )));
    }
    if body.purpose == SecretPurpose::Slack {
        let Some(o) = origin_of(&body.value) else {
            return Err(ApiError::bad_request(
                "a slack secret is the incoming webhook URL",
            ));
        };
        if origin.as_ref().is_some_and(|x| *x != o) {
            return Err(ApiError::bad_request(
                "origin is not the origin of the URL in value",
            ));
        }
    }
    let Some(keys) = &s.cfg.integrations.secrets else {
        return Err(ApiError::unavailable(
            "secrets need FASTPROOF_SECRETS_KEY_FILE on the server",
        ));
    };
    let sealed = keys
        .seal(
            project.id,
            &name,
            Some(body.purpose.as_str()),
            origin.as_deref(),
            &body.value,
        )
        .map_err(|_| ApiError::internal())?;
    let created = s
        .store
        .put_secret(project.id, &name, &sealed, &auth.principal.name)
        .await?;
    s.audit(
        &auth,
        if created {
            "secret.create"
        } else {
            "secret.update"
        },
        Some(&project.key),
        serde_json::json!({"name": name, "keyId": sealed.key_id, "purpose": body.purpose.as_str(),
            "origin": origin}),
    )
    .await;
    Ok(if created {
        StatusCode::CREATED
    } else {
        StatusCode::NO_CONTENT
    })
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/secrets/{name}", tag = "integrations", operation_id = "integrations_delete_secret",
    params(("key" = String, Path), ("name" = String, Path)),
    responses((status = 204), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn delete_secret(
    State(s): State<AppState>,
    auth: Auth,
    P((key, name)): P<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if !valid_name(&name) || !s.store.delete_secret(project.id, &name).await? {
        return Err(ApiError::not_found("secret"));
    }
    s.audit(
        &auth,
        "secret.delete",
        Some(&project.key),
        serde_json::json!({"name": name}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Query of the delivery list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryQuery {
    /// `pending`, `sending`, `delivered` or `failed`.
    pub status: Option<String>,
    #[serde(rename = "launchId")]
    pub launch_id: Option<Uuid>,
    /// Cursor from `next`.
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/notifications/deliveries", tag = "integrations", operation_id = "integrations_deliveries",
    description = "Notification and VCS deliveries, newest first (without targets or payloads).",
    params(("key" = String, Path), DeliveryQuery),
    responses((status = 200, body = Page<DeliveryView>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
pub async fn deliveries(
    State(s): State<AppState>,
    auth: Auth,
    P(key): P<String>,
    Q(q): Q<DeliveryQuery>,
) -> ApiResult<Json<Page<DeliveryView>>> {
    let project = s.project_for(&auth, &key, &s.perms.projects_read).await?;
    if let Some(st) = &q.status
        && !matches!(st.as_str(), "pending" | "sending" | "delivered" | "failed")
    {
        return Err(ApiError::bad_request(
            "status must be pending, sending, delivered or failed",
        ));
    }
    Ok(Json(
        s.store
            .deliveries(
                project.id,
                q.status.as_deref(),
                q.launch_id,
                q.before,
                page_limit(q.limit)?,
            )
            .await?,
    ))
}

/// The result of a test notification.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationTestResult {
    /// Whether the receiver accepted the message (`2xx`, or the SMTP relay took it).
    pub delivered: bool,
    /// The HTTP status (or SMTP code) of the answer, when there was one.
    pub response_status: Option<u16>,
    /// Why it was not delivered; never contains the target URL.
    pub error: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/projects/{key}/notifications/{id}/test", tag = "integrations", operation_id = "integrations_test_notification",
    description = "Sends a sample message (a made-up launch with a failed gate and new failures, marked as a test) to one saved notification of the project, once and without retries, and returns how the receiver answered. Every kind can be tested. At most one test per notification every 5 seconds.",
    params(("key" = String, Path), ("id" = String, Path, description = "The notification id")),
    responses((status = 200, body = NotificationTestResult), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, description = "the project settings are invalid", body = ErrorBody),
        (status = 429, description = "a test of this notification was sent less than 5 seconds ago", body = ErrorBody)))]
pub async fn test_notification(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, String)>,
) -> ApiResult<Json<NotificationTestResult>> {
    let project = s.project_for(&auth, &key, &s.perms.settings_write).await?;
    if !valid_name(&id) {
        return Err(ApiError::not_found("notification"));
    }
    let doc = s.store.project_settings_json(project.id).await?;
    let settings = IntegrationSettings::parse(&doc)
        .map_err(|e| ApiError::conflict(format!("invalid project settings: {e}")))?;
    let Some(n) = settings.notifications.iter().find(|n| n.id == id) else {
        return Err(ApiError::not_found("notification"));
    };
    if !notify::test_allowed(project.id, &n.id) {
        return Err(ApiError::too_many_requests(
            "a test of this notification was sent less than 5 seconds ago",
        ));
    }
    let sample = notify::sample_summary(
        &project.key,
        dispatch::page_url(&s, &format!("p/{}", project.key)),
        n,
    );
    let out = match notify::send(&s, project.id, Uuid::now_v7(), n, &sample).await {
        Outcome::Delivered(c) => NotificationTestResult {
            delivered: true,
            response_status: c,
            error: None,
        },
        Outcome::Retry(e, c) | Outcome::Throttled(e, c, _) | Outcome::Fail(e, c) => {
            NotificationTestResult {
                delivered: false,
                response_status: c,
                error: Some(e),
            }
        }
    };
    s.audit(
        &auth,
        "notification.test",
        Some(&project.key),
        serde_json::json!({"id": n.id, "kind": n.kind, "delivered": out.delivered,
            "responseStatus": out.response_status}),
    )
    .await;
    Ok(Json(out))
}

/// Query of the mock interactions.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MockQuery {
    /// Attempt number, 1-based (default: the last attempt).
    pub attempt: Option<i32>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}/mock-interactions", tag = "integrations", operation_id = "integrations_mock_interactions",
    description = "Requests the project's FastMock answered during an attempt: by the attempt's trace id, else by its time window (and the `fastmock.space` label or launch attribute).",
    params(("key" = String, Path), ("id" = Uuid, Path), MockQuery),
    responses((status = 200, body = MockInteractions), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, description = "FastMock is not configured", body = ErrorBody),
        (status = 502, description = "FastMock is unreachable or refused", body = ErrorBody)))]
pub async fn mock_interactions(
    State(s): State<AppState>,
    auth: Auth,
    P((key, id)): P<(String, Uuid)>,
    Q(q): Q<MockQuery>,
) -> ApiResult<Json<MockInteractions>> {
    let project = s.project_for(&auth, &key, &s.perms.launches_read).await?;
    if let Some(a) = q.attempt
        && !(1..=1000).contains(&a)
    {
        return Err(ApiError::bad_request("attempt must be 1 to 1000"));
    }
    Ok(Json(
        fastmock::interactions(&s, &project, id, q.attempt).await?,
    ))
}
