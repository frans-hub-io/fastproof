//! Unique errors on a real PostgreSQL (contract `76` §7.4, §13 "E"): random launch sequences
//! analysed in launch order and in a random order give the states of the reference rules,
//! aliases merge and split, commit ranges, `isNew`/`newOnBranch`, `newErrors` with and without
//! a baseline, resolution by a closed issue, the throttle of `errorRegressed` deliveries and
//! the list of 50 000 errors. They need `FASTPROOF_TEST_DATABASE_URL`; without it they pass
//! without doing anything.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use fastproof_analysis::lifecycle::{ReplayLaunch, replay};
use fastproof_store::analysis::errors::ErrorStep;
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::types::*;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::TmError;
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

fn fp(i: usize) -> String {
    format!("{:016x}", 0xe000 + i)
}

/// A launch created `at`, running until [`finish`].
async fn launch(
    db: &TestDb,
    p: &Project,
    branch: Option<&str>,
    commit: &str,
    at: DateTime<Utc>,
    debug: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into launches (id, project_id, number, name, status, debug, created_at, started_at, branch, vcs) \
         values ($1, $2, (select coalesce(max(number), 0) + 1 from launches where project_id = $2), 'l', \
           'running', $3, $4, $4, $5, $6)",
    )
    .bind(id)
    .bind(p.id)
    .bind(debug)
    .bind(at)
    .bind(branch)
    .bind(json!({"branch": branch, "commit": commit}))
    .execute(&db.pool)
    .await
    .unwrap();
    id
}

/// Failed results of a launch with these fingerprints (and their catalog rows).
async fn fail(db: &TestDb, p: &Project, l: Uuid, fps: &[String]) {
    let at: DateTime<Utc> = sqlx::query_scalar("select created_at from launches where id = $1")
        .bind(l)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    for (i, f) in fps.iter().enumerate() {
        let tc = Uuid::from_u128(0x7e57 + i as u128);
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, status, fingerprint, message) values ($1, $2, $3, $4, $5, $5, $6, $6, 'failed', $7, 'boom')",
        )
        .bind(Uuid::now_v7())
        .bind(at)
        .bind(p.id)
        .bind(l)
        .bind(tc)
        .bind(format!("test {i}"))
        .bind(f)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, first_launch_id, \
               last_seen_at, last_launch_id, occurrences) values ($1, $2, $3, $4, $5, $4, $5, 1) \
             on conflict (project_id, fingerprint) do update set \
               first_launch_id = case when excluded.first_seen_at < failure_fingerprints.first_seen_at \
                 then excluded.first_launch_id else failure_fingerprints.first_launch_id end, \
               first_seen_at = least(failure_fingerprints.first_seen_at, excluded.first_seen_at), \
               last_launch_id = case when excluded.last_seen_at >= failure_fingerprints.last_seen_at \
                 then excluded.last_launch_id else failure_fingerprints.last_launch_id end, \
               last_seen_at = greatest(failure_fingerprints.last_seen_at, excluded.last_seen_at), \
               occurrences = failure_fingerprints.occurrences + 1",
        )
        .bind(p.id)
        .bind(f)
        .bind(format!("error {f}"))
        .bind(at)
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
    }
}

async fn finish(db: &TestDb, l: Uuid) {
    sqlx::query("update launches set status = 'finished', finished_at = now() where id = $1")
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
}

fn settings(k: u32) -> ProjectSettings {
    let mut s = ProjectSettings::default();
    s.analysis.resolve_after_launches = k;
    s
}

/// Finishes a launch and runs the `errors` step for it.
async fn step(db: &TestDb, store: &Store, p: &Project, l: Uuid, s: &ProjectSettings) -> ErrorStep {
    finish(db, l).await;
    let launch = store.launch(p.id, l).await.unwrap().unwrap();
    let limits = AnalysisLimits::default();
    let ctx = LaunchCtx {
        project_id: p.id,
        project_key: &p.key,
        launch: &launch,
        settings: s,
        limits: &limits,
        now: Utc::now(),
    };
    store.error_step(&ctx).await.unwrap()
}

