//! The gate for writes of new formats (§3): `FASTPROOF_TM_GATE=auto|open|closed`. In `auto`
//! mode the rule of [`crate::replica`] is read through a per-process cache (an open gate for
//! 30 s, a closed one for 5 s, per database).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::PgConnection;

use super::{TmError, TmGateMode, TmLimits, TmResult};
use crate::replica::{TM_LEVEL, tm_gate_on};

/// How long an open gate is cached.
pub const OPEN_TTL: Duration = Duration::from_secs(30);
/// How long a closed gate is cached.
pub const CLOSED_TTL: Duration = Duration::from_secs(5);

static CACHE: Mutex<Option<HashMap<String, (Instant, bool)>>> = Mutex::new(None);

/// 409 `upgrade_in_progress {what, requiredLevel}`.
pub fn closed(what: &'static str) -> TmError {
    TmError::Conflict {
        code: "upgrade_in_progress",
        message: format!(
            "{what} needs every replica on the new version; an upgrade is in progress"
        ),
        details: Some(serde_json::json!({"what": what, "requiredLevel": TM_LEVEL})),
    }
}

/// Whether new formats may be written now (mode, then the cached rule).
pub async fn is_open(conn: &mut PgConnection, limits: &TmLimits) -> anyhow::Result<bool> {
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
    let g = tm_gate_on(conn, Duration::from_secs(limits.gate_settle_seconds)).await?;
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let m = c.get_or_insert_with(HashMap::new);
    if m.len() > 64 {
        m.clear();
    }
    m.insert(key, (Instant::now(), g.open));
    Ok(g.open)
}

/// Refuses a write of a new format (`what` names it) while the gate is closed.
pub async fn require(
    conn: &mut PgConnection,
    limits: &TmLimits,
    what: &'static str,
) -> TmResult<()> {
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
