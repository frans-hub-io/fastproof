//! Bounded deletes on a real PostgreSQL: the purge of a deleted project, launch deletion and
//! folder deletion never let one statement cascade through a large row set. A trigger on every
//! table records each deleted or updated row with its transaction, statement and trigger depth
//! (a depth above 1 means a foreign-key cascade did it). They need
//! `FASTPROOF_TEST_DATABASE_URL`; without it they pass without doing anything.

use std::collections::BTreeMap;

use fastproof_store::Store;
use fastproof_store::tm::FolderDeleteMode;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn migrated() -> Option<Self> {
        let Ok(url) = std::env::var("FASTPROOF_TEST_DATABASE_URL") else {
            eprintln!("FASTPROOF_TEST_DATABASE_URL is not set: skipped");
            return None;
        };
        let admin: PgConnectOptions = url.parse().expect("FASTPROOF_TEST_DATABASE_URL");
        let name = format!("fastproof_test_{:016x}", rand::random::<u64>());
        let mut conn = admin
            .connect()
            .await
            .expect("connecting to the test server");
        sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}")))
            .execute(&mut conn)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        fastproof_store::migrate(&pool).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(SEED))
            .execute(&pool)
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                if let Ok(mut c) = admin.connect().await {
                    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                        "drop database if exists {name} with (force)"
                    )))
                    .execute(&mut c)
                    .await;
                }
            });
        })
        .join()
        .unwrap();
    }
}

/// `purge_seed(project, key)` writes a project with rows in every table under it: 7 launches
/// (uploads, ingest jobs, results, attempts, containers, attachment links, refs and media rows, log chunks,
/// gates, sync queue entries), a managed plan with a manual run (run items, step results,
/// executions), cases with versions, keys, suggestions and attachments in a folder tree, and
/// the analysis and integration rows. `purge_audit_on()` records every later delete and update.
const SEED: &str = r#"
create function purge_seed(p uuid, k text) returns uuid language plpgsql as $f$
declare
    l uuid;
    ml uuid;
    u uuid;
    c uuid;
    f1 uuid := gen_random_uuid();
    f2 uuid := gen_random_uuid();
    f3 uuid := gen_random_uuid();
    plan uuid := gen_random_uuid();
    t0 timestamptz := now();