async fn states(db: &TestDb, p: &Project) -> BTreeMap<String, Option<String>> {
    let rows: Vec<(String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select fingerprint, state, state_at from failure_fingerprints where project_id = $1",
    )
    .bind(p.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(f, s, at)| (f, at.map(|_| s)))
        .collect()
}

async fn state_of(db: &TestDb, p: &Project, f: &str) -> String {
    sqlx::query_scalar(
        "select state from failure_fingerprints where project_id = $1 and fingerprint = $2",
    )
    .bind(p.id)
    .bind(f)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

/// SplitMix64: deterministic random sequences.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn code(e: &TmError) -> &str {
    match e {
        TmError::NotFound(_) => "not_found",
        TmError::Invalid { code, .. }
        | TmError::Conflict { code, .. }
        | TmError::Forbidden { code, .. } => code,
        _ => "other",
    }
}

#[tokio::test]
async fn random_launch_sequences_give_the_reference_states_in_any_order() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let cases: u64 = std::env::var("FASTPROOF_ERRORS_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let mut rng = Rng(0xe770_5eed);
    for case in 0..cases {
        let len = 4 + rng.below(13) as usize;
        let nb = 1 + rng.below(3);
        let nf = 1 + rng.below(3) as usize;
        let k = 1 + rng.below(3) as u32;
        let s = settings(k);
        let plan: Vec<(String, Vec<usize>)> = (0..len)
            .map(|_| {
                let b = format!("b{}", rng.below(nb));
                let f = (0..nf).filter(|_| rng.below(10) < 4).collect();
                (b, f)
            })
            .collect();
        let mut order: Vec<usize> = (0..len).collect();
        for i in (1..len).rev() {
            order.swap(i, rng.below(i as u64 + 1) as usize);
        }
        let base = Utc::now() - Duration::hours(2);
        let mut finals: Vec<BTreeMap<String, Option<String>>> = Vec::new();
        for (run, ord) in [(0..len).collect::<Vec<_>>(), order.clone()]
            .into_iter()
            .enumerate()
        {
            let p = project(&store, &format!("q{case}r{run}")).await;
            let mut ids = Vec::new();
            for (i, (b, fs)) in plan.iter().enumerate() {
                let l = launch(
                    &db,
                    &p,
                    Some(b),
                    &format!("c{i}"),
                    base + Duration::seconds(i as i64),
                    false,
                )
                .await;
                let names: Vec<String> = fs.iter().map(|f| fp(*f)).collect();
                fail(&db, &p, l, &names).await;
                ids.push(l);
            }
            for &i in &ord {
                let out = step(&db, &store, &p, ids[i], &s).await;
                if run == 0 {
                    // Launch order: every change follows the reference rules.
                    for c in &out.changes {
                        if c.to == ErrorState::Regressed {
                            assert_eq!(
                                c.from,
                                Some(ErrorState::Resolved),
                                "case {case} launch {i}: {c:?}"
                            );
                        }
                    }
                    let now = states(&db, &p).await;
                    for f in 0..nf {
                        let seq: Vec<ReplayLaunch> = plan[..=i]
                            .iter()
                            .map(|(b, fs)| ReplayLaunch {
                                branch: b.clone(),
                                seen: fs.contains(&f),
                            })
                            .collect();
                        let want = replay(&seq, k).last().copied().flatten();
                        let got = now.get(&fp(f)).cloned().flatten();
                        assert_eq!(
                            got.as_deref(),
                            want.map(|s| s.as_str()),
                            "case {case} after launch {i} (k={k}) fp {f}: {plan:?}"
                        );
                    }
                }
            }
            finals.push(states(&db, &p).await);
        }
        assert_eq!(
            finals[0], finals[1],
            "case {case} (k={k}): order {order:?} changed the states of {plan:?}"
        );
    }
}

