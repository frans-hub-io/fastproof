//! Routes of triage suggestions, memory, backtest and the decision-event export (contract §8).
//!
//! Owner: S. Mounted once by the foundation; empty until that branch adds its routes.

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
