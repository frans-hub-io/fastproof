//! Content-addressed attachments (sha256 per project) linked to the launches that use them.

use std::collections::HashMap;
use std::time::Duration;

use sqlx::Row;
use uuid::Uuid;

use crate::{Store, StoredAttachment, no_nul};

/// An attachment body that was stored in blob storage.
#[derive(Clone, Debug)]
pub struct NewAttachment {
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub original_name: Option<String>,
    pub blob_key: String,
}

impl Store {
    /// Marks the given hashes as seen now and returns those that exist with their ids (their
    /// blobs need not be uploaded again). Touching keeps them out of the garbage collector.
    pub async fn touch_attachments(
        &self,
        project_id: Uuid,
        sha256: &[String],
    ) -> anyhow::Result<HashMap<String, Uuid>> {
        if sha256.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "update attachments set last_seen_at = now() where project_id = $1 and sha256 = any($2) \
             returning id, sha256",
        )
        .bind(project_id)
        .bind(sha256)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id, sha)| (sha, id)).collect())
    }

    /// Inserts attachment rows (existing hashes keep their row) and links them to a launch.
    /// Returns the id of every hash.
    pub async fn insert_attachments(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        items: &[NewAttachment],
    ) -> anyhow::Result<HashMap<String, Uuid>> {
        if items.is_empty() {
            return Ok(HashMap::new());
        }
        let mut ids = Vec::with_capacity(items.len());
        let mut shas = Vec::with_capacity(items.len());
        let mut sizes = Vec::with_capacity(items.len());
        let mut cts = Vec::with_capacity(items.len());
        let mut names: Vec<Option<String>> = Vec::with_capacity(items.len());
        let mut keys = Vec::with_capacity(items.len());
        let mut seen = std::collections::HashSet::new();
        for a in items {
            if !seen.insert(a.sha256.as_str()) {
                continue;
            }
            ids.push(Uuid::now_v7());
            shas.push(a.sha256.clone());
            sizes.push(a.size);
            cts.push(no_nul(&a.content_type));
            names.push(a.original_name.as_deref().map(no_nul));
            keys.push(a.blob_key.clone());
        }
        let mut tx = self.pool.begin().await?;
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "insert into attachments (id, project_id, sha256, size, content_type, original_name, blob_key) \
             select i, $1, s, z, c, n, k from unnest($2::uuid[], $3::text[], $4::bigint[], $5::text[], $6::text[], $7::text[]) \
               as t(i, s, z, c, n, k) \
             on conflict (project_id, sha256) do update set last_seen_at = now() \
             returning id, sha256",
        )
        .bind(project_id)
        .bind(&ids)
        .bind(&shas)
        .bind(&sizes)
        .bind(&cts)
        .bind(&names)
        .bind(&keys)
        .fetch_all(&mut *tx)
        .await?;
        let all: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        sqlx::query(
            "insert into attachment_links (attachment_id, launch_id) select a, $2 from unnest($1::uuid[]) as a \
             on conflict do nothing",
        )
        .bind(&all)
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(|(id, sha)| (sha, id)).collect())
    }

    /// Links existing attachments to a launch.
    pub async fn link_attachments(&self, ids: &[Uuid], launch_id: Uuid) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "insert into attachment_links (attachment_id, launch_id) select a, $2 from unnest($1::uuid[]) as a \
             on conflict do nothing",
        )
        .bind(ids)
        .bind(launch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The subset of `ids` that are attachments of the project, with size and content type.
    pub async fn attachments_in_project(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, (i64, String, Option<String>)>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, i64, String, Option<String>)> = sqlx::query_as(
            "select id, size, content_type, original_name from attachments where project_id = $1 and id = any($2)",
        )
        .bind(project_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, s, c, n)| (id, (s, c, n)))
            .collect())
    }

    pub async fn attachment(
        &self,
        project_id: Uuid,
        id: Uuid,
    ) -> anyhow::Result<Option<StoredAttachment>> {
        let r = sqlx::query(
            "select id, project_id, sha256, size, content_type, original_name, blob_key from attachments \
             where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = r else { return Ok(None) };
        Ok(Some(StoredAttachment {
            id: r.try_get("id")?,
            project_id: r.try_get("project_id")?,
            sha256: r.try_get("sha256")?,
            size: r.try_get("size")?,
            content_type: r.try_get("content_type")?,
            original_name: r.try_get("original_name")?,
            blob_key: r.try_get("blob_key")?,
        }))
    }

    /// Deletes up to `batch` attachments that no launch links and that were not seen for
    /// `grace`; returns their blob keys (delete the blobs afterwards).
    pub async fn collect_attachments(
        &self,
        grace: Duration,
        batch: i64,
    ) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "delete from attachments where id in (select a.id from attachments a \
               where a.last_seen_at < now() - make_interval(secs => $1) \
                 and not exists (select 1 from attachment_links l where l.attachment_id = a.id) \
               order by a.last_seen_at limit $2 for update skip locked) \
             returning blob_key",
        )
        .bind(grace.as_secs_f64())
        .bind(batch)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Count of attachment rows of a project (tests, dedup checks).
    pub async fn attachment_count(&self, project_id: Uuid) -> anyhow::Result<i64> {
        Ok(
            sqlx::query_scalar("select count(*) from attachments where project_id = $1")
                .bind(project_id)
                .fetch_one(&self.pool)
                .await?,
        )
    }
}
