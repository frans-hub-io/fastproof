//! Case folders: a tree per project with `path_ids` (ancestors and self) and `depth` kept in
//! step on every create and move, lazy listing by level with subtree counts, and delete with
//! `moveUp` or `archiveCases`. Structure changes of a project are serialized by an advisory
//! lock, so concurrent moves cannot build a cycle.

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{
    CaseFolder, CaseFolderPatch, CaseFolderPathEntry, CaseFolderWrite, FolderCounts,
    FolderDeleteMode, FolderRow, TmError, TmLimits, TmResult, check_name, lock_project,
    violated_constraint,
};
use crate::Store;

const ROOT: Uuid = Uuid::nil();
const SIBLING_NAME: &str = "case_folders_sibling_name";

/// What deleting a folder did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderDeleted {
    /// Folders removed (the folder and, with `archiveCases`, its subfolders).
    pub folders: i64,
    /// Cases moved to the parent (`moveUp`).
    pub cases_moved: i64,
    /// Cases archived (`archiveCases`).
    pub cases_archived: i64,
}

const FOLDER_COLS: &str = "id, project_id, parent_id, name, position, path_ids, depth";

fn folder_row(r: &PgRow) -> sqlx::Result<FolderRow> {
    Ok(FolderRow {
        id: r.try_get("id")?,
        project_id: r.try_get("project_id")?,
        parent_id: r.try_get("parent_id")?,
        name: r.try_get("name")?,
        position: r.try_get("position")?,
        path_ids: r.try_get("path_ids")?,
        depth: r.try_get::<i16, _>("depth")? as i32,
    })
}

/// Folders with subtree counts and `hasChildren`; `$1` is the project, the caller appends the
/// condition on `f` and the order.
const VIEW_SELECT: &str = "select f.id, f.parent_id, f.name, f.position, f.depth, \
    coalesce(c.total, 0) as total, coalesce(c.automated, 0) as automated, \
    exists (select 1 from case_folders ch where ch.parent_id = f.id) as has_children \
    from case_folders f left join lateral (select count(*) as total, \
      count(*) filter (where k.automated) as automated \
      from case_folders d join cases k on k.project_id = $1 and k.folder_id = d.id and k.archived_at is null \
      where d.project_id = $1 and d.path_ids @> array[f.id]) c on true \
    where f.project_id = $1 ";

fn view_row(r: &PgRow) -> sqlx::Result<CaseFolder> {
    let total: i64 = r.try_get("total")?;
    let automated: i64 = r.try_get("automated")?;
    Ok(CaseFolder {
        id: r.try_get("id")?,
        parent_id: r.try_get("parent_id")?,
        name: r.try_get("name")?,
        position: r.try_get("position")?,
        depth: r.try_get::<i16, _>("depth")? as i32,
        counts: FolderCounts {
            total,
            manual: total - automated,
            automated,
        },
        has_children: r.try_get("has_children")?,
    })
}

fn name_taken(e: sqlx::Error) -> TmError {
    if violated_constraint(&e) == Some(SIBLING_NAME) {
        TmError::conflict(
            "folder_name_taken",
            "a folder with this name exists in the same parent",
        )
    } else {
        e.into()
    }
}

/// A folder of the project.
pub async fn folder(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<FolderRow>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select {FOLDER_COLS} from case_folders where project_id = $1 and id = $2"
    )))
    .bind(project_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    r.as_ref().map(folder_row).transpose()
}

/// The folder with its subtree counts.
pub async fn folder_view(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<CaseFolder>> {
    let r = sqlx::query(sqlx::AssertSqlSafe(format!("{VIEW_SELECT} and f.id = $2")))
        .bind(project_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    r.as_ref().map(view_row).transpose()
}

/// The path from the root to the folder (the folder last); empty when it does not exist.
pub async fn folder_path(
    conn: &mut PgConnection,
    project_id: Uuid,
    id: Uuid,
) -> sqlx::Result<Vec<CaseFolderPathEntry>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select a.id, a.name from case_folders f \
         cross join lateral unnest(f.path_ids) with ordinality as p(id, ord) \
         join case_folders a on a.id = p.id and a.project_id = $1 \
         where f.project_id = $1 and f.id = $2 order by p.ord",
    )
    .bind(project_id)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| CaseFolderPathEntry { id, name })
        .collect())
}

/// One level of the tree (`parent = None` is the top level), ordered by position and name.
/// An unknown parent is [`TmError::NotFound`].
pub async fn folder_level(
    conn: &mut PgConnection,
    project_id: Uuid,
    parent: Option<Uuid>,
) -> TmResult<Vec<CaseFolder>> {
    if let Some(p) = parent
        && folder(conn, project_id, p).await?.is_none()
    {
        return Err(TmError::NotFound("folder"));
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{VIEW_SELECT} and coalesce(f.parent_id, $3) = coalesce($2, $3) \
         order by f.position, lower(f.name), f.id"
    )))
    .bind(project_id)
    .bind(parent)
    .bind(ROOT)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(view_row).collect::<Result<_, _>>()?)
}

