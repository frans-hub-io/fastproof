//! FastProof Analysis foundation on a real PostgreSQL: the upgrade to migration 1011, the
//! analysis replica gate, the decision journal with history and undo, the analysis queue and
//! its outbox consumer, the rule hardening, the task kinds of the standard job, the defect-type
//! clean-up, pruning and capabilities. They need `FASTPROOF_TEST_DATABASE_URL`; without it they
//! pass without doing anything.

use std::time::Duration;

use chrono::{DateTime, Utc};
use fastproof_store::analysis::decisions;
use fastproof_store::analysis::rules::{RulePhase, enabled_rules};
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::triage::{MuteRequest, TriageRequest};
use fastproof_store::analysis::types::*;
use fastproof_store::ingest::LaunchRef;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::{TmError, TmGateMode};
use fastproof_store::{Project, Store};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

#[path = "support/testdb.rs"]
mod testdb;

struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        Some(Self { admin, name, pool })
    }

    async fn migrated() -> Option<(Self, Store)> {
        let db = Self::new().await?;
        fastproof_store::migrate(&db.pool).await.unwrap();
        let store = Store::new(db.pool.clone());
        Some((db, store))
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        drop(self.pool.close());
        testdb::drop_database(&self.admin, &self.name);
    }
}

async fn project(store: &Store, key: &str) -> Project {
    match store.create_project(key, key, "").await.unwrap() {
        CreateProject::Created(p) => *p,
        CreateProject::Exists => panic!("project {key} exists"),
    }
}

type Launch = (Uuid, DateTime<Utc>);

async fn launch(db: &TestDb, p: &Project, status: &str) -> Launch {
    let id = Uuid::now_v7();
    let at: DateTime<Utc> = sqlx::query_scalar(
        "insert into launches (id, project_id, number, name, status, started_at) \
         values ($1, $2, (select coalesce(max(number), 0) + 1 from launches where project_id = $2), 'l', $3, now()) \
         returning created_at",
    )
    .bind(id)
    .bind(p.id)
    .bind(status)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    (id, at)
}

async fn result(db: &TestDb, p: &Project, l: Launch, fp: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, \
           status, fingerprint) values ($1, $2, $3, $4, $5, $6, 'r', 'r', 'failed', $7)",
    )
    .bind(id)
    .bind(l.1)
    .bind(p.id)
    .bind(l.0)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(fp)
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

async fn type_of(store: &Store, p: &Project, group: &str) -> Uuid {
    store
        .defect_types(p.id)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.group == group && t.built_in)
        .unwrap()
        .id
}

fn code(e: &TmError) -> &str {
    match e {
        TmError::NotFound(_) => "not_found",
        TmError::Invalid { code, .. }
        | TmError::Conflict { code, .. }
        | TmError::Forbidden { code, .. } => code,
        TmError::Stale { .. } => "precondition_failed",
        TmError::Other(_) => "other",
    }
}

fn person(name: &str) -> UndoActor {
    UndoActor {
        can_mute: true,
        agent: false,
        name: name.into(),
    }
}

async fn triage(
    store: &Store,
    p: &Project,
    ids: &[Uuid],
    t: Uuid,
    by: &str,
    ctx: &DecisionCtx,
) -> Option<Uuid> {
    let req = TriageRequest {
        result_ids: Some(ids.to_vec()),
        defect_type_id: Some(t),
        ..Default::default()
    };
    store
        .triage_with(p.id, &p.key, &req, by, ctx)
        .await
        .unwrap()
        .unwrap()
        .batch_id
}

async fn group_of(db: &TestDb, r: Uuid) -> Option<String> {
    sqlx::query_scalar("select defect->>'group' from results where id = $1")
        .bind(r)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn batch(store: &Store, p: &Project, b: Uuid) -> Vec<TriageDecision> {
    store
        .triage_history(
            p.id,
            &TriageHistoryQuery {
                batch_id: Some(b),
                limit: 500,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn upgrade_from_the_tm_level_keeps_data_and_queries() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    fastproof_store::migrator()
        .run_to(912, &db.pool)
        .await
        .unwrap();
    let pid = Uuid::now_v7();
    sqlx::query("insert into projects (id, key, name) values ($1, 'old', 'Old')")
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    let lid = Uuid::now_v7();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status) values ($1, $2, 1, 'n', 'finished')",
    )
    .bind(lid)
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, last_seen_at, first_launch_id) \
         values ($1, '0123456789abcdef', 'boom', now(), now(), $2)",
    )
    .bind(pid)
    .bind(lid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into rules (id, project_id, name, match, action) values ($1, $2, 'r', '{\"fingerprint\": \"0123456789abcdef\"}', '{\"comment\": \"c\"}')",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("insert into analysis_tasks (id, project_id, kind) values ($1, $2, 'rule.apply')")
        .bind(Uuid::now_v7())
        .bind(pid)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("insert into replica_levels (instance, tm_level, version) values ('old-1', 2, '0.2.0-dev2')")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into launch_gates (launch_id, project_id, state, status, evaluated_at) values ($1, $2, 'done', 'passed', now())",
    )
    .bind(lid)
    .bind(pid)
    .execute(&db.pool)
    .await
    .unwrap();

    let mut applied = fastproof_store::migrate(&db.pool).await.unwrap();
    applied.sort();
    assert_eq!(applied, (1000..=1011).collect::<Vec<i64>>());
    fastproof_store::check_schema(&db.pool).await.unwrap();
    let (state, regressed): (String, i32) = sqlx::query_as(
        "select state, regressed_count from failure_fingerprints where project_id = $1",
    )
    .bind(pid)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((state.as_str(), regressed), ("active", 0));
    let level: i32 =
        sqlx::query_scalar("select analysis_level from replica_levels where instance = 'old-1'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(level, 0);
    let invalid: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint where conname = 'failure_fingerprints_state_check' and not convalidated",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(invalid.is_empty(), "{invalid:?}");
    let indexes: Vec<(String, bool)> = sqlx::query_as(
        "select c.relname::text, i.indisvalid from pg_index i join pg_class c on c.oid = i.indexrelid \
         where c.relname in ('failure_fingerprints_state', 'failure_fingerprints_alias', 'launch_gates_history') order by 1",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            ("failure_fingerprints_alias".to_string(), true),
            ("failure_fingerprints_state".to_string(), true),
            ("launch_gates_history".to_string(), true)
        ]
    );
    let store = Store::new(db.pool.clone());
    let rules = store.rules(pid).await.unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].readable() && rules[0].phase == RulePhase::Ingest);
    let levels = store.replica_levels().await.unwrap();
    assert_eq!(levels[0].analysis_level, 0);
    let s = store.project_settings(pid).await.unwrap();
    assert!(s.analysis.is_default());
    assert_eq!(store.analysis_launch_status(lid).await.unwrap(), None);
}