begin
    insert into projects (id, key, name, case_seq) values (p, k, k, 8);
    insert into test_cases (id, project_id, test_case_hash, full_name, display_name)
        select gen_random_uuid(), p, k || 'h' || gs, 'T' || gs, 'T' || gs from generate_series(1, 5) gs;
    insert into series (id, project_id, test_case_id, series_key, parameters_hash)
        select gen_random_uuid(), p, t.id, t.full_name, '' from test_cases t where t.project_id = p;
    insert into attachments (id, project_id, sha256, size, content_type, blob_key)
        select gen_random_uuid(), p, k || 'sha' || gs, 1, 'text/plain', 'b/' || k || gs from generate_series(1, 4) gs;
    insert into attachment_media (attachment_id, project_id, status)
        select a.id, p, 'native' from attachments a where a.project_id = p;
    for n in 1..7 loop
        l := gen_random_uuid();
        u := gen_random_uuid();
        insert into launches (id, project_id, number, name, created_at) values (l, p, n, 'l' || n, t0);
        insert into uploads (id, project_id, launch_id) values (u, p, l);
        insert into ingest_jobs (id, upload_id) values (gen_random_uuid(), u);
        insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, status)
            select gen_random_uuid(), t0, p, l, s.test_case_id, s.id, 'n', s.series_key, 'passed'
            from series s where s.project_id = p;
        insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, client_key, seq, status)
            select gen_random_uuid(), t0, p, l, r.id, r.series_id, r.id::text, 1, 'passed'
            from results r where r.launch_id = l;
        insert into launch_containers (launch_id, uuid, fixtures_z) values (l, 'c1', '\x00'), (l, 'c2', '\x00');
        insert into attachment_links (attachment_id, launch_id) select a.id, l from attachments a where a.project_id = p;
        insert into attachment_refs (id, attachment_id, launch_id, content_type)
            select gen_random_uuid(), a.id, l, 'text/plain' from attachments a where a.project_id = p;
        insert into attempt_log_chunks (project_id, launch_id, result_key, attempt, first_seq, last_seq, line_count, bytes, lines_z)
            values (p, l, 'r', 1, 1, 2, 2, 2, '\x00'), (p, l, 'r', 1, 3, 4, 2, 2, '\x00');
        insert into launch_log_bytes (launch_id, bytes) values (l, 4);
        insert into launch_gates (launch_id, project_id) values (l, p);
        insert into case_sync_queue (launch_id) values (l);
        insert into launch_reruns (id, project_id, source_launch_id, external_id, provider, mode, requested_by)
            values (gen_random_uuid(), p, l, 'rerun-' || l::text, 'webhook', 'failed', 'seed');
    end loop;
    insert into series_daily (project_id, series_id, day) select p, s.id, current_date from series s where s.project_id = p;
    insert into series_stats (series_id, project_id) select s.id, p from series s where s.project_id = p;
    insert into series_mutes (series_id, project_id) select s.id, p from series s where s.project_id = p limit 2;
    insert into failure_fingerprints (project_id, fingerprint, first_seen_at, last_seen_at)
        values (p, 'f1', t0, t0), (p, 'f2', t0, t0);
    insert into launch_daily (project_id, day) values (p, current_date);
    insert into defect_types (id, project_id, grp, name) values (gen_random_uuid(), p, 'productBug', 'pb');
    insert into rules (id, project_id, name) values (gen_random_uuid(), p, 'r');
    insert into analysis_tasks (id, project_id, kind) values (gen_random_uuid(), p, 'k');
    insert into project_secrets (project_id, name, key_id, nonce, ciphertext) values (p, 's', 'k', '\x00', '\x00');
    insert into integration_deliveries (id, project_id, channel, target_id, payload)
        values (gen_random_uuid(), p, 'c', 't', '{}');
    insert into case_folders (id, project_id, parent_id, name, path_ids, depth) values
        (f1, p, null, 'a', array[f1], 0), (f2, p, f1, 'b', array[f1, f2], 1), (f3, p, f2, 'c', array[f1, f2, f3], 2);
    insert into case_folders (id, project_id, parent_id, name, path_ids, depth)
        select x.g, p, f3, 'd' || x.gs, array[f1, f2, f3, x.g], 3
        from (select gen_random_uuid() g, gs from generate_series(1, 4) gs) x;
    insert into case_field_defs (id, project_id, key, name, type)
        values (gen_random_uuid(), p, 'f1', 'F1', 'text'), (gen_random_uuid(), p, 'f2', 'F2', 'text');
    for n in 1..8 loop
        c := gen_random_uuid();
        insert into cases (id, project_id, number, name, origin, body, folder_id)
            values (c, p, n, 'c' || n, 'ui', '{}', case when n % 2 = 0 then f3 else f2 end);
        insert into case_versions (case_id, version, project_id, source, snapshot_z, created_by)
            select c, v, p, 'ui', '\x00', 't' from generate_series(1, 3) v;
        insert into case_keys (project_id, kind, value, case_id) values (p, 'external', 'k' || n, c);
        insert into case_suggestions (id, project_id, case_id, base_revision, proposal, agent)
            values (gen_random_uuid(), p, c, 1, '{}', 'a');
    end loop;
    insert into case_attachments (case_id, attachment_id, name, content_type)
        select cs.id, a.id, 'n', 't' from cases cs, attachments a
        where cs.project_id = p and a.project_id = p and cs.number <= 2;
    update test_cases t set case_id = (select cs.id from cases cs where cs.project_id = p and cs.number = 1)
        where t.project_id = p and t.full_name = 'T1';
    insert into test_plans (id, project_id, name, kind) values (plan, p, 'pl', 'managed'), (gen_random_uuid(), p, 'ad', 'adhoc');
    insert into test_plan_items (plan_id, case_id) select plan, cs.id from cases cs where cs.project_id = p;
    ml := gen_random_uuid();
    insert into launches (id, project_id, number, name, created_at, test_plan_id, manual_state)
        values (ml, p, 100, 'run', t0, plan, 'open');
    insert into run_items (id, project_id, launch_id, launch_created_at, case_id, case_version, series_id)
        select gen_random_uuid(), p, ml, t0, cs.id, 1, gen_random_uuid() from cases cs where cs.project_id = p;
    insert into run_step_results (run_item_id, step_path, status, updated_by)
        select ri.id, 's' || gs, 'passed', 't' from run_items ri, generate_series(1, 4) gs where ri.launch_id = ml;
    insert into run_item_executions (run_item_id, execution_no, status, case_version, rerun_by)
        select ri.id, gs, 'passed', 1, 't' from run_items ri, generate_series(1, 2) gs where ri.launch_id = ml;
    update results r set run_item_id = (select ri.id from run_items ri where ri.launch_id = ml limit 1)
        where r.project_id = p and r.full_name = 'T1';
    insert into tm_jobs (id, project_id, kind, params) values (gen_random_uuid(), p, 'import', '{}'), (gen_random_uuid(), p, 'export', '{}');
    return ml;
