//! Flaky causes on a real PostgreSQL (contract `76` §7.8, §13 "H"): the series window (raw
//! retention, debug launches, labels, attempt fingerprints, signals), every kind computed from
//! stored runs and their context (fingerprint catalog, bursts, tests failing together, tests
//! earlier on the same worker, the same test in other environments), persistence and the
//! readers of the API and the flaky list, the launch and daily targets, the PostgreSQL and
//! ClickHouse windows on the same runs, and the time for a series of 50 runs. They need
//! `FASTPROOF_TEST_DATABASE_URL`; without it they pass without doing anything.

use std::time::Instant;

use chrono::{DateTime, Duration, Timelike, Utc};
use fastproof_analysis::model::{FlakyHintKind, HintConfidence};
use fastproof_store::analysis::flaky::FlakyFilter;
use fastproof_store::analysis::settings::ProjectSettings;
use fastproof_store::analysis::types::*;
use fastproof_store::projects::CreateProject;
use fastproof_store::tm::TmError;
use fastproof_store::{Project, Store};
use serde_json::{Value, json};
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

/// A launch of the project.
#[derive(Clone, Copy)]
struct L {
    id: Uuid,
    at: DateTime<Utc>,
}

/// One result to store.
#[derive(Clone, Default)]
struct R<'a> {
    status: &'a str,
    flaky_in_run: bool,
    duration_ms: Option<i64>,
    host: Option<&'a str>,
    thread: Option<&'a str>,
    started: Option<DateTime<Utc>>,
    message: Option<&'a str>,
    /// Fingerprints of failed attempts (with catalog messages).
    attempts: Vec<(&'a str, &'a str)>,
    transition: Option<&'a str>,
}

impl<'a> R<'a> {
    fn pass() -> Self {
        Self {
            status: "passed",
            ..Self::default()
        }
    }
    fn fail(fp: &'a str, msg: &'a str) -> Self {
        Self {
            status: "failed",
            message: Some(msg),
            attempts: vec![(fp, msg)],
            ..Self::default()
        }
    }
}

struct Fx<'a> {
    db: &'a TestDb,
    p: Project,
}

