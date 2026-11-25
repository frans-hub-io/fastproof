//! Log rules: evaluation after a launch, the retro on PostgreSQL and `result_patterns`
//! (contract §7.6).
//!
//! Owner: P. Until it fills this file no result has a pattern mark.

use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use super::types::ResultPattern;
use crate::Store;

/// Narrows a results query (alias `r`) to results marked with `pattern`. Stub until P: no
/// result matches.
pub fn result_pattern_filter(qb: &mut QueryBuilder<Postgres>, pattern: &str) {
    let _ = pattern;
    qb.push(" and false");
}

impl Store {
    /// Pattern marks of a result. Stub until P: none.
    pub async fn result_patterns(&self, result_id: Uuid) -> anyhow::Result<Vec<ResultPattern>> {
        let _ = result_id;
        Ok(Vec::new())
    }
}