#[tokio::test]
async fn aliases_merge_split_and_refuse_cycles() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "al").await;
    let other = project(&store, "ot").await;
    let s = settings(10);
    let base = Utc::now() - Duration::hours(1);
    let (a, b, c) = (fp(1), fp(2), fp(3));
    let l1 = launch(&db, &p, Some("main"), "c1", base, false).await;
    fail(&db, &p, l1, &[a.clone(), b.clone(), b.clone()]).await;
    step(&db, &store, &p, l1, &s).await;
    let l2 = launch(
        &db,
        &p,
        Some("main"),
        "c2",
        base + Duration::seconds(1),
        false,
    )
    .await;
    fail(&db, &p, l2, std::slice::from_ref(&c)).await;
    step(&db, &store, &p, l2, &s).await;
    let lo = launch(&db, &other, Some("main"), "x", base, false).await;
    fail(&db, &other, lo, &[fp(9)]).await;

    let d = store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(a.clone())),
            },
            "u",
        )
        .await
        .unwrap();
    assert_eq!(d.summary.alias_of.as_deref(), Some(a.as_str()));
    let list = store
        .errors(
            p.id,
            &ErrorQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let fps: BTreeSet<String> = list.items.iter().map(|e| e.fingerprint.clone()).collect();
    assert_eq!(fps, [a.clone(), c.clone()].into());
    let ea = list.items.iter().find(|e| e.fingerprint == a).unwrap();
    assert_eq!((ea.aliases, ea.occurrences), (1, 3));
    // An alias of an alias points at the canonical error; a cycle is refused.
    let d = store
        .patch_error(
            p.id,
            &c,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(b.clone())),
            },
            "u",
        )
        .await
        .unwrap();
    assert_eq!(d.summary.alias_of.as_deref(), Some(a.as_str()));
    let e = store
        .patch_error(
            p.id,
            &a,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(c.clone())),
            },
            "u",
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "alias_cycle");
    let e = store
        .patch_error(
            p.id,
            &a,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(fp(9))),
            },
            "u",
        )
        .await
        .unwrap_err();
    assert_eq!(
        code(&e),
        "alias_cycle",
        "another project's error is not a target"
    );
    let e = store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: Some(ErrorPatchState::Ignored),
                alias_of: None,
            },
            "u",
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let e = store.error_detail(other.id, &a).await.unwrap_err();
    assert_eq!(code(&e), "not_found");
    let map = store
        .canonical_fingerprints(p.id, &[a.clone(), b.clone(), c.clone(), fp(7)])
        .await
        .unwrap();
    assert_eq!(map[&b], a);
    assert_eq!(map[&c], a);
    assert_eq!(map[&fp(7)], fp(7));
    // The launch's clusters are grouped by the canonical error.
    let launch1 = store.launch(p.id, l1).await.unwrap().unwrap();
    let clusters = store.clusters(&launch1, None).await.unwrap();
    assert_eq!(clusters.len(), 1);
    assert_eq!(
        (clusters[0].fingerprint.as_str(), clusters[0].count),
        (a.as_str(), 3)
    );
    let detail = store.error_detail(p.id, &a).await.unwrap();
    assert_eq!(detail.alias_list.len(), 2);
    assert_eq!(
        detail.summary.state,
        ErrorState::Active,
        "seen in two launches once merged"
    );
    // Split: the alias is its own error again, with its own state.
    let d = store
        .patch_error(
            p.id,
            &c,
            &ErrorPatch {
                state: None,
                alias_of: Some(None),
            },
            "u",
        )
        .await
        .unwrap();
    assert_eq!(d.summary.alias_of, None);
    assert_eq!(d.summary.state, ErrorState::New);
    assert_eq!(state_of(&db, &p, &a).await, "new");
}

