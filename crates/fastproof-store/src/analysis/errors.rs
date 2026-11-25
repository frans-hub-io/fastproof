//! Unique errors: the lifecycle step, branches, aliases, commits, list and detail, and the
//! `newErrors` count (contract §7.4).
//!
//! Owner: E. Until it fills this file, only [`Store::new_error_count`] answers, as a stub.

use uuid::Uuid;

use crate::{Launch, Store};

impl Store {
    /// New errors of a launch for the gate metric `newErrors`. Stub until E: the distinct
    /// fingerprints of the launch whose catalog row was first seen in this launch
    /// (`baseline` is not used yet).
    pub async fn new_error_count(
        &self,
        project_id: Uuid,
        launch: &Launch,
        baseline: Option<&str>,
    ) -> anyhow::Result<i64> {
        let _ = baseline;
        Ok(sqlx::query_scalar(
            "select count(distinct r.fingerprint) from results r join failure_fingerprints f \
               on f.project_id = r.project_id and f.fingerprint = r.fingerprint \
             where r.launch_id = $1 and r.launch_created_at = $2 and r.project_id = $3 \
               and r.fingerprint is not null and f.first_launch_id = $1",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?)
    }
}
