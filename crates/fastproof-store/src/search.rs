//! Optional trigram search (`pg_trgm`): substring search on test names uses trigram indexes
//! when the extension can be created, and sequential scans otherwise.

use sqlx::PgPool;

/// Whether trigram indexes are in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigram {
    Enabled,
    /// The extension is not available or may not be created by this role.
    Unavailable,
}

const LOCK_KEY: i64 = 0x6670_7472_6772_6d00;

/// Creates `pg_trgm` if allowed and builds the trigram indexes concurrently. Never fails the
/// start-up: problems are logged as warnings.
pub async fn ensure_trigram(pool: &PgPool) -> Trigram {
    match try_ensure(pool).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "trigram search indexes could not be created: substring search on test names uses sequential scans");
            Trigram::Unavailable
        }
    }
}

async fn try_ensure(pool: &PgPool) -> anyhow::Result<Trigram> {
    let mut conn = pool.acquire().await?;
    let present: bool =
        sqlx::query_scalar("select exists(select 1 from pg_extension where extname = 'pg_trgm')")
            .fetch_one(&mut *conn)
            .await?;
    if !present
        && let Err(e) = sqlx::query("create extension if not exists pg_trgm")
            .execute(&mut *conn)
            .await
    {
        tracing::warn!(error = %e, "the pg_trgm extension is not available (ask the database administrator to run `create extension pg_trgm`): substring search on test names uses sequential scans");
        return Ok(Trigram::Unavailable);
    }
    if !sqlx::query_scalar::<_, bool>("select pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?
    {
        return Ok(Trigram::Enabled);
    }
    let r = async {
        for sql in [
            "create index concurrently if not exists test_cases_full_name_trgm on test_cases using gin (full_name gin_trgm_ops)",
            "create index concurrently if not exists test_cases_display_name_trgm on test_cases using gin (display_name gin_trgm_ops)",
            "create index concurrently if not exists cases_name_trgm on cases using gin (name gin_trgm_ops)",
            "create index concurrently if not exists triage_memory_message_trgm on triage_memory using gin (message_norm gin_trgm_ops)",
        ] {
            sqlx::query(sql).execute(&mut *conn).await?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let _ = sqlx::query("select pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    r?;
    Ok(Trigram::Enabled)
}
