//! FastProof Analysis suggestions on a real PostgreSQL: the memory fold (grouping, only
//! people teach, overrides, idempotence, undo taken back), the seed, candidates with and
//! without `pg_trgm`, persistence with supersede, accept and reject, the rule offer,
//! auto-apply, and the backtest on stored events against the pure harness. They need
//! `FASTPROOF_TEST_DATABASE_URL`; without it they pass without doing anything.

use std::time::Duration;

use chrono::Utc;
use fastproof_analysis::backtest::{Backtest, BacktestOptions};
use fastproof_analysis::synth::{SynthProfile, generate};
use fastproof_store::analysis::backtest::{PgSource, options_of};
use fastproof_store::analysis::triage::TriageRequest;
use fastproof_store::analysis::types::*;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::{TmError, TmGateMode};
use fastproof_store::{Launch, Project, Store};
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
    async fn migrated() -> Option<(Self, Store)> {
        let admin = testdb::admin()?;
        let name = testdb::create(&admin).await;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        fastproof_store::migrate(&pool).await.unwrap();
        let store = Store::new(pool.clone());
        Some((Self { admin, name, pool }, store))
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

async fn launch(db: &TestDb, store: &Store, p: &Project) -> Launch {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status, started_at, finished_at, branch, env_key) \
         values ($1, $2, (select coalesce(max(number), 0) + 1 from launches where project_id = $2), 'l', \
           'finished', now(), now(), 'main', 'staging')",
    )
    .bind(id)
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    store.launch(p.id, id).await.unwrap().unwrap()
}

/// The catalog row of a fingerprint (first seen in `first`).
async fn catalog(db: &TestDb, p: &Project, fp: &str, message: &str, first: &Launch) {
    sqlx::query(
        "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, first_launch_id, \
           last_seen_at, last_launch_id, occurrences) values ($1, $2, $3, now(), $4, now(), $4, 1) \
         on conflict (project_id, fingerprint) do nothing",
    )
    .bind(p.id)
    .bind(fp)
    .bind(message)
    .bind(first.id)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// A failed result with a test case and an attempt whose trace has two frames.
async fn result(db: &TestDb, p: &Project, l: &Launch, fp: &str, test: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    let series = Uuid::now_v7();
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, \
           status, fingerprint, message) values ($1, $2, $3, $4, $5, $6, $7, $7, 'failed', $8, 'boom')",
    )
    .bind(id)
    .bind(l.created_at)
    .bind(p.id)
    .bind(l.id)
    .bind(test)
    .bind(series)
    .bind(format!("t-{}", &id.simple().to_string()[24..]))
    .bind(fp)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, client_key, seq, \
           status, trace) values ($1, $2, $3, $4, $5, $6, $7, 1, 'failed', $8)",
    )
    .bind(Uuid::now_v7())
    .bind(l.created_at)
    .bind(p.id)
    .bind(l.id)
    .bind(id)
    .bind(series)
    .bind(id.to_string())
    .bind(format!(
        "java.lang.IllegalStateException: boom\n\tat com.example.shop.Cart.total(Cart.java:1)\n\tat com.example.shop.{fp}.run(X.java:2)\n"
    ))
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

async fn triage(
    store: &Store,
    p: &Project,
    ids: &[Uuid],
    t: Uuid,
    by: &str,
    source: &str,
) -> Option<Uuid> {
    let req = TriageRequest {
        result_ids: Some(ids.to_vec()),
        defect_type_id: Some(t),
        source: Some(source.into()),
        ..Default::default()
    };
    store
        .triage_with(p.id, &p.key, &req, by, &DecisionCtx::api())
        .await
        .unwrap()
        .unwrap()
        .batch_id
}

async fn fold(store: &Store) -> u64 {
    store
        .fold_decisions(2000, Duration::from_secs(30), 200_000)
        .await
        .unwrap()
}

/// `(fingerprint, group, confirmed, overridden, tests)` of the memory, ordered in Rust.
async fn memory(db: &TestDb, p: &Project) -> Vec<(String, String, i32, i32, usize)> {
    let mut v: Vec<(String, String, i32, i32, Vec<Uuid>)> = sqlx::query_as(
        "select fingerprint, grp, confirmed, overridden, test_case_ids from triage_memory where project_id = $1",
    )
    .bind(p.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    v.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    v.into_iter()
        .map(|(f, g, c, o, t)| (f, g, c, o, t.len()))
        .collect()
}

fn person(name: &str) -> UndoActor {
    UndoActor {
        can_mute: true,
        agent: false,
        name: name.into(),
    }
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

fn ctx<'a>(
    p: &'a Project,
    l: &'a Launch,
    settings: &'a fastproof_store::analysis::ProjectSettings,
    limits: &'a AnalysisLimits,
) -> LaunchCtx<'a> {
    LaunchCtx {
        project_id: p.id,
        project_key: &p.key,
        launch: l,
        settings,
        limits,
        now: Utc::now(),
    }
}

