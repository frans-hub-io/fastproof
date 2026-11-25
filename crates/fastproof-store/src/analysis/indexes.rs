//! Indexes that must not block writes while they are built: the fingerprint and suite indexes
//! of the partitioned `results` and the trace index of the partitioned `attempts` (parent
//! `ON ONLY`, then each partition `CONCURRENTLY` and attached; partitions created later get
//! them from the parent) and the trigram indexes of the fingerprint catalog and the triage
//! memory.

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

/// The index of attempts by trace id (signals of grids and CI resolve to attempts with it).
pub const TRACE_INDEX: &str = "attempts_trace_id";
const TRACE_DEF: &str = "(trace_id) where trace_id is not null";

/// Trigram indexes built when `pg_trgm` is installed: name, table and column.
const TRIGRAM_INDEXES: [(&str, &str, &str); 2] = [
    (
        "failure_fingerprints_message_trgm",
        "failure_fingerprints",
        "message",
    ),
    (
        "triage_memory_message_trgm",
        "triage_memory",
        "message_norm",
    ),
];

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
/// replica holding the lock makes this a no-op. Every statement that locks `results` itself
/// (not only a partition, concurrently) waits at most [`DDL_LOCK_WAIT`] for that lock and is
/// tried again at the next run otherwise, so writers never queue behind it for long.
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
            built += ensure_partitioned(&mut conn, "results", index, &def, suffix).await?;
        }
        built += ensure_partitioned(&mut conn, "attempts", TRACE_INDEX, TRACE_DEF, "trace").await?;
        for (old, new) in RETIRED {
            retire(&mut conn, old, new).await?;
        }
        for (index, table, column) in TRIGRAM_INDEXES {
            let trgm: bool = sqlx::query_scalar(
                "select exists(select 1 from pg_extension where extname = 'pg_trgm') \
                   and to_regclass($1) is null",
            )
            .bind(index)
            .fetch_one(&mut *conn)
            .await?;
            if trgm {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "create index concurrently if not exists {index} \
                     on {table} using gin ({column} gin_trgm_ops)"
                )))
                .execute(&mut *conn)
                .await?;
            }
        }
        rebuild_invalid_migration_indexes(&mut conn).await?;
        Ok::<_, anyhow::Error>(built)
    }
    .await;
    let _ = sqlx::query("select pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    r
}

/// Longest wait for the lock of `results` (the parent table) by a statement of this job.
pub const DDL_LOCK_WAIT: &str = "500ms";
/// Longest run of such a statement: a drop takes the parent and then each partition in turn,
/// keeping what it already holds, so the lock wait alone does not bound it.
pub const DDL_STATEMENT_TIME: &str = "3s";

/// Runs `sql` in its own transaction with [`DDL_LOCK_WAIT`] and [`DDL_STATEMENT_TIME`].
/// Returns false when it gave up (lock or statement timeout); the caller tries at its next run.
async fn short_ddl(conn: &mut sqlx::PgConnection, sql: String) -> anyhow::Result<bool> {
    let r = async {
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "set local lock_timeout = '{DDL_LOCK_WAIT}'"
        )))
        .execute(&mut *tx)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "set local statement_timeout = '{DDL_STATEMENT_TIME}'"
        )))
        .execute(&mut *tx)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match r {
        Ok(()) => Ok(true),
        Err(e) if gave_up(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// A lock timeout (55P03) or a statement timeout (57014).
fn gave_up(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(c.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::Database(d)) if matches!(d.code().as_deref(), Some("55P03" | "57014")))
    })
}

/// Builds the partitioned index `index` of `table` (`results` or `attempts`): the parent
/// `ON ONLY` (only when it is missing: the statement locks the table even when the index
/// exists), then the index of every
/// partition that has none attached (an invalid one left by an interrupted build is dropped
/// first), each attached with a short lock wait. Returns how many partition indexes were
/// built and attached.
async fn ensure_partitioned(
    conn: &mut sqlx::PgConnection,
    table: &'static str,
    index: &str,
    def: &str,
    suffix: &str,
) -> anyhow::Result<u32> {
    let mut built = 0;
    let exists: bool = sqlx::query_scalar("select to_regclass($1) is not null")
        .bind(index)
        .fetch_one(&mut *conn)
        .await?;
    if !exists
        && !short_ddl(
            conn,
            format!("create index if not exists {index} on only {table} {def}"),
        )
        .await?
    {
        tracing::info!(
            index,
            table,
            "creating a partitioned index waits for a quieter moment"
        );
        return Ok(0);
    }
    let parts: Vec<(String, Option<String>)> = sqlx::query_as(
        "select c.relname::text, (select ci.relname::text from pg_inherits ii \
           join pg_class ci on ci.oid = ii.inhrelid join pg_index ix on ix.indexrelid = ci.oid \
           where ii.inhparent = to_regclass($1) and ix.indrelid = c.oid limit 1) \
         from pg_inherits i join pg_class c on c.oid = i.inhrelid join pg_class p on p.oid = i.inhparent \
         where p.relname = $2 order by 1",
    )
    .bind(index)
    .bind(table)
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
        if !short_ddl(
            conn,
            format!("alter index {index} attach partition {child}"),
        )
        .await?
        {
            tracing::info!(index, partition = %part, "attaching a partition index waits for a quieter moment");
            continue;
        }
        built += 1;
    }
    Ok(built)
}