/// The whole tree in one query, parents before children (by depth, then position and name),
/// at most `max` folders; `true` when more exist. Subtree counts come from one pass over the
/// cases grouped by folder and spread to the ancestors through `path_ids`.
pub async fn all_folders(
    conn: &mut PgConnection,
    project_id: Uuid,
    max: usize,
) -> TmResult<(Vec<CaseFolder>, bool)> {
    let rows = sqlx::query(
        "with direct as (select folder_id, count(*) as total, count(*) filter (where automated) as automated \
           from cases where project_id = $1 and archived_at is null and folder_id is not null group by folder_id), \
         sub as (select a.id, sum(direct.total)::bigint as total, sum(direct.automated)::bigint as automated \
           from direct join case_folders d on d.id = direct.folder_id and d.project_id = $1 \
           cross join unnest(d.path_ids) as a(id) group by a.id) \
         select f.id, f.parent_id, f.name, f.position, f.depth, \
           coalesce(sub.total, 0) as total, coalesce(sub.automated, 0) as automated, \
           exists (select 1 from case_folders ch where ch.parent_id = f.id) as has_children \
         from case_folders f left join sub on sub.id = f.id where f.project_id = $1 \
         order by f.depth, f.position, lower(f.name), f.id limit $2",
    )
    .bind(project_id)
    .bind(max as i64 + 1)
    .fetch_all(&mut *conn)
    .await?;
    let mut items: Vec<CaseFolder> = rows.iter().map(view_row).collect::<Result<_, _>>()?;
    let more = items.len() > max;
    items.truncate(max);
    Ok((items, more))
}

