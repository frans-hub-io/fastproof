//! Saved FPQL queries, personal and shared (§8). A personal query is visible only to its
//! owner (others get 404); a shared one to everybody who reads cases, and only callers who
//! may write cases create, change or delete it. Each query is checked like a query that runs
//! (names resolved, saved queries it names without cycles).
//!
//! Ownership is the caller's owner key (`QueryCtx::owner`: kind and name, or the managed token
//! id), not the principal name: a token named like a user is not that user. `owner` keeps the
//! name for display.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::query::{QueryCtx, prepare};
use super::{
    QueryEntity, SavedQuery, SavedQueryList, SavedQueryPatch, SavedQueryWrite, TmError, TmLimits,
    TmResult, check_name, lock_project, violated_constraint,
};
use crate::{Store, no_nul};

/// Bytes of a saved query's name, at most.
pub const MAX_SAVED_QUERY_NAME: usize = 200;

const COLS: &str =
    "id, entity, name, query, owner, owner_key, shared, position, created_at, updated_at";

fn row(r: &PgRow, who: &QueryCtx) -> sqlx::Result<SavedQuery> {
    let entity: String = r.try_get("entity")?;
    let owner_key: String = r.try_get("owner_key")?;
    Ok(SavedQuery {
        id: r.try_get("id")?,
        entity: QueryEntity::parse(&entity).unwrap_or(QueryEntity::Cases),
        name: r.try_get("name")?,
        query: r.try_get("query")?,
        mine: !who.owner.is_empty() && owner_key == who.owner,
        owner: r.try_get("owner")?,
        shared: r.try_get("shared")?,
        position: r.try_get("position")?,
        created_at: r.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: r.try_get::<DateTime<Utc>, _>("updated_at")?,
    })
}

fn forbidden(m: &str) -> TmError {
    TmError::Forbidden {
        code: "forbidden",
        message: m.to_string(),
    }
}

fn name_taken(e: &sqlx::Error) -> Option<TmError> {
    (violated_constraint(e) == Some("saved_queries_name_unique")).then(|| {
        TmError::conflict(
            "saved_query_name_taken",
            "you already have a saved query with this name",
        )
    })
}

/// A query of this caller: their own, or a shared one; 404 otherwise.
async fn visible(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
    who: &QueryCtx,
    lock: bool,
) -> TmResult<SavedQuery> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {COLS} from saved_queries where project_id = $1 and id = $2 \
         and ((owner_key = $3 and $3 <> '') or shared){}",
        if lock { " for update" } else { "" }
    )))
    .bind(project_id)
    .bind(id)
    .bind(no_nul(&who.owner))
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(TmError::NotFound("saved query"))?;
    Ok(row(&r, who)?)
}

/// A caller that may own saved queries (an owner key and a name).
fn owner_of(who: &QueryCtx) -> TmResult<&str> {
    if who.owner.trim().is_empty() || who.me.trim().is_empty() {
        return Err(TmError::bad_request(
            "saved queries need a signed-in caller",
        ));
    }
    Ok(&who.owner)
}

async fn check_query(
    conn: &mut PgConnection,
    project_id: Uuid,
    entity: QueryEntity,
    query: &str,
    who: &QueryCtx,
    saving: Option<Uuid>,
) -> TmResult<()> {
    if query.trim().is_empty() {
        return Err(TmError::bad_request("query must not be empty"));
    }
    let ctx = QueryCtx {
        saving,
        ..who.clone()
    };
    prepare(conn, project_id, entity, query, &ctx).await?;
    Ok(())
}

async fn check_counts(
    conn: &mut PgConnection,
    project_id: Uuid,
    entity: QueryEntity,
    owner: &str,
    shared: bool,
    limits: &TmLimits,
) -> TmResult<()> {
    let (own, all_shared): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where owner_key = $3), count(*) filter (where shared) \
         from saved_queries where project_id = $1 and entity = $2",
    )
    .bind(project_id)
    .bind(entity.as_str())
    .bind(no_nul(owner))
    .fetch_one(&mut *conn)
    .await?;
    if own as usize >= limits.max_saved_queries {
        return Err(TmError::limit(
            "maxSavedQueries",
            limits.max_saved_queries as u64,
        ));
    }
    if shared && all_shared as usize >= 2 * limits.max_saved_queries {
        return Err(TmError::limit(
            "maxSharedQueries",
            2 * limits.max_saved_queries as u64,
        ));
    }
    Ok(())
}

