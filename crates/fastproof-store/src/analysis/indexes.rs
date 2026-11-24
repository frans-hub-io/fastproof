//! Indexes that must not block writes while they are built: the fingerprint and suite indexes
//! of the partitioned `results` (parent `ON ONLY`, then each partition `CONCURRENTLY` and
//! attached; partitions created later get them from the parent) and the trigram index of the
//! fingerprint catalog.

use sqlx::PgPool;

const LOCK_KEY: i64 = 0x6670_616e_6978_0001;

/// Name, columns and predicate of the fingerprint index.
pub const FINGERPRINT_INDEX: &str = "results_fingerprint";
const FINGERPRINT_DEF: &str =
    "(project_id, fingerprint, launch_created_at) where fingerprint is not null";

/// The index of the results of a launch by their stored suite path (`suite_tree`), so the
/// Suites tree reads the tests of the groups it shows without scanning the launch. The path is
/// hashed: it comes from test names and may exceed the size of a B-tree entry.
pub const SUITE_INDEX: &str = "results_launch_suite";
/// The indexed expression; a query must repeat it exactly to use [`SUITE_INDEX`].
pub const SUITE_HASH: &str = "hash_array_extended(suite_tree, 0::bigint)";

/// The index of the results of a launch by name (see `results::NAME_ORDER`). It holds the
/// first 600 characters of the full name: at most 2400 bytes, within the 2704 bytes of a
/// B-tree entry, for any name up to `max_name_bytes`. It replaces `results_launch_name`
/// (`(launch_id, full_name, id)`), which refused longer names.
pub const NAME_INDEX: &str = "results_launch_name_key";

/// The full-text index of failure messages, over their first 64 Ki characters: a
/// `tsvector` holds at most 1 MiB, which a message of 1 MiB of words exceeds. It replaces
/// `results_message_fts`, which refused such results.
pub const MESSAGE_INDEX: &str = "results_message_tsv";

/// Partitioned indexes of `results`: name, definition, suffix of the partition indexes.
fn results_indexes() -> [(&'static str, String, &'static str); 4] {
    [
        (FINGERPRINT_INDEX, FINGERPRINT_DEF.to_string(), "fp"),
        (SUITE_INDEX, format!("(launch_id, {SUITE_HASH})"), "suite"),
        (
            NAME_INDEX,
            "(launch_id, left(full_name, 600))".to_string(),
            "name",
        ),
        (
            MESSAGE_INDEX,
            "using gin (to_tsvector('simple', left(coalesce(message, ''), 65536)))".to_string(),
            "msg",
        ),
    ]
}

/// Indexes of `results` dropped once their replacement is valid: old name, replacement.
const RETIRED: [(&str, &str); 2] = [
    ("results_launch_name", NAME_INDEX),
    ("results_message_fts", MESSAGE_INDEX),
];

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
        for (index, def, suffix) in results_indexes() {
            built += ensure_partitioned(&mut conn, index, &def, suffix).await?;
        }
        for (old, new) in RETIRED {
            retire(&mut conn, old, new).await?;
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

/// Builds the partitioned index `index` of `results`: the parent `ON ONLY`, then the index of
/// every partition that has none attached (an invalid one left by an interrupted build is
/// dropped first). Returns how many partition indexes were built.
async fn ensure_partitioned(
    conn: &mut sqlx::PgConnection,
    index: &str,
    def: &str,
    suffix: &str,
) -> anyhow::Result<u32> {
    let mut built = 0;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "create index if not exists {index} on only results {def}"
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
    .bind(index)
    .fetch_all(&mut *conn)
    .await?;
    for (part, attached) in parts {
        if attached.is_some() {
            continue;
        }
        let child = format!("{part}_{suffix}");
        if !safe_ident(&part) || !safe_ident(&child) {
            continue;
        }
        let state: Option<bool> = sqlx::query_scalar(
            "select ix.indisvalid from pg_class c join pg_index ix on ix.indexrelid = c.oid where c.relname = $1",
        )
        .bind(&child)
        .fetch_optional(&mut *conn)
        .await?;
        if state == Some(false) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "drop index concurrently if exists {child}"
            )))
            .execute(&mut *conn)
            .await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create index concurrently if not exists {child} on {part} {def}"
        )))
        .execute(&mut *conn)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter index {index} attach partition {child}"
        )))
        .execute(&mut *conn)
        .await?;
        built += 1;
    }
    Ok(built)
}

/// Drops the partitioned index `old` of `results` once `new` is valid. The drop locks
/// `results` exclusively for a moment: it waits at most 2 seconds for running statements and
/// is tried again at the next run otherwise.
async fn retire(conn: &mut sqlx::PgConnection, old: &str, new: &str) -> anyhow::Result<()> {
    let (old_there, new_valid): (bool, bool) = sqlx::query_as(
        "select to_regclass($1) is not null, coalesce((select ix.indisvalid from pg_index ix \
           where ix.indexrelid = to_regclass($2)), false)",
    )
    .bind(old)
    .bind(new)
    .fetch_one(&mut *conn)
    .await?;
    if !old_there || !new_valid || !safe_ident(old) {
        return Ok(());
    }
    let r = async {
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        sqlx::query("set local lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop index if exists {old}")))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match r {
        Err(e) if crate::merges::lock_timeout(&e) => {
            tracing::info!(
                index = old,
                "dropping a replaced index waits for a quieter moment"
            );
            Ok(())
        }
        r => {
            if r.is_ok() {
                tracing::info!(index = old, replaced_by = new, "replaced index dropped");
            }
            r
        }
    }
}