impl Fx<'_> {
    async fn series(&self, name: &str, env: Option<&str>) -> Uuid {
        self.series_params(name, env, "p0").await
    }

    async fn series_params(&self, name: &str, env: Option<&str>, params: &str) -> Uuid {
        let tc: Uuid = match sqlx::query_scalar::<_, Uuid>(
            "select id from test_cases where project_id = $1 and test_case_hash = $2",
        )
        .bind(self.p.id)
        .bind(name)
        .fetch_optional(&self.db.pool)
        .await
        .unwrap()
        {
            Some(id) => id,
            None => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "insert into test_cases (id, project_id, test_case_hash, full_name, display_name) \
                     values ($1, $2, $3, $3, $3)",
                )
                .bind(id)
                .bind(self.p.id)
                .bind(name)
                .execute(&self.db.pool)
                .await
                .unwrap();
                id
            }
        };
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into series (id, project_id, test_case_id, series_key, parameters_hash, env_key) \
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(self.p.id)
        .bind(tc)
        .bind(format!("{name}.{params}.{}", env.unwrap_or("-")))
        .bind(params)
        .bind(env)
        .execute(&self.db.pool)
        .await
        .unwrap();
        id
    }

    async fn stats(&self, series: Uuid, flaky: bool, score: f32) {
        sqlx::query(
            "insert into series_stats (series_id, project_id, flaky, flaky_score) values ($1, $2, $3, $4) \
             on conflict (series_id) do update set flaky = excluded.flaky, flaky_score = excluded.flaky_score",
        )
        .bind(series)
        .bind(self.p.id)
        .bind(flaky)
        .bind(score)
        .execute(&self.db.pool)
        .await
        .unwrap();
    }

    async fn launch_full(
        &self,
        at: DateTime<Utc>,
        debug: bool,
        env: Option<&str>,
        runner: Option<&str>,
    ) -> L {
        let id = Uuid::now_v7();
        let attrs = match runner {
            Some(r) => json!([{"key": "runner", "value": r}]),
            None => json!([]),
        };
        sqlx::query(
            "insert into launches (id, project_id, number, name, status, debug, created_at, started_at, finished_at, \
               branch, env_key, attributes) \
             values ($1, $2, (select coalesce(max(number), 0) + 1 from launches where project_id = $2), 'l', \
               'finished', $3, $4, $4, $4, 'main', $5, $6)",
        )
        .bind(id)
        .bind(self.p.id)
        .bind(debug)
        .bind(at)
        .bind(env)
        .bind(attrs)
        .execute(&self.db.pool)
        .await
        .unwrap();
        L { id, at }
    }

    async fn launch(&self, at: DateTime<Utc>) -> L {
        self.launch_full(at, false, None, None).await
    }

    async fn result(&self, l: L, series: Uuid, r: R<'_>) -> Uuid {
        let id = Uuid::now_v7();
        let tc: Uuid = sqlx::query_scalar("select test_case_id from series where id = $1")
            .bind(series)
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        let mut labels = Vec::new();
        if let Some(h) = r.host {
            labels.push(json!({"name": "host", "value": h}));
        }
        if let Some(t) = r.thread {
            labels.push(json!({"name": "thread", "value": t}));
        }
        let started = r.started.unwrap_or(l.at);
        let stopped = started + Duration::milliseconds(r.duration_ms.unwrap_or(1000));
        let fingerprint = r.attempts.last().map(|a| a.0);
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, status, flaky_in_run, attempt_count, duration_ms, labels, fingerprint, message, \
               started_at, stopped_at, transition) \
             values ($1, $2, $3, $4, $5, $6, 'n', 'n', $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(id)
        .bind(l.at)
        .bind(self.p.id)
        .bind(l.id)
        .bind(tc)
        .bind(series)
        .bind(r.status)
        .bind(r.flaky_in_run)
        .bind(r.attempts.len().max(1) as i32)
        .bind(r.duration_ms)
        .bind(Value::Array(labels))
        .bind(if r.status == "passed" { None } else { fingerprint })
        .bind(r.message)
        .bind(started)
        .bind(stopped)
        .bind(r.transition)
        .execute(&self.db.pool)
        .await
        .unwrap();
        for (i, (fp, msg)) in r.attempts.iter().enumerate() {
            sqlx::query(
                "insert into attempts (id, launch_created_at, project_id, launch_id, result_id, series_id, client_key, \
                   seq, status, message, fingerprint) values ($1, $2, $3, $4, $5, $6, $7, $8, 'failed', $9, $10)",
            )
            .bind(Uuid::now_v7())
            .bind(l.at)
            .bind(self.p.id)
            .bind(l.id)
            .bind(id)
            .bind(series)
            .bind(format!("{id}-{i}"))
            .bind(i as i64)
            .bind(*msg)
            .bind(*fp)
            .execute(&self.db.pool)
            .await
            .unwrap();
            sqlx::query(
                "insert into failure_fingerprints (project_id, fingerprint, message, first_seen_at, last_seen_at) \
                 values ($1, $2, $3, $4, $4) on conflict do nothing",
            )
            .bind(self.p.id)
            .bind(*fp)
            .bind(*msg)
            .bind(l.at)
            .execute(&self.db.pool)
            .await
            .unwrap();
        }
        id
    }

    async fn signal(&self, l: L, result: Uuid, source: &str, kind: &str) {
        sqlx::query(
            "insert into attempt_signals (project_id, launch_id, launch_created_at, result_id, attempt, source, kind, \
               at, confidence, dedup_key, resolved) values ($1, $2, $3, $4, 0, $5, $6, $3, 'exact', $7, true)",
        )
        .bind(self.p.id)
        .bind(l.id)
        .bind(l.at)
        .bind(result)
        .bind(source)
        .bind(kind)
        .bind(Uuid::now_v7().to_string())
        .execute(&self.db.pool)
        .await
        .unwrap();
    }
}

fn kinds(h: &FlakyHints) -> Vec<FlakyHintKind> {
    h.hints.iter().map(|x| x.kind).collect()
}

