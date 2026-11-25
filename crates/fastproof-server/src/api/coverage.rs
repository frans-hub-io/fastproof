//! Requirement coverage and its exports (filled by the requirements area).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area (none yet).
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
