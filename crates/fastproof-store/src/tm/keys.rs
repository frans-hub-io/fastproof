//! External keys of cases (`case_keys`): Allure ID, TestRail, Qase, TestOps and other ids,
//! unique per project and kind.

use sqlx::PgConnection;
use uuid::Uuid;

use super::{CaseKey, CaseKeyKind, TmError, TmResult};
use crate::no_nul;

/// Bytes of a key value, at most.
pub const MAX_KEY_BYTES: usize = 200;

/// Checks a key value: 1 to [`MAX_KEY_BYTES`] bytes, no control characters.
pub fn check_key_value(v: &str) -> TmResult<()> {
    if v.is_empty() || v.len() > MAX_KEY_BYTES || v.chars().any(char::is_control) {
        return Err(TmError::bad_request(format!(
            "key values must have 1 to {MAX_KEY_BYTES} bytes without control characters"
        )));
    }
    Ok(())
}

/// Keys of a case, by kind and value.
pub async fn case_keys(conn: &mut PgConnection, case_id: Uuid) -> sqlx::Result<Vec<CaseKey>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select kind, value from case_keys where case_id = $1 order by kind, value limit 1000",
    )
    .bind(case_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, value)| CaseKeyKind::parse(&k).map(|kind| CaseKey { kind, value }))
        .collect())
}

/// The case that holds a key in the project.
pub async fn case_by_key(
    conn: &mut PgConnection,
    project_id: Uuid,
    kind: CaseKeyKind,
    value: &str,
) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "select case_id from case_keys where project_id = $1 and kind = $2 and value = $3",
    )
    .bind(project_id)
    .bind(kind.as_str())
    .bind(no_nul(value))
    .fetch_optional(&mut *conn)
    .await
}

/// Gives a key to a case. Returns `false` when another case of the project holds it (the key
/// is left unchanged); `true` when the case holds it now. The case must belong to the project.
pub async fn set_case_key(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    kind: CaseKeyKind,
    value: &str,
) -> TmResult<bool> {
    check_key_value(value)?;
    let holder: Uuid = sqlx::query_scalar(
        "with ins as (insert into case_keys (project_id, kind, value, case_id) \
           select $1, $2, $3, c.id from cases c where c.id = $4 and c.project_id = $1 \
           on conflict (project_id, kind, value) do nothing returning case_id) \
         select case_id from ins union all \
         select case_id from case_keys where project_id = $1 and kind = $2 and value = $3 limit 1",
    )
    .bind(project_id)
    .bind(kind.as_str())
    .bind(value)
    .bind(case_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("case"))?;
    Ok(holder == case_id)
}

/// Removes a key from a case; returns whether it was there.
pub async fn remove_case_key(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
    kind: CaseKeyKind,
    value: &str,
) -> sqlx::Result<bool> {
    Ok(sqlx::query(
        "delete from case_keys where project_id = $1 and case_id = $2 and kind = $3 and value = $4",
    )
    .bind(project_id)
    .bind(case_id)
    .bind(kind.as_str())
    .bind(no_nul(value))
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0)
}