const FA: &str = "aaaaaaaaaaaaaaaa";
const FB: &str = "bbbbbbbbbbbbbbbb";
const FC: &str = "cccccccccccccccc";
const FD: &str = "dddddddddddddddd";

#[tokio::test]
async fn the_fold_learns_from_people_only_once_and_takes_back_what_is_undone() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fold").await;
    let pb = type_of(&store, &p, "productBug").await;
    let si = type_of(&store, &p, "systemIssue").await;
    let ti = type_of(&store, &p, "toInvestigate").await;
    let l1 = launch(&db, &store, &p).await;
    catalog(&db, &p, FA, "expected total <n> but was <n>", &l1).await;
    catalog(&db, &p, FB, "connection refused to db port <n>", &l1).await;
    let (t1, t2, t3) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let r1 = result(&db, &p, &l1, FA, t1).await;
    let r2 = result(&db, &p, &l1, FA, t2).await;
    let r3 = result(&db, &p, &l1, FB, t3).await;
    let r4 = result(&db, &p, &l1, FB, Uuid::now_v7()).await;

    // One batch on two results of one fingerprint is one group: one confirmation, both tests.
    let b1 = triage(&store, &p, &[r1, r2], pb, "ann", "user")
        .await
        .unwrap();
    // An agent and "to investigate" teach nothing.
    triage(&store, &p, &[r3], si, "bot", "ai").await.unwrap();
    triage(&store, &p, &[r4], ti, "ann", "user").await.unwrap();
    assert_eq!(
        fold(&store).await,
        3,
        "the user rows are read (to investigate teaches nothing), the agent's are not"
    );
    assert_eq!(
        memory(&db, &p).await,
        vec![(FA.into(), "productBug".into(), 1, 0, 2)]
    );
    let (frames, msg, sample): (Vec<String>, String, Option<Uuid>) = sqlx::query_as(
        "select frames, message_norm, sample_result_id from triage_memory where project_id = $1",
    )
    .bind(p.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert_eq!(msg, "expected total <n> but was <n>");
    assert!(sample == Some(r1) || sample == Some(r2));
    // Idempotent: nothing left to read, nothing counted twice.
    assert_eq!(fold(&store).await, 0);
    assert_eq!(memory(&db, &p).await[0].2, 1);

    // A person decides B as a system issue; later an agent sets B to a system issue on
    // another result and a person changes it to a product bug: an override of the first.
    let l2 = launch(&db, &store, &p).await;
    let r5 = result(&db, &p, &l2, FB, t3).await;
    triage(&store, &p, &[r5], si, "ann", "user").await.unwrap();
    fold(&store).await;
    triage(&store, &p, &[r3], pb, "bob", "user").await.unwrap();
    fold(&store).await;
    let m = memory(&db, &p).await;
    assert!(
        m.contains(&(FB.into(), "systemIssue".into(), 1, 1, 1)),
        "the agent's type was overridden by a person: {m:?}"
    );
    assert!(
        m.contains(&(FB.into(), "productBug".into(), 1, 0, 1)),
        "{m:?}"
    );
    let prev: Option<Uuid> = sqlx::query_scalar(
        "select previous_type_id from triage_memory_events where project_id = $1 and fingerprint = $2 \
           and defect_type_id = $3",
    )
    .bind(p.id)
    .bind(FB)
    .bind(pb)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(prev, Some(si));
    // A comment on the same decision is no new decision.
    let req = TriageRequest {
        result_ids: Some(vec![r1]),
        comment: Some("still".into()),
        ..Default::default()
    };
    store
        .triage_with(p.id, &p.key, &req, "ann", &DecisionCtx::api())
        .await
        .unwrap()
        .unwrap();
    fold(&store).await;
    assert!(
        memory(&db, &p)
            .await
            .contains(&(FA.into(), "productBug".into(), 1, 0, 2))
    );

    // Undo of the first batch takes back what it taught: the row and its event go.
    let u = store
        .undo_triage(p.id, &p.key, b1, "ann", &person("ann"))
        .await
        .unwrap();
    assert_eq!(u.restored, 1, "r1 changed since (a comment): {u:?}");
    fold(&store).await;
    let m = memory(&db, &p).await;
    assert!(
        !m.iter().any(|r| r.0 == FA),
        "an undone decision leaves no memory: {m:?}"
    );
    let events: i64 = sqlx::query_scalar(
        "select count(*) from triage_memory_events where project_id = $1 and batch_id = $2",
    )
    .bind(p.id)
    .bind(b1)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events, 0);
    // The export lists what is left, in id order.
    let ev = store.decision_events(p.id, None, 100).await.unwrap();
    assert_eq!(ev.len(), 2);
    assert!(ev.windows(2).all(|w| w[0].0 < w[1].0));
    let after = store
        .decision_events(p.id, Some(ev[0].0), 100)
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    // The memory read of a fingerprint.
    let entries = store.memory_entries(p.id, FB).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| e.sample.is_some() && !e.defect_name.is_empty())
    );
}