fn hint(h: &FlakyHints, k: FlakyHintKind) -> &FlakyHint {
    h.hints
        .iter()
        .find(|x| x.kind == k)
        .unwrap_or_else(|| panic!("no {k:?} hint in {:#?}", h.hints))
}

async fn compute(store: &Store, p: &Project, series: Uuid) -> FlakyHints {
    store
        .compute_flaky_hints(p.id, series, &ProjectSettings::default(), None, Utc::now())
        .await
        .unwrap()
        .unwrap()
}

fn ago(days: i64, i: i64) -> DateTime<Utc> {
    (Utc::now() - Duration::days(days) + Duration::minutes(i * 10))
        .with_nanosecond(0)
        .unwrap()
}

#[tokio::test]
async fn the_window_reads_recent_runs_with_their_facts() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fw").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let s = fx.series("checkout", None).await;
    for i in 0..2 {
        let l = fx.launch(ago(120, i)).await;
        fx.result(l, s, R::pass()).await;
    }
    let dbg = fx.launch_full(ago(1, 0), true, None, None).await;
    fx.result(dbg, s, R::pass()).await;
    let mut last = None;
    for i in 0..60 {
        let l = fx
            .launch_full(ago(30, i), false, Some("staging"), Some("runner-7"))
            .await;
        let r = if i == 59 {
            R {
                host: Some("ci-3"),
                thread: Some("w2"),
                duration_ms: Some(4200),
                ..R::fail("fp-a", "boom")
            }
        } else {
            R::pass()
        };
        let id = fx.result(l, s, r).await;
        last = Some((l, id));
    }
    let (l, id) = last.unwrap();
    fx.signal(l, id, "grid", "session_crashed").await;
    let w = store.series_window(p.id, s, 100).await.unwrap();
    assert_eq!(w.len(), 50);
    assert!(w.windows(2).all(|x| x[0].created_at <= x[1].created_at));
    let n = w.last().unwrap();
    assert_eq!(n.launch_id, l.id);
    assert_eq!(n.status, "failed");
    assert_eq!(
        (
            n.host.as_deref(),
            n.thread.as_deref(),
            n.ci_runner.as_deref(),
            n.env_key.as_deref()
        ),
        (Some("ci-3"), Some("w2"), Some("runner-7"), Some("staging"))
    );
    assert_eq!(n.attempt_fingerprints, vec!["fp-a".to_string()]);
    assert_eq!(n.signals, vec!["grid:session_crashed".to_string()]);
    assert_eq!(n.duration_ms, Some(4200));
    assert_eq!(store.series_window(p.id, s, 10).await.unwrap().len(), 10);
    // Another project's series reads as nothing.
    let other = project(&store, "fw2").await;
    assert!(
        store
            .series_window(other.id, s, 50)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn every_kind_from_stored_runs() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fk").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };

    // timing: timeouts in the catalog; passing runs come close to the limit.
    let s = fx.series("timing", None).await;
    for i in 0..20 {
        let l = fx.launch(ago(10, i)).await;
        let r = if i % 3 == 0 {
            R {
                duration_ms: Some(30_050),
                ..R::fail(
                    "fp-to",
                    "Timeout 30000ms exceeded while waiting for selector \"#pay\"",
                )
            }
        } else {
            R {
                duration_ms: Some(24_000 + i * 100),
                ..R::pass()
            }
        };
        fx.result(l, s, r).await;
    }
    let h = compute(&store, &p, s).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Timing], "{h:#?}");
    let t = hint(&h, FlakyHintKind::Timing);
    assert_eq!(t.evidence["timeoutFailures"], 7);
    assert_eq!(t.confidence, HintConfidence::High);
    assert_eq!(
        t.actions,
        vec![HintAction::Mute, HintAction::Issue, HintAction::Rule]
    );
    assert_eq!(
        (h.window_runs, h.failures, h.backend),
        (20, 7, AnalyticsBackend::Postgres)
    );
    assert!(t.evidence["examples"][0]["resultId"].is_string());

    // environment: one host fails.
    let s = fx.series("host", None).await;
    for i in 0..30 {
        let l = fx.launch(ago(10, i)).await;
        let host = ["ci-1", "ci-2", "ci-3"][i as usize % 3];
        let r = if host == "ci-3" && i % 2 == 0 {
            R::fail(&format!("fp-h{i}"), "assertion failed").clone_static()
        } else {
            R::pass()
        };
        fx.result(
            l,
            s,
            R {
                host: Some(host),
                ..r
            },
        )
        .await;
    }
    let h = compute(&store, &p, s).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Environment], "{h:#?}");
    let e = hint(&h, FlakyHintKind::Environment);
    assert_eq!(
        (
            e.evidence["dimension"].as_str(),
            e.evidence["value"].as_str()
        ),
        (Some("host"), Some("ci-3"))
    );
    assert!(e.evidence["p"].as_f64().unwrap() < 0.05);

    // environment through the same test in other environments.
    let staging = fx.series_params("envy", Some("staging"), "q").await;
    let prod = fx.series_params("envy", Some("prod"), "q").await;
    for i in 0..20 {
        let l = fx.launch(ago(9, i)).await;
        fx.result(
            l,
            staging,
            if i % 2 == 0 {
                R::fail("fp-e", "expected 200 got 500")
            } else {
                R::pass()
            },
        )
        .await;
        fx.result(l, prod, R::pass()).await;
    }
    let h = compute(&store, &p, staging).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Environment], "{h:#?}");
    assert_eq!(
        hint(&h, FlakyHintKind::Environment).evidence["dimension"],
        "env"
    );
    assert!(compute(&store, &p, prod).await.hints.is_empty());

    // order: fails together with another test.
    let a = fx.series("order-a", None).await;
    let b = fx.series("order-b", None).await;
    for i in 0..15 {
        let l = fx.launch(ago(8, i)).await;
        let both = i % 3 == 0;
        fx.result(
            l,
            a,
            if both {
                R::fail("fp-oa", "stale cart")
            } else {
                R::pass()
            },
        )
        .await;
        fx.result(
            l,
            b,
            if both || i == 4 {
                R::fail("fp-ob", "row exists")
            } else {
                R::pass()
            },
        )
        .await;
    }
    let h = compute(&store, &p, a).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Order], "{h:#?}");
    let o = hint(&h, FlakyHintKind::Order);
    assert_eq!(
        (o.evidence["via"].as_str(), o.evidence["name"].as_str()),
        (Some("coFailure"), Some("order-b"))
    );
    assert_eq!(o.evidence["shared"], 5);

    // order: fails after a test on the same worker.
    let v = fx.series("victim", None).await;
    let pol = fx.series("polluter", None).await;
    let other = fx.series("bystander", None).await;
    for i in 0..20 {
        let l = fx.launch(ago(7, i)).await;
        let t0 = l.at + Duration::seconds(10);
        let after = i % 4 == 0;
        let first = if after { pol } else { other };
        fx.result(
            l,
            first,
            R {
                thread: Some("w1"),
                host: Some("h"),
                started: Some(t0),
                ..R::pass()
            },
        )
        .await;
        fx.result(
            l,
            if after { other } else { pol },
            R {
                thread: Some("w2"),
                host: Some("h"),
                started: Some(t0),
                ..R::pass()
            },
        )
        .await;
        let r = if after {
            R::fail(&format!("fp-v{i}"), "count was 3").clone_static()
        } else {
            R::pass()
        };
        fx.result(
            l,
            v,
            R {
                thread: Some("w1"),
                host: Some("h"),
                started: Some(t0 + Duration::seconds(5)),
                ..r
            },
        )
        .await;
    }
    let h = compute(&store, &p, v).await;
    let o = hint(&h, FlakyHintKind::Order);
    assert_eq!(
        (o.evidence["via"].as_str(), o.evidence["name"].as_str()),
        (Some("worker"), Some("polluter")),
        "{h:#?}"
    );

    // retry: different errors, passes on a retry of the same run.
    let s = fx.series("retry", None).await;
    for i in 0..12 {
        let l = fx.launch(ago(6, i)).await;
        let r = if i % 3 == 1 {
            R {
                status: "passed",
                flaky_in_run: true,
                attempts: vec![(
                    ["fp-r1", "fp-r2", "fp-r3", "fp-r4"][i as usize / 3],
                    "flaky",
                )],
                ..R::default()
            }
        } else {
            R::pass()
        };
        fx.result(l, s, r).await;
    }
    let h = compute(&store, &p, s).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Retry], "{h:#?}");
    assert_eq!(hint(&h, FlakyHintKind::Retry).evidence["retriedRuns"], 4);

    // infra through signals (seeded as the signals writer stores them).
    let s = fx.series("grid", None).await;
    for i in 0..12 {
        let l = fx.launch(ago(5, i)).await;
        let failing = i % 4 == 0;
        let id = fx
            .result(
                l,
                s,
                if failing {
                    R::fail(&format!("fp-g{i}"), "element not found").clone_static()
                } else {
                    R::pass()
                },
            )
            .await;
        if failing && i < 8 {
            fx.signal(l, id, "grid", "session_crashed").await;
        }
    }
    let h = compute(&store, &p, s).await;
    assert_eq!(kinds(&h), vec![FlakyHintKind::Infra], "{h:#?}");
    assert_eq!(
        (h.failures, h.infra_failures, h.infra_share),
        (3, 2, Some(0.667))
    );
    assert_eq!(
        hint(&h, FlakyHintKind::Infra).evidence["signals"]["grid:session_crashed"],
        2
    );

    // infra through a burst of infrastructure errors in the launch.
    let s = fx.series("burst", None).await;
    let crowd: Vec<Uuid> = {
        let mut v = Vec::new();
        for k in 0..8 {
            v.push(fx.series(&format!("crowd{k}"), None).await);
        }
        v
    };
    for i in 0..10 {
        let l = fx.launch(ago(4, i)).await;
        let burst = i % 4 == 0;
        let t0 = l.at + Duration::seconds(30);
        let r = if burst {
            R {
                started: Some(t0),
                ..R::fail("fp-cr", "connect ECONNREFUSED 198.51.100.7:443")
            }
        } else {
            R::pass()
        };
        fx.result(l, s, r).await;
        for (k, c) in crowd.iter().enumerate() {
            let r = if burst && k < 4 {
                R {
                    started: Some(t0 + Duration::seconds(k as i64 * 5)),
                    ..R::fail("fp-cr", "connect ECONNREFUSED 198.51.100.7:443")
                }
            } else {
                R::pass()
            };
            fx.result(l, *c, r).await;
        }
        sqlx::query("update launches set count_total = 9 where id = $1")
            .bind(l.id)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let h = compute(&store, &p, s).await;
    let i = hint(&h, FlakyHintKind::Infra);
    assert_eq!(i.evidence["bursts"], 3, "{h:#?}");
    assert_eq!(h.infra_share, Some(1.0));

    // Below every minimum: four failures, no context.
    let s = fx.series("quiet", None).await;
    for i in 0..9 {
        let l = fx.launch(ago(3, i)).await;
        fx.result(
            l,
            s,
            if i % 2 == 1 {
                R::fail("fp-to", "Timeout 30000ms exceeded")
            } else {
                R::pass()
            },
        )
        .await;
    }
    let h = compute(&store, &p, s).await;
    assert!(h.hints.is_empty(), "{h:#?}");
    assert_eq!((h.window_runs, h.failures), (9, 4));
}

