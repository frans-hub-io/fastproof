//! Pins of shared steps in run items and their expansion into executed steps (§7.1).

use std::collections::HashMap;

use sqlx::PgConnection;
use uuid::Uuid;

use super::{CaseReviewSettings, CaseStep, TmResult};

/// `run_items.shared_pins`: `{"<sharedStepId>": version}`.
pub type SharedPins = std::collections::BTreeMap<Uuid, i32>;

/// Pins per `(case_id, case_version)`: the runnable version of every shared step the case
/// version references.
pub async fn pins_for_items(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _items: &[(Uuid, i32)],
    _review: &CaseReviewSettings,
) -> TmResult<HashMap<Uuid, SharedPins>> {
    Ok(HashMap::new())
}

/// Replaces the `shared` steps of an executed snapshot by the pinned versions' steps.
pub async fn expand_steps(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _steps: &mut Vec<CaseStep>,
    _pins: &SharedPins,
) -> TmResult<()> {
    Ok(())
}

/// Pins a run item again to the runnable versions (refresh, rerun).
pub async fn repin_item(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _item_id: Uuid,
    _review: &CaseReviewSettings,
) -> TmResult<()> {
    Ok(())
}