end
$f$;

create table purge_audit (xid text, ts timestamptz, depth int, tbl text, op text);

create function purge_audit_row() returns trigger language plpgsql as $f$
begin
    insert into purge_audit values (pg_current_xact_id()::text, statement_timestamp(), pg_trigger_depth(), tg_table_name, tg_op);
    return null;
end
$f$;

create function purge_audit_on() returns void language plpgsql as $f$
declare
    t text;
begin
    for t in select c.relname from pg_class c
        where c.relnamespace = 'public'::regnamespace and c.relkind in ('r', 'p') and not c.relispartition
          and c.relname <> 'purge_audit'
    loop
        execute format('create trigger purge_audit after delete or update on %I for each row execute function purge_audit_row()', t);
    end loop;
end
$f$;
"#;

/// Tables that hold rows of a project: every table with a foreign-key path to `projects`, and
/// every table with a `project_id` or `launch_id` column (some have no foreign key). Read from
/// the catalog, so a new table fails the seed check below until the purge handles it.
const OWNED_TABLES: &str = r#"
with recursive owned(rel) as (
    select 'projects'::regclass
    union
    select c.conrelid::regclass from pg_constraint c join owned o on c.confrelid = o.rel
    where c.contype = 'f' and c.conparentid = 0
)
select rel::text from owned
union
select c.relname::text from pg_class c join pg_attribute a on a.attrelid = c.oid
where c.relnamespace = 'public'::regnamespace and c.relkind in ('r', 'p') and not c.relispartition
  and a.attname in ('project_id', 'launch_id') and not a.attisdropped
order by 1
"#;

/// Kept on purpose: the key of a deleted project stays reserved.
const KEPT: [&str; 1] = ["deleted_project_keys"];

async fn seed(db: &TestDb, key: &str) -> (Uuid, Uuid) {
    let id = Uuid::now_v7();
    let run: Uuid = sqlx::query_scalar("select purge_seed($1, $2)")
        .bind(id)
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    (id, run)
}

async fn counts(db: &TestDb) -> BTreeMap<String, i64> {
    let tables: Vec<String> = sqlx::query_scalar(OWNED_TABLES)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let mut out = BTreeMap::new();
    for t in tables {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {t}")))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        out.insert(t, n);
    }
    out
}

async fn audit_on(db: &TestDb) {
    sqlx::query("select purge_audit_on()")
        .execute(&db.pool)
        .await
        .unwrap();
}

