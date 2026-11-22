//! Manual runs (T2b): run items, step results, close and reopen, manual report.

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