/// Fixture strings in results are borrowed; this makes a computed one live long enough.
trait Static {
    fn clone_static(self) -> R<'static>;
}

impl Static for R<'_> {
    fn clone_static(self) -> R<'static> {
        let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
        R {
            status: leak(self.status),
            flaky_in_run: self.flaky_in_run,
            duration_ms: self.duration_ms,
            host: self.host.map(leak),
            thread: self.thread.map(leak),
            started: self.started,
            message: self.message.map(leak),
            attempts: self
                .attempts
                .iter()
                .map(|(a, b)| (leak(a), leak(b)))
                .collect(),
            transition: self.transition.map(leak),
        }
    }
}

#[tokio::test]
async fn stored_hints_feed_the_reader_and_the_flaky_list() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fr").await;
    let other = project(&store, "fr2").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let s = fx.series("grid", None).await;
    fx.stats(s, true, 0.6).await;
    for i in 0..10 {
        let l = fx.launch(ago(2, i)).await;
        let id = fx
            .result(
                l,
                s,
                if i % 2 == 0 {
                    R::fail("fp-x", "lost")
                } else {
                    R::pass()
                },
            )
            .await;
        if i % 2 == 0 && i < 6 {
            fx.signal(l, id, "k8s", "pod_evicted").await;
        }
    }
    let before = store.flaky_hints(p.id, s).await.unwrap();
    assert_eq!(
        (before.computed_at, before.window_runs, before.hints.len()),
        (None, 0, 0)
    );
    assert!(matches!(
        store.flaky_hints(other.id, s).await,
        Err(TmError::NotFound(_))
    ));
    assert!(matches!(
        store.flaky_hints(p.id, Uuid::now_v7()).await,
        Err(TmError::NotFound(_))
    ));
    let computed = compute(&store, &p, s).await;
    let read = store.flaky_hints(p.id, s).await.unwrap();
    assert_eq!(read.hints, computed.hints);
    assert_eq!(
        (read.failures, read.infra_failures, read.infra_share),
        (5, 3, Some(0.6))
    );
    assert!(read.computed_at.is_some());
    let list = store
        .flaky_tests(
            p.id,
            &ProjectSettings::default(),
            &FlakyFilter {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].series_id, s);
    assert_eq!(list[0].infra_share, Some(0.6));
    assert_eq!(list[0].hints[0].kind, FlakyHintKind::Infra);
    let b = store.flaky_hint_briefs(other.id, &[s]).await.unwrap();
    assert!(b.is_empty(), "another project's briefs leak");
    let j = serde_json::to_value(&list[0]).unwrap();
    assert_eq!(
        j["hints"][0],
        json!({"kind": "infra", "confidence": "medium"})
    );
    assert_eq!(j["seriesId"], json!(s));
}