#[tokio::test]
async fn the_seed_learns_earlier_triage_once_per_project() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "seed").await;
    let pb = type_of(&store, &p, "productBug").await;
    let l = launch(&db, &store, &p).await;
    catalog(&db, &p, FA, "expected total <n> but was <n>", &l).await;
    let r1 = result(&db, &p, &l, FA, Uuid::now_v7()).await;
    let r2 = result(&db, &p, &l, FA, Uuid::now_v7()).await;
    let r3 = result(&db, &p, &l, FB, Uuid::now_v7()).await;
    // r1 and r2: decided by a person before the journal (no journal rows); r3 by a rule.
    for (r, source) in [(r1, "user"), (r2, "user"), (r3, "rule")] {
        sqlx::query(
            "update results set defect = jsonb_build_object('typeId', $2::text, 'group', 'productBug', \
               'name', 'Product bug', 'source', $3::text, 'at', now(), 'by', 'ann') where id = $1",
        )
        .bind(r)
        .bind(pb.to_string())
        .bind(source)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    assert_eq!(store.memory_seed_pending(10).await.unwrap(), vec![p.id]);
    let limits = AnalysisLimits::default();
    assert!(
        store
            .seed_triage_memory(p.id, &limits, Duration::from_secs(30))
            .await
            .unwrap()
    );
    assert_eq!(
        memory(&db, &p).await,
        vec![(FA.into(), "productBug".into(), 1, 0, 2)]
    );
    assert!(store.memory_seed_pending(10).await.unwrap().is_empty());
    assert!(
        store
            .seed_triage_memory(p.id, &limits, Duration::from_secs(30))
            .await
            .unwrap()
    );
    assert_eq!(memory(&db, &p).await[0].2, 1, "seeded once");
    let via: String =
        sqlx::query_scalar("select via from triage_memory_events where project_id = $1")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(via, "api");
}

/// Memory for fingerprint A (three confirmations as productBug on test `t`), B (a system
/// issue with a message similar to D's).
async fn teach(db: &TestDb, store: &Store, p: &Project, t: Uuid) {
    let pb = type_of(store, p, "productBug").await;
    let si = type_of(store, p, "systemIssue").await;
    for _ in 0..3 {
        let l = launch(db, store, p).await;
        catalog(db, p, FA, "expected total <n> but was <n>", &l).await;
        catalog(
            db,
            p,
            FB,
            "connection refused to db port <n> while saving the cart",
            &l,
        )
        .await;
        let a = result(db, p, &l, FA, t).await;
        let b = result(db, p, &l, FB, Uuid::now_v7()).await;
        triage(store, p, &[a], pb, "ann", "user").await.unwrap();
        triage(store, p, &[b], si, "ann", "user").await.unwrap();
    }
    fold(store).await;
}

