//! Projects: the scope unit of access control.

use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::{Project, Store, no_nul};

const COLUMNS: &str = "p.id, p.key, p.name, p.description, p.created_at, p.last_launch_at, p.launch_count, \
    ll.id as ll_id, ll.number as ll_number, ll.name as ll_name, ll.status as ll_status, ll.started_at as ll_started_at, \
    ll.count_total, ll.count_passed, ll.count_failed, ll.count_broken, ll.count_skipped, ll.count_unknown, \
    ll.count_flaky, ll.count_muted";

const FROM: &str = "from projects p left join lateral (select id, number, name, status, started_at, count_total, \
    count_passed, count_failed, count_broken, count_skipped, count_unknown, count_flaky, count_muted \
    from launches where project_id = p.id and not debug order by number desc limit 1) ll on true";

fn last_launch(r: &PgRow) -> Result<Option<crate::LastLaunch>, sqlx::Error> {
    let Some(id) = r.try_get::<Option<Uuid>, _>("ll_id")? else {
        return Ok(None);
    };
    let c = |n: &str| -> Result<i64, sqlx::Error> { Ok(r.try_get::<i32, _>(n)? as i64) };
    Ok(Some(crate::LastLaunch {
        id,
        number: r.try_get("ll_number")?,
        name: r.try_get("ll_name")?,
        status: r.try_get("ll_status")?,
        started_at: r.try_get("ll_started_at")?,
        counts: crate::Counts {
            total: c("count_total")?,
            passed: c("count_passed")?,
            failed: c("count_failed")?,
            broken: c("count_broken")?,
            skipped: c("count_skipped")?,
            unknown: c("count_unknown")?,
            flaky: c("count_flaky")?,
            muted: c("count_muted")?,
        },
    }))
}

fn plain_row(r: &PgRow) -> Result<Project, sqlx::Error> {
    Ok(Project {
        id: r.try_get("id")?,
        key: r.try_get("key")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        created_at: r.try_get("created_at")?,
        last_launch_at: r.try_get("last_launch_at")?,
        launch_count: r.try_get("launch_count")?,
        last_launch: None,
    })
}

fn row(r: &PgRow) -> Result<Project, sqlx::Error> {
    Ok(Project {
        last_launch: last_launch(r)?,
        id: r.try_get("id")?,
        key: r.try_get("key")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        created_at: r.try_get("created_at")?,
        last_launch_at: r.try_get("last_launch_at")?,
        launch_count: r.try_get("launch_count")?,
    })
}

/// Outcome of creating a project.
#[derive(Debug)]
pub enum CreateProject {
    Created(Box<Project>),
    /// The key is taken (also while a deleted project with that key is being purged).
    Exists,
}

/// Changes of a project; `None` keeps a field.
#[derive(Debug, Default)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub description: Option<String>,
}

impl Store {
    pub async fn create_project(
        &self,
        key: &str,
        name: &str,
        description: &str,
    ) -> anyhow::Result<CreateProject> {
        let r = sqlx::query(
            "insert into projects (id, key, name, description) values ($1, $2, $3, $4) \
             on conflict (key) do nothing \
             returning id, key, name, description, created_at, last_launch_at, launch_count",
        )
        .bind(Uuid::now_v7())
        .bind(key)
        .bind(no_nul(name))
        .bind(no_nul(description))
        .fetch_optional(&self.pool)
        .await?;
        Ok(match r {
            Some(r) => CreateProject::Created(Box::new(plain_row(&r)?)),
            None => CreateProject::Exists,
        })
    }

    /// A project that is not deleted.
    pub async fn project(&self, key: &str) -> anyhow::Result<Option<Project>> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} {FROM} where p.key = $1 and p.deleted_at is null"
        )))
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(row).transpose()?)
    }

    /// Every project that is not deleted, by key.
    pub async fn projects(&self) -> anyhow::Result<Vec<Project>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} {FROM} where p.deleted_at is null order by p.key limit 10000"
        )))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row).collect::<Result<_, _>>()?)
    }

    pub async fn project_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select key from projects where deleted_at is null order by key limit 10000",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn update_project(
        &self,
        key: &str,
        patch: &ProjectPatch,
    ) -> anyhow::Result<Option<Project>> {
        let r = sqlx::query(
            "update projects set name = coalesce($2, name), description = coalesce($3, description), \
             updated_at = now() where key = $1 and deleted_at is null \
             returning id, key, name, description, created_at, last_launch_at, launch_count",
        )
        .bind(key)
        .bind(patch.name.as_deref().map(no_nul))
        .bind(patch.description.as_deref().map(no_nul))
        .fetch_optional(&self.pool)
        .await?;
        if r.is_none() {
            return Ok(None);
        }
        self.project(key).await
    }

    /// When a project with this key was deleted (the key may be bound in the access file).
    pub async fn deleted_key(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
        Ok(
            sqlx::query_scalar("select deleted_at from deleted_project_keys where key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Forgets that a key belonged to a deleted project (it is being used again on purpose).
    pub async fn forget_deleted_key(&self, key: &str) -> anyhow::Result<()> {
        sqlx::query("delete from deleted_project_keys where key = $1")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Marks a project deleted and queues the deletion of its blobs (its prefix and the files of
    /// its import and export jobs under `tm-jobs/`); the rows are removed by
    /// the purge job once the blobs are gone. Remembers the key in `deleted_project_keys` and
    /// emits `project.deleted` (`{projectId, key, deletedAt}`) so that analytics sinks drop
    /// the project's rows. Returns false when there was no such project.
    pub async fn delete_project(&self, key: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "update projects set deleted_at = now(), updated_at = now() \
             where key = $1 and deleted_at is null returning id",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return Ok(false);
        };
        sqlx::query("insert into blob_deletions (prefix, project_id) values ($1, $2)")
            .bind(format!("projects/{key}"))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "insert into blob_deletions (prefix, project_id) \
             select b, null from tm_jobs j, lateral (values (j.params ->> 'blob'), (j.params ->> 'zip')) as v(b) \
             where j.project_id = $1 and b like 'tm-jobs/%'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "insert into blob_deletions (prefix, project_id) \
             select k, null from launch_exports e, lateral (values (e.blob_key), \
               ('tm-jobs/' || e.id::text || '/launch-export.' || e.format)) as v(k) \
             where e.project_id = $1 and k is not null \
               and (k = e.blob_key or e.status in ('queued', 'running'))",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("update ingest_jobs set status = 'failed', last_error = 'project deleted', updated_at = now() \
             where status in ('queued', 'running') and upload_id in (select id from uploads where project_id = $1)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "insert into deleted_project_keys (key, project_id, deleted_at) values ($1, $2, now()) \
             on conflict (key) do update set project_id = excluded.project_id, deleted_at = excluded.deleted_at \
             returning deleted_at",
        )
        .bind(key)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        crate::events::emit(
            &mut tx,
            "project.deleted",
            Some(key),
            serde_json::json!({"projectId": id, "key": key, "deletedAt": at}),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
