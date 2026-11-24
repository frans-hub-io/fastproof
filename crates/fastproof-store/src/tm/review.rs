//! Version review of cases and shared steps (§7.3). F writes the policy reads and the runnable
//! version rule; the review area fills the save hooks, votes and the queue.

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::cases::CaseCtx;
use super::{CaseReviewSettings, CaseRow, ReviewMode, SharedStepRow, TmResult, VersionSource};
use crate::analysis::settings::ProjectSettings;

/// The `caseReview` policy of a project (defaults when absent or invalid).
pub async fn review_settings(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<CaseReviewSettings> {
    let v: Option<Value> = sqlx::query_scalar("select settings from projects where id = $1")
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(ProjectSettings::from_value(v.unwrap_or(Value::Null)).case_review)
}

/// SQL of the runnable version of a case over alias `c`: the head (`c.version`) when review is
/// off, when the case is out of scope (`manual` mode and automated) or was never edited under
/// review (`review_state is null`); otherwise `c.approved_version` (null: not runnable).
pub fn runnable_version_sql(policy: &CaseReviewSettings) -> &'static str {
    match policy.mode {
        ReviewMode::Off => "c.version",
        ReviewMode::Manual => {
            "(case when c.automated or c.review_state is null then c.version else c.approved_version end)"
        }
        ReviewMode::All => {
            "(case when c.review_state is null then c.version else c.approved_version end)"
        }
    }
}

/// After a case version was written: moves the review state of the head (§7.3).
pub async fn on_save(
    _conn: &mut PgConnection,
    _ctx: &CaseCtx<'_>,
    _before: Option<&CaseRow>,
    _after: &CaseRow,
    _changed: &[String],
    _source: VersionSource,
) -> TmResult<()> {
    Ok(())
}

/// After a shared step version was written: moves its review state (§7.3).
pub async fn on_shared_step_save(
    _conn: &mut PgConnection,
    _ctx: &CaseCtx<'_>,
    _before: Option<&SharedStepRow>,
    _after: &SharedStepRow,
    _changed: &[String],
    _source: VersionSource,
) -> TmResult<()> {
    Ok(())
}