/// The audit: rows per statement (deletes, and anything a cascade did), and cascaded rows.
async fn audit(db: &TestDb, tables: &[&str]) -> (i64, i64, i64) {
    let (max, statements): (Option<i64>, i64) = sqlx::query_as(
        "select max(n), count(*) from (select count(*) n from purge_audit \
         where (op = 'DELETE' or depth > 1) and (cardinality($1::text[]) = 0 or tbl = any($1)) \
         group by xid, ts) s",
    )
    .bind(tables)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let cascaded: i64 = sqlx::query_scalar("select count(*) from purge_audit where depth > 1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    (max.unwrap_or(0), statements, cascaded)
}

async fn queued(store: &Store, project: Uuid) -> fastproof_store::maintenance::BlobDeletion {
    store
        .pending_blob_deletions(100)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.project_id == Some(project))
        .expect("the project's prefix is queued")
}

#[tokio::test]
async fn purge_removes_every_project_row_in_bounded_statements() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    const N: i64 = 3;
    let store = Store::new(db.pool.clone()).with_purge_batch(N);
    seed(&db, "keep").await;
    let kept = counts(&db).await;
    let (gone, _) = seed(&db, "gone").await;
    assert!(store.delete_project("gone").await.unwrap());
    let before = counts(&db).await;
    let unseeded: Vec<&String> = before
        .iter()
        .filter(|(t, n)| **n <= kept[*t] && !KEPT.contains(&t.as_str()))
        .map(|(t, _)| t)
        .collect();
    assert!(
        unseeded.is_empty(),
        "tables {unseeded:?} hold project rows but purge_seed writes none: seed them here and \
         make the purge (fastproof_store::purge) delete them before the rows they reference"
    );
    let events: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let deleted_events: i64 = sqlx::query_scalar(
        "select count(*) from events where type = 'project.deleted' and project = 'gone'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(deleted_events, 1);

    audit_on(&db).await;
    let d = queued(&store, gone).await;
    store.finish_blob_deletion(&d).await.unwrap();

    let after = counts(&db).await;
    for (t, n) in &after {
        let want = if KEPT.contains(&t.as_str()) {
            before[t]
        } else {
            kept[t]
        };
        assert_eq!(*n, want, "rows left in {t}");
    }
    let (max, statements, cascaded) = audit(&db, &[]).await;
    assert!(max <= N, "a statement deleted {max} rows (bound {N})");
    assert_eq!(cascaded, 0, "a foreign-key cascade deleted rows");
    let removed: i64 = before.values().sum::<i64>() - after.values().sum::<i64>();
    assert!(
        statements * N >= removed && statements > 50,
        "{statements} statements for {removed} rows"
    );
    let txs: i64 = sqlx::query_scalar("select count(distinct xid) from purge_audit")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(
        txs >= statements - 1,
        "every purge statement commits on its own (only the project row and the queue entry \
         go together): {txs} transactions, {statements} statements"
    );
    let events_after: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(events_after, events, "the purge emits no events");
    let key_kept: Option<Uuid> =
        sqlx::query_scalar("select project_id from deleted_project_keys where key = 'gone'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(key_kept, Some(gone));

    store.finish_blob_deletion(&d).await.unwrap();
    store.purge_project_rows(gone, None).await.unwrap();
    assert_eq!(counts(&db).await, after, "a second run changes nothing");
}

#[tokio::test]
async fn interrupted_purge_continues_on_the_next_run() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let store = Store::new(db.pool.clone()).with_purge_batch(2);
    seed(&db, "keep").await;
    let kept = counts(&db).await;
    let (gone, _) = seed(&db, "gone").await;
    assert!(store.delete_project("gone").await.unwrap());
    let d = queued(&store, gone).await;
    let mut interrupted = 0;
    loop {
        match store
            .purge_project_rows(gone, Some(25 + interrupted % 7))
            .await
        {
            Ok(()) => break,
            Err(e) => {
                assert!(e.to_string().contains("interrupted"), "{e:#}");
                interrupted += 1;
                assert!(interrupted < 1000, "the purge makes no progress");
            }
        }
    }
    assert!(interrupted > 3, "only {interrupted} interruptions");
    let left: i64 = sqlx::query_scalar("select count(*) from projects where id = $1")
        .bind(gone)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(left, 1, "the project row goes with the queue entry");
    store.finish_blob_deletion(&d).await.unwrap();
    let after = counts(&db).await;
    for (t, n) in &after {
        if !KEPT.contains(&t.as_str()) {
            assert_eq!(*n, kept[t], "rows left in {t}");
        }
    }
}

