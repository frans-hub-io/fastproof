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

#[utoipa::path(get, path = "/api/v1/audit", tag = "admin", params(AuditQuery),
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
    let f = AuditFilter {
        action: q.action,
        principal: q.principal,
        scope: q.scope,
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

#[utoipa::path(get, path = "/api/v1/admin/jobs", tag = "admin",
    responses((status = 200, body = JobList), (status = 403, body = ErrorBody)))]
pub async fn jobs(State(s): State<AppState>, auth: Auth) -> ApiResult<Json<JobList>> {
    require(&auth, &s.perms.settings_write, None)?;
    Ok(Json(JobList {
        items: s.store.scheduled_jobs().await?,
    }))
}
