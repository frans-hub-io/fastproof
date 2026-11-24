//! Requirements, their links to cases (manual, from code, AI proposals) and the tracker import
//! (§7.6, night 2).

use sqlx::PgConnection;
use uuid::Uuid;

use super::TmResult;
use super::cases::CaseCtx;
use super::sync::CodeData;
use crate::analysis::settings::CaseSyncSettings;

/// Links cases to requirements from the requirement links of trusted code; returns how many
/// links changed.
pub async fn sync_code_links(
    _conn: &mut PgConnection,
    _ctx: &CaseCtx<'_>,
    _settings: &CaseSyncSettings,
    _items: &[(Uuid, &CodeData)],
) -> TmResult<u64> {
    Ok(0)
}