#[tokio::test]
async fn purge_leaves_a_live_project_alone() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let store = Store::new(db.pool.clone()).with_purge_batch(3);
    let (live, _) = seed(&db, "live").await;
    let before = counts(&db).await;
    store.purge_project_rows(live, None).await.unwrap();
    store
        .purge_project_rows(Uuid::now_v7(), None)
        .await
        .unwrap();
    assert_eq!(counts(&db).await, before);
}

#[tokio::test]
async fn deleting_a_manual_run_never_cascades() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    const N: i64 = 3;
    let store = Store::new(db.pool.clone()).with_purge_batch(N);
    let (p, run) = seed(&db, "runs").await;
    let items: i64 = sqlx::query_scalar("select count(*) from run_items where launch_id = $1")
        .bind(run)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(items > N);
    audit_on(&db).await;
    assert!(store.delete_launch(p, "runs", run).await.unwrap());
    let (max, _, cascaded) = audit(&db, &["run_items", "uploads", "launches"]).await;
    assert!(
        max <= N,
        "a statement deleted {max} referenced rows (bound {N})"
    );
    assert_eq!(cascaded, 0, "a foreign-key cascade deleted rows");
    let txs: i64 = sqlx::query_scalar("select count(distinct xid) from purge_audit")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(txs, 1, "the launch goes in one transaction");
    let left: (i64, i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from launches where id = $1), \
           (select count(*) from run_items where launch_id = $1), \
           (select count(*) from run_step_results s where not exists (select 1 from run_items r where r.id = s.run_item_id)), \
           (select count(*) from launches where project_id = $2)",
    )
    .bind(run)
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(left, (0, 0, 0, 7));
    let deleted: i64 = sqlx::query_scalar(
        "select count(*) from events where type = 'launch.deleted' and project = 'runs'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(deleted, 1);
}

#[tokio::test]
async fn deleting_a_folder_tree_never_cascades() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    const N: i64 = 2;
    let store = Store::new(db.pool.clone()).with_purge_batch(N);
    let (p, run) = seed(&db, "tree").await;
    assert!(store.delete_launch(p, "tree", run).await.unwrap());
    let root: Uuid =
        sqlx::query_scalar("select id from case_folders where project_id = $1 and depth = 0")
            .bind(p)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    audit_on(&db).await;
    let out = store
        .delete_case_folder(p, root, Some(FolderDeleteMode::ArchiveCases), "t")
        .await
        .unwrap();
    assert_eq!((out.folders, out.cases_archived), (7, 8));
    let (max, _, cascaded) = audit(&db, &["case_folders"]).await;
    assert!(max <= N, "a statement deleted {max} folders (bound {N})");
    assert_eq!(cascaded, 0, "a foreign-key cascade changed rows");
    let left: (i64, i64) = sqlx::query_as(
        "select (select count(*) from case_folders where project_id = $1), \
           (select count(*) from cases where project_id = $1 and (folder_id is not null or archived_at is null))",
    )
    .bind(p)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));
}

/// The size that made the single-transaction purge run out of memory: 20 000 cases with 5
/// versions each, a plan of all of them, a manual run with 10 000 items and 50 000 step
/// results, 200 folders and 5 000 results. Run with `--ignored`.
const BIG: &str = r#"
do $$
declare
    p uuid := gen_random_uuid();
    l uuid;
    t0 timestamptz := now();
    ml uuid := gen_random_uuid();
    plan uuid := gen_random_uuid();
    root uuid := gen_random_uuid();
