//! Global administration: the audit log and the scheduler's jobs.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Utc};
use fastproof_store::ScheduledJob;
use franshub_access::AuditFilter;
use franshub_auth::{Auth, require};
use franshub_http::{ApiError, ApiResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{ErrorBody, Q};
use crate::state::AppState;

/// Query of the audit log.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// An action or a `prefix*`.
    pub action: Option<String>,
    pub principal: Option<String>,
    /// A project key; with it, `audit:read` in that project is enough.
    pub scope: Option<String>,
    pub before: Option<String>,
    pub limit: Option<u32>,
}

/// An audit record.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditItem {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub principal: String,
    pub principal_kind: Option<String>,
    pub roles: String,
    pub scope: Option<String>,
    pub action: String,
    pub detail: serde_json::Value,
}

/// A page of audit records, newest first.
#[derive(Serialize, ToSchema)]
pub struct AuditList {
    pub items: Vec<AuditItem>,
    pub next: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/audit", tag = "admin", operation_id = "admin_audit", params(AuditQuery),
    responses((status = 200, body = AuditList), (status = 403, body = ErrorBody)))]
pub async fn audit(
    State(s): State<AppState>,
    auth: Auth,
    Q(q): Q<AuditQuery>,
) -> ApiResult<Json<AuditList>> {
    require(&auth, &s.perms.audit_read, q.scope.as_deref())?;
    let Some(store) = s.platform.audit_store() else {
        return Ok(Json(AuditList {
            items: Vec::new(),
            next: None,
        }));
    };
    let before = match q.before.as_deref() {
        None => None,
        Some(b) => Some(
            b.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid cursor"))?,
        ),
    };
    if q.limit.is_some_and(|l| l == 0 || l > 500) {
        return Err(ApiError::bad_request("limit must be 1..=500"));
    }
    // A caller with audit:read in one project sees that project's records only, not those of
    // an earlier, deleted project with the same key.
    let since = match q.scope.as_deref() {
        Some(k) if !auth.allowed(&s.perms.audit_read, None) => match s.store.project(k).await? {
            Some(p) => Some(p.created_at),
            None => return Err(ApiError::not_found("project")),
        },
        _ => None,
    };
    let f = AuditFilter {
        action: q.action,
        principal: q.principal,
        scope: q.scope,
        since,
        before,
        limit: Some(q.limit.unwrap_or(50)),
        ..AuditFilter::default()
    };
    let page = store.query(&f).await?;
    Ok(Json(AuditList {
        next: page.next_before.map(|n| n.to_string()),
        items: page
            .events
            .into_iter()
            .map(|e| AuditItem {
                id: e.id,
                at: e.at,
                principal: e.principal,
                principal_kind: e.principal_kind,
                roles: e.roles,
                scope: e.scope,
                action: e.action,
                detail: e.detail,
            })
            .collect(),
    }))
}

/// Periodic jobs of the scheduler.
#[derive(Serialize, ToSchema)]
pub struct JobList {
    pub items: Vec<ScheduledJob>,
}

#[utoipa::path(get, path = "/api/v1/admin/jobs", tag = "admin", operation_id = "admin_jobs",
    responses((status = 200, body = JobList), (status = 403, body = ErrorBody)))]
pub async fn jobs(State(s): State<AppState>, auth: Auth) -> ApiResult<Json<JobList>> {
    require(&auth, &s.perms.settings_write, None)?;
    Ok(Json(JobList {
        items: s.store.scheduled_jobs().await?,
    }))
}

#[utoipa::path(get, path = "/api/v1/admin/replicas", tag = "admin", operation_id = "admin_replicas",
    description = "Replicas seen within a day with their test-management and analysis levels, and whether new data \
        formats may be written (`FASTPROOF_TM_GATE`, `FASTPROOF_TM_GATE_SETTLE_SECONDS`; `analysis`: \
        `FASTPROOF_ANALYSIS_GATE`, `FASTPROOF_ANALYSIS_GATE_SETTLE_SECONDS`). Needs `settings:write` without a project scope.",
    responses((status = 200, body = fastproof_store::tm::ReplicaLevels), (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody)))]
pub async fn replicas(
    State(s): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<fastproof_store::tm::ReplicaLevels>> {
    require(&auth, &s.perms.settings_write, None)?;
    let tm = &s.cfg.tm;
    let g = s
        .store
        .tm_gate(std::time::Duration::from_secs(tm.gate_settle_seconds))
        .await?;
    let gate_open = match tm.gate_mode {
        fastproof_store::tm::TmGateMode::Open => true,
        fastproof_store::tm::TmGateMode::Closed => false,
        fastproof_store::tm::TmGateMode::Auto => g.open,
    };
    let al = &s.cfg.analysis.limits;
    let a = s
        .store
        .analysis_gate(std::time::Duration::from_secs(al.gate_settle_seconds))
        .await?;
    let analysis = fastproof_store::analysis::types::GateView {
        gate_open: match al.gate_mode {
            fastproof_store::tm::TmGateMode::Open => true,
            fastproof_store::tm::TmGateMode::Closed => false,
            fastproof_store::tm::TmGateMode::Auto => a.open,
        },
        mode: al.gate_mode,
        settle_until: a.settle_until,
    };
    Ok(Json(fastproof_store::tm::ReplicaLevels {
        gate_open,
        mode: tm.gate_mode,
        settle_until: g.settle_until,
        replicas: s.store.replica_levels().await?,
        analysis,
    }))
}

// Documentation of the platform routes (`/api/v1/me`, `/api/v1/tokens`), served by
// `franshub_auth::routes`. The types below mirror their JSON and are used only in the document.

/// Permissions of the caller: in project P they are `global ∪ allScopes ∪ scopes[P]`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Grants {
    pub global: Vec<String>,
    pub scopes: std::collections::BTreeMap<String, Vec<String>>,
    pub all_scopes: Vec<String>,
}

