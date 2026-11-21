//! G9 log search benchmark (ignored by default): loads synthetic log lines into PostgreSQL and
//! measures full-text (`tsvector`, GIN) and trigram (`pg_trgm`, GIN) search latency.
//!
//! Lines are stored one per row in the `message` column of `attempts` (the stage B storage;
//! a dedicated per-attempt log chunk table is not on the main branch yet). Loading stops at
//! `FASTPROOF_BENCH_LOG_LINES` (default 10 000 000) or after `FASTPROOF_BENCH_LOAD_SECS`
//! (default 360) of loading, whichever comes first; the report states what was loaded.
//!
//! `FASTPROOF_TEST_DATABASE_URL=... cargo test --release -p fastproof-server --test log_search_bench -- --ignored --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::TestDb;
use sqlx::{Connection, Executor};

const BATCH: i64 = 500_000;

fn env_i64(k: &str, d: i64) -> i64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

#[tokio::test]
#[ignore]
async fn log_search_latency() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrate(&db.pool).await.unwrap();
    let target = env_i64("FASTPROOF_BENCH_LOG_LINES", 10_000_000);
    let load_budget = Duration::from_secs(env_i64("FASTPROOF_BENCH_LOAD_SECS", 360) as u64);
    let mut conn = sqlx::PgConnection::connect(&db.url).await.unwrap();
    conn.execute("set maintenance_work_mem = '512MB'")
        .await
        .unwrap();
    let trgm = conn
        .execute("create extension if not exists pg_trgm")
        .await
        .is_ok();
    let (project, launch): (uuid::Uuid, uuid::Uuid) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    sqlx::query("insert into projects (id, key, name) values ($1, 'bench', 'bench')")
        .bind(project)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("insert into launches (id, project_id, number, name) values ($1, $2, 1, 'bench')")
        .bind(launch)
        .bind(project)
        .execute(&mut conn)
        .await
        .unwrap();
    // 400 words: lines look like application logs with ids, numbers and a level.
    sqlx::query(
        "create temporary table vocab as select array_agg(w) as w from (select (array['request','response','user', \
           'order','payment','checkout','cart','session','token','cache','database','query','timeout','retry', \
           'connection','refused','accepted','started','finished','failed','error','warning','debug','info', \
           'service','gateway','upstream','downstream','latency','handler','worker','queue','message','event', \
           'commit','rollback','transaction','lock','acquired','released','invoice','shipping','address','email', \
           'notification','browser','page','click','button','form','submit','validation','schema','field'])[1 + (i % 54)] \
           || case when i >= 54 then (i / 54)::text else '' end as w from generate_series(0, 399) i) x",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    let started = Instant::now();
    let mut loaded = 0i64;
    while loaded < target && started.elapsed() < load_budget {
        let n = BATCH.min(target - loaded);
        sqlx::query(
            "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, client_key, \
               seq, status, message) \
             select gen_random_uuid(), l.created_at, $1, $2, gen_random_uuid(), gen_random_uuid(), 'k' || i, i, \
               'passed', to_char(timestamp '2026-11-21 08:00:00' + make_interval(secs => i / 1000.0), \
                 'YYYY-MM-DD HH24:MI:SS.MS') || ' ' || (array['INFO','INFO','INFO','DEBUG','WARN','ERROR'])[1 + i % 6] \
               || ' ' || v.w[1 + (hashint8(i) & 1023) % 400] || ' ' || v.w[1 + (hashint8(i + 7) & 1023) % 400] \
               || ' ' || v.w[1 + (hashint8(i + 13) & 1023) % 400] || ' id=' || (hashint8(i) & 16777215)::text \
               || ' took ' || (i % 997)::text || 'ms' \
               || case when i % 100000 = 0 then ' zeta-needle-' || (i / 100000)::text else '' end \
             from generate_series($3::bigint, $4::bigint) i, vocab v, launches l where l.id = $2",
        )
        .bind(project)
        .bind(launch)
        .bind(loaded)
        .bind(loaded + n - 1)
        .execute(&mut conn)
        .await
        .unwrap();
        loaded += n;
    }
    let load_s = started.elapsed().as_secs_f64();
    let t = Instant::now();
    conn.execute("analyze attempts").await.unwrap();
    let t_fts = Instant::now();
    conn.execute(
        "create index bench_attempts_message_fts on attempts using gin (to_tsvector('simple', coalesce(message, '')))",
    )
    .await
    .unwrap();
    let fts_build = t_fts.elapsed().as_secs_f64();
    let mut trgm_build = None;
    if trgm {
        let t_tr = Instant::now();
        conn.execute(
            "create index bench_attempts_message_trgm on attempts using gin (message gin_trgm_ops)",
        )
        .await
        .unwrap();
        trgm_build = Some(t_tr.elapsed().as_secs_f64());
    }
    conn.execute("analyze attempts").await.unwrap();
    let analyze_s = t.elapsed().as_secs_f64() - fts_build - trgm_build.unwrap_or(0.0);
    let sizes: (i64, i64) = sqlx::query_as(
        "select pg_table_size('attempts_default') + coalesce((select sum(pg_table_size(c.oid))::bigint from pg_inherits i \
           join pg_class c on c.oid = i.inhrelid where i.inhparent = 'attempts'::regclass), 0), \
         coalesce((select sum(pg_relation_size(ix.indexrelid))::bigint from pg_index ix join pg_class c on c.oid = ix.indexrelid \
           where c.relname like '%message%'), 0)",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();

    let queries: Vec<(&str, String)> = {
        let mut q = vec![
            (
                "fts rare token, all matches",
                "select count(*) from attempts where to_tsvector('simple', coalesce(message, '')) @@ to_tsquery('simple', 'zeta')".to_string(),
            ),
            (
                "fts two words AND, first 50",
                "select id from attempts where to_tsvector('simple', coalesce(message, '')) @@ to_tsquery('simple', 'payment & refused') limit 50".to_string(),
            ),
            (
                "fts two words AND + ERROR, count",
                "select count(*) from attempts where to_tsvector('simple', coalesce(message, '')) @@ to_tsquery('simple', 'payment & refused & error')".to_string(),
            ),
            (
                "fts common word, first 50 of launch",
                format!("select id from attempts where launch_id = '{launch}' and to_tsvector('simple', coalesce(message, '')) @@ to_tsquery('simple', 'timeout') limit 50"),
            ),
        ];
        if trgm {
            q.push((
                "trigram substring, rare",
                "select count(*) from attempts where message ilike '%zeta-needle-4%'".to_string(),
            ));
            q.push((
                "trigram substring, 3 words, first 50",
                "select id from attempts where message ilike '%payment refused timeout%' limit 50"
                    .to_string(),
            ));
            q.push((
                "trigram substring, id fragment, first 50",
                "select id from attempts where message ilike '%id=123456%' limit 50".to_string(),
            ));
        }
        q
    };
    eprintln!(
        "BENCH G9: {loaded} log lines (target {target}) loaded in {load_s:.1} s ({:.0} lines/s); table {} MiB, \
         message indexes {} MiB; fts GIN build {fts_build:.1} s; trigram GIN build {}; analyze {analyze_s:.1} s",
        loaded as f64 / load_s,
        sizes.0 >> 20,
        sizes.1 >> 20,
        trgm_build
            .map(|b| format!("{b:.1} s"))
            .unwrap_or_else(|| "skipped (no pg_trgm)".into()),
    );
    for (name, sql) in &queries {
        let mut times = Vec::new();
        let mut rows = 0;
        for _ in 0..5 {
            let t = Instant::now();
            let r = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
                .fetch_all(&mut conn)
                .await
                .unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            rows = if sql.starts_with("select count") {
                sqlx::Row::get::<i64, _>(&r[0], 0) as usize
            } else {
                r.len()
            };
        }
        let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("explain {sql}")))
            .fetch_all(&mut conn)
            .await
            .unwrap();
        let uses_index = plan.iter().any(|l| l.contains("Bitmap Index Scan"));
        eprintln!(
            "BENCH G9: {name}: median {:.1} ms, first {:.1} ms, max {:.1} ms ({rows} rows, index {uses_index})",
            median(times.clone()),
            times[0],
            times.iter().cloned().fold(0.0, f64::max)
        );
    }
}
