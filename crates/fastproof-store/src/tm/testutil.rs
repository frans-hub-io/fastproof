//! Helpers for tests (feature `test-util`): minimal case rows with a first version, written
//! without the validation and side effects of `cases.rs`.

use sqlx::PgConnection;
use uuid::Uuid;

use super::lookup::case_by_id;
use super::{CaseBody, CaseRow, CaseSnapshot, CaseStatus};

/// Inserts a case with the next number of the project and its version 1 (`source = ui`).
pub async fn insert_case(
    conn: &mut PgConnection,
    project_id: Uuid,
    name: &str,
    body: &CaseBody,
    folder_id: Option<Uuid>,
) -> anyhow::Result<CaseRow> {
    let number: i64 = sqlx::query_scalar(
        "update projects set case_seq = case_seq + 1 where id = $1 returning case_seq",
    )
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into cases (id, project_id, number, name, status, origin, folder_id, body, created_by, updated_by) \
         values ($1, $2, $3, $4, 'active', 'ui', $5, $6, 'test', 'test')",
    )
    .bind(id)
    .bind(project_id)
    .bind(number)
    .bind(name)
    .bind(folder_id)
    .bind(serde_json::to_value(body)?)
    .execute(&mut *conn)
    .await?;
    let snapshot = CaseSnapshot {
        name: name.to_string(),
        status: CaseStatus::Active,
        folder_id,
        body: body.clone(),
        fields: Default::default(),
        tags: Vec::new(),
    };
    sqlx::query(
        "insert into case_versions (case_id, version, project_id, source, snapshot_z, created_by) \
         values ($1, 1, $2, 'ui', $3, 'test')",
    )
    .bind(id)
    .bind(project_id)
    .bind(crate::codec::encode(&snapshot)?)
    .execute(&mut *conn)
    .await?;
    case_by_id(conn, project_id, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("inserted case vanished"))
}
