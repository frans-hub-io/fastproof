//! Configurable case workflow: states within the status categories, transitions with guards,
//! and the remap job (§7.2). F declares the hooks; the workflow area fills them.

use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::cases::CaseCtx;
use super::{CaseRow, CaseStatus, CaseWorkflowSettings, TmJobProgress, TmJobStatus, TmResult};
use crate::Store;

/// The state a case moves to when a person changes its status to `to` (a category), or 409
/// `invalid_transition`; `Ok(None)` without a configured workflow.
pub async fn check_transition(
    _conn: &mut PgConnection,
    _ctx: &CaseCtx<'_>,
    _cur: &CaseRow,
    _to: CaseStatus,
) -> TmResult<Option<String>> {
    Ok(None)
}

/// The state of a case that code sync, an import or a create gives it (no transition check).
pub async fn system_state(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _automated: bool,
    _status: CaseStatus,
) -> TmResult<Option<String>> {
    Ok(None)
}

/// Refuses a settings change that removes states cases still use (409
/// `workflow_status_in_use`).
pub async fn check_settings_change(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _old: Option<&CaseWorkflowSettings>,
    _new: Option<&CaseWorkflowSettings>,
) -> TmResult<()> {
    Ok(())
}

impl Store {
    /// The `workflow_remap` job: moves cases of removed states to their new keys.
    pub async fn run_workflow_remap_job(&self, job_id: Uuid, owner: &str) -> TmResult<()> {
        fail_job(self, job_id, owner, "workflow_remap").await
    }
}

/// Ends a claimed job of a kind this build cannot run yet as `failed` ("not available").
pub(crate) async fn fail_job(store: &Store, job_id: Uuid, owner: &str, kind: &str) -> TmResult<()> {
    let key: Option<String> = sqlx::query_scalar(
        "select p.key from tm_jobs j join projects p on p.id = j.project_id where j.id = $1",
    )
    .bind(job_id)
    .fetch_optional(&store.pool)
    .await?;
    let report = super::ImportReport {
        errors: vec![super::ImportRowError {
            row: 0,
            code: "not_available".into(),
            message: format!("{kind} jobs are not available"),
        }],
        ..Default::default()
    };
    store
        .finish_tm_job(
            job_id,
            owner,
            key.as_deref().unwrap_or_default(),
            TmJobStatus::Failed,
            TmJobProgress::default(),
            Some(&report),
            Some(json!({})),
        )
        .await?;
    Ok(())
}
