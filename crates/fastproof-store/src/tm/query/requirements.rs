//! FPQL attributes of requirements (§8, night 2). Until they exist, queries over
//! `entity = requirements` answer 400 "not available".
//!
//! To add them: list each attribute in [`ATTRIBUTES`] (kinds of [`super::attrs::Kind`]), set
//! [`AVAILABLE`], and compile a resolved condition on requirements aliased `r` in
//! [`push_condition`] with `push_bind` only.

use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::ast::Cond;
use super::attrs::AttrSpec;

/// Whether requirement queries are available in this build.
pub const AVAILABLE: bool = false;

/// Attributes of requirements: `number, title, type, status, parent, issue, externalState,
/// coverage` (night 2).
pub static ATTRIBUTES: &[AttrSpec] = &[];

/// Appends one condition on requirements aliased `r` (no leading ` and `).
pub fn push_condition(qb: &mut QueryBuilder<Postgres>, project_id: Uuid, c: &Cond) {
    let _ = (project_id, c);
    qb.push("false");
}
