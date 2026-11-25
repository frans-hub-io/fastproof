//! Defect types per project: five built-in groups (seeded on first use) and custom subtypes.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, no_nul};

/// The built-in groups, in display order.
pub const GROUPS: [&str; 5] = [
    "productBug",
    "automationBug",
    "systemIssue",
    "noDefect",
    "toInvestigate",
];

/// Default group of failed and broken results.
pub const TO_INVESTIGATE: &str = "toInvestigate";

/// Custom defect types per project, at most.
pub const MAX_DEFECT_TYPES: i64 = 100;

const BUILT_IN: [(&str, &str, &str, &str); 5] = [
    ("productBug", "Product bug", "PB", "#e5484d"),
    ("automationBug", "Automation bug", "AB", "#f5a524"),
    ("systemIssue", "System issue", "SI", "#3e63dd"),
    ("noDefect", "No defect", "ND", "#8b8d98"),
    ("toInvestigate", "To investigate", "TI", "#a855f7"),
];

/// A defect type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefectType {
    pub id: Uuid,
    /// `productBug`, `automationBug`, `systemIssue`, `noDefect` or `toInvestigate`.
    pub group: String,
    pub name: String,
    pub short_name: String,
    pub color: String,
    pub built_in: bool,
}

/// Body of `POST .../defect-types`.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDefectType {
    pub group: String,
    pub name: String,
    pub short_name: Option<String>,
    pub color: Option<String>,
}

/// Body of `PATCH .../defect-types/{id}`.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefectTypePatch {
    pub name: Option<String>,
    pub short_name: Option<String>,
    pub color: Option<String>,
}

/// Checks name, short name and color.
pub fn validate_fields(
    name: Option<&str>,
    short_name: Option<&str>,
    color: Option<&str>,
) -> Result<(), String> {
    if let Some(n) = name
        && (n.trim().is_empty() || n.len() > 100)
    {
        return Err("name must have 1 to 100 bytes".into());
    }
    if let Some(s) = short_name
        && s.chars().count() > 8
    {
        return Err("shortName must have at most 8 characters".into());
    }
    if let Some(c) = color
        && !c.is_empty()
        && !(c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("color must be #rrggbb".into());
    }
    Ok(())
}

type Row = (Uuid, String, String, String, String, bool);

fn row(r: Row) -> DefectType {
    DefectType {
        id: r.0,
        group: r.1,
        name: r.2,
        short_name: r.3,
        color: r.4,
        built_in: r.5,
    }
}

const COLUMNS: &str = "id, grp, name, short_name, color, built_in";

/// Seeds the built-in types of a project when they are missing and returns them by group.
pub async fn builtin_types(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> anyhow::Result<Vec<DefectType>> {
    let q = format!(
        "select {COLUMNS} from defect_types where project_id = $1 and built_in order by grp"
    );
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(q.clone()))
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
    if rows.len() == BUILT_IN.len() {
        return Ok(rows.into_iter().map(row).collect());
    }
    for (grp, name, short, color) in BUILT_IN {
        sqlx::query(
            "insert into defect_types (id, project_id, grp, name, short_name, color, built_in) \
             values ($1, $2, $3, $4, $5, $6, true) on conflict do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(grp)
        .bind(name)
        .bind(short)
        .bind(color)
        .execute(&mut *conn)
        .await?;
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(q))
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().map(row).collect())
}

/// Outcome of a defect type change.
#[derive(Debug)]
pub enum DefectTypeChange {
    Done(DefectType),
    NotFound,
    /// The name is taken, the limit is reached, or the type is in use / built in.
    Conflict(String),
}

impl Store {
    /// Every defect type of a project (built-ins first).
    pub async fn defect_types(&self, project_id: Uuid) -> anyhow::Result<Vec<DefectType>> {
        let mut conn = self.pool.acquire().await?;
        builtin_types(&mut conn, project_id).await?;
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from defect_types where project_id = $1 \
             order by built_in desc, array_position(array['productBug','automationBug','systemIssue','noDefect','toInvestigate'], grp), name limit 1000"
        )))
        .bind(project_id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(rows.into_iter().map(row).collect())
    }

    pub async fn defect_type(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<DefectType>> {
        let r: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from defect_types where project_id = $1 and id = $2"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(row))
    }

    pub async fn create_defect_type(
        &self,
        project_id: Uuid,
        t: &NewDefectType,
    ) -> anyhow::Result<DefectTypeChange> {
        let mut tx = self.pool.begin().await?;
        builtin_types(&mut tx, project_id).await?;
        sqlx::query("select id from projects where id = $1 for update")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        let n: i64 = sqlx::query_scalar(
            "select count(*) from defect_types where project_id = $1 and not built_in",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if n >= MAX_DEFECT_TYPES {
            return Ok(DefectTypeChange::Conflict(format!(
                "at most {MAX_DEFECT_TYPES} custom defect types"
            )));
        }
        let r: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "insert into defect_types (id, project_id, grp, name, short_name, color, built_in) \
             values ($1, $2, $3, $4, $5, $6, false) on conflict do nothing returning {COLUMNS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(&t.group)
        .bind(no_nul(t.name.trim()))
        .bind(no_nul(t.short_name.as_deref().unwrap_or_default()))
        .bind(t.color.as_deref().unwrap_or_default())
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(match r {
            Some(r) => DefectTypeChange::Done(row(r)),
            None => DefectTypeChange::Conflict("a defect type with this name exists".into()),
        })
    }

    pub async fn update_defect_type(
        &self,
        project_id: Uuid,
        id: Uuid,
        p: &DefectTypePatch,
    ) -> anyhow::Result<DefectTypeChange> {
        let r = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(format!(
            "update defect_types set name = coalesce($3, name), short_name = coalesce($4, short_name), \
             color = coalesce($5, color), updated_at = now() where project_id = $1 and id = $2 returning {COLUMNS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(p.name.as_deref().map(|n| no_nul(n.trim())))
        .bind(p.short_name.as_deref().map(no_nul))
        .bind(p.color.as_deref())
        .fetch_optional(&self.pool)
        .await;
        match r {
            Ok(Some(r)) => Ok(DefectTypeChange::Done(row(r))),
            Ok(None) => Ok(DefectTypeChange::NotFound),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(
                DefectTypeChange::Conflict("a defect type with this name exists".into()),
            ),
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes a custom type that no rule uses. Results keep the name and group they were
    /// triaged with.
    pub async fn delete_defect_type(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<DefectTypeChange> {
        let mut tx = self.pool.begin().await?;
        let t: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from defect_types where project_id = $1 and id = $2 for update"
        )))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(t) = t.map(row) else {
            return Ok(DefectTypeChange::NotFound);
        };
        if t.built_in {
            return Ok(DefectTypeChange::Conflict(
                "built-in defect types cannot be deleted".into(),
            ));
        }
        let used: bool = sqlx::query_scalar(
            "select exists(select 1 from rules where project_id = $1 and action->>'defectTypeId' = $2)",
        )
        .bind(project_id)
        .bind(id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        if used {
            return Ok(DefectTypeChange::Conflict(
                "a rule uses this defect type".into(),
            ));
        }
        sqlx::query("delete from defect_types where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // What Analysis learned about the type goes with it.
        sqlx::query("delete from triage_memory where project_id = $1 and defect_type_id = $2")
            .bind(project_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "update triage_suggestions set state = 'superseded', decided_at = now() \
             where project_id = $1 and defect_type_id = $2 and state = 'open'",
        )
        .bind(project_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(DefectTypeChange::Done(t))
    }
}