/// The caller (`GET /api/v1/me`).
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    /// User or token name.
    pub name: String,
    /// `user`, `token` (static) or `apiToken` (managed).
    pub kind: String,
    /// `session`, `bearer`, `staticToken` or `managedToken`.
    pub auth_method: String,
    pub roles: Vec<String>,
    /// OIDC users only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Managed tokens only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<uuid::Uuid>,
    pub permissions: Grants,
}

/// Body of `POST /api/v1/tokens`. Give `scopes` or `global: true`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewToken {
    /// `[A-Za-z0-9._@-]`, 1 to 128 characters, unique among active tokens.
    pub name: String,
    /// A role: `viewer`, `uploader`, `editor`, `admin` or a custom one.
    pub role: String,
    /// Project keys, or `["*"]` for every project including later ones.
    pub scopes: Option<Vec<String>>,
    /// Grant the role everywhere.
    pub global: Option<bool>,
    /// Lifetime in days: 1 to 365. Every token expires; without this field it is 90 days.
    pub expires_in_days: Option<u32>,
    /// Free text, at most 1024 bytes.
    pub description: Option<String>,
}

/// A managed token; the value is never shown after creation.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Token {
    pub id: uuid::Uuid,
    pub name: String,
    pub role: String,
    /// Project keys; `["*"]` for every project; empty for a global token.
    pub scopes: Vec<String>,
    pub global: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Recorded at most once a minute.
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// `active`, `expired` or `revoked`.
    pub status: String,
    /// The managed token that created this one: it expires no later and is revoked with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<uuid::Uuid>,
}

/// Answer of `POST /api/v1/tokens`: the token metadata and its value, shown only here.
#[derive(Serialize, ToSchema)]
pub struct CreatedToken {
    /// The value (`fp_…`): store it now.
    pub token: String,
    #[serde(flatten)]
    pub info: Token,
}

/// Answer of `GET /api/v1/tokens`, newest first (not paged).
#[derive(Serialize, ToSchema)]
pub struct TokenList {
    pub tokens: Vec<Token>,
}

/// A role and where the caller may grant it.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrantableRole {
    pub name: String,
    pub builtin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub permissions: Vec<String>,
    /// Grantable globally.
    pub global: bool,
    /// Projects where it is grantable.
    pub scopes: Vec<String>,
    /// Grantable with `scopes: ["*"]`.
    pub all_scopes: bool,
}

/// Answer of `GET /api/v1/tokens/options`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenOptions {
    /// Longest lifetime, days (365).
    pub max_days: u32,
    /// Lifetime when `expiresInDays` is not given (90).
    pub default_days: u32,
    pub scopes: Vec<String>,
    pub roles: Vec<GrantableRole>,
}

/// Query of `GET /api/v1/tokens`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct TokensQueryDoc {
    /// Only tokens of this project (needs `tokens:read` there).
    pub scope: Option<String>,
    /// `active`, `expired` or `revoked`.
    pub status: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/me", tag = "admin", operation_id = "platform_me",
    responses((status = 200, body = Me), (status = 401, body = ErrorBody)))]
#[allow(dead_code)]
fn me_doc() {}

#[utoipa::path(get, path = "/api/v1/tokens", tag = "admin", operation_id = "platform_tokens_list",
    description = "Managed tokens the caller may read (`tokens:read`). 404 when managed tokens are not configured.",
    params(TokensQueryDoc),
    responses((status = 200, body = TokenList), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_list_doc() {}

#[utoipa::path(post, path = "/api/v1/tokens", tag = "admin", operation_id = "platform_tokens_create",
    description = "Creates a managed token (`tokens:write`). A token can grant only permissions its creator holds where \
        it grants them; a token created by a managed token expires no later than its creator and is revoked with it.",
    request_body = NewToken,
    responses((status = 201, body = CreatedToken), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_create_doc() {}

#[utoipa::path(get, path = "/api/v1/tokens/options", tag = "admin", operation_id = "platform_tokens_options",
    description = "Lifetimes, projects and roles the caller may grant (`tokens:write`).",
    responses((status = 200, body = TokenOptions), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_options_doc() {}

#[utoipa::path(get, path = "/api/v1/tokens/self", tag = "admin", operation_id = "platform_tokens_self",
    description = "The managed token of the request.",
    responses((status = 200, body = Token), (status = 404, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_self_doc() {}

#[utoipa::path(get, path = "/api/v1/tokens/{id}", tag = "admin", operation_id = "platform_tokens_get",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = Token), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_get_doc() {}

#[utoipa::path(delete, path = "/api/v1/tokens/{id}", tag = "admin", operation_id = "platform_tokens_revoke",
    description = "Revokes a token (and the tokens it created). A token may always revoke itself.",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = Token), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
#[allow(dead_code)]
fn tokens_revoke_doc() {}

/// The platform routes in the OpenAPI document.
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    me_doc,
    tokens_list_doc,
    tokens_create_doc,
    tokens_options_doc,
    tokens_self_doc,
    tokens_get_doc,
    tokens_revoke_doc
))]
pub struct PlatformDoc;

/// Adds the platform routes to the API document.
pub fn document_platform(doc: &mut utoipa::openapi::OpenApi) {
    use utoipa::OpenApi;
    doc.merge(PlatformDoc::openapi());
}