#[tokio::test]
async fn commits_branches_and_new_flags() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "cm").await;
    let s = settings(2);
    let base = Utc::now() - Duration::hours(1);
    let at = |i: i64| base + Duration::seconds(i);
    let a = fp(1);
    let l1 = launch(&db, &p, Some("main"), "aaa111", at(0), false).await;
    step(&db, &store, &p, l1, &s).await;
    let l2 = launch(&db, &p, Some("main"), "bbb222", at(1), false).await;
    fail(&db, &p, l2, std::slice::from_ref(&a)).await;
    let out = step(&db, &store, &p, l2, &s).await;
    assert_eq!(out.changes.len(), 1);
    assert_eq!(
        (out.changes[0].from, out.changes[0].to),
        (None, ErrorState::New)
    );
    let d = store.error_detail(p.id, &a).await.unwrap();
    let cr = d.commits.clone().unwrap();
    assert_eq!(
        (cr.last_good.as_deref(), cr.first.as_deref()),
        (Some("aaa111"), Some("bbb222"))
    );
    let cl = store
        .clusters(&store.launch(p.id, l2).await.unwrap().unwrap(), None)
        .await
        .unwrap();
    assert!(cl[0].is_new && cl[0].new_on_branch);
    assert_eq!(cl[0].error_state, Some(ErrorState::New));
    // On another branch: not new, but new on that branch.
    let l3 = launch(&db, &p, Some("feature"), "ccc333", at(2), false).await;
    fail(&db, &p, l3, std::slice::from_ref(&a)).await;
    step(&db, &store, &p, l3, &s).await;
    let cl = store
        .clusters(&store.launch(p.id, l3).await.unwrap().unwrap(), None)
        .await
        .unwrap();
    assert!(!cl[0].is_new && cl[0].new_on_branch);
    assert_eq!(cl[0].error_state, Some(ErrorState::Active));
    // Two launches of each branch without it resolve it; the next occurrence regresses it.
    for (i, b) in [(3, "main"), (4, "main"), (5, "feature")] {
        let l = launch(&db, &p, Some(b), &format!("ddd{i}"), at(i), false).await;
        step(&db, &store, &p, l, &s).await;
        assert_eq!(
            state_of(&db, &p, &a).await,
            "active",
            "feature has one launch only"
        );
    }
    let l = launch(&db, &p, Some("feature"), "eee666", at(6), false).await;
    let out = step(&db, &store, &p, l, &s).await;
    assert_eq!(out.changes[0].to, ErrorState::Resolved);
    let d = store.error_detail(p.id, &a).await.unwrap();
    assert!(d.mttr_seconds.is_some_and(|m| m >= 0));
    assert_eq!(d.branch_list.len(), 2);
    let main = d.branch_list.iter().find(|b| b.branch == "main").unwrap();
    assert_eq!((main.occurrences, main.launches_since), (1, 2));
    let l = launch(&db, &p, Some("main"), "fff777", at(7), false).await;
    fail(&db, &p, l, std::slice::from_ref(&a)).await;
    let out = step(&db, &store, &p, l, &s).await;
    assert_eq!(
        (out.changes[0].from, out.changes[0].to),
        (Some(ErrorState::Resolved), ErrorState::Regressed)
    );
    let d = store.error_detail(p.id, &a).await.unwrap();
    assert_eq!(d.summary.regressed_count, 1);
    let cr = d.commits.unwrap();
    assert_eq!(
        (cr.last_good.as_deref(), cr.first.as_deref()),
        (Some("ddd4"), Some("fff777"))
    );
    // Re-running the step for the same launch changes nothing and counts it once.
    let again = step(&db, &store, &p, l, &s).await;
    assert!(again.changes.is_empty());
    let n: i64 = sqlx::query_scalar(
        "select occurrences from fingerprint_branches where project_id = $1 and branch = 'main'",
    )
    .bind(p.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 2);
    // Debug launches are not analysed.
    let dl = launch(&db, &p, Some("main"), "dbg", at(8), true).await;
    fail(&db, &p, dl, &[fp(5)]).await;
    let out = step(&db, &store, &p, dl, &s).await;
    assert_eq!(out.skipped, Some("debug launch"));
}