#[tokio::test]
async fn launch_and_daily_targets() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "ft").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let flaky = fx.series("flaky", None).await;
    let scored = fx.series("scored", None).await;
    let calm = fx.series("calm", None).await;
    let unchanged = fx.series("unchanged", None).await;
    fx.stats(flaky, true, 0.1).await;
    fx.stats(scored, false, 0.3).await;
    fx.stats(calm, false, 0.1).await;
    fx.stats(unchanged, true, 0.9).await;
    let l = fx.launch(ago(0, -10)).await;
    for s in [flaky, scored, calm] {
        fx.result(
            l,
            s,
            R {
                transition: Some("regressed"),
                ..R::fail("fp", "x")
            },
        )
        .await;
    }
    fx.result(l, unchanged, R::pass()).await;
    let launch = store.launch(p.id, l.id).await.unwrap().unwrap();
    let (mut t, more) = store.flaky_hint_targets(p.id, &launch, 0.2).await.unwrap();
    t.sort();
    let mut want = vec![flaky, scored];
    want.sort();
    assert_eq!((t, more), (want, false));
    let (t, _) = store.flaky_hint_targets(p.id, &launch, 0.5).await.unwrap();
    assert_eq!(t, vec![flaky]);

    let now = Utc::now();
    let stale = store
        .stale_flaky_hint_series(p.id, 0.2, now, 10)
        .await
        .unwrap();
    assert_eq!(
        stale.len(),
        3,
        "flaky, scored and unchanged have no hints yet"
    );
    compute(&store, &p, unchanged).await;
    let stale = store
        .stale_flaky_hint_series(p.id, 0.2, now, 10)
        .await
        .unwrap();
    assert!(!stale.contains(&unchanged));
    assert!(
        store
            .stale_flaky_hint_series(p.id, 0.2, now + Duration::hours(21), 10)
            .await
            .unwrap()
            .contains(&unchanged)
    );
    compute(&store, &p, calm).await;
    fx.stats(unchanged, false, 0.0).await;
    assert_eq!(store.drop_settled_flaky_hints(p.id, 0.2).await.unwrap(), 2);
    assert_eq!(
        store
            .flaky_hints(p.id, unchanged)
            .await
            .unwrap()
            .computed_at,
        None
    );
}