async fn folder_count(conn: &mut PgConnection, project_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar("select count(*) from case_folders where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *conn)
        .await
}

async fn next_position(
    conn: &mut PgConnection,
    project_id: Uuid,
    parent: Option<Uuid>,
) -> sqlx::Result<i32> {
    sqlx::query_scalar(
        "select coalesce(max(position) + 1, 0) from case_folders \
         where project_id = $1 and coalesce(parent_id, $3) = coalesce($2, $3)",
    )
    .bind(project_id)
    .bind(parent)
    .bind(ROOT)
    .fetch_one(&mut *conn)
    .await
}

async fn parent_of(
    conn: &mut PgConnection,
    project_id: Uuid,
    parent: Option<Uuid>,
) -> TmResult<(Vec<Uuid>, i32)> {
    match parent {
        None => Ok((Vec::new(), 0)),
        Some(p) => {
            let f = folder(conn, project_id, p)
                .await?
                .ok_or(TmError::NotFound("folder"))?;
            Ok((f.path_ids, f.depth))
        }
    }
}

/// Creates a folder inside a transaction the caller owns (takes the project's folder lock).
pub async fn create_folder_in(
    conn: &mut PgConnection,
    project_id: Uuid,
    w: &CaseFolderWrite,
    limits: &TmLimits,
) -> TmResult<FolderRow> {
    let name = check_name("name", &w.name, limits.max_name_bytes)?;
    lock_project(conn, project_id, "folders").await?;
    let (mut path_ids, parent_depth) = parent_of(conn, project_id, w.parent_id).await?;
    let depth = parent_depth + 1;
    if depth as usize > limits.max_folder_depth {
        return Err(TmError::limit(
            "maxFolderDepth",
            limits.max_folder_depth as u64,
        ));
    }
    if folder_count(conn, project_id).await? as usize >= limits.max_folders {
        return Err(TmError::limit("maxFolders", limits.max_folders as u64));
    }
    let position = match w.position {
        Some(p) => p,
        None => next_position(conn, project_id, w.parent_id).await?,
    };
    let id = Uuid::now_v7();
    path_ids.push(id);
    sqlx::query(
        "insert into case_folders (id, project_id, parent_id, name, position, path_ids, depth) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(project_id)
    .bind(w.parent_id)
    .bind(&name)
    .bind(position)
    .bind(&path_ids)
    .bind(depth as i16)
    .execute(&mut *conn)
    .await
    .map_err(name_taken)?;
    Ok(FolderRow {
        id,
        project_id,
        parent_id: w.parent_id,
        name,
        position,
        path_ids,
        depth,
    })
}

/// Returns the folder at the path of names under `parent`, creating missing folders
/// (case-insensitive match; code sync and imports). Call inside a transaction. An empty path
/// is `parent` itself.
pub async fn ensure_folder_path(
    conn: &mut PgConnection,
    project_id: Uuid,
    parent: Option<Uuid>,
    names: &[&str],
    limits: &TmLimits,
) -> TmResult<Option<Uuid>> {
    let mut current = parent;
    for raw in names {
        let name = check_name("folder name", raw, limits.max_name_bytes)?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "select id from case_folders where project_id = $1 \
             and coalesce(parent_id, $3) = coalesce($2, $3) and lower(name) = lower($4)",
        )
        .bind(project_id)
        .bind(current)
        .bind(ROOT)
        .bind(&name)
        .fetch_optional(&mut *conn)
        .await?;
        current = Some(match existing {
            Some(id) => id,
            None => {
                create_folder_in(
                    conn,
                    project_id,
                    &CaseFolderWrite {
                        name,
                        parent_id: current,
                        position: None,
                    },
                    limits,
                )
                .await?
                .id
            }
        });
    }
    Ok(current)
}

impl Store {
    /// A folder of the project with its counts.
    pub async fn case_folder(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<CaseFolder>> {
        let mut conn = self.pool.acquire().await?;
        Ok(folder_view(&mut conn, project_id, id).await?)
    }

    /// One level of the folder tree.
    pub async fn case_folder_level(
        &self,
        project_id: Uuid,
        parent: Option<Uuid>,
    ) -> TmResult<Vec<CaseFolder>> {
        let mut conn = self.pool.acquire().await.map_err(TmError::from)?;
        folder_level(&mut conn, project_id, parent).await
    }

    /// Every folder of the project (see [`all_folders`]).
    pub async fn case_folder_tree(
        &self,
        project_id: Uuid,
        max: usize,
    ) -> TmResult<(Vec<CaseFolder>, bool)> {
        let mut conn = self.pool.acquire().await.map_err(TmError::from)?;
        all_folders(&mut conn, project_id, max).await
    }

    /// Creates a folder.
    pub async fn create_case_folder(
        &self,
        project_id: Uuid,
        w: &CaseFolderWrite,
        limits: &TmLimits,
    ) -> TmResult<CaseFolder> {
        let mut tx = self.pool.begin().await?;
        let f = create_folder_in(&mut tx, project_id, w, limits).await?;
        let view = folder_view(&mut tx, project_id, f.id)
            .await?
            .ok_or(TmError::NotFound("folder"))?;
        tx.commit().await?;
        Ok(view)
    }

    /// Renames, reorders or moves a folder (with its subtree).
    pub async fn update_case_folder(
        &self,
        project_id: Uuid,
        id: Uuid,
        p: &CaseFolderPatch,
        limits: &TmLimits,
    ) -> TmResult<CaseFolder> {
        let name = p
            .name
            .as_deref()
            .map(|n| check_name("name", n, limits.max_name_bytes))
            .transpose()?;
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "folders").await?;
        let f = folder(&mut tx, project_id, id)
            .await?
            .ok_or(TmError::NotFound("folder"))?;
        if let Some(new_parent) = p.parent_id
            && new_parent != f.parent_id
        {
            let (prefix, parent_depth) = parent_of(&mut tx, project_id, new_parent).await?;
            if prefix.contains(&f.id) {
                return Err(TmError::bad_request(
                    "a folder cannot move into itself or one of its subfolders",
                ));
            }
            let deepest: i16 = sqlx::query_scalar(
                "select max(depth) from case_folders where project_id = $1 and path_ids @> array[$2]::uuid[]",
            )
            .bind(project_id)
            .bind(f.id)
            .fetch_one(&mut *tx)
            .await?;
            let new_depth = parent_depth + 1;
            if (new_depth + deepest as i32 - f.depth) as usize > limits.max_folder_depth {
                return Err(TmError::limit(
                    "maxFolderDepth",
                    limits.max_folder_depth as u64,
                ));
            }
            let position = match p.position {
                Some(x) => x,
                None => next_position(&mut tx, project_id, new_parent).await?,
            };
            sqlx::query(
                "update case_folders set parent_id = $3, position = $4, name = $5, updated_at = now() \
                 where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(f.id)
            .bind(new_parent)
            .bind(position)
            .bind(name.as_deref().unwrap_or(&f.name))
            .execute(&mut *tx)
            .await
            .map_err(name_taken)?;
            sqlx::query(
                "update case_folders set path_ids = $3::uuid[] || path_ids[$4:], \
                 depth = depth + $5, updated_at = now() \
                 where project_id = $1 and path_ids @> array[$2]::uuid[]",
            )
            .bind(project_id)
            .bind(f.id)
            .bind(&prefix)
            .bind(f.depth)
            .bind((new_depth - f.depth) as i16)
            .execute(&mut *tx)
            .await?;
        } else if let Some(position) = p.position {
            sqlx::query(
                "update case_folders set position = $3, updated_at = now() where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(f.id)
            .bind(position)
            .execute(&mut *tx)
            .await?;
        }
        let moved = p.parent_id.is_some_and(|np| np != f.parent_id);
        if let Some(name) = name.filter(|_| !moved) {
            sqlx::query(
                "update case_folders set name = $3, updated_at = now() where project_id = $1 and id = $2",
            )
            .bind(project_id)
            .bind(f.id)
            .bind(&name)
            .execute(&mut *tx)
            .await
            .map_err(name_taken)?;
        }
        let view = folder_view(&mut tx, project_id, f.id)
            .await?
            .ok_or(TmError::NotFound("folder"))?;
        tx.commit().await?;
        Ok(view)
    }

    /// Deletes a folder. A folder with subfolders or (not archived) cases needs `mode`:
    /// `moveUp` moves its subfolders and cases to its parent, `archiveCases` archives every
    /// case of the subtree and deletes the subfolders. Moved and archived cases get a new
    /// revision and `updated_by = by`. Cases left in deleted folders (archived ones) lose the
    /// folder, and the folders go deepest first, in bounded chunks (no cascade over the tree).
    pub async fn delete_case_folder(
        &self,
        project_id: Uuid,
        id: Uuid,
        mode: Option<FolderDeleteMode>,
        by: &str,
    ) -> TmResult<FolderDeleted> {
        let mut tx = self.pool.begin().await?;
        lock_project(&mut tx, project_id, "folders").await?;
        let f = folder(&mut tx, project_id, id)
            .await?
            .ok_or(TmError::NotFound("folder"))?;
        let (children, cases): (bool, i64) = sqlx::query_as(
            "select exists (select 1 from case_folders where parent_id = $2), \
             (select count(*) from cases where project_id = $1 and folder_id = $2 and archived_at is null)",
        )
        .bind(project_id)
        .bind(f.id)
        .fetch_one(&mut *tx)
        .await?;
        let mut out = FolderDeleted::default();
        match mode {
            None if children || cases > 0 => {
                return Err(TmError::conflict(
                    "folder_not_empty",
                    "the folder has subfolders or cases: delete with mode=moveUp or mode=archiveCases",
                ));
            }
            None => {}
            Some(FolderDeleteMode::MoveUp) => {
                sqlx::query(
                    "update case_folders set parent_id = $3, updated_at = now() \
                     where project_id = $1 and parent_id = $2",
                )
                .bind(project_id)
                .bind(f.id)
                .bind(f.parent_id)
                .execute(&mut *tx)
                .await
                .map_err(name_taken)?;
                sqlx::query(
                    "update case_folders set path_ids = array_remove(path_ids, $2), depth = depth - 1, \
                     updated_at = now() where project_id = $1 and path_ids @> array[$2]::uuid[] and id <> $2",
                )
                .bind(project_id)
                .bind(f.id)
                .execute(&mut *tx)
                .await?;
                out.cases_moved = sqlx::query(
                    "update cases set folder_id = $3, revision = revision + 1, updated_at = now(), \
                     updated_by = $4 where project_id = $1 and folder_id = $2",
                )
                .bind(project_id)
                .bind(f.id)
                .bind(f.parent_id)
                .bind(by)
                .execute(&mut *tx)
                .await?
                .rows_affected() as i64;
            }
            Some(FolderDeleteMode::ArchiveCases) => {
                out.cases_archived = sqlx::query(
                    "update cases set archived_at = now(), revision = revision + 1, updated_at = now(), \
                     updated_by = $3 where project_id = $1 and archived_at is null and folder_id in \
                     (select id from case_folders where project_id = $1 and path_ids @> array[$2]::uuid[])",
                )
                .bind(project_id)
                .bind(f.id)
                .bind(by)
                .execute(&mut *tx)
                .await?
                .rows_affected() as i64;
                out.folders = sqlx::query_scalar::<_, i64>(
                    "select count(*) from case_folders where project_id = $1 and path_ids @> array[$2]::uuid[]",
                )
                .bind(project_id)
                .bind(f.id)
                .fetch_one(&mut *tx)
                .await?
                    - 1;
            }
        }
        crate::purge::Purge::new(&mut tx, self.purge_batch(), crate::purge::Mode::Atomic)
            .folder_tree(project_id, f.id)
            .await?;
        out.folders += 1;
        tx.commit().await?;
        Ok(out)
    }

    /// The path from the root to a folder (the folder last).
    pub async fn case_folder_path(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Vec<CaseFolderPathEntry>> {
        let mut conn = self.pool.acquire().await?;
        Ok(folder_path(&mut conn, project_id, id).await?)
    }
}