#[tokio::test]
async fn new_errors_follow_the_baseline_and_skip_ignored() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ne").await;
    let base = Utc::now() - Duration::hours(1);
    let (a, b) = (fp(1), fp(2));
    let l1 = launch(&db, &p, Some("main"), "c1", base, false).await;
    fail(&db, &p, l1, std::slice::from_ref(&a)).await;
    finish(&db, l1).await;
    let l2 = launch(
        &db,
        &p,
        Some("feature"),
        "c2",
        base + Duration::seconds(1),
        false,
    )
    .await;
    fail(&db, &p, l2, &[a.clone(), b.clone()]).await;
    finish(&db, l2).await;
    let launch2 = store.launch(p.id, l2).await.unwrap().unwrap();
    // Without the errors step (Analysis off) the count is read from the results.
    assert_eq!(
        store.new_error_count(p.id, &launch2, None).await.unwrap(),
        2
    );
    assert_eq!(
        store
            .new_error_count(p.id, &launch2, Some("main"))
            .await
            .unwrap(),
        1
    );
    let launch1 = store.launch(p.id, l1).await.unwrap().unwrap();
    assert_eq!(
        store.new_error_count(p.id, &launch1, None).await.unwrap(),
        1
    );
    store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: Some(ErrorPatchState::Ignored),
                alias_of: None,
            },
            "u",
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .new_error_count(p.id, &launch2, Some("main"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store.new_error_count(p.id, &launch2, None).await.unwrap(),
        1
    );
    // An alias of an error seen on the baseline is not new.
    store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: Some(ErrorPatchState::Active),
                alias_of: None,
            },
            "u",
        )
        .await
        .unwrap();
    store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(a.clone())),
            },
            "u",
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .new_error_count(p.id, &launch2, Some("main"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store.new_error_count(p.id, &launch2, None).await.unwrap(),
        1
    );
    // Ignored errors never change by themselves.
    let s = settings(1);
    store
        .patch_error(
            p.id,
            &a,
            &ErrorPatch {
                state: Some(ErrorPatchState::Ignored),
                alias_of: None,
            },
            "u",
        )
        .await
        .unwrap();
    let l3 = launch(
        &db,
        &p,
        Some("main"),
        "c3",
        base + Duration::seconds(2),
        false,
    )
    .await;
    fail(&db, &p, l3, std::slice::from_ref(&a)).await;
    let out = step(&db, &store, &p, l3, &s).await;
    assert!(out.changes.is_empty());
    assert_eq!(state_of(&db, &p, &a).await, "ignored");
}

