//! Routes of unique errors and their timeline (contract §8).
//!
//! Owner: E. Mounted once by the foundation; empty until that branch adds its routes.

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