#[tokio::test]
async fn the_analysis_gate_follows_levels_settle_and_mode() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    fastproof_store::analysis::gate::reset_cache();
    let settle = Duration::from_secs(600);
    let g = store.analysis_gate(settle).await.unwrap();
    assert!(g.open, "a fresh install is open: {g:?}");
    store.replica_heartbeat("new-1", "0.3.0").await.unwrap();
    assert!(store.analysis_gate(settle).await.unwrap().open);
    sqlx::query("insert into replica_levels (instance, tm_level, version) values ('old-1', 2, '0.2.0-dev2')")
        .execute(&db.pool)
        .await
        .unwrap();
    let g = store.analysis_gate(settle).await.unwrap();
    assert!(!g.open);
    assert_eq!(g.behind, vec!["old-1".to_string()]);
    let tm = store.tm_gate(settle).await.unwrap();
    assert!(
        tm.open,
        "the TM level of the old replica is current: {tm:?}"
    );
    sqlx::query(
        "update replica_levels set seen_at = now() - interval '3 minutes' where instance = 'old-1'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(store.analysis_gate(settle).await.unwrap().open);

    // An upgrade (not a fresh install): the settle time after migration 1011 holds it closed.
    sqlx::query(
        "update fastproof_schema_migrations set installed_on = now() - interval '1 day' where version < 1000",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let g = store.analysis_gate(settle).await.unwrap();
    assert!(
        !g.open && g.settle_until.is_some_and(|u| u > Utc::now()),
        "{g:?}"
    );
    assert!(store.analysis_gate(Duration::ZERO).await.unwrap().open);

    let mut limits = AnalysisLimits::default();
    let mut conn = db.pool.acquire().await.unwrap();
    fastproof_store::analysis::gate::reset_cache();
    let e = fastproof_store::analysis::gate::require(&mut conn, &limits, "analysis.autoApply")
        .await
        .unwrap_err();
    match &e {
        TmError::Conflict { code, details, .. } => {
            assert_eq!(*code, "upgrade_in_progress");
            assert_eq!(
                details.clone().unwrap(),
                json!({"what": "analysis.autoApply", "requiredLevel": 1, "area": "analysis"})
            );
        }
        other => panic!("{other:?}"),
    }
    limits.gate_mode = TmGateMode::Open;
    fastproof_store::analysis::gate::require(&mut conn, &limits, "x")
        .await
        .unwrap();
    limits.gate_mode = TmGateMode::Closed;
    limits.gate_settle_seconds = 0;
    assert!(
        !fastproof_store::analysis::gate::is_open(&mut conn, &limits)
            .await
            .unwrap()
    );
    limits.gate_mode = TmGateMode::Auto;
    fastproof_store::analysis::gate::reset_cache();
    assert!(
        fastproof_store::analysis::gate::is_open(&mut conn, &limits)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn journal_history_and_undo() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "jr").await;
    let other = project(&store, "ot").await;
    let pb = type_of(&store, &p, "productBug").await;
    let ab = type_of(&store, &p, "automationBug").await;
    let si = type_of(&store, &p, "systemIssue").await;
    let l = launch(&db, &p, "finished").await;
    let r1 = result(&db, &p, l, "0000000000000001").await;
    let r2 = result(&db, &p, l, "0000000000000001").await;
    let r3 = result(&db, &p, l, "0000000000000002").await;
    let api = DecisionCtx::api();

    // A batch per request, one row per changed result.
    let b1 = triage(&store, &p, &[r1, r2], pb, "alice", &api)
        .await
        .unwrap();
    let rows = batch(&store, &p, b1).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.via == DecisionVia::Api
        && r.source == DecisionSource::User
        && r.by == "alice"
        && r.before.defect.is_none()
        && r.after.defect.as_ref().unwrap().type_id == pb
        && r.undoable
        && r.fingerprint.as_deref() == Some("0000000000000001")));
    let decided: i64 = sqlx::query_scalar(
        "select count(*) from events where type = 'triage.decided' and payload->>'batchId' = $1",
    )
    .bind(b1.to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(decided, 1);
    let (st, _) = sqlx::query_as::<_, (String, serde_json::Value)>(
        "select type, payload from events where type = 'triage.decided' order by id desc limit 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(st, "triage.decided");

    // Undo restores; it is a batch of its own, and can be undone in turn.
    let u = store
        .undo_triage(p.id, &p.key, b1, "alice", &person("alice"))
        .await
        .unwrap();
    assert_eq!((u.restored, u.reverts, u.skipped.len()), (2, b1, 0));
    assert_eq!(group_of(&db, r1).await, None);
    let ub = u.batch_id.unwrap();
    let rows = batch(&store, &p, ub).await;
    assert!(
        rows.iter()
            .all(|r| r.via == DecisionVia::Undo && r.reverts_batch == Some(b1))
    );
    assert!(
        !batch(&store, &p, b1).await[0].undoable,
        "nothing is at its after any more"
    );
    let uu = store
        .undo_triage(p.id, &p.key, ub, "alice", &person("alice"))
        .await
        .unwrap();
    assert_eq!(uu.restored, 2);
    assert_eq!(group_of(&db, r2).await.as_deref(), Some("productBug"));

    // Rows changed since are skipped.
    let b3 = triage(&store, &p, &[r1, r2], ab, "alice", &api)
        .await
        .unwrap();
    triage(&store, &p, &[r1], si, "bob", &api).await.unwrap();
    let u = store
        .undo_triage(p.id, &p.key, b3, "alice", &person("alice"))
        .await
        .unwrap();
    assert_eq!(u.restored, 1);
    assert_eq!(
        u.skipped,
        vec![TriageUndoSkip {
            result_id: r1,
            code: TriageUndoSkipCode::ChangedSince
        }]
    );
    assert_eq!(group_of(&db, r1).await.as_deref(), Some("systemIssue"));
    assert_eq!(group_of(&db, r2).await.as_deref(), Some("productBug"));

    // A history of a result, newest first, paged.
    let h = store
        .triage_history(
            p.id,
            &TriageHistoryQuery {
                result_id: Some(r1),
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(h.items.len(), 2);
    assert!(h.items[0].id > h.items[1].id && h.next.is_some());
    assert_eq!(h.items[0].by, "bob");
    let h2 = store
        .triage_history(
            p.id,
            &TriageHistoryQuery {
                result_id: Some(r1),
                before: h.next.clone(),
                limit: 500,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(h2.items.iter().all(|i| i.id < h.items[1].id));
    let e = store
        .triage_history(p.id, &TriageHistoryQuery::default())
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let e = store
        .triage_history(
            p.id,
            &TriageHistoryQuery {
                result_id: Some(r1),
                before: Some("x".into()),
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let foreign = store
        .triage_history(
            other.id,
            &TriageHistoryQuery {
                result_id: Some(r1),
                limit: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(foreign.items.is_empty(), "another project sees nothing");
    let e = store
        .undo_triage(other.id, &other.key, b3, "x", &person("x"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found");
    let e = store
        .undo_triage(p.id, &p.key, Uuid::now_v7(), "x", &person("x"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found");

    // A mute batch needs results:mute; undo drops the mute the batch wrote.
    let mreq = TriageRequest {
        result_ids: Some(vec![r3]),
        mute: Some(MuteRequest {
            until: None,
            reason: "flaky".into(),
        }),
        ..Default::default()
    };
    let bm = store
        .triage_with(p.id, &p.key, &mreq, "alice", &api)
        .await
        .unwrap()
        .unwrap()
        .batch_id
        .unwrap();
    let no_mute = UndoActor {
        can_mute: false,
        ..person("alice")
    };
    let e = store
        .undo_triage(p.id, &p.key, bm, "alice", &no_mute)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "forbidden");
    let u = store
        .undo_triage(p.id, &p.key, bm, "alice", &person("alice"))
        .await
        .unwrap();
    assert_eq!(u.restored, 1);
    let (muted, mutes, counted): (bool, i64, i32) = sqlx::query_as(
        "select r.muted, (select count(*) from series_mutes m where m.series_id = r.series_id), \
           (select count_muted from launches where id = r.launch_id) from results r where r.id = $1",
    )
    .bind(r3)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((muted, mutes, counted), (false, 0, 0));
    // An unmute batch undone recreates the mute.
    store
        .triage_with(p.id, &p.key, &mreq, "alice", &api)
        .await
        .unwrap()
        .unwrap();
    let ureq = TriageRequest {
        result_ids: Some(vec![r3]),
        unmute: Some(true),
        ..Default::default()
    };
    let bu = store
        .triage_with(p.id, &p.key, &ureq, "alice", &api)
        .await
        .unwrap()
        .unwrap()
        .batch_id
        .unwrap();
    store
        .undo_triage(p.id, &p.key, bu, "alice", &person("alice"))
        .await
        .unwrap();
    let (muted, reason): (bool, Option<String>) = sqlx::query_as(
        "select r.muted, (select reason from series_mutes m where m.series_id = r.series_id) from results r where r.id = $1",
    )
    .bind(r3)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((muted, reason.as_deref()), (true, Some("restored by undo")));

    // An agent undoes only its own batches; its undo is `ai`.
    let ai = DecisionCtx {
        source: Some(DecisionSource::Ai),
        ..DecisionCtx::new(DecisionVia::Mcp)
    };
    let bb = triage(&store, &p, &[r2], ab, "bot", &ai).await.unwrap();
    assert_eq!(batch(&store, &p, bb).await[0].source, DecisionSource::Ai);
    let other_bot = UndoActor {
        can_mute: false,
        agent: true,
        name: "other-bot".into(),
    };
    let e = store
        .undo_triage(p.id, &p.key, bb, "other-bot", &other_bot)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_your_batch");
    let bot = UndoActor {
        name: "bot".into(),
        ..other_bot
    };
    let u = store
        .undo_triage(p.id, &p.key, bb, "bot", &bot)
        .await
        .unwrap();
    assert_eq!(
        batch(&store, &p, u.batch_id.unwrap()).await[0].source,
        DecisionSource::Ai
    );

    // Rule batches are not undoable.
    let lr = LaunchRef {
        id: l.0,
        project_id: p.id,
        project_key: p.key.clone(),
        created_at: l.1,
    };
    let mut conn = db.pool.acquire().await.unwrap();
    let rb = decisions::record_rule_changes(
        &mut conn,
        &lr,
        &[decisions::RuleChangeRow {
            result_id: r1,
            series_id: Uuid::now_v7(),
            fingerprint: None,
            rule_id: Uuid::now_v7(),
            before_defect: None,
            before_muted: false,
            before_mute_until: None,
            after_defect: Some(json!({"x": 1})),
            after_muted: false,
            after_mute_until: None,
        }],
    )
    .await
    .unwrap()
    .unwrap();
    drop(conn);
    let rows = batch(&store, &p, rb).await;
    assert!(
        rows[0].via == DecisionVia::Rule && rows[0].by.starts_with("rule:") && !rows[0].undoable
    );
    let e = store
        .undo_triage(p.id, &p.key, rb, "alice", &person("alice"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "batch_not_undoable");

    // Results of a merged launch and results that are gone are skipped.
    let l2 = launch(&db, &p, "finished").await;
    let r4 = result(&db, &p, l2, "0000000000000003").await;
    let r5 = result(&db, &p, l2, "0000000000000003").await;
    let b9 = triage(&store, &p, &[r4, r5], pb, "alice", &api)
        .await
        .unwrap();
    sqlx::query("delete from results where id = $1")
        .bind(r5)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update launches set merged_into = $1 where id = $2")
        .bind(l.0)
        .bind(l2.0)
        .execute(&db.pool)
        .await
        .unwrap();
    let u = store
        .undo_triage(p.id, &p.key, b9, "alice", &person("alice"))
        .await
        .unwrap();
    assert_eq!(u.restored, 0);
    assert_eq!(u.batch_id, None);
    let mut codes: Vec<(Uuid, TriageUndoSkipCode)> =
        u.skipped.iter().map(|s| (s.result_id, s.code)).collect();
    codes.sort_by_key(|c| c.0);
    assert_eq!(
        codes,
        vec![
            (r4, TriageUndoSkipCode::LaunchMerged),
            (r5, TriageUndoSkipCode::ResultGone)
        ]
    );

    // A batch above the undo limit.
    let big = Uuid::now_v7();
    sqlx::query(
        "insert into triage_decisions (project_id, launch_id, launch_created_at, result_id, series_id, batch_id, \
           via, source, by, before, after) \
         select $1, $2, $3, gen_random_uuid(), gen_random_uuid(), $4, 'api', 'user', 'alice', '{}', '{}' \
         from generate_series(1, $5)",
    )
    .bind(p.id)
    .bind(l.0)
    .bind(l.1)
    .bind(big)
    .bind(MAX_UNDO_ROWS as i32 + 1)
    .execute(&db.pool)
    .await
    .unwrap();
    let e = store
        .undo_triage(p.id, &p.key, big, "alice", &person("alice"))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "batch_too_large");
}

#[tokio::test]
async fn internal_callers_protect_people_and_set_their_source() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ic").await;
    let pb = type_of(&store, &p, "productBug").await;
    let ab = type_of(&store, &p, "automationBug").await;
    let l = launch(&db, &p, "finished").await;
    let person_decided = result(&db, &p, l, "0000000000000001").await;
    let open = result(&db, &p, l, "0000000000000001").await;
    triage(
        &store,
        &p,
        &[person_decided],
        pb,
        "alice",
        &DecisionCtx::api(),
    )
    .await
    .unwrap();
    let auto = DecisionCtx {
        source: Some(DecisionSource::Auto),
        skip_protected: true,
        suggestion_id: Some(Uuid::now_v7()),
        ..DecisionCtx::new(DecisionVia::Auto)
    };
    let req = TriageRequest {
        result_ids: Some(vec![person_decided, open]),
        defect_type_id: Some(ab),
        ..Default::default()
    };
    let out = store
        .triage_with(p.id, &p.key, &req, "fastproof-analysis", &auto)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.updated, 1);
    assert_eq!(
        group_of(&db, person_decided).await.as_deref(),
        Some("productBug")
    );
    assert_eq!(group_of(&db, open).await.as_deref(), Some("automationBug"));
    let source: String = sqlx::query_scalar("select defect->>'source' from results where id = $1")
        .bind(open)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(source, "auto");
    let rows = batch(&store, &p, out.batch_id.unwrap()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].via, rows[0].source, rows[0].suggestion_id),
        (DecisionVia::Auto, DecisionSource::Auto, auto.suggestion_id)
    );
    // Only protected results: nothing changes, no batch.
    let req = TriageRequest {
        result_ids: Some(vec![person_decided]),
        defect_type_id: Some(ab),
        ..Default::default()
    };
    let out = store
        .triage_with(p.id, &p.key, &req, "fastproof-analysis", &auto)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((out.updated, out.batch_id), (0, None));
    let v = serde_json::to_value(&out).unwrap();
    assert_eq!(v, json!({"updated": 0, "batchId": null}));
}

#[tokio::test]
async fn the_consumer_queues_finished_launches_and_workers_claim_them() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "qu").await;
    let old = launch(&db, &p, "finished").await;
    let running = launch(&db, &p, "running").await;
    let emit = |id: Uuid| {
        let pool = db.pool.clone();
        let key = p.key.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            fastproof_store::events::emit(
                &mut conn,
                "launch.finished",
                Some(&key),
                json!({"id": id}),
            )
            .await
            .unwrap();
        }
    };
    // First run: the cursor starts at the end of the outbox and the recent launches are queued.
    let n = store
        .consume_analysis_launches("analysis", Duration::ZERO, 500)
        .await
        .unwrap();
    assert_eq!(n, Some(0));
    let row = store.analysis_launch_row(old.0).await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.reason),
        (AnalysisLaunchState::Queued, QueueReason::Backfill)
    );
    assert!(
        store
            .analysis_launch_row(running.0)
            .await
            .unwrap()
            .is_none()
    );

    // A finished launch is queued once its event is older than the settle time.
    let l = launch(&db, &p, "finished").await;
    emit(l.0).await;
    let n = store
        .consume_analysis_launches("analysis", Duration::from_secs(3600), 500)
        .await
        .unwrap();
    assert_eq!(n, Some(0));
    assert!(store.analysis_launch_row(l.0).await.unwrap().is_none());
    let n = store
        .consume_analysis_launches("analysis", Duration::ZERO, 500)
        .await
        .unwrap();
    assert_eq!(n, Some(1));
    let row = store.analysis_launch_row(l.0).await.unwrap().unwrap();
    assert_eq!(row.reason, QueueReason::Finished);

    // A launch another transaction holds waits in `pending`.
    let held = launch(&db, &p, "finished").await;
    emit(held.0).await;
    let mut lock = db.pool.begin().await.unwrap();
    sqlx::query("select 1 from launches where id = $1 for update")
        .bind(held.0)
        .execute(&mut *lock)
        .await
        .unwrap();
    store
        .consume_analysis_launches("analysis", Duration::ZERO, 500)
        .await
        .unwrap();
    assert!(store.analysis_launch_row(held.0).await.unwrap().is_none());
    let pending: Vec<Uuid> =
        sqlx::query_scalar("select pending from integration_cursors where name = 'analysis'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pending, vec![held.0]);
    lock.rollback().await.unwrap();
    store
        .consume_analysis_launches("analysis", Duration::ZERO, 500)
        .await
        .unwrap();
    assert!(store.analysis_launch_row(held.0).await.unwrap().is_some());
    let gates: i64 = sqlx::query_scalar("select count(*) from launch_gates")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(gates, 0, "the analysis consumer does not queue gates");

    // Claims: oldest due first; a launch with an upload still processing waits.
    sqlx::query(
        "update analysis_launches set due_at = now() + interval '1 hour' where launch_id <> $1",
    )
    .bind(l.0)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into uploads (id, project_id, launch_id, status) values ($1, $2, $3, 'processing')",
    )
    .bind(Uuid::now_v7())
    .bind(p.id)
    .bind(l.0)
    .execute(&db.pool)
    .await
    .unwrap();
    let lease = Duration::from_secs(60);
    assert!(
        store
            .claim_analysis_launch("w1", lease)
            .await
            .unwrap()
            .is_none()
    );
    let row = store.analysis_launch_row(l.0).await.unwrap().unwrap();
    assert!(row.state == AnalysisLaunchState::Queued && row.due_at > Utc::now());
    sqlx::query("update uploads set status = 'done' where launch_id = $1")
        .bind(l.0)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("update analysis_launches set due_at = now() where launch_id = $1")
        .bind(l.0)
        .execute(&db.pool)
        .await
        .unwrap();
    let c = store
        .claim_analysis_launch("w1", lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.launch_id, c.state, c.attempts),
        (l.0, AnalysisLaunchState::Running, 1)
    );
    assert!(
        store
            .claim_analysis_launch("w2", lease)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.renew_analysis_launch(l.0, "w1", lease).await.unwrap());
    assert!(!store.renew_analysis_launch(l.0, "w2", lease).await.unwrap());

    // Queued again while it runs: it runs once more after.
    emit(l.0).await;
    store
        .consume_analysis_launches("analysis", Duration::ZERO, 500)
        .await
        .unwrap();
    let steps = LaunchSteps {
        errors: Some(StepOutcome::skipped("not available")),
        ..Default::default()
    };
    assert!(
        !store
            .finish_analysis_launch(l.0, "w2", &steps, None)
            .await
            .unwrap()
    );
    assert!(
        store
            .finish_analysis_launch(l.0, "w1", &steps, None)
            .await
            .unwrap()
    );
    let row = store.analysis_launch_row(l.0).await.unwrap().unwrap();
    assert_eq!(row.state, AnalysisLaunchState::Queued);
    assert_eq!(row.reason, QueueReason::Finished);
    let c = store
        .claim_analysis_launch("w1", lease)
        .await
        .unwrap()
        .unwrap();
    store
        .finish_analysis_launch(c.launch_id, "w1", &steps, None)
        .await
        .unwrap();
    let st = store.analysis_launch_status(l.0).await.unwrap().unwrap();
    assert_eq!(st.state, AnalysisLaunchState::Done);
    assert_eq!(st.steps, steps);

    // Failures are retried with a growing delay, then the launch is failed.
    for attempt in 1..=4 {
        if attempt > 1
            || store
                .enqueue_analysis_launch(p.id, l.0, QueueReason::Recompute, Duration::ZERO)
                .await
                .unwrap()
        {
            sqlx::query("update analysis_launches set due_at = now() where launch_id = $1")
                .bind(l.0)
                .execute(&db.pool)
                .await
                .unwrap();
        }
        let c = store
            .claim_analysis_launch("w1", lease)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(c.attempts, attempt);
        store
            .finish_analysis_launch(l.0, "w1", &LaunchSteps::default(), Some("boom"))
            .await
            .unwrap();
        let row = store.analysis_launch_row(l.0).await.unwrap().unwrap();
        if attempt <= 3 {
            assert_eq!(row.state, AnalysisLaunchState::Queued);
            assert!(row.due_at > Utc::now() + chrono::Duration::seconds(50 * attempt as i64));
        } else {
            assert_eq!(row.state, AnalysisLaunchState::Failed);
            assert_eq!(row.error.as_deref(), Some("boom"));
        }
    }
    let d = store.analysis_queue_depth().await.unwrap();
    assert_eq!(d.failed, 1);

    // Recompute: a running launch is never queued; the delay counts from the last run.
    assert!(
        !store
            .enqueue_analysis_launch(p.id, running.0, QueueReason::Recompute, Duration::ZERO)
            .await
            .unwrap()
    );
    let other = project(&store, "qo").await;
    assert!(
        !store
            .enqueue_analysis_launch(other.id, l.0, QueueReason::Recompute, Duration::ZERO)
            .await
            .unwrap()
    );
    assert!(
        store
            .enqueue_analysis_launch(p.id, l.0, QueueReason::Recompute, Duration::from_secs(30))
            .await
            .unwrap()
    );
    let row = store.analysis_launch_row(l.0).await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.reason, row.attempts, row.error),
        (AnalysisLaunchState::Queued, QueueReason::Recompute, 0, None)
    );
    assert!(row.due_at > Utc::now() + chrono::Duration::seconds(20));
}