#[tokio::test]
async fn a_closed_issue_resolves_and_deliveries_are_throttled() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "is").await;
    let s = settings(10);
    let base = Utc::now() - Duration::hours(1);
    let a = fp(1);
    let l1 = launch(&db, &p, Some("main"), "c1", base, false).await;
    fail(&db, &p, l1, std::slice::from_ref(&a)).await;
    step(&db, &store, &p, l1, &s).await;
    let l1b = launch(
        &db,
        &p,
        Some("main"),
        "c1b",
        base + Duration::seconds(1),
        false,
    )
    .await;
    fail(&db, &p, l1b, std::slice::from_ref(&a)).await;
    step(&db, &store, &p, l1b, &s).await;
    let issue = Uuid::now_v7();
    sqlx::query(
        "insert into tracker_issues (id, project_id, tracker_id, tracker_type, issue_key, url, category, created_by, closed_at) \
         values ($1, $2, 'jira', 'jira', 'PAY-1', 'https://tracker.example.com/PAY-1', 'closed', 'u', $3)",
    )
    .bind(issue)
    .bind(p.id)
    .bind(base + Duration::seconds(2))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into tracker_issue_links (id, issue_id, project_id, fingerprint, created_by) values ($1, $2, $3, $4, 'u')",
    )
    .bind(Uuid::now_v7())
    .bind(issue)
    .bind(p.id)
    .bind(&a)
    .execute(&db.pool)
    .await
    .unwrap();
    let l2 = launch(
        &db,
        &p,
        Some("main"),
        "c2",
        base + Duration::seconds(3),
        false,
    )
    .await;
    step(&db, &store, &p, l2, &s).await;
    assert_eq!(
        state_of(&db, &p, &a).await,
        "active",
        "only the issue job checks closed issues"
    );
    let targets = store.error_recheck_targets(&[p.id]).await.unwrap();
    assert_eq!(targets, vec![(p.id, vec![a.clone()])]);
    let changes = store
        .recompute_error_states(p.id, &targets[0].1, 10)
        .await
        .unwrap();
    assert_eq!(changes[0].to, ErrorState::Resolved);
    let d = store.error_detail(p.id, &a).await.unwrap();
    assert_eq!(
        d.summary.issue.as_ref().map(|i| i.state),
        Some(ErrorIssueState::Closed)
    );
    let l3 = launch(
        &db,
        &p,
        Some("main"),
        "c3",
        base + Duration::seconds(4),
        false,
    )
    .await;
    fail(&db, &p, l3, std::slice::from_ref(&a)).await;
    let out = step(&db, &store, &p, l3, &s).await;
    assert_eq!(out.changes[0].to, ErrorState::Regressed);

    let throttle = std::time::Duration::from_secs(600);
    let payload = json!({"errors": {"items": []}});
    let d = |channel: &'static str, target: &'static str| {
        fastproof_store::analysis::errors::ErrorDelivery {
            project_id: p.id,
            launch_id: l3,
            channel,
            target_id: target,
            payload: &payload,
            max_attempts: 3,
        }
    };
    assert!(
        store
            .queue_error_delivery(&d("webhook", "w1"), throttle)
            .await
            .unwrap()
    );
    assert!(
        !store
            .queue_error_delivery(&d("webhook", "w1"), throttle)
            .await
            .unwrap()
    );
    assert!(
        store
            .queue_error_delivery(&d("slack", "s1"), throttle)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn fifty_thousand_errors_list_quickly() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "big").await;
    sqlx::query(
        "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, last_seen_at, occurrences, state, state_at) \
         select $1, lpad(to_hex(i), 16, '0'), 'Connection refused: host ' || i || ' port ' || (i % 97), \
           now() - make_interval(secs => i * 60), now() - make_interval(secs => i), i % 1000, \
           (array['new', 'active', 'resolved', 'regressed', 'ignored'])[1 + i % 5], now() \
         from generate_series(1, 50000) as g(i)",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into fingerprint_branches (project_id, fingerprint, branch, first_seen_at, last_seen_at, occurrences) \
         select $1, lpad(to_hex(i), 16, '0'), case when i % 3 = 0 then 'main' else 'dev' end, now() - interval '1 day', now(), i % 7 \
         from generate_series(1, 50000) as g(i)",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("analyze failure_fingerprints")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("analyze fingerprint_branches")
        .execute(&db.pool)
        .await
        .unwrap();
    let queries = [
        ErrorQuery {
            limit: 50,
            ..Default::default()
        },
        ErrorQuery {
            limit: 50,
            state: Some(ErrorState::Regressed),
            ..Default::default()
        },
        ErrorQuery {
            limit: 50,
            branch: Some("main".into()),
            ..Default::default()
        },
        ErrorQuery {
            limit: 50,
            q: Some("host 4242".into()),
            ..Default::default()
        },
        ErrorQuery {
            limit: 50,
            sort: ErrorSort::Occurrences,
            ..Default::default()
        },
        ErrorQuery {
            limit: 50,
            sort: ErrorSort::FirstSeen,
            ..Default::default()
        },
    ];
    for q in &queries {
        store.errors(p.id, q).await.unwrap();
        let started = std::time::Instant::now();
        let page = store.errors(p.id, q).await.unwrap();
        let took = started.elapsed();
        eprintln!("errors list {q:?}: {} items in {took:?}", page.items.len());
        assert!(
            took < std::time::Duration::from_millis(200),
            "{q:?} took {took:?}"
        );
    }
    // Paging walks every row once.
    let mut seen = BTreeSet::new();
    let mut q = ErrorQuery {
        limit: 500,
        state: Some(ErrorState::New),
        ..Default::default()
    };
    loop {
        let page = store.errors(p.id, &q).await.unwrap();
        for e in &page.items {
            assert!(
                seen.insert(e.fingerprint.clone()),
                "{} twice",
                e.fingerprint
            );
        }
        match page.next {
            Some(n) => q.before = Some(n),
            None => break,
        }
    }
    assert_eq!(seen.len(), 10_000);
    let e = store
        .errors(
            p.id,
            &ErrorQuery {
                limit: 50,
                q: Some("ab".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
    let e = store
        .errors(
            p.id,
            &ErrorQuery {
                limit: 50,
                before: Some("nope".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "bad_request");
}

/// Contract §14: error states after a launch of 5000 results in 300 clusters in under a
/// second, with 20 earlier launches and errors coming and going. Run it on a release build:
/// `cargo test --release -p fastproof-store --test analysis_errors -- --ignored`.
#[tokio::test]
#[ignore]
async fn error_states_of_a_large_launch_take_under_a_second() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    fastproof_store::analysis::indexes::ensure_analysis_indexes(&db.pool)
        .await
        .unwrap();
    let p = project(&store, "perf").await;
    let s = settings(10);
    let base = Utc::now() - Duration::hours(1);
    let mut took = Vec::new();
    for i in 0..21i64 {
        let l = launch(
            &db,
            &p,
            Some(if i % 4 == 3 { "dev" } else { "main" }),
            &format!("c{i}"),
            base + Duration::seconds(i),
            false,
        )
        .await;
        let at: DateTime<Utc> = sqlx::query_scalar("select created_at from launches where id = $1")
            .bind(l)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, full_name, \
               status, fingerprint, message) \
             select gen_random_uuid(), $1, $2, $3, md5('t' || g)::uuid, md5('t' || g)::uuid, 't' || g, 't' || g, 'failed', \
               lpad(to_hex(4096 + (g + $4 * 7) % 300 + case when g % 17 = 0 then $4 * 1000 else 0 end), 16, '0'), 'boom' \
             from generate_series(1, 5000) as g",
        )
        .bind(at)
        .bind(p.id)
        .bind(l)
        .bind(i)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, first_launch_id, last_seen_at, last_launch_id, occurrences) \
             select $1, fingerprint, 'boom ' || fingerprint, $2, $3, $2, $3, count(*) from results \
             where launch_id = $3 and launch_created_at = $2 group by fingerprint \
             on conflict (project_id, fingerprint) do update set last_seen_at = excluded.last_seen_at, \
               last_launch_id = excluded.last_launch_id, occurrences = failure_fingerprints.occurrences + excluded.occurrences",
        )
        .bind(p.id)
        .bind(at)
        .bind(l)
        .execute(&db.pool)
        .await
        .unwrap();
        let started = std::time::Instant::now();
        let out = step(&db, &store, &p, l, &s).await;
        took.push(started.elapsed());
        assert!(out.errors >= 300, "{}", out.errors);
    }
    eprintln!("errors step per launch: {took:?}");
    let last = *took.last().unwrap();
    assert!(
        last < std::time::Duration::from_secs(1),
        "the last launch took {last:?}"
    );
}

