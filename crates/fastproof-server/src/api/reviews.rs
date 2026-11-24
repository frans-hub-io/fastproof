//! Version review of cases and shared steps (filled by the review area).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area (none yet).
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
