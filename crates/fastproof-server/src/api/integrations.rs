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
use crate::integrations::fastmock::{self, MockInteractions};
use crate::integrations::gates::{self, GateReport};
use crate::integrations::secrets::{MAX_SECRET_BYTES, valid_name};
use crate::integrations::settings::IntegrationSettings;
use crate::state::AppState;

#[utoipa::path(get, path = "/api/v1/projects/{key}/launches/{id}/gate", tag = "integrations",
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
    let doc = s.store.project_settings(project.id).await?;
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
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/secrets", tag = "integrations",
    description = "Names of the project's secrets; values are write-only.",
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

#[utoipa::path(put, path = "/api/v1/projects/{key}/secrets/{name}", tag = "integrations", request_body = SecretValue,
    description = "Creates or replaces a secret, encrypted at rest with `FASTPROOF_SECRETS_KEY_FILE`. Settings refer to it by name (`secretRef`, `tokenSecretRef`).",
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
    let Some(keys) = &s.cfg.integrations.secrets else {
        return Err(ApiError::unavailable(
            "secrets need FASTPROOF_SECRETS_KEY_FILE on the server",
        ));
    };
    let sealed = keys
        .seal(project.id, &name, &body.value)
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
        serde_json::json!({"name": name, "keyId": sealed.key_id}),
    )
    .await;
    Ok(if created {
        StatusCode::CREATED
    } else {
        StatusCode::NO_CONTENT
    })
}

#[utoipa::path(delete, path = "/api/v1/projects/{key}/secrets/{name}", tag = "integrations",
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

#[utoipa::path(get, path = "/api/v1/projects/{key}/notifications/deliveries", tag = "integrations",
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

/// Query of the mock interactions.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MockQuery {
    /// Attempt number, 1-based (default: the last attempt).
    pub attempt: Option<i32>,
}

#[utoipa::path(get, path = "/api/v1/projects/{key}/results/{id}/mock-interactions", tag = "integrations",
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