/// Drops the partitioned index `old` of `results` once `new` is valid. The drop locks
/// `results` exclusively: it waits at most [`DDL_LOCK_WAIT`] for each lock and runs at most
/// [`DDL_STATEMENT_TIME`] in all, and is tried again at the next run (10 minutes) otherwise.
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
    if short_ddl(conn, format!("drop index if exists {old}")).await? {
        tracing::info!(index = old, replaced_by = new, "replaced index dropped");
    } else {
        metrics::counter!("fastproof_index_swaps_deferred_total").increment(1);
        tracing::info!(
            index = old,
            "dropping a replaced index waits for a quieter moment"
        );
    }
    Ok(())
}

/// Index names created by the no-transaction (`CREATE INDEX CONCURRENTLY`) migrations.
pub fn concurrent_migration_indexes() -> Vec<String> {
    let re = regex::Regex::new(
        r"(?i)create\s+(?:unique\s+)?index\s+concurrently\s+if\s+not\s+exists\s+([a-z0-9_]+)",
    )
    .expect("valid pattern");
    crate::migrator()
        .iter()
        .filter(|m| m.no_tx)
        .flat_map(|m| {
            re.captures_iter(m.sql.as_str())
                .map(|c| c[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Rebuilds indexes of the no-transaction migrations that an interrupted
/// `CREATE INDEX CONCURRENTLY` left invalid: the migration counts as applied on its next try
/// (`if not exists` finds the invalid index), but PostgreSQL never uses that index. Runs only
/// while no migration runs (their advisory lock), so a build in progress is not taken for an
/// interrupted one. Returns the names rebuilt.
pub async fn rebuild_invalid_migration_indexes(
    conn: &mut sqlx::PgConnection,
) -> anyhow::Result<Vec<String>> {
    let names = concurrent_migration_indexes();
    let invalid: Vec<(String, String)> = sqlx::query_as(
        "select c.relname::text, pg_get_indexdef(ix.indexrelid) from pg_index ix \
         join pg_class c on c.oid = ix.indexrelid \
         where c.relname = any($1) and not ix.indisvalid and c.relkind = 'i'",
    )
    .bind(&names)
    .fetch_all(&mut *conn)
    .await?;
    if invalid.is_empty() {
        return Ok(Vec::new());
    }
    if !sqlx::query_scalar::<_, bool>("select pg_try_advisory_lock($1)")
        .bind(crate::LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?
    {
        return Ok(Vec::new());
    }
    let r = async {
        let mut done = Vec::new();
        for (name, def) in invalid {
            let Some(rest) = def
                .strip_prefix("CREATE INDEX ")
                .map(|r| ("CREATE INDEX CONCURRENTLY ", r))
                .or_else(|| {
                    def.strip_prefix("CREATE UNIQUE INDEX ")
                        .map(|r| ("CREATE UNIQUE INDEX CONCURRENTLY ", r))
                })
            else {
                continue;
            };
            if !safe_ident(&name) {
                continue;
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "drop index concurrently if exists {name}"
            )))
            .execute(&mut *conn)
            .await?;
            sqlx::query(sqlx::AssertSqlSafe(format!("{}{}", rest.0, rest.1)))
                .execute(&mut *conn)
                .await?;
            tracing::warn!(index = %name, "rebuilt an index an interrupted migration left invalid");
            done.push(name);
        }
        Ok::<_, anyhow::Error>(done)
    }
    .await;
    let _ = sqlx::query("select pg_advisory_unlock($1)")
        .bind(crate::LOCK_KEY)
        .execute(&mut *conn)
        .await;
    r
}
