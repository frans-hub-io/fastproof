//! Signals about test attempts: push insert, resolution, FastMock signal writes and readers
//! (contract §7.7).
//!
//! Owner: K (night B). Until it fills this file the readers answer nothing.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::types::AttemptSignal;
use crate::Store;

impl Store {
    /// Stored signals of results. Stub until K: none.
    pub async fn signals_for_results(
        &self,
        project_id: Uuid,
        result_ids: &[Uuid],
    ) -> anyhow::Result<Vec<AttemptSignal>> {
        let _ = (project_id, result_ids);
        Ok(Vec::new())
    }

    /// Stored signals of the results of a series since a time, with their result. Stub until
    /// K: none.
    pub async fn signals_for_series(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<(Uuid, AttemptSignal)>> {
        let _ = (project_id, series_id, since);
        Ok(Vec::new())
    }
}