#[tokio::test]
async fn postgres_and_clickhouse_windows_agree() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fc").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let s = fx.series("host", None).await;
    for i in 0..30 {
        let l = fx.launch(ago(10, i)).await;
        let host = ["ci-1", "ci-2", "ci-3"][i as usize % 3];
        let r = if host == "ci-3" && i % 2 == 0 {
            R::fail("fp-h", "assertion failed")
        } else {
            R::pass()
        };
        fx.result(
            l,
            s,
            R {
                host: Some(host),
                ..r
            },
        )
        .await;
    }
    let pg = compute(&store, &p, s).await;
    let window = store.series_window(p.id, s, 50).await.unwrap();
    let ch = store
        .compute_flaky_hints(
            p.id,
            s,
            &ProjectSettings::default(),
            Some(window.clone()),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ch.backend, AnalyticsBackend::Clickhouse);
    assert_eq!(
        (ch.window_runs, ch.failures, ch.infra_share),
        (pg.window_runs, pg.failures, pg.infra_share)
    );
    assert_eq!(kinds(&ch), kinds(&pg));
    for (a, b) in ch.hints.iter().zip(&pg.hints) {
        assert_eq!(
            (a.confidence, &a.summary, a.sample),
            (b.confidence, &b.summary, b.sample)
        );
        assert_eq!(
            a.evidence["examples"], b.evidence["examples"],
            "PostgreSQL facts merge by launch"
        );
    }
    // An empty ClickHouse answer (series not there yet) is the PostgreSQL window.
    let empty = store
        .compute_flaky_hints(
            p.id,
            s,
            &ProjectSettings::default(),
            Some(Vec::new()),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty.backend, AnalyticsBackend::Postgres);

    // A long history: older runs only ClickHouse still has, failing at night (UTC).
    let start = (Utc::now() - Duration::days(200))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let long: Vec<SeriesRun> = (0..80)
        .map(|i| {
            let at = start + Duration::hours(i * 6);
            let night = at.hour() < 6;
            SeriesRun {
                launch_id: Uuid::from_u128(0xc0de_0000 + i as u128),
                created_at: at,
                status: if night && i % 3 != 0 {
                    "failed".into()
                } else {
                    "passed".into()
                },
                flaky_in_run: false,
                duration_ms: Some(900),
                env_key: None,
                branch: Some("main".into()),
                host: None,
                thread: None,
                ci_runner: None,
                attempt_fingerprints: if night && i % 3 != 0 {
                    vec!["fp-n".into()]
                } else {
                    Vec::new()
                },
                signals: Vec::new(),
            }
        })
        .collect();
    let s2 = fx.series("nightly", None).await;
    let ch = store
        .compute_flaky_hints(
            p.id,
            s2,
            &ProjectSettings::default(),
            Some(long.clone()),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kinds(&ch).contains(&FlakyHintKind::Time), "{ch:#?}");
    assert_eq!(ch.window_runs, 80);
    // The same runs on PostgreSQL could not say `time` (and are beyond rawDays there anyway).
    let ctx = fastproof_analysis::hints::Context::default();
    let runs: Vec<_> = long
        .iter()
        .map(|r| fastproof_analysis::hints::Run {
            launch_id: r.launch_id,
            at: r.created_at,
            outcome: fastproof_analysis::hints::Outcome::of(&r.status),
            ..Default::default()
        })
        .collect();
    assert!(
        !fastproof_analysis::hints::classify(&runs, &ctx)
            .hints
            .iter()
            .any(|h| h.kind == FlakyHintKind::Time)
    );
}

