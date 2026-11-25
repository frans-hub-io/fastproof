//! The analysis gate for writes of new formats (contract `76` §3):
//! `FASTPROOF_ANALYSIS_GATE=auto|open|closed`. In `auto` mode the rule of
//! [`crate::replica::analysis_gate_on`] is read through a per-process cache (an open gate for
//! 30 s, a closed one for 5 s, keyed by database and settle time).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::PgConnection;

use super::types::{AnalysisLimits, AnalysisResult};
use crate::replica::{ANALYSIS_LEVEL, analysis_gate_on};
use crate::tm::{TmError, TmGateMode};

/// How long an open gate is cached.
pub const OPEN_TTL: Duration = Duration::from_secs(30);
/// How long a closed gate is cached.
pub const CLOSED_TTL: Duration = Duration::from_secs(5);

static CACHE: Mutex<Option<HashMap<String, (Instant, bool)>>> = Mutex::new(None);

/// 409 `upgrade_in_progress {what, requiredLevel: 1, area: "analysis"}`.
pub fn closed(what: &'static str) -> TmError {
    TmError::Conflict {
        code: "upgrade_in_progress",
        message: format!(
            "{what} needs every replica on the new version; an upgrade is in progress"
        ),
        details: Some(
            serde_json::json!({"what": what, "requiredLevel": ANALYSIS_LEVEL, "area": "analysis"}),
        ),
    }
}

/// Whether analysis formats may be written now (mode, then the cached rule).
pub async fn is_open(conn: &mut PgConnection, limits: &AnalysisLimits) -> anyhow::Result<bool> {
    match limits.gate_mode {
        TmGateMode::Open => return Ok(true),
        TmGateMode::Closed => return Ok(false),
        TmGateMode::Auto => {}
    }
    let db: String = sqlx::query_scalar("select current_database()::text")
        .fetch_one(&mut *conn)
        .await?;
    let key = format!("{db}:{}", limits.gate_settle_seconds);
    if let Some((at, open)) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&key).copied())
    {
        let ttl = if open { OPEN_TTL } else { CLOSED_TTL };
        if at.elapsed() < ttl {
            return Ok(open);
        }
    }
    let g = analysis_gate_on(conn, Duration::from_secs(limits.gate_settle_seconds)).await?;
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let m = c.get_or_insert_with(HashMap::new);
    if m.len() > 64 {
        m.clear();
    }
    m.insert(key, (Instant::now(), g.open));
    Ok(g.open)
}

/// Refuses a write of an analysis format (`what` names it) while the gate is closed.
pub async fn require(
    conn: &mut PgConnection,
    limits: &AnalysisLimits,
    what: &'static str,
) -> AnalysisResult<()> {
    if is_open(conn, limits).await? {
        Ok(())
    } else {
        Err(closed(what))
    }
}

/// Forgets the cached gate (tests).
#[cfg(any(test, feature = "test-util"))]
pub fn reset_cache() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Feature names that need ClickHouse (`AnalysisCapabilities.withClickHouse`).
pub const CLICKHOUSE_FEATURES: [&str; 2] = ["portfolio", "crossProjectSimilar"];

impl crate::Store {
    /// `GET .../analysis/capabilities`: what Analysis answers for a project. A feature is on
    /// when its code is in the build (`inputs.implemented`), the instance and the project have
    /// Analysis on, and, for the features of [`CLICKHOUSE_FEATURES`], ClickHouse is there.
    pub async fn analysis_capabilities(
        &self,
        project_id: uuid::Uuid,
        inputs: &super::types::CapabilityInputs,
    ) -> anyhow::Result<super::types::AnalysisCapabilities> {
        let settings = self.project_settings(project_id).await?;
        let trigram = self.trigram_installed().await?;
        let gate_open = {
            let mut conn = self.pool.acquire().await?;
            is_open(&mut conn, &inputs.limits).await?
        };
        let project_enabled = settings.analysis.enabled;
        let enabled = inputs.instance_enabled && project_enabled;
        let i = inputs.implemented;
        let ch = inputs.clickhouse;
        let features = super::types::AnalysisFeatures {
            suggestions: enabled && i.suggestions,
            errors: enabled && i.errors,
            log_patterns: enabled && i.log_patterns,
            gate_analytics: enabled && i.gate_analytics,
            flaky_hints: enabled && i.flaky_hints,
            signals: enabled && i.signals,
            analytics: enabled && i.analytics,
            portfolio: enabled && ch && i.portfolio,
            cross_project_similar: enabled && ch && i.cross_project_similar,
        };
        Ok(super::types::AnalysisCapabilities {
            enabled,
            instance_enabled: inputs.instance_enabled,
            project_enabled,
            clickhouse: ch,
            trigram,
            gate_open,
            features,
            windows: super::types::AnalysisWindows {
                raw_days: settings.retention.raw_days,
                memory_days: settings.analysis.memory_days,
                analytics_days: if ch {
                    inputs.clickhouse_days
                } else {
                    super::types::ANALYTICS_PG_DAYS as u32
                },
            },
            with_click_house: CLICKHOUSE_FEATURES.iter().map(|s| s.to_string()).collect(),
        })
    }
}