#[tokio::test]
async fn suggestions_are_ranked_persisted_accepted_and_rejected() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "sug").await;
    let pb = type_of(&store, &p, "productBug").await;
    let ab = type_of(&store, &p, "automationBug").await;
    let t = Uuid::now_v7();
    teach(&db, &store, &p, t).await;
    let l = launch(&db, &store, &p).await;
    catalog(&db, &p, FC, "a brand new failure nobody saw", &l).await;
    catalog(
        &db,
        &p,
        FD,
        "connection refused to db port <n> while saving the order",
        &l,
    )
    .await;
    let a1 = result(&db, &p, &l, FA, t).await;
    let a2 = result(&db, &p, &l, FA, Uuid::now_v7()).await;
    let _c = result(&db, &p, &l, FC, Uuid::now_v7()).await;
    let d = result(&db, &p, &l, FD, Uuid::now_v7()).await;
    let settings = store.project_settings(p.id).await.unwrap();
    let limits = AnalysisLimits::default();
    let out = store
        .compute_suggestions(&ctx(&p, &l, &settings, &limits), &[])
        .await
        .unwrap();
    assert_eq!(out.status, AnalysisStepStatus::Done, "{out:?}");
    let v = store.launch_suggestions(p.id, &l).await.unwrap();
    assert_eq!(v.state, LaunchSuggestionsState::Ready);
    let by: std::collections::HashMap<&str, &ClusterSuggestions> = v
        .clusters
        .iter()
        .map(|c| (c.fingerprint.as_str(), c))
        .collect();
    let a = by[FA];
    assert_eq!(a.suggestions.len(), 1);
    let s = &a.suggestions[0];
    assert_eq!(
        (s.band, s.defect_type_id, s.rank),
        (SuggestionBand::Same, pb, 1)
    );
    assert!(s.why.iter().any(|w| w.signal == WhySignal::SameFingerprint));
    assert!(
        s.why
            .iter()
            .any(|w| w.signal == WhySignal::Confirmed && w.detail.contains("3 times"))
    );
    assert!(s.rule_draft_offered, "three confirmations and no rule");
    assert!(s.sample.as_ref().is_some_and(|x| x.launch_number.is_some()));
    assert_eq!(s.precision, None, "no backtest yet");
    assert_eq!(by[FC].no_suggestion, Some(NoSuggestion::NewError));
    let ds = &by[FD].suggestions;
    assert_eq!(ds.len(), 1, "similar message: {:?}", by[FD]);
    assert_eq!(ds[0].signal, SuggestionSignal::Similar);
    assert_ne!(ds[0].band, SuggestionBand::Same);

    // Recompute supersedes the open ones.
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &limits), &[])
        .await
        .unwrap();
    let states: Vec<(String, i64)> = sqlx::query_as(
        "select state, count(*) from triage_suggestions where launch_id = $1 group by state",
    )
    .bind(l.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let n = |st: &str| states.iter().find(|s| s.0 == st).map(|s| s.1).unwrap_or(0);
    assert_eq!((n("open"), n("superseded")), (2, 2), "{states:?}");
    let v = store.launch_suggestions(p.id, &l).await.unwrap();
    let a = v.clusters.iter().find(|c| c.fingerprint == FA).unwrap();
    let sid = a.suggestions[0].id;
    let did = v
        .clusters
        .iter()
        .find(|c| c.fingerprint == FD)
        .unwrap()
        .suggestions[0]
        .id;

    // Reject leaves the memory alone.
    let before = memory(&db, &p).await;
    let r = store
        .reject_suggestion(
            p.id,
            did,
            &SuggestionReject {
                reason: Some("not this".into()),
            },
            "ann",
        )
        .await
        .unwrap();
    assert_eq!(r.state, SuggestionState::Rejected);
    assert_eq!(memory(&db, &p).await, before);
    let again = store
        .reject_suggestion(p.id, did, &SuggestionReject::default(), "ann")
        .await
        .unwrap_err();
    assert_eq!(code(&again), "suggestion_closed");
    let long = store
        .reject_suggestion(
            p.id,
            sid,
            &SuggestionReject {
                reason: Some("x".repeat(501)),
            },
            "ann",
        )
        .await
        .unwrap_err();
    assert_eq!(code(&long), "bad_request");

    // A person decides one result of the cluster; accepting the cluster leaves it alone.
    triage(&store, &p, &[a2], ab, "bob", "user").await.unwrap();
    let person = AcceptActor {
        agent: false,
        can_mute: true,
    };
    let wrong = store
        .accept_suggestion(
            p.id,
            &p.key,
            sid,
            &SuggestionAccept {
                apply_to: Some(SuggestionApplyTo::Results),
                result_ids: Some(vec![d]),
                ..Default::default()
            },
            "ann",
            &person,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&wrong), "bad_request", "a result of another cluster");
    let out = store
        .accept_suggestion(
            p.id,
            &p.key,
            sid,
            &SuggestionAccept::default(),
            "ann",
            &person,
        )
        .await
        .unwrap();
    assert_eq!((out.updated, out.skipped), (1, 1), "{out:?}");
    let batch = out.batch_id.unwrap();
    let (via, source, sug): (String, String, Option<Uuid>) = sqlx::query_as(
        "select via, source, suggestion_id from triage_decisions where batch_id = $1",
    )
    .bind(batch)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (via.as_str(), source.as_str(), sug),
        ("suggestion", "user", Some(sid))
    );
    let g: (String, String) =
        sqlx::query_as("select defect->>'group', defect->>'source' from results where id = $1")
            .bind(a1)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(g, ("productBug".into(), "user".into()));
    let kept: String = sqlx::query_scalar("select defect->>'group' from results where id = $1")
        .bind(a2)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(kept, "automationBug");
    let closed = store
        .accept_suggestion(
            p.id,
            &p.key,
            sid,
            &SuggestionAccept::default(),
            "ann",
            &person,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&closed), "suggestion_closed");
    // Another project's suggestion is not found.
    let other = project(&store, "other").await;
    let nf = store
        .accept_suggestion(
            other.id,
            &other.key,
            sid,
            &SuggestionAccept::default(),
            "ann",
            &person,
        )
        .await
        .unwrap_err();
    assert_eq!(code(&nf), "not_found");
    // A rule with this fingerprint ends the offer.
    sqlx::query(
        "insert into rules (id, project_id, name, enabled, match, action) values ($1, $2, 'r', false, $3, '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(p.id)
    .bind(serde_json::json!({"fingerprint": FA}))
    .execute(&db.pool)
    .await
    .unwrap();
    let s = store.suggestion(p.id, sid).await.unwrap().unwrap();
    assert!(!s.rule_draft_offered);
    assert_eq!(s.state, SuggestionState::Accepted);
}