#[tokio::test]
async fn a_series_of_fifty_runs_in_big_launches() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fp").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let s = fx.series("target", None).await;
    let mut crowd = Vec::new();
    let size: usize = std::env::var("FASTPROOF_FLAKY_BENCH_CROWD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    for k in 0..size {
        crowd.push(fx.series(&format!("c{k}"), None).await);
    }
    for i in 0..50 {
        let l = fx.launch(ago(20, i * 6)).await;
        let t0 = l.at;
        let tc: Vec<Uuid> = crowd.clone();
        // The crowd through one statement per launch.
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, status, labels, started_at, stopped_at, message) \
             select gen_random_uuid(), $1, $2, $3, s.test_case_id, s.id, 'n', 'n', \
               case when random() < 0.05 then 'failed' else 'passed' end, \
               jsonb_build_array(jsonb_build_object('name', 'thread', 'value', 'w' || (o % 4)), \
                 jsonb_build_object('name', 'host', 'value', 'h1')), \
               $4 + make_interval(secs => o), $4 + make_interval(secs => o + 1), 'boom' \
             from unnest($5::uuid[]) with ordinality as x(id, o) join series s on s.id = x.id",
        )
        .bind(l.at)
        .bind(p.id)
        .bind(l.id)
        .bind(t0)
        .bind(&tc)
        .execute(&db.pool)
        .await
        .unwrap();
        let r = if i % 3 == 0 {
            R::fail(&format!("fp-{i}"), "Timeout 30000ms exceeded").clone_static()
        } else {
            R::pass()
        };
        fx.result(
            l,
            s,
            R {
                thread: Some("w1"),
                host: Some("h1"),
                started: Some(t0 + Duration::seconds(150)),
                duration_ms: Some(1200),
                ..r
            },
        )
        .await;
    }
    sqlx::query("analyze").execute(&db.pool).await.unwrap();
    compute(&store, &p, s).await;
    let mut times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let h = compute(&store, &p, s).await;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(h.window_runs, 50);
        assert!(
            h.hints.iter().any(|x| x.kind == FlakyHintKind::Timing),
            "{h:#?}"
        );
    }
    times.sort_by(f64::total_cmp);
    eprintln!(
        "flaky hints of a series of 50 runs in launches of {} results: median {:.1} ms",
        size + 1,
        times[2]
    );
    assert!(times[2] < 2_000.0, "{times:?}");
}

