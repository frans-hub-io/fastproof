//! Routes of the gate history and simulation (contract §8).
//!
//! Owner: G. Mounted once by the foundation; empty until that branch adds its routes.

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

/// Whether this build answers the gate analytics (`AnalysisFeatures.gateAnalytics`).
pub const AVAILABLE: bool = false;

/// The routes of this area.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