impl Store {
    /// The caller's queries and the shared ones of an entity, by position and name.
    pub async fn list_saved_queries(
        &self,
        project_id: Uuid,
        entity: QueryEntity,
        who: &QueryCtx,
    ) -> TmResult<SavedQueryList> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLS} from saved_queries where project_id = $1 and entity = $2 \
             and ((owner_key = $3 and $3 <> '') or shared) \
             order by shared, position, lower(name), id limit 1000"
        )))
        .bind(project_id)
        .bind(entity.as_str())
        .bind(no_nul(&who.owner))
        .fetch_all(&self.pool)
        .await?;
        Ok(SavedQueryList {
            items: rows
                .iter()
                .map(|r| row(r, who))
                .collect::<sqlx::Result<_>>()?,
        })
    }

    /// Saves a query of the caller; `shared` needs `can_write`.
    pub async fn create_saved_query(
        &self,
        project_id: Uuid,
        who: &QueryCtx,
        can_write: bool,
        w: &SavedQueryWrite,
        limits: &TmLimits,
    ) -> TmResult<SavedQuery> {
        let owner = owner_of(who)?;
        let shared = w.shared.unwrap_or(false);
        if shared && !can_write {
            return Err(forbidden("sharing a query needs cases:write"));
        }
        let name = check_name("name", &w.name, MAX_SAVED_QUERY_NAME)?;
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "saved_queries").await?;
        super::query::set_timeout(&mut tx).await?;
        check_query(&mut tx, project_id, w.entity, &w.query, who, None).await?;
        super::query::reset_timeout(&mut tx).await?;
        check_counts(&mut tx, project_id, w.entity, owner, shared, limits).await?;
        let position = match w.position {
            Some(p) => p.clamp(0, 1_000_000),
            None => sqlx::query_scalar::<_, Option<i32>>(
                "select max(position) + 1 from saved_queries where project_id = $1 and entity = $2 and owner_key = $3",
            )
            .bind(project_id)
            .bind(w.entity.as_str())
            .bind(no_nul(owner))
            .fetch_one(&mut *tx)
            .await?
            .unwrap_or(0)
            .min(1_000_000),
        };
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into saved_queries (id, project_id, entity, name, query, owner, owner_key, shared, position) \
             values ($1, $2, $3, $4, $5, $6, $9, $7, $8) returning {COLS}"
        )))
        .bind(Uuid::now_v7())
        .bind(project_id)
        .bind(w.entity.as_str())
        .bind(no_nul(&name))
        .bind(no_nul(w.query.trim()))
        .bind(no_nul(&who.me))
        .bind(shared)
        .bind(position)
        .bind(no_nul(owner))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| name_taken(&e).unwrap_or_else(|| e.into()))?;
        let q = row(&r, who)?;
        tx.commit().await?;
        Ok(q)
    }

    /// Changes a saved query: the owner's personal one, or a shared one with `can_write`.
    /// Returns the query before and after.
    pub async fn update_saved_query(
        &self,
        project_id: Uuid,
        id: Uuid,
        who: &QueryCtx,
        can_write: bool,
        p: &SavedQueryPatch,
        limits: &TmLimits,
    ) -> TmResult<(SavedQuery, SavedQuery)> {
        let name = p
            .name
            .as_deref()
            .map(|n| check_name("name", n, MAX_SAVED_QUERY_NAME))
            .transpose()?;
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "saved_queries").await?;
        let cur = visible(&mut tx, project_id, id, who, true).await?;
        if cur.shared && !can_write {
            return Err(forbidden("changing a shared query needs cases:write"));
        }
        if let Some(s) = p.shared
            && s != cur.shared
        {
            if !can_write {
                return Err(forbidden("sharing a query needs cases:write"));
            }
            if !cur.mine {
                return Err(forbidden("only the owner shares or unshares a query"));
            }
            if s {
                let shared: i64 = sqlx::query_scalar(
                    "select count(*) from saved_queries where project_id = $1 and entity = $2 and shared",
                )
                .bind(project_id)
                .bind(cur.entity.as_str())
                .fetch_one(&mut *tx)
                .await?;
                if shared as usize >= 2 * limits.max_saved_queries {
                    return Err(TmError::limit(
                        "maxSharedQueries",
                        2 * limits.max_saved_queries as u64,
                    ));
                }
            }
        }
        if let Some(q) = &p.query {
            super::query::set_timeout(&mut tx).await?;
            check_query(&mut tx, project_id, cur.entity, q, who, Some(id)).await?;
            super::query::reset_timeout(&mut tx).await?;
        }
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "update saved_queries set name = coalesce($3, name), query = coalesce($4, query), \
             shared = coalesce($5, shared), position = coalesce($6, position), updated_at = now() \
             where project_id = $1 and id = $2 returning {COLS}"
        )))
        .bind(project_id)
        .bind(id)
        .bind(name.as_deref().map(no_nul))
        .bind(p.query.as_deref().map(|q| no_nul(q.trim())))
        .bind(p.shared)
        .bind(p.position.map(|x| x.clamp(0, 1_000_000)))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| name_taken(&e).unwrap_or_else(|| e.into()))?;
        let after = row(&r, who)?;
        tx.commit().await?;
        Ok((cur, after))
    }

    /// Deletes a saved query: the owner's personal one, or a shared one with `can_write`.
    pub async fn delete_saved_query(
        &self,
        project_id: Uuid,
        id: Uuid,
        who: &QueryCtx,
        can_write: bool,
    ) -> TmResult<SavedQuery> {
        let mut tx = self.pool.begin().await?;
        let cur = visible(&mut tx, project_id, id, who, true).await?;
        if cur.shared && !can_write {
            return Err(forbidden("deleting a shared query needs cases:write"));
        }
        sqlx::query("delete from saved_queries where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(cur)
    }
}