/// Environments are read like the series' own window (the newest 50 runs of each), and the
/// series' own environment is always among the compared ones, however many there are and however
/// often the others run.
#[tokio::test]
async fn environment_reads_are_bounded_by_the_window() {
    let Some((db, store)) = TestDb::migrated().await else {
        return;
    };
    let p = project(&store, "fe").await;
    let fx = Fx {
        db: &db,
        p: p.clone(),
    };
    let own = fx.series_params("wide", Some("staging"), "q").await;
    let mut others = Vec::new();
    for k in 0..22 {
        others.push(
            fx.series_params("wide", Some(&format!("env-{k:02}")), "q")
                .await,
        );
    }
    for i in 0..60 {
        let l = fx.launch(ago(9, i)).await;
        sqlx::query(
            "insert into results (id, launch_created_at, project_id, launch_id, test_case_id, series_id, name, \
               full_name, status, started_at, stopped_at) \
             select gen_random_uuid(), $1, $2, $3, s.test_case_id, s.id, 'n', 'n', $5, $1, $1 \
             from series s where s.id = any($4)",
        )
        .bind(l.at)
        .bind(p.id)
        .bind(l.id)
        .bind(&others)
        .bind(if i == 59 { "failed" } else { "passed" })
        .execute(&db.pool)
        .await
        .unwrap();
        if i >= 40 {
            let r = if i % 2 == 0 {
                R::fail("fp-w", "expected 200 got 500")
            } else {
                R::pass()
            };
            fx.result(l, own, r).await;
        }
    }
    let h = compute(&store, &p, own).await;
    let e = hint(&h, FlakyHintKind::Environment);
    assert_eq!(
        (
            e.evidence["dimension"].as_str(),
            e.evidence["value"].as_str(),
            e.evidence["runs"].as_u64(),
            e.evidence["failures"].as_u64(),
        ),
        (Some("env"), Some("staging"), Some(20), Some(10)),
        "{h:#?}"
    );
    // 19 other environments (20 with the own one), the newest 50 runs of each.
    assert_eq!(e.evidence["otherRuns"], 19 * 50, "{h:#?}");
    assert_eq!(e.evidence["otherFailures"], 19, "{h:#?}");
}
