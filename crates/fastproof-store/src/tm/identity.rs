//! The manual identity of a case: a `test_cases` row with `kind = 'manual'` and
//! `test_case_hash = md5('fastproof:manual:' || case_id)`, and its one series, so that manual
//! results land in `results` like automated ones.

use fastproof_model::identity::{md5_hex, parameters_hash, retry_hash};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::no_nul;

/// `md5('fastproof:manual:' || case_id)`.
pub fn manual_hash(case_id: Uuid) -> String {
    md5_hex(&format!("fastproof:manual:{case_id}"))
}

/// Gets or creates the manual identity of a case and its series: `(test_case_id, series_id)`.
/// The identity's full name is `TC-<n>` and its display name the case name. Fails when the
/// case is not in the project.
pub async fn manual_identity(
    conn: &mut PgConnection,
    project_id: Uuid,
    case_id: Uuid,
) -> anyhow::Result<(Uuid, Uuid)> {
    let (number, name): (i64, String) =
        sqlx::query_as("select number, name from cases where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(case_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| anyhow::anyhow!("case {case_id} is not in the project"))?;
    let hash = manual_hash(case_id);
    let test_case_id: Uuid = sqlx::query_scalar(
        "with ins as (insert into test_cases (id, project_id, test_case_hash, full_name, display_name, \
           kind, case_id, first_seen_at, last_seen_at) \
           values ($1, $2, $3, $4, $5, 'manual', $6, now(), now()) \
           on conflict (project_id, test_case_hash) do update set display_name = excluded.display_name, \
             case_id = excluded.case_id \
           where test_cases.display_name is distinct from excluded.display_name \
             or test_cases.case_id is distinct from excluded.case_id \
           returning id) \
         select id from ins union all \
         select id from test_cases where project_id = $2 and test_case_hash = $3 limit 1",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(&hash)
    .bind(format!("TC-{number}"))
    .bind(no_nul(&name))
    .bind(case_id)
    .fetch_one(&mut *conn)
    .await?;
    let params = parameters_hash(&[]);
    let series_key = retry_hash(&hash, &params, None);
    let series_id: Uuid = sqlx::query_scalar(
        "with ins as (insert into series (id, project_id, test_case_id, series_key, parameters_hash, \
           env_key, parameters, first_seen_at, last_seen_at) \
           values ($1, $2, $3, $4, $5, null, '[]', now(), now()) \
           on conflict (project_id, series_key) do nothing returning id) \
         select id from ins union all \
         select id from series where project_id = $2 and series_key = $4 limit 1",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(test_case_id)
    .bind(&series_key)
    .bind(&params)
    .fetch_one(&mut *conn)
    .await?;
    Ok((test_case_id, series_id))
}
