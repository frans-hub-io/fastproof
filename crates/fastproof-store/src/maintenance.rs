//! Partitions, pending blob deletions and the purge of deleted projects.

use uuid::Uuid;

use crate::Store;

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

    /// Completes a blob deletion: the entry goes, and a deleted project's rows with it.
    pub async fn finish_blob_deletion(&self, d: &BlobDeletion) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        if let Some(p) = d.project_id {
            sqlx::query("delete from attempts where project_id = $1")
                .bind(p)
                .execute(&mut *tx)
                .await?;
            sqlx::query("delete from results where project_id = $1")
                .bind(p)
                .execute(&mut *tx)
                .await?;
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