/// The step reads the catalog rows without row locks and writes the states at the end in the
/// writers' order: an ingest that updates the same rows while the step computes does not wait
/// for the whole step (a background job holding rows that ingest writes made ingest wait).
#[tokio::test]
async fn the_errors_step_leaves_catalog_rows_to_ingest_while_it_computes() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "lk").await;
    let s = settings(10);
    let base = Utc::now() - Duration::hours(1);
    let (a, b) = (fp(1), fp(2));
    let l1 = launch(&db, &p, Some("main"), "c1", base, false).await;
    fail(&db, &p, l1, &[a.clone(), b.clone()]).await;
    step(&db, &store, &p, l1, &s).await;
    let l2 = launch(
        &db,
        &p,
        Some("main"),
        "c2",
        base + Duration::seconds(1),
        false,
    )
    .await;
    fail(&db, &p, l2, &[a.clone(), b.clone()]).await;
    finish(&db, l2).await;
    let launch2 = store.launch(p.id, l2).await.unwrap().unwrap();
    let limits = AnalysisLimits::default();
    let ctx = LaunchCtx {
        project_id: p.id,
        project_key: &p.key,
        launch: &launch2,
        settings: &s,
        limits: &limits,
        now: Utc::now(),
    };
    // Parks the step inside its computation: it reads the issue links after the catalog rows.
    let mut parker = db.pool.begin().await.unwrap();
    sqlx::query("lock table tracker_issue_links in access exclusive mode")
        .execute(&mut *parker)
        .await
        .unwrap();
    let (pool, pid, fps) = (db.pool.clone(), p.id, vec![a.clone(), b.clone()]);
    let probe = async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity where datname = current_database() \
                   and pid <> pg_backend_pid() and wait_event_type = 'Lock' \
                   and query like '%join tracker_issue_links k on%'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the step never reached the issue links"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let mut ingest = pool.begin().await.unwrap();
        sqlx::query("set local lock_timeout = '2s'")
            .execute(&mut *ingest)
            .await
            .unwrap();
        let r = sqlx::query(
            "update failure_fingerprints set occurrences = occurrences + 1 \
             where project_id = $1 and fingerprint = any($2)",
        )
        .bind(pid)
        .bind(&fps)
        .execute(&mut *ingest)
        .await;
        match &r {
            Ok(_) => ingest.commit().await.unwrap(),
            Err(_) => ingest.rollback().await.unwrap(),
        }
        parker.commit().await.unwrap();
        r.map(|done| done.rows_affected())
    };
    let (out, ingested) = tokio::join!(store.error_step(&ctx), probe);
    assert_eq!(
        ingested.expect("ingest updated the catalog rows while the step computed"),
        2
    );
    let out = out.unwrap();
    assert_eq!(out.evaluated, 2);
    assert_eq!(state_of(&db, &p, &a).await, "active");
    assert_eq!(state_of(&db, &p, &b).await, "active");
    let occ: i64 = sqlx::query_scalar(
        "select occurrences from failure_fingerprints where project_id = $1 and fingerprint = $2",
    )
    .bind(p.id)
    .bind(&a)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(occ, 3, "the step keeps what ingest wrote");
}

