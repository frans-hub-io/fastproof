//! Shared steps: CRUD, versions, usages, inline and extract (`74-tm-next-interfaces.md` §7.1).
//! F declares the hooks the case code calls; the shared-steps area fills them.

use sqlx::PgConnection;
use uuid::Uuid;

use super::cases::{CaseCtx, invalid_case};
use super::{CaseBody, CaseReviewSettings, CaseRow, SharedStepBrief, TmLimits, TmResult};
use crate::Store;

/// Checks the `shared` steps of a case body: the shared steps exist in the project, are not
/// archived when newly referenced, and the limits hold.
pub async fn validate_refs(
    _conn: &mut PgConnection,
    _ctx: &CaseCtx<'_>,
    body: &CaseBody,
    _current: Option<&CaseRow>,
) -> TmResult<()> {
    if let Some(i) = body.steps.iter().position(|s| s.shared.is_some()) {
        return Err(invalid_case(
            &format!("body.steps[{i}].shared"),
            "shared steps are not available",
        ));
    }
    Ok(())
}

/// Records which shared steps a case uses (`shared_step_usages`).
pub async fn sync_usages(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _case_id: Uuid,
    _body: &CaseBody,
) -> TmResult<()> {
    Ok(())
}

/// The shared steps a case body references, at their runnable version.
pub async fn briefs_for_case(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _body: &CaseBody,
    _review: &CaseReviewSettings,
) -> TmResult<Vec<SharedStepBrief>> {
    Ok(Vec::new())
}

impl Store {
    /// The `shared_inline` job: replaces the references to a shared step in every using case.
    pub async fn run_shared_inline_job(
        &self,
        job_id: Uuid,
        owner: &str,
        _limits: &TmLimits,
    ) -> TmResult<()> {
        super::workflow::fail_job(self, job_id, owner, "shared_inline").await
    }
}