#[tokio::test]
async fn the_standard_task_job_runs_only_rule_apply() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "tk").await;
    for kind in ["backtest", "log_rule.apply", "some.future.kind"] {
        sqlx::query("insert into analysis_tasks (id, project_id, kind, params, created_by) values ($1, $2, $3, '{}', 'alice')")
            .bind(Uuid::now_v7())
            .bind(p.id)
            .bind(kind)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    assert_eq!(
        store.run_tasks("t", Duration::from_secs(5)).await.unwrap(),
        0
    );
    let queued: i64 =
        sqlx::query_scalar("select count(*) from analysis_tasks where status = 'queued'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(queued, 3);
    let t = store
        .claim_analysis_task(
            "an",
            &["backtest", "log_rule.apply"],
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (t.kind.as_str(), t.created_by.as_str()),
        ("backtest", "alice")
    );
    assert!(
        store
            .task_progress(&t, json!({"events": 1}), Duration::from_secs(60))
            .await
            .unwrap()
    );
    assert!(
        store
            .finish_analysis_task(&t, Err("not available".into()))
            .await
            .unwrap()
    );
    let task = store.task(p.id, t.id).await.unwrap().unwrap();
    assert_eq!(
        (task.status.as_str(), task.error.as_deref()),
        ("failed", Some("not available"))
    );
    let t = store
        .claim_analysis_task(
            "an",
            &["backtest", "log_rule.apply"],
            Duration::from_secs(60),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.kind, "log_rule.apply");
    assert!(
        store
            .claim_analysis_task(
                "an",
                &["backtest", "log_rule.apply"],
                Duration::from_secs(60)
            )
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unreadable_rules_are_never_evaluated() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ur").await;
    let insert = |m: serde_json::Value, a: serde_json::Value| {
        let pool = db.pool.clone();
        async move {
            let id = Uuid::now_v7();
            sqlx::query("insert into rules (id, project_id, name, match, action) values ($1, $2, 'r', $3, $4)")
                .bind(id)
                .bind(p.id)
                .bind(m)
                .bind(a)
                .execute(&pool)
                .await
                .unwrap();
            id
        }
    };
    let unknown = insert(
        json!({"messageRegex": ".*", "fromTheFuture": true}),
        json!({"comment": "c"}),
    )
    .await;
    let bad_action = insert(json!({"messageRegex": ".*"}), json!({"explode": 1})).await;
    let log = insert(
        json!({"log": {"contains": "OOM"}}),
        json!({"pattern": "oom"}),
    )
    .await;
    let plain = insert(json!({"messageRegex": "boom"}), json!({"comment": "c"})).await;
    let rules = store.rules(p.id).await.unwrap();
    let by = |id: Uuid| rules.iter().find(|r| r.id == id).unwrap();
    for id in [unknown, bad_action] {
        let r = by(id);
        assert!(!r.readable());
        assert_eq!(r.phase, RulePhase::AfterLaunch);
        assert!(r.note.as_deref().unwrap().contains("not readable"));
        let v = serde_json::to_value(r).unwrap();
        assert_eq!(v["phase"], "afterLaunch");
    }
    assert_eq!(by(log).phase, RulePhase::AfterLaunch);
    assert!(by(log).readable());
    assert_eq!(by(plain).phase, RulePhase::Ingest);
    assert_eq!(serde_json::to_value(by(plain)).unwrap().get("note"), None);
    let mut conn = db.pool.acquire().await.unwrap();
    let ingest: Vec<Uuid> = enabled_rules(&mut conn, p.id, RulePhase::Ingest)
        .await
        .unwrap()
        .iter()
        .map(|r| r.rule.id)
        .collect();
    assert_eq!(ingest, vec![plain]);
    let after: Vec<Uuid> = enabled_rules(&mut conn, p.id, RulePhase::AfterLaunch)
        .await
        .unwrap()
        .iter()
        .map(|r| r.rule.id)
        .collect();
    assert_eq!(after, vec![log]);
}

#[tokio::test]
async fn deleting_a_custom_type_drops_what_analysis_learned_about_it() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "dt").await;
    let created = store
        .create_defect_type(
            p.id,
            &fastproof_store::analysis::defects::NewDefectType {
                group: "productBug".into(),
                name: "Payments".into(),
                short_name: None,
                color: None,
            },
        )
        .await
        .unwrap();
    let t = match created {
        fastproof_store::analysis::defects::DefectTypeChange::Done(t) => t.id,
        other => panic!("{other:?}"),
    };
    let keep = type_of(&store, &p, "productBug").await;
    let l = launch(&db, &p, "finished").await;
    for ty in [t, keep] {
        sqlx::query(
            "insert into triage_memory (project_id, fingerprint, defect_type_id, grp, first_decided_at, last_decided_at, last_decided_by) \
             values ($1, '0000000000000001', $2, 'productBug', now(), now(), 'alice')",
        )
        .bind(p.id)
        .bind(ty)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into triage_suggestions (id, project_id, launch_id, fingerprint, rank, band, score, defect_type_id, grp, why, signal) \
             values ($1, $2, $3, '0000000000000001', $4, 'high', 0.8, $5, 'productBug', '[]', 'memory')",
        )
        .bind(Uuid::now_v7())
        .bind(p.id)
        .bind(l.0)
        .bind(if ty == t { 1i16 } else { 2i16 })
        .bind(ty)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    store.delete_defect_type(p.id, t).await.unwrap();
    let memory: Vec<Uuid> =
        sqlx::query_scalar("select defect_type_id from triage_memory where project_id = $1")
            .bind(p.id)
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(memory, vec![keep]);
    let states: Vec<(Uuid, String)> = sqlx::query_as(
        "select defect_type_id, state from triage_suggestions where project_id = $1 order by rank",
    )
    .bind(p.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        vec![(t, "superseded".into()), (keep, "open".into())]
    );
}

