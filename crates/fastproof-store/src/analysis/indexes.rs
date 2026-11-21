//! Indexes of the analysis features that must not block writes while they are built: the
//! fingerprint index of the partitioned `results` (parent `ON ONLY`, then each partition
//! `CONCURRENTLY` and attached) and the trigram index of the fingerprint catalog.

use sqlx::PgPool;

const LOCK_KEY: i64 = 0x6670_616e_6978_0001;

/// Name, columns and predicate of the fingerprint index.
pub const FINGERPRINT_INDEX: &str = "results_fingerprint";
const FINGERPRINT_DEF: &str =
    "(project_id, fingerprint, launch_created_at) where fingerprint is not null";

fn safe_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Builds missing analysis indexes; returns how many partition indexes were built. Another
/// replica holding the lock makes this a no-op.
pub async fn ensure_analysis_indexes(pool: &PgPool) -> anyhow::Result<u32> {
    let mut conn = pool.acquire().await?;
    if !sqlx::query_scalar::<_, bool>("select pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?
    {
        return Ok(0);
    }
    let r = async {
        let mut built = 0;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create index if not exists {FINGERPRINT_INDEX} on only results {FINGERPRINT_DEF}"
        )))
        .execute(&mut *conn)
        .await?;
        let parts: Vec<(String, Option<String>)> = sqlx::query_as(
            "select c.relname::text, (select ci.relname::text from pg_inherits ii \
               join pg_class ci on ci.oid = ii.inhrelid join pg_index ix on ix.indexrelid = ci.oid \
               where ii.inhparent = to_regclass($1) and ix.indrelid = c.oid limit 1) \
             from pg_inherits i join pg_class c on c.oid = i.inhrelid join pg_class p on p.oid = i.inhparent \
             where p.relname = 'results' order by 1",
        )
        .bind(FINGERPRINT_INDEX)
        .fetch_all(&mut *conn)
        .await?;
        for (part, attached) in parts {
            if attached.is_some() {
                continue;
            }
            if !safe_ident(&part) {
                continue;
            }
            let child = format!("{part}_fp");
            let state: Option<bool> = sqlx::query_scalar(
                "select ix.indisvalid from pg_class c join pg_index ix on ix.indexrelid = c.oid where c.relname = $1",
            )
            .bind(&child)
            .fetch_optional(&mut *conn)
            .await?;
            if state == Some(false) {
                sqlx::query(sqlx::AssertSqlSafe(format!("drop index concurrently if exists {child}")))
                    .execute(&mut *conn)
                    .await?;
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "create index concurrently if not exists {child} on {part} {FINGERPRINT_DEF}"
            )))
            .execute(&mut *conn)
            .await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter index {FINGERPRINT_INDEX} attach partition {child}"
            )))
            .execute(&mut *conn)
            .await?;
            built += 1;
        }
        let trgm: bool = sqlx::query_scalar(
            "select exists(select 1 from pg_extension where extname = 'pg_trgm')",
        )
        .fetch_one(&mut *conn)
        .await?;
        if trgm {
            sqlx::query(
                "create index concurrently if not exists failure_fingerprints_message_trgm \
                 on failure_fingerprints using gin (message gin_trgm_ops)",
            )
            .execute(&mut *conn)
            .await?;
        }
        Ok::<_, anyhow::Error>(built)
    }
    .await;
    let _ = sqlx::query("select pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    r
}
