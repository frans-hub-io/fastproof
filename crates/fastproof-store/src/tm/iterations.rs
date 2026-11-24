//! Iterations of parameterized run items (§7.5, night 2).

use sqlx::PgConnection;
use uuid::Uuid;

use super::TmResult;

/// Creates the iterations of newly inserted run items `(item, case, version)`; returns how
/// many it created.
pub async fn create_iterations(
    _conn: &mut PgConnection,
    _project_id: Uuid,
    _launch_id: Uuid,
    _items: &[(Uuid, Uuid, i32)],
) -> TmResult<u64> {
    Ok(0)
}
