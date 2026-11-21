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
    /// Returns an id per hash: the attachment's, or, when the hash is stored with another
    /// content type or name, the id of a reference that keeps this upload's type and name
    /// (content is stored once per hash, its metadata per upload).
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
        // Hash order: concurrent uploads sharing attachments lock their rows in one order.
        let mut sorted: Vec<&NewAttachment> = items.iter().collect();
        sorted.sort_by(|a, b| a.sha256.cmp(&b.sha256));
        for a in sorted {
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
        let wanted: HashMap<&str, (&str, Option<&str>)> = shas
            .iter()
            .zip(cts.iter().zip(names.iter()))
            .map(|(s, (c, n))| (s.as_str(), (c.as_str(), n.as_deref())))
            .collect();
        let mut tx = self.pool.begin().await?;
        let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
            "insert into attachments (id, project_id, sha256, size, content_type, original_name, blob_key) \
             select i, $1, s, z, c, n, k from unnest($2::uuid[], $3::text[], $4::bigint[], $5::text[], $6::text[], $7::text[]) \
               as t(i, s, z, c, n, k) \
             on conflict (project_id, sha256) do update set last_seen_at = now() \
             returning id, sha256, content_type, original_name",
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
        let mut all: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        all.sort();
        all.dedup();
        sqlx::query(
            "insert into attachment_links (attachment_id, launch_id) select a, $2 from unnest($1::uuid[]) as a \
             on conflict do nothing",
        )
        .bind(&all)
        .bind(launch_id)
        .execute(&mut *tx)
        .await?;
        let mut out: HashMap<String, Uuid> = HashMap::with_capacity(rows.len());
        let (mut ref_ids, mut ref_atts, mut ref_cts, mut ref_names, mut ref_shas) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (id, sha, ct, name) in rows {
            match wanted.get(sha.as_str()) {
                Some((c, n)) if *c != ct.as_str() || *n != name.as_deref() => {
                    ref_ids.push(Uuid::now_v7());
                    ref_atts.push(id);
                    ref_cts.push(c.to_string());
                    ref_names.push(n.map(str::to_string));
                    ref_shas.push(sha);
                }
                _ => {
                    out.insert(sha, id);
                }
            }
        }
        if !ref_ids.is_empty() {
            let refs: Vec<(Uuid, Uuid)> = sqlx::query_as(
                "insert into attachment_refs (id, attachment_id, launch_id, content_type, original_name) \
                 select i, a, $2, c, n from unnest($1::uuid[], $3::uuid[], $4::text[], $5::text[]) as t(i, a, c, n) \
                 on conflict (attachment_id, launch_id, content_type, coalesce(original_name, '')) \
                   do update set content_type = excluded.content_type \
                 returning id, attachment_id",
            )
            .bind(&ref_ids)
            .bind(launch_id)
            .bind(&ref_atts)
            .bind(&ref_cts)
            .bind(&ref_names)
            .fetch_all(&mut *tx)
            .await?;
            let by_att: HashMap<Uuid, Uuid> = refs.into_iter().map(|(r, a)| (a, r)).collect();
            for (sha, att) in ref_shas.into_iter().zip(ref_atts) {
                out.insert(sha, by_att.get(&att).copied().unwrap_or(att));
            }
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Cancels queued deletions of these blob keys. Call before storing a blob again: a
    /// deletion in progress holds its queue row, so this waits for it to finish first.
    pub async fn cancel_blob_deletions(&self, blob_keys: &[String]) -> anyhow::Result<u64> {
        if blob_keys.is_empty() {
            return Ok(0);
        }
        Ok(
            sqlx::query("delete from blob_deletions where project_id is null and prefix = any($1)")
                .bind(blob_keys)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    /// Links existing attachments to a launch.
    pub async fn link_attachments(&self, ids: &[Uuid], launch_id: Uuid) -> anyhow::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut ids = ids.to_vec();
        ids.sort();
        ids.dedup();
        sqlx::query(
            "insert into attachment_links (attachment_id, launch_id) select a, $2 from unnest($1::uuid[]) as a \
             on conflict do nothing",
        )
        .bind(&ids)
        .bind(launch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The subset of `ids` that are attachments (or attachment references) of the project:
    /// `id -> (attachment id, size, content type, original name)`.
    pub async fn attachments_in_project(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, (Uuid, i64, String, Option<String>)>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, Uuid, i64, String, Option<String>)> = sqlx::query_as(
            "select id, id, size, content_type, original_name from attachments where project_id = $1 and id = any($2) \
             union all \
             select r.id, a.id, a.size, r.content_type, r.original_name from attachment_refs r \
               join attachments a on a.id = r.attachment_id where a.project_id = $1 and r.id = any($2)",
        )
        .bind(project_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, a, s, c, n)| (id, (a, s, c, n)))
            .collect())
    }

    /// An attachment by its id or the id of a reference to it (with the reference's content
    /// type and name).
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
        let r = match r {
            Some(r) => r,
            None => {
                let r = sqlx::query(
                    "select r.id, a.project_id, a.sha256, a.size, r.content_type, r.original_name, a.blob_key \
                     from attachment_refs r join attachments a on a.id = r.attachment_id \
                     where a.project_id = $1 and r.id = $2",
                )
                .bind(project_id)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
                let Some(r) = r else { return Ok(None) };
                r
            }
        };
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
    /// `grace`, and queues their blobs in `blob_deletions` (same transaction); returns how
    /// many. The blobs are deleted by the purge job, which an upload of the same content
    /// cancels ([`Store::cancel_blob_deletions`]).
    pub async fn collect_attachments(&self, grace: Duration, batch: i64) -> anyhow::Result<u64> {
        let r = sqlx::query(
            "with gone as (delete from attachments where id in (select a.id from attachments a \
               where a.last_seen_at < now() - make_interval(secs => $1) \
                 and not exists (select 1 from attachment_links l where l.attachment_id = a.id) \
               order by a.last_seen_at limit $2 for update skip locked) returning blob_key) \
             insert into blob_deletions (prefix, project_id) select blob_key, null from gone",
        )
        .bind(grace.as_secs_f64())
        .bind(batch)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
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