/// `(fingerprint, type, band, score)` of the open suggestions, ordered in Rust.
async fn open_suggestions(
    store: &Store,
    p: &Project,
    l: &Launch,
) -> Vec<(String, Uuid, SuggestionBand, i64)> {
    let mut v: Vec<(String, Uuid, SuggestionBand, i64)> = store
        .launch_suggestions(p.id, l)
        .await
        .unwrap()
        .clusters
        .into_iter()
        .flat_map(|c| {
            let fp = c.fingerprint.clone();
            c.suggestions.into_iter().map(move |s| {
                (
                    fp.clone(),
                    s.defect_type_id,
                    s.band,
                    (s.score * 1000.0) as i64,
                )
            })
        })
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn candidates_with_and_without_pg_trgm_give_the_same_answer() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let available: bool = sqlx::query_scalar(
        "select exists(select 1 from pg_available_extensions where name = 'pg_trgm')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    if !available {
        eprintln!("pg_trgm is not available: only the Rust path is checked");
    }
    let p = project(&store, "trgm").await;
    teach(&db, &store, &p, Uuid::now_v7()).await;
    let l = launch(&db, &store, &p).await;
    catalog(
        &db,
        &p,
        FD,
        "connection refused to db port <n> while saving the order",
        &l,
    )
    .await;
    result(&db, &p, &l, FA, Uuid::now_v7()).await;
    result(&db, &p, &l, FD, Uuid::now_v7()).await;
    let settings = store.project_settings(p.id).await.unwrap();
    let limits = AnalysisLimits::default();
    let c = ctx(&p, &l, &settings, &limits);
    sqlx::query("drop extension if exists pg_trgm cascade")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(!store.trigram_installed().await.unwrap());
    store.compute_suggestions(&c, &[]).await.unwrap();
    let rust = open_suggestions(&store, &p, &l).await;
    assert_eq!(rust.len(), 2, "{rust:?}");
    if available {
        sqlx::query("create extension if not exists pg_trgm")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(store.trigram_installed().await.unwrap());
        store.compute_suggestions(&c, &[]).await.unwrap();
        assert_eq!(open_suggestions(&store, &p, &l).await, rust);
    }
}

