//! FPQL validation, schema and saved queries (filled by the query area).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area (none yet).
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
