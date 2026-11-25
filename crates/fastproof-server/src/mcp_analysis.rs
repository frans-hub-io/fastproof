//! FastProof Analysis tools of the MCP endpoint (contract `76` §7.10): suggestions, unique
//! errors, flaky hints, log search, failure analytics, triage history and undo, gate
//! simulation. Same tokens, permissions, `tm_rate`, output limit and `deny_unknown_fields`
//! arguments as the other tools.
//!
//! Owner: M (night B). Until it fills this file there are no tools here.

use franshub_auth::Auth;
use serde_json::Value;

use crate::mcp::{ToolResult, ToolSpec};
use crate::state::AppState;

/// The analysis tools.
pub(crate) const TOOLS: &[ToolSpec] = &[];

/// Runs an analysis tool; `None` when `name` is not one.
pub(crate) async fn run(s: &AppState, name: &str, a: &Value, auth: &Auth) -> Option<ToolResult> {
    let _ = (s, a, auth);
    if !TOOLS.iter().any(|t| t.name == name) {
        return None;
    }
    None
}
