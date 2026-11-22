//! Read-only access to cases and their version snapshots, shared by every module (writes
//! belong to `cases.rs` and `versions.rs`).

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{CaseOrigin, CaseRow, CaseSnapshot, CaseStatus};
use crate::Store;

/// Columns of [`case_row`] (`cases` without `search_text`), unqualified.
pub const CASE_COLS: &str = "id, project_id, number, name, status, automated, origin, ai_unreviewed, \
    folder_id, body, fields, tags, code_hash, revision, version, last_trusted_at, created_by, created_at, \
    updated_by, updated_at, archived_at";

/// A `cases` row selected with [`CASE_COLS`].
pub fn case_row(r: &PgRow) -> sqlx::Result<CaseRow> {
    let status: String = r.try_get("status")?;
    let origin: String = r.try_get("origin")?;
    let body: serde_json::Value = r.try_get("body")?;
    let fields: serde_json::Value = r.try_get("fields")?;
    Ok(CaseRow {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        status: CaseStatus::parse(&status).unwrap_or_default(),
        automated: r.try_get("automated")?,
        origin: CaseOrigin::parse(&origin).unwrap_or(CaseOrigin::Ui),
        ai_unreviewed: r.try_get("ai_unreviewed")?,
        folder_id: r.try_get("folder_id")?,
        body: serde_json::from_value(body).unwrap_or_default(),
        fields: serde_json::from_value(fields).unwrap_or_default(),
        tags: r.try_get("tags")?,
        code_hash: r.try_get("code_hash")?,
        revision: r.try_get("revision")?,
        version: r.try_get("version")?,
        last_trusted_at: r.try_get("last_trusted_at")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        updated_by: r.try_get("updated_by")?,
        updated_at: r.try_get("updated_at")?,
        archived_at: r.try_get("archived_at")?,
    })
}

/// The case with number `n` in the project (archived ones included).
pub async fn case_by_number(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
) -> sqlx::Result<Option<CaseRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {CASE_COLS} from cases where project_id = $1 and number = $2"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(case_row).transpose()
}

/// The case with id `id` in the project.
pub async fn case_by_id(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<CaseRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {CASE_COLS} from cases where project_id = $1 and id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(case_row).transpose()
}

/// The snapshot of a case at a version (decoded from `snapshot_z`).
pub async fn case_version_snapshot(
    conn: &mut PgConnection,
    case_id: Uuid,
    version: i32,
) -> anyhow::Result<Option<CaseSnapshot>> {
    let z: Option<Vec<u8>> = sqlx::query_scalar(
        "select snapshot_z from case_versions where case_id = $1 and version = $2",
    )
    .bind(case_id)
    .bind(version)
    .fetch_optional(&mut *conn)
    .await?;
    z.map(|z| crate::codec::decode(&z)).transpose()
}

impl Store {
    /// The case with number `n` in the project (archived ones included).
    pub async fn case_by_number(
        &self,
        project_id: Uuid,
        n: i64,
    ) -> anyhow::Result<Option<CaseRow>> {
        let mut conn = self.pool.acquire().await?;
        Ok(case_by_number(&mut conn, project_id, n).await?)
    }

    /// The snapshot of a case at a version. Callers check that the case is in their project
    /// (ids come from [`Store::case_by_number`] or a run item of the project).
    pub async fn case_version_snapshot(
        &self,
        case_id: Uuid,
        version: i32,
    ) -> anyhow::Result<Option<CaseSnapshot>> {
        let mut conn = self.pool.acquire().await?;
        case_version_snapshot(&mut conn, case_id, version).await
    }
}
