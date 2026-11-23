//! The identity of the blob store the attachments were written to, so that a changed
//! configuration cannot silently point the server at an empty store.

use crate::Store;

impl Store {
    /// The recorded identity of the blob store, if any.
    pub async fn blob_identity(&self) -> anyhow::Result<Option<String>> {
        Ok(
            sqlx::query_scalar("select identity from blob_identity where id")
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Records (or replaces) the identity of the blob store.
    pub async fn set_blob_identity(&self, identity: &str) -> anyhow::Result<()> {
        sqlx::query(
            "insert into blob_identity (id, identity) values (true, $1) \
             on conflict (id) do update set identity = excluded.identity, recorded_at = now()",
        )
        .bind(identity)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the identity only when none is recorded yet; returns the identity on record.
    pub async fn init_blob_identity(&self, identity: &str) -> anyhow::Result<String> {
        sqlx::query(
            "insert into blob_identity (id, identity) values (true, $1) on conflict (id) do nothing",
        )
        .bind(identity)
        .execute(&self.pool)
        .await?;
        Ok(
            sqlx::query_scalar("select identity from blob_identity where id")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// The blob keys of the `n` most recently seen attachments.
    pub async fn recent_blob_keys(&self, n: i64) -> anyhow::Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "select blob_key from attachments order by last_seen_at desc limit $1",
        )
        .bind(n)
        .fetch_all(&self.pool)
        .await?)
    }
}