#[tokio::test]
async fn auto_apply_takes_only_same_with_the_gate_open_and_is_undoable() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "auto").await;
    let t = Uuid::now_v7();
    teach(&db, &store, &p, t).await;
    let l = launch(&db, &store, &p).await;
    catalog(
        &db,
        &p,
        FD,
        "connection refused to db port <n> while saving the order",
        &l,
    )
    .await;
    let a = result(&db, &p, &l, FA, t).await;
    let d = result(&db, &p, &l, FD, Uuid::now_v7()).await;
    let mut settings = store.project_settings(p.id).await.unwrap();
    settings.analysis.auto_apply.enabled = true;
    store.put_project_settings(p.id, &settings).await.unwrap();
    let closed = AnalysisLimits {
        gate_mode: TmGateMode::Closed,
        ..Default::default()
    };
    let open = AnalysisLimits {
        gate_mode: TmGateMode::Open,
        ..Default::default()
    };
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &open), &[])
        .await
        .unwrap();
    assert_eq!(
        store
            .auto_apply(&ctx(&p, &l, &settings, &closed))
            .await
            .unwrap(),
        None,
        "not with the gate closed"
    );
    let mut off = settings.clone();
    off.analysis.auto_apply.enabled = false;
    assert_eq!(
        store.auto_apply(&ctx(&p, &l, &off, &open)).await.unwrap(),
        None
    );
    let applied = store
        .auto_apply(&ctx(&p, &l, &settings, &open))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((applied.clusters, applied.results), (1, 1), "{applied:?}");
    let (group, source, by, muted): (String, String, String, bool) = sqlx::query_as(
        "select defect->>'group', defect->>'source', defect->>'by', muted from results where id = $1",
    )
    .bind(a)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        (group.as_str(), source.as_str(), by.as_str(), muted),
        ("productBug", "auto", "fastproof-analysis", false)
    );
    let d_defect: Option<serde_json::Value> =
        sqlx::query_scalar("select defect from results where id = $1")
            .bind(d)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        d_defect.is_none(),
        "a high or low suggestion is never applied"
    );
    let batch = applied.batch_id.unwrap();
    let (via, source): (String, String) =
        sqlx::query_as("select via, source from triage_decisions where batch_id = $1")
            .bind(batch)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((via.as_str(), source.as_str()), ("auto", "auto"));
    // Auto decisions never teach.
    let before = memory(&db, &p).await;
    fold(&store).await;
    assert_eq!(memory(&db, &p).await, before);
    // Undoable as one batch.
    let u = store
        .undo_triage(p.id, &p.key, batch, "ann", &person("ann"))
        .await
        .unwrap();
    assert_eq!(u.restored, 1);
    let restored: Option<serde_json::Value> =
        sqlx::query_scalar("select defect from results where id = $1")
            .bind(a)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(restored.is_none());
    // Analysing the launch again does not put back what a person undid: the suggestion
    // stays open for people.
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &open), &[])
        .await
        .unwrap();
    let again = store
        .auto_apply(&ctx(&p, &l, &settings, &open))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((again.clusters, again.results), (0, 0), "{again:?}");
    let still: Option<serde_json::Value> =
        sqlx::query_scalar("select defect from results where id = $1")
            .bind(a)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(
        still.is_none(),
        "an undone auto decision is not applied again"
    );
    let open_same: i64 = sqlx::query_scalar(
        "select count(*) from triage_suggestions where launch_id = $1 and fingerprint = $2 \
           and state = 'open' and band = 'same'",
    )
    .bind(l.id)
    .bind(FA)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(open_same, 1);
    // Superseded suggestions do not pile up: those older than an hour go at the next run.
    sqlx::query(
        "update triage_suggestions set decided_at = now() - interval '2 hours' \
         where launch_id = $1 and state = 'superseded'",
    )
    .bind(l.id)
    .execute(&db.pool)
    .await
    .unwrap();
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &open), &[])
        .await
        .unwrap();
    let (old, fresh): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where decided_at < now() - interval '1 hour'), \
           count(*) filter (where decided_at >= now() - interval '1 hour') \
         from triage_suggestions where launch_id = $1 and state = 'superseded'",
    )
    .bind(l.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(old, 0, "old superseded suggestions are deleted");
    assert!(fresh > 0, "the ones just superseded stay for the fold");
}

