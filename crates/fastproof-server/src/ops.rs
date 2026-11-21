//! The ops listener: `/metrics`, `/healthz` (process alive), `/readyz` (database reachable,
//! migrations at the expected version, blob store reachable). It serves from the first moment
//! of start-up: while FastProof still waits for PostgreSQL or Kafka, `/healthz` answers and
//! `/readyz` says `starting`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusHandle;
use serde_json::json;

use crate::state::AppState;

async fn healthz() -> &'static str {
    "ok"
}

async fn check<F: std::future::Future<Output = anyhow::Result<()>>>(name: &str, f: F) -> bool {
    match tokio::time::timeout(Duration::from_secs(5), f).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!(check = name, error = %format!("{e:#}"), "readiness check failed");
            false
        }
        Err(_) => {
            tracing::warn!(check = name, "readiness check timed out");
            false
        }
    }
}

/// The state once start-up is complete.
pub type Started = Arc<OnceLock<AppState>>;

async fn readyz(State(started): State<Started>) -> (StatusCode, Json<serde_json::Value>) {
    let Some(s) = started.get() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "starting", "checks": {}})),
        );
    };
    let db = check("database", s.store.ping()).await;
    let schema = db
        && check("migrations", async {
            franshub_store_pg::check_schema(s.store.pool()).await?;
            fastproof_store::check_schema(s.store.pool()).await
        })
        .await;
    let blobs = check("blobs", s.blobs.check()).await;
    let ok = db && schema && blobs;
    let body = json!({
        "status": if ok { "ready" } else { "not ready" },
        "checks": {"database": db, "migrations": schema, "blobs": blobs},
    });
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    )
}

/// The ops router; `/readyz` fails until `started` is set.
pub fn router(started: Started, prometheus: Option<PrometheusHandle>) -> Router {
    let mut r = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(started);
    if let Some(h) = prometheus {
        r = r.merge(franshub_observe::metrics_router(h));
    }
    r
}