#[tokio::test]
async fn settings_without_analysis_stay_byte_identical() {
    let doc = json!({"flaky": {"historyDepth": 10, "stabilizationPeriod": 3, "minFlips": 2},
        "gates": [{"name": "g", "rules": [{"metric": "failed", "op": "lte", "value": 0}]}],
        "vcs": {"url": "https://git.example.com/a/b"}});
    let s = ProjectSettings::from_value(doc.clone());
    assert!(s.analysis.is_default());
    let back = serde_json::to_value(&s).unwrap();
    assert!(back.get("analysis").is_none(), "{back}");
    let again = serde_json::to_value(ProjectSettings::from_value(back.clone())).unwrap();
    assert_eq!(
        serde_json::to_string(&back).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
    let mut with = back.clone();
    with["analysis"] = json!({"enabled": false, "memoryDays": 90});
    let s = ProjectSettings::from_value(with);
    assert!(!s.analysis.enabled && s.analysis.memory_days == 90);
    assert_eq!(
        serde_json::to_value(&s).unwrap()["analysis"]["memoryDays"],
        90
    );
    let mut bad = back.clone();
    bad["analysis"] = json!({"enabled": false, "unknown": 1});
    let s = ProjectSettings::from_value(bad);
    assert!(
        s.analysis.is_default(),
        "an unreadable area falls back to its default"
    );
    assert_eq!(s.flaky.history_depth, 10, "other areas are kept");
    let mut invalid = ProjectSettings::default();
    invalid.analysis.memory_days = 10;
    assert!(
        invalid
            .validate()
            .unwrap_err()
            .contains("analysis.memoryDays")
    );
}

#[tokio::test]
async fn prune_and_capabilities() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "pr").await;
    let mut s = store.project_settings(p.id).await.unwrap();
    s.analysis.memory_days = 30;
    store.put_project_settings(p.id, &s).await.unwrap();
    for (fp, days) in [("0000000000000001", 40), ("0000000000000002", 5)] {
        sqlx::query(
            "insert into triage_memory (project_id, fingerprint, defect_type_id, grp, first_decided_at, last_decided_at, last_decided_by) \
             values ($1, $2, gen_random_uuid(), 'productBug', now(), now() - make_interval(days => $3), 'alice')",
        )
        .bind(p.id)
        .bind(fp)
        .bind(days)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into triage_memory_events (project_id, decided_at, batch_id, fingerprint, defect_type_id, grp, results, by, via) \
             values ($1, now() - make_interval(days => $3), gen_random_uuid(), $2, gen_random_uuid(), 'productBug', 1, 'alice', 'api')",
        )
        .bind(p.id)
        .bind(fp)
        .bind(days)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into analysis_backtests (id, project_id, source, report, decisions, created_by, created_at) \
         select gen_random_uuid(), $1, 'synthetic', '{}', 0, 'alice', now() - make_interval(mins => g) from generate_series(1, 25) g",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into attempt_signals (project_id, source, kind, at, confidence, dedup_key, created_at) values \
         ($1, 'ci', 'runner_oom', now(), 'exact', 'old', now() - interval '3 days'), \
         ($1, 'ci', 'runner_oom', now(), 'exact', 'new', now())",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let old = launch(&db, &p, "finished").await;
    sqlx::query("update launches set created_at = now() - interval '40 days' where id = $1")
        .bind(old.0)
        .execute(&db.pool)
        .await
        .unwrap();
    let fresh = launch(&db, &p, "finished").await;
    for l in [old.0, fresh.0] {
        sqlx::query(
            "insert into triage_suggestions (id, project_id, launch_id, fingerprint, rank, band, score, defect_type_id, grp, why, signal) \
             values ($1, $2, $3, '0000000000000001', 1, 'high', 0.8, gen_random_uuid(), 'productBug', '[]', 'memory')",
        )
        .bind(Uuid::now_v7())
        .bind(p.id)
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let n = store.prune_analysis(Duration::from_secs(60)).await.unwrap();
    assert_eq!(n, 1 + 1 + 1 + 5 + 1);
    let count = |sql: &'static str| {
        let pool = db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count("select count(*) from triage_memory").await, 1);
    assert_eq!(count("select count(*) from triage_memory_events").await, 1);
    assert_eq!(count("select count(*) from analysis_backtests").await, 20);
    assert_eq!(count("select count(*) from attempt_signals").await, 1);
    assert_eq!(
        count("select count(*) from triage_suggestions where state = 'expired'").await,
        1
    );
    assert_eq!(
        store.prune_analysis(Duration::from_secs(60)).await.unwrap(),
        0
    );

    let mut inputs = CapabilityInputs {
        limits: AnalysisLimits {
            gate_mode: TmGateMode::Open,
            ..Default::default()
        },
        implemented: AnalysisFeatures {
            suggestions: true,
            portfolio: true,
            ..Default::default()
        },
        clickhouse_days: 730,
        ..Default::default()
    };
    let c = store.analysis_capabilities(p.id, &inputs).await.unwrap();
    assert!(!c.enabled && !c.instance_enabled && c.project_enabled && c.gate_open);
    assert!(!c.features.suggestions && !c.features.portfolio);
    assert_eq!(
        (
            c.windows.raw_days,
            c.windows.memory_days,
            c.windows.analytics_days
        ),
        (90, 30, 30)
    );
    assert_eq!(c.with_click_house, vec!["portfolio", "crossProjectSimilar"]);
    inputs.instance_enabled = true;
    let c = store.analysis_capabilities(p.id, &inputs).await.unwrap();
    assert!(c.enabled && c.features.suggestions && !c.features.errors && !c.features.portfolio);
    inputs.clickhouse = true;
    let c = store.analysis_capabilities(p.id, &inputs).await.unwrap();
    assert!(c.clickhouse && c.features.portfolio && !c.features.cross_project_similar);
    assert_eq!(c.windows.analytics_days, 730);
    let mut s = store.project_settings(p.id).await.unwrap();
    s.analysis.enabled = false;
    store.put_project_settings(p.id, &s).await.unwrap();
    let c = store.analysis_capabilities(p.id, &inputs).await.unwrap();
    assert!(!c.enabled && !c.project_enabled && !c.features.suggestions);
    let v = serde_json::to_value(&c).unwrap();
    assert!(
        v["features"].get("crossProjectSimilar").is_some() && v.get("withClickHouse").is_some()
    );
}
