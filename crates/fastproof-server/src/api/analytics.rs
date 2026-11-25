//! Routes of failure analytics, portfolio and cross-project similar failures (tag `analytics`, contract §8).
//!
//! Owner: C (night B). Mounted once by the foundation; empty until that branch adds its routes.

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