/// A cluster carries its canonical fingerprint: triaging it and listing its results reach the
/// results of its aliases too, and never another project's.
#[tokio::test]
async fn triage_and_the_results_filter_follow_aliases() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ta").await;
    let other = project(&store, "tb").await;
    let s = settings(10);
    let base = Utc::now() - Duration::hours(1);
    let (a, b, c) = (fp(1), fp(2), fp(3));
    let l1 = launch(&db, &p, Some("main"), "c1", base, false).await;
    fail(&db, &p, l1, &[a.clone(), b.clone(), c.clone()]).await;
    step(&db, &store, &p, l1, &s).await;
    let lo = launch(&db, &other, Some("main"), "x", base, false).await;
    fail(&db, &other, lo, &[a.clone(), b.clone()]).await;
    // The result list joins the series (both projects' fixtures share the test ids).
    sqlx::query(
        "insert into test_cases (id, project_id, test_case_hash, full_name, display_name) \
         select distinct r.test_case_id, $1, r.test_case_id::text, r.full_name, r.name from results r \
         where r.project_id = $1 on conflict do nothing",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into series (id, project_id, test_case_id, series_key, parameters_hash) \
         select distinct r.series_id, $1, r.test_case_id, r.series_id::text, '' from results r \
         where r.project_id = $1 on conflict do nothing",
    )
    .bind(p.id)
    .execute(&db.pool)
    .await
    .unwrap();
    store
        .patch_error(
            p.id,
            &b,
            &ErrorPatch {
                state: None,
                alias_of: Some(Some(a.clone())),
            },
            "u",
        )
        .await
        .unwrap();
    let launch1 = store.launch(p.id, l1).await.unwrap().unwrap();
    let listed = store
        .results(
            &launch1,
            &fastproof_store::results::ResultFilter {
                fingerprint: Some(a.clone()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut got: Vec<String> = listed
        .items
        .iter()
        .filter_map(|r| r.fingerprint.clone())
        .collect();
    got.sort();
    assert_eq!(got, vec![a.clone(), b.clone()]);
    let alias_only = store
        .results(
            &launch1,
            &fastproof_store::results::ResultFilter {
                fingerprint: Some(b.clone()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(alias_only.items.len(), 1);
    let req = fastproof_store::analysis::triage::TriageRequest {
        launch_id: Some(l1),
        fingerprint: Some(a.clone()),
        comment: Some("one error".into()),
        ..Default::default()
    };
    let done = store
        .triage(p.id, &p.key, &req, "u")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.updated, 2, "the canonical error and its alias");
    let launch_o = store.launch(other.id, lo).await.unwrap().unwrap();
    let theirs = store
        .results(
            &launch_o,
            &fastproof_store::results::ResultFilter {
                fingerprint: Some(a.clone()),
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        theirs.items.len(),
        1,
        "the aliases of another project are not followed"
    );
}