#[tokio::test]
async fn an_auto_apply_batch_stays_undoable() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "autocap").await;
    let t = Uuid::now_v7();
    teach(&db, &store, &p, t).await;
    let l = launch(&db, &store, &p).await;
    let a = result(&db, &p, &l, FA, t).await;
    let big = fastproof_store::analysis::suggest::AUTO_APPLY_MAX_RESULTS;
    sqlx::query(
        "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
           full_name, status, fingerprint, message) \
         select gen_random_uuid(), $1, $2, $3, gen_random_uuid(), gen_random_uuid(), 'b' || g, 'b' || g, \
           'failed', $4, 'boom' from generate_series(1, $5) g",
    )
    .bind(l.created_at)
    .bind(p.id)
    .bind(l.id)
    .bind(FB)
    .bind(big)
    .execute(&db.pool)
    .await
    .unwrap();
    let mut settings = store.project_settings(p.id).await.unwrap();
    settings.analysis.auto_apply.enabled = true;
    store.put_project_settings(p.id, &settings).await.unwrap();
    let open = AnalysisLimits {
        gate_mode: TmGateMode::Open,
        ..Default::default()
    };
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &open), &[])
        .await
        .unwrap();
    let same: i64 = sqlx::query_scalar(
        "select count(*) from triage_suggestions where launch_id = $1 and state = 'open' and rank = 1 \
           and band = 'same'",
    )
    .bind(l.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(same, 2, "both clusters are `same`");
    let applied = store
        .auto_apply(&ctx(&p, &l, &settings, &open))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (applied.clusters, applied.results, applied.capped),
        (1, 1, 1),
        "the big cluster would make the batch too large to undo: {applied:?}"
    );
    let auto_b: i64 = sqlx::query_scalar(
        "select count(*) from results where launch_id = $1 and fingerprint = $2 and defect is not null",
    )
    .bind(l.id)
    .bind(FB)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(auto_b, 0);
    let u = store
        .undo_triage(
            p.id,
            &p.key,
            applied.batch_id.unwrap(),
            "ann",
            &person("ann"),
        )
        .await
        .unwrap();
    assert_eq!(u.restored, 1);
    let restored: Option<serde_json::Value> =
        sqlx::query_scalar("select defect from results where id = $1")
            .bind(a)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert!(restored.is_none());
}

#[tokio::test]
async fn the_trigram_search_is_indexed_and_bounded_in_time() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let available: bool = sqlx::query_scalar(
        "select exists(select 1 from pg_available_extensions where name = 'pg_trgm')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    if !available {
        eprintln!("pg_trgm is not available: nothing to check");
        return;
    }
    assert_eq!(
        fastproof_store::search::ensure_trigram(&db.pool).await,
        fastproof_store::search::Trigram::Enabled
    );
    let indexed: bool = sqlx::query_scalar(
        "select exists(select 1 from pg_indexes where tablename = 'triage_memory' \
           and indexname = 'triage_memory_message_trgm')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(indexed, "the memory's messages have a trigram index");
    let p = project(&store, "trgmcost").await;
    let pb = type_of(&store, &p, "productBug").await;
    sqlx::query(
        "insert into triage_memory (project_id, fingerprint, defect_type_id, grp, message_norm, confirmed, \
           first_decided_at, last_decided_at, last_decided_by) \
         select $1, lpad(to_hex(g), 16, '0'), $2, 'productBug', \
           'request failed with status <n> for order ' || g || ' after retry ' || (g % 97), 1, now(), now(), 'ann' \
         from generate_series(1, 20000) g",
    )
    .bind(p.id)
    .bind(pb)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("drop index triage_memory_message_trgm")
        .execute(&db.pool)
        .await
        .unwrap();
    let messages: Vec<String> = (0..200)
        .map(|i| format!("request failed with status <n> for order {i} after retry {i}"))
        .collect();
    let cut = store
        .similar_by_trigram(p.id, &messages, Duration::from_millis(1))
        .await
        .unwrap();
    assert!(
        cut.is_none(),
        "past its time the search gives up instead of running on"
    );
    let got = store
        .similar_by_trigram(p.id, &messages[..2], Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|rows| !rows.is_empty() && rows.len() <= 20));
}

#[tokio::test]
async fn projects_with_analysis_off_do_not_hold_the_seed_back() {
    let Some((_db, store)) = TestDb::migrated().await else {
        return;
    };
    let off = project(&store, "seedoff").await;
    let mut s = store.project_settings(off.id).await.unwrap();
    s.analysis.enabled = false;
    store.put_project_settings(off.id, &s).await.unwrap();
    let on = project(&store, "seedon").await;
    assert_eq!(store.memory_seed_pending(1).await.unwrap(), vec![on.id]);
}

