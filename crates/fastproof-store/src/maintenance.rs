//! Partitions, pending blob deletions and the purge of deleted projects.

use std::future::Future;

use uuid::Uuid;

use crate::Store;
use crate::purge::{Mode, Purge};

/// A queued deletion of every blob under a prefix.
#[derive(Clone, Debug)]
pub struct BlobDeletion {
    pub id: i64,
    pub prefix: String,
    pub project_id: Option<Uuid>,
}

impl Store {
    /// Creates the monthly partitions of results and attempts from `months_back` before to
    /// `months_ahead` after the current month; returns how many were created.
    pub async fn ensure_partitions(
        &self,
        months_back: i32,
        months_ahead: i32,
    ) -> anyhow::Result<i32> {
        Ok(
            sqlx::query_scalar("select fastproof_ensure_partitions($1, $2)")
                .bind(months_back)
                .bind(months_ahead)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Names of the partitions of a partitioned table.
    pub async fn partitions(&self, parent: &str) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select c.relname::text from pg_inherits i join pg_class c on c.oid = i.inhrelid \
             join pg_class p on p.oid = i.inhparent where p.relname = $1 order by 1",
        )
        .bind(parent)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn pending_blob_deletions(&self, limit: i64) -> anyhow::Result<Vec<BlobDeletion>> {
        let rows: Vec<(i64, String, Option<Uuid>)> = sqlx::query_as(
            "select id, prefix, project_id from blob_deletions order by id limit $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, prefix, project_id)| BlobDeletion {
                id,
                prefix,
                project_id,
            })
            .collect())
    }

    /// Deletes one queued blob while holding its queue row: `delete` runs only when the entry
    /// is still queued, and the entry goes in the same transaction. An upload of the same
    /// content cancels the entry ([`Store::cancel_blob_deletions`]) and so either removes it
    /// before this runs or waits until the blob is gone, then stores it again. Returns false
    /// when the entry was cancelled.
    pub async fn delete_blob_locked<F, Fut>(
        &self,
        d: &BlobDeletion,
        delete: F,
    ) -> anyhow::Result<bool>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let mut tx = self.pool.begin().await?;
        let found: Option<i64> =
            sqlx::query_scalar("select id from blob_deletions where id = $1 for update")
                .bind(d.id)
                .fetch_optional(&mut *tx)
                .await?;
        if found.is_none() {
            tx.commit().await?;
            return Ok(false);
        }
        delete().await?;
        sqlx::query("delete from blob_deletions where id = $1")
            .bind(d.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Completes a blob deletion: the entry goes, and a deleted project's rows with it. The rows
    /// go in bounded steps first ([`Store::purge_project_rows`]), each in its own short
    /// transaction; the project row and the entry go last, together. A crash in between leaves
    /// the entry queued, and the next run continues where this one stopped.
    pub async fn finish_blob_deletion(&self, d: &BlobDeletion) -> anyhow::Result<()> {
        if let Some(p) = d.project_id {
            self.purge_project_rows(p, None).await?;
        }
        let mut tx = self.pool.begin().await?;
        if let Some(p) = d.project_id {
            sqlx::query("delete from projects where id = $1 and deleted_at is not null")
                .bind(p)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("delete from blob_deletions where id = $1")
            .bind(d.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes every row of a deleted project except the project row, with at most
    /// [`Store::purge_batch`] rows per statement and each statement in its own transaction, so
    /// that neither the server's memory nor a transaction grows with the project. Children go
    /// before the rows they reference, so no cascade has anything left to delete. Idempotent;
    /// a project that is not deleted (or no longer exists) is left alone. `statements` stops
    /// the purge with an error after that many statements (tests simulate a crash with it).
    pub async fn purge_project_rows(
        &self,
        project_id: Uuid,
        statements: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut conn = self.pool.acquire().await?;
        let deleted: Option<bool> =
            sqlx::query_scalar("select deleted_at is not null from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&mut *conn)
                .await?;
        if deleted != Some(true) {
            return Ok(());
        }
        Purge::new(&mut conn, self.purge_batch, Mode::Steps)
            .with_budget(statements)
            .project(project_id)
            .await
    }

    pub async fn fail_blob_deletion(&self, d: &BlobDeletion, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "update blob_deletions set attempts = attempts + 1, last_error = $2 where id = $1",
        )
        .bind(d.id)
        .bind(crate::no_nul(error))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
