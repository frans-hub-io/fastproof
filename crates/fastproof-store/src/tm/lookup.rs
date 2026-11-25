//! Read-only access to cases and their version snapshots, shared by every module (writes
//! belong to `cases.rs` and `versions.rs`).

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{
    CaseOrigin, CaseRow, CaseSnapshot, CaseStatus, RequirementOrigin, RequirementRow,
    RequirementStatus, ResultSource, ReviewState, SharedStepOrigin, SharedStepRow,
};
use crate::Store;

/// Columns of [`case_row`] (`cases` without `search_text`), unqualified.
pub const CASE_COLS: &str = "id, project_id, number, name, status, automated, origin, ai_unreviewed, \
    folder_id, body, fields, tags, code_hash, revision, version, last_trusted_at, created_by, created_at, \
    updated_by, updated_at, archived_at, state, approved_version, review_state, last_status, last_status_at, \
    last_source, last_launch_id, last_result_id";

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
        state: r.try_get("state")?,
        approved_version: r.try_get("approved_version")?,
        review_state: r
            .try_get::<Option<String>, _>("review_state")?
            .and_then(|s| ReviewState::parse(&s)),
        last_status: r.try_get("last_status")?,
        last_status_at: r.try_get("last_status_at")?,
        last_source: r
            .try_get::<Option<String>, _>("last_source")?
            .and_then(|s| ResultSource::parse(&s)),
        last_launch_id: r.try_get("last_launch_id")?,
        last_result_id: r.try_get("last_result_id")?,
    })
}

/// Columns of [`shared_step_row`] (`shared_steps` without `search_text`), unqualified.
pub const SHARED_STEP_COLS: &str = "id, project_id, number, name, description, steps, version, \
    approved_version, review_state, revision, origin, ai_unreviewed, created_by, created_at, updated_by, \
    updated_at, archived_at";

/// A `shared_steps` row selected with [`SHARED_STEP_COLS`].
pub fn shared_step_row(r: &PgRow) -> sqlx::Result<SharedStepRow> {
    let steps: serde_json::Value = r.try_get("steps")?;
    let origin: String = r.try_get("origin")?;
    Ok(SharedStepRow {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        number: r.try_get("number")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        steps: serde_json::from_value(steps).unwrap_or_default(),
        version: r.try_get("version")?,
        approved_version: r.try_get("approved_version")?,
        review_state: r
            .try_get::<Option<String>, _>("review_state")?
            .and_then(|s| ReviewState::parse(&s)),
        revision: r.try_get("revision")?,
        origin: SharedStepOrigin::parse(&origin).unwrap_or(SharedStepOrigin::Ui),
        ai_unreviewed: r.try_get("ai_unreviewed")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        updated_by: r.try_get("updated_by")?,
        updated_at: r.try_get("updated_at")?,
        archived_at: r.try_get("archived_at")?,
    })
}

/// Columns of [`requirement_row`], unqualified.
pub const REQUIREMENT_COLS: &str = "id, project_id, number, title, description, type, status, parent_id, \
    path_ids, depth, tracker_id, issue_key, url, external_state, synced_at, content_changed_at, origin, \
    revision, created_by, created_at, updated_by, updated_at, archived_at";

/// A `requirements` row selected with [`REQUIREMENT_COLS`].
pub fn requirement_row(r: &PgRow) -> sqlx::Result<RequirementRow> {
    let status: String = r.try_get("status")?;
    let origin: String = r.try_get("origin")?;
    Ok(RequirementRow {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        number: r.try_get("number")?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        req_type: r.try_get("type")?,
        status: RequirementStatus::parse(&status).unwrap_or(RequirementStatus::Active),
        parent_id: r.try_get("parent_id")?,
        path_ids: r.try_get("path_ids")?,
        depth: r.try_get("depth")?,
        tracker_id: r.try_get("tracker_id")?,
        issue_key: r.try_get("issue_key")?,
        url: r.try_get("url")?,
        external_state: r.try_get("external_state")?,
        synced_at: r.try_get("synced_at")?,
        content_changed_at: r.try_get("content_changed_at")?,
        origin: RequirementOrigin::parse(&origin).unwrap_or(RequirementOrigin::Ui),
        revision: r.try_get("revision")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        updated_by: r.try_get("updated_by")?,
        updated_at: r.try_get("updated_at")?,
        archived_at: r.try_get("archived_at")?,
    })
}

/// The shared step with number `n` in the project (archived ones included).
pub async fn shared_step_by_number(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
) -> sqlx::Result<Option<SharedStepRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {SHARED_STEP_COLS} from shared_steps where project_id = $1 and number = $2"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(shared_step_row).transpose()
}

/// The shared step with id `id` in the project.
pub async fn shared_step_by_id(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<SharedStepRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {SHARED_STEP_COLS} from shared_steps where project_id = $1 and id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(shared_step_row).transpose()
}

/// The requirement with number `n` in the project (archived ones included).
pub async fn requirement_by_number(
    conn: &mut PgConnection,
    project_id: Uuid,
    n: i64,
) -> sqlx::Result<Option<RequirementRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {REQUIREMENT_COLS} from requirements where project_id = $1 and number = $2"
    )))
    .bind(project_id)
    .bind(n)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(requirement_row).transpose()
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