begin
    insert into projects (id, key, name, case_seq) values (p, 'big', 'big', 20000);
    insert into test_cases (id, project_id, test_case_hash, full_name, display_name)
        select gen_random_uuid(), p, 'h' || g, 'T' || g, 'T' || g from generate_series(1, 500) g;
    insert into series (id, project_id, test_case_id, series_key, parameters_hash)
        select gen_random_uuid(), p, t.id, t.full_name, '' from test_cases t where t.project_id = p;
    for n in 1..10 loop
        l := gen_random_uuid();
        insert into launches (id, project_id, number, name, created_at) values (l, p, n, 'l', t0);
        insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, status)
            select gen_random_uuid(), t0, p, l, s.test_case_id, s.id, 'n', s.series_key, 'passed'
            from series s where s.project_id = p;
    end loop;
    insert into case_folders (id, project_id, parent_id, name, path_ids, depth) values (root, p, null, 'root', array[root], 0);
    insert into case_folders (id, project_id, parent_id, name, path_ids, depth)
        select x.g, p, root, 'f' || x.n, array[root, x.g], 1 from (select gen_random_uuid() g, n from generate_series(1, 199) n) x;
    insert into cases (id, project_id, number, name, origin, body, folder_id)
        select gen_random_uuid(), p, g, 'c' || g, 'ui', '{}',
            (select f.id from case_folders f where f.project_id = p and f.depth = 1 and f.name = 'f' || (1 + g % 199))
        from generate_series(1, 20000) g;
    insert into case_versions (case_id, version, project_id, source, snapshot_z, created_by)
        select cs.id, v, p, 'ui', decode(repeat('ab', 512), 'hex'), 't' from cases cs, generate_series(1, 5) v where cs.project_id = p;
    insert into case_keys (project_id, kind, value, case_id)
        select p, 'external', 'k' || cs.number, cs.id from cases cs where cs.project_id = p;
    insert into test_plans (id, project_id, name, kind) values (plan, p, 'pl', 'managed');
    insert into test_plan_items (plan_id, case_id) select plan, cs.id from cases cs where cs.project_id = p;
    insert into launches (id, project_id, number, name, created_at, test_plan_id, manual_state)
        values (ml, p, 100, 'run', t0, plan, 'open');
    insert into run_items (id, project_id, launch_id, launch_created_at, case_id, case_version, series_id)
        select gen_random_uuid(), p, ml, t0, cs.id, 1, gen_random_uuid() from cases cs
        where cs.project_id = p and cs.number <= 10000;
    insert into run_step_results (run_item_id, step_path, status, updated_by)
        select ri.id, 's' || g, 'passed', 't' from run_items ri, generate_series(1, 5) g where ri.launch_id = ml;
    update projects set deleted_at = now() where id = p;
    insert into blob_deletions (prefix, project_id) values ('projects/big', p);
end
$$;
"#;

#[tokio::test]
#[ignore = "large: run with --ignored"]
async fn purge_at_lab_scale_stays_bounded() {
    let Some(db) = TestDb::migrated().await else {
        return;
    };
    let store = Store::new(db.pool.clone());
    let n = store.purge_batch();
    sqlx::raw_sql(BIG).execute(&db.pool).await.unwrap();
    let (p,): (Uuid,) = sqlx::query_as("select id from projects where key = 'big'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    audit_on(&db).await;
    let started = std::time::Instant::now();
    let d = queued(&store, p).await;
    store.finish_blob_deletion(&d).await.unwrap();
    let took = started.elapsed();
    let (max, statements, cascaded) = audit(&db, &[]).await;
    eprintln!("purged in {took:?}: {statements} statements, largest {max} rows");
    assert!(max <= n, "a statement deleted {max} rows (bound {n})");
    assert_eq!(cascaded, 0);
    let left: i64 = sqlx::query_scalar(
        "select (select count(*) from cases) + (select count(*) from case_versions) \
         + (select count(*) from run_step_results) + (select count(*) from results) \
         + (select count(*) from case_folders) + (select count(*) from projects)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}
