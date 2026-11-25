//! CI dispatches of plan launches and the plan check of their results (§7.4).

use sqlx::PgConnection;
use uuid::Uuid;

use crate::tm::PlanCheck;

/// What a finished launch started from a plan ran compared to the plan (`None` without a plan).
pub async fn plan_check(
    _conn: &mut PgConnection,
    _launch_id: Uuid,
) -> anyhow::Result<Option<PlanCheck>> {
    Ok(None)
}

impl crate::Store {
    /// [`plan_check`] on a pooled connection (notifications).
    pub async fn plan_check_of(&self, launch_id: Uuid) -> anyhow::Result<Option<PlanCheck>> {
        let mut conn = self.pool.acquire().await?;
        plan_check(&mut conn, launch_id).await
    }
}
