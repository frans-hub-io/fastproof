//! Exports of test cases (T3).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this module, mounted once by `crate::app::api_router`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