#[tokio::test]
async fn signals_of_the_cluster_boost_its_group() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "sig").await;
    teach(&db, &store, &p, Uuid::now_v7()).await;
    let l = launch(&db, &store, &p).await;
    catalog(
        &db,
        &p,
        FD,
        "connection refused to db port <n> while saving the order",
        &l,
    )
    .await;
    let d = result(&db, &p, &l, FD, Uuid::now_v7()).await;
    sqlx::query(
        "insert into attempt_signals (project_id, launch_id, launch_created_at, result_id, attempt, source, kind, \
           at, confidence, dedup_key, resolved) values ($1, $2, $3, $4, 1, 'grid', 'node_lost', now(), 'exact', 'k1', true)",
    )
    .bind(p.id)
    .bind(l.id)
    .bind(l.created_at)
    .bind(d)
    .execute(&db.pool)
    .await
    .unwrap();
    let settings = store.project_settings(p.id).await.unwrap();
    let limits = AnalysisLimits::default();
    store
        .compute_suggestions(&ctx(&p, &l, &settings, &limits), &[])
        .await
        .unwrap();
    let v = store.launch_suggestions(p.id, &l).await.unwrap();
    let s = &v
        .clusters
        .iter()
        .find(|c| c.fingerprint == FD)
        .unwrap()
        .suggestions[0];
    let boosted = s.why.iter().any(|w| w.signal == WhySignal::Grid);
    // The reader is K's (night B): until it returns the stored signals, nothing is added.
    let read = store.signals_for_results(p.id, &[d]).await.unwrap();
    assert_eq!(boosted, !read.is_empty(), "{s:?}");
}

#[tokio::test]
async fn the_backtest_on_stored_events_is_the_pure_harness() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "bt").await;
    let data = generate(&SynthProfile::named("s").unwrap());
    let shift = Utc::now() - chrono::Duration::days(30) - data.decisions[0].at;
    let events: Vec<DecisionEvent> = data
        .decisions
        .iter()
        .take(600)
        .map(|e| {
            let mut e = e.clone();
            e.at += shift;
            e
        })
        .collect();
    for e in &events {
        sqlx::query(
            "insert into triage_memory_events (project_id, decided_at, batch_id, fingerprint, defect_type_id, grp, \
               previous_type_id, message_norm, frames, test_case_id, env_key, branch, results, by, via) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'api')",
        )
        .bind(p.id)
        .bind(e.at)
        .bind(Uuid::now_v7())
        .bind(&e.fingerprint)
        .bind(e.defect_type_id)
        .bind(&e.group)
        .bind(e.previous_type_id)
        .bind(&e.message_norm)
        .bind(&e.frames)
        .bind(e.test_case_id)
        .bind(&e.env_key)
        .bind(&e.branch)
        .bind(e.results as i32)
        .bind(&e.by)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let settings = store.project_settings(p.id).await.unwrap();
    let limits = AnalysisLimits::default();
    let opts: BacktestOptions = options_of(&settings, &limits);
    let report = store
        .run_backtest(p.id, &BacktestRequest::default(), "ann", &limits)
        .await
        .unwrap();
    // Stored timestamps are microseconds: compare with the pure harness on what was stored.
    let mut stored = Vec::new();
    let mut src = PgSource::new(&store, p.id, Utc::now() - chrono::Duration::days(180), 7);
    loop {
        let page = src.next_page().await.unwrap();
        if page.is_empty() {
            break;
        }
        stored.extend(page);
    }
    assert_eq!(stored.len(), 600);
    let mut pure = Backtest::new(&opts);
    pure.feed(&stored);
    let pure = pure.finish();
    assert_eq!(report.decisions, pure.decisions);
    assert_eq!(report.coverage, pure.coverage);
    assert_eq!(report.no_suggestion, pure.no_suggestion);
    for (a, b) in report.bands.iter().zip(&pure.bands) {
        assert_eq!(
            (a.band, a.suggested, a.correct, a.precision),
            (b.band, b.suggested, b.correct, b.precision)
        );
    }
    assert_eq!(report.source, BacktestSource::Project);
    assert!(!report.truncated);
    let latest = store.latest_backtest(p.id).await.unwrap().unwrap();
    assert_eq!(latest.id, report.id);
    // A backtest stops at its wall budget and says so.
    let cut = store
        .run_backtest_within(
            p.id,
            &BacktestRequest::default(),
            "ann",
            &limits,
            Duration::ZERO,
        )
        .await
        .unwrap();
    assert!(cut.truncated, "{cut:?}");
    assert_eq!(cut.decisions, 0);
    // Window: only the last day.
    let short = store
        .run_backtest(p.id, &BacktestRequest { days: Some(1) }, "ann", &limits)
        .await
        .unwrap();
    assert_eq!(short.decisions, 0);
    // The newest 20 are kept.
    for _ in 0..21 {
        store
            .save_backtest(p.id, &report_copy(&report), serde_json::json!({}))
            .await
            .unwrap();
    }
    let n: i64 =
        sqlx::query_scalar("select count(*) from analysis_backtests where project_id = $1")
            .bind(p.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 20);
}

fn report_copy(r: &BacktestReport) -> BacktestReport {
    let mut c = r.clone();
    c.id = Uuid::now_v7();
    c.created_at = Utc::now();
    c
}
