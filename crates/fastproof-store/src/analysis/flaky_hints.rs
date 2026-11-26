//! Flaky causes on PostgreSQL (contract §7.8): the window of a series' recent runs, the context
//! the classifiers of `fastproof_analysis::hints` need (timeout and infrastructure errors from
//! the fingerprint catalog, bursts of infrastructure errors in a launch, tests failing in the
//! same launches, tests that ran earlier on the same worker, the same test in its other
//! environments), the persisted hints and the readers of the API and the flaky list.
//!
//! Every read is bounded: at most [`hints::PG_MAX_RUNS`] runs within `rawDays`, the order
//! evidence from the newest [`ORDER_RUNS`] of them, [`EARLIER_TESTS`] earlier tests per run,
//! [`CO_FAILING_MAX`] other tests and [`ENV_SIBLINGS_MAX`] environments.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use fastproof_analysis::hints::{self, Action, Context, EnvShare, OtherSeries, Outcome, Run};
use serde_json::Value;
use uuid::Uuid;

use super::settings::ProjectSettings;
use super::types::{
    AnalysisResult, AnalyticsBackend, FlakyHint, FlakyHintBrief, FlakyHints, HintAction, SeriesRun,
};
use crate::tm::TmError;
use crate::{Launch, Store};

/// Runs of the window whose launches the order evidence reads, newest first.
pub const ORDER_RUNS: usize = 20;
/// Tests that ran earlier on the same worker, per run, at most.
pub const EARLIER_TESTS: i64 = 10;
/// How far back an earlier test on the same worker may have started.
pub const WORKER_LOOKBACK_MINUTES: i64 = 30;
/// Other tests failing in the same launches, at most.
pub const CO_FAILING_MAX: i64 = 20;
/// Environments of the same test compared, at most.
pub const ENV_SIBLINGS_MAX: i64 = 20;
/// Series recomputed after one launch, at most (the daily job takes the rest).
pub const LAUNCH_SERIES_MAX: i64 = 200;
/// Hints older than this are recomputed by the daily job.
pub const STALE_HOURS: i64 = 20;

/// Messages of timeouts (PostgreSQL regular expression, case-insensitive).
pub const TIMEOUT_PATTERN: &str = "timed? ?out|timeout|deadline exceeded|took longer than|exceeded .{0,40}(time|limit)|waiting for .{0,80} to (be|become)";
/// Messages of infrastructure errors (PostgreSQL regular expression, case-insensitive).
pub const INFRA_PATTERN: &str = "connection refused|econnrefused|connection reset|econnreset|broken pipe|could not resolve|name or service not known|getaddrinfo|enotfound|nxdomain|dns (resolution|lookup|error)|bad gateway|service unavailable|gateway time-?out|\\m50[234]\\M|out of memory|oomkilled|\\moom\\M|cannot allocate memory|session not created|invalid session id|no such session|browser (has )?crashed|target crashed|chrome not reachable|disconnected: not connected to devtools|node (lost|not ready)|evicted";

/// A run of the window with what only PostgreSQL has (the result, its times, its worker).
#[derive(Clone, Debug, sqlx::FromRow)]
struct WindowRow {
    result_id: Uuid,
    launch_id: Uuid,
    launch_created_at: DateTime<Utc>,
    status: String,
    flaky_in_run: bool,
    duration_ms: Option<i64>,
    env_key: Option<String>,
    branch: Option<String>,
    host: Option<String>,
    thread: Option<String>,
    ci_runner: Option<String>,
    fingerprints: Vec<String>,
    signals: Vec<String>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
}

impl WindowRow {
    fn series_run(&self) -> SeriesRun {
        SeriesRun {
            launch_id: self.launch_id,
            created_at: self.launch_created_at,
            status: self.status.clone(),
            flaky_in_run: self.flaky_in_run,
            duration_ms: self.duration_ms,
            env_key: self.env_key.clone(),
            branch: self.branch.clone(),
            host: self.host.clone(),
            thread: self.thread.clone(),
            ci_runner: self.ci_runner.clone(),
            attempt_fingerprints: self.fingerprints.clone(),
            signals: self.signals.clone(),
        }
    }

    fn worker(&self) -> Option<String> {
        self.thread
            .as_ref()
            .map(|t| format!("{}/{t}", self.host.as_deref().unwrap_or("")))
    }

    fn failing(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "broken")
            || (self.flaky_in_run && self.status == "passed")
    }
}

fn dims(
    env: Option<&str>,
    host: Option<&str>,
    runner: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut d = Vec::new();
    if let Some(v) = env.filter(|v| !v.is_empty()) {
        d.push(("env", v.to_string()));
    }
    if let Some(v) = host.filter(|v| !v.is_empty()) {
        d.push(("host", v.to_string()));
    }
    if let Some(v) = runner.filter(|v| !v.is_empty()) {
        d.push(("ciRunner", v.to_string()));
    }
    d
}

/// A run of the classifiers from a ClickHouse run (no result; the PostgreSQL extras are merged
/// in by launch when the run is still there).
fn run_of(r: &SeriesRun) -> Run {
    Run {
        launch_id: r.launch_id,
        result_id: None,
        at: r.created_at,
        outcome: Outcome::of(&r.status),
        flaky_in_run: r.flaky_in_run,
        duration_ms: r.duration_ms,
        dims: dims(
            r.env_key.as_deref(),
            r.host.as_deref(),
            r.ci_runner.as_deref(),
        ),
        fingerprints: r.attempt_fingerprints.clone(),
        signals: r.signals.clone(),
        burst: false,
        worker: r
            .thread
            .as_ref()
            .map(|t| format!("{}/{t}", r.host.as_deref().unwrap_or(""))),
        earlier: Vec::new(),
    }
}

fn action(a: Action) -> HintAction {
    match a {
        Action::Mute => HintAction::Mute,
        Action::Issue => HintAction::Issue,
        Action::Rule => HintAction::Rule,
    }
}

fn backend_name(b: AnalyticsBackend) -> &'static str {
    match b {
        AnalyticsBackend::Postgres => "postgres",
        AnalyticsBackend::Clickhouse => "clickhouse",
    }
}

fn share(failures: i32, infra: i32) -> Option<f64> {
    (failures > 0).then(|| (infra as f64 / failures as f64 * 1000.0).round() / 1000.0)
}

/// The hints of a stored row; an unreadable document (a newer format) reads as none.
fn parse_hints(v: Value) -> Vec<FlakyHint> {
    serde_json::from_value(v).unwrap_or_default()
}

/// Briefs of a stored hints document, read leniently (kind and confidence only).
fn parse_briefs(v: &Value) -> Vec<FlakyHintBrief> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    Some(FlakyHintBrief {
                        kind: serde_json::from_value(h.get("kind")?.clone()).ok()?,
                        confidence: serde_json::from_value(h.get("confidence")?.clone()).ok()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the flaky list shows of a series' hints.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlakyBrief {
    pub hints: Vec<FlakyHintBrief>,
    pub infra_share: Option<f64>,
}

/// Identity of a series that the context reads.
struct SeriesMeta {
    test_case_id: Uuid,
    parameters_hash: String,
    env_key: Option<String>,
}

impl Store {
    async fn window_rows(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        since: DateTime<Utc>,
        max_runs: usize,
    ) -> anyhow::Result<Vec<WindowRow>> {
        let mut rows: Vec<WindowRow> = sqlx::query_as(
            "select r.id as result_id, r.launch_id, r.launch_created_at, r.status, r.flaky_in_run, r.duration_ms, \
               l.env_key, l.branch, \
               (select x->>'value' from jsonb_array_elements(case when jsonb_typeof(r.labels) = 'array' \
                  then r.labels else '[]'::jsonb end) x where x->>'name' = 'host' limit 1) as host, \
               (select x->>'value' from jsonb_array_elements(case when jsonb_typeof(r.labels) = 'array' \
                  then r.labels else '[]'::jsonb end) x where x->>'name' = 'thread' limit 1) as thread, \
               (select x->>'value' from jsonb_array_elements(case when jsonb_typeof(l.attributes) = 'array' \
                  then l.attributes else '[]'::jsonb end) x where x->>'key' in ('runner', 'ci.runner') limit 1) \
                  as ci_runner, \
               coalesce((select array_agg(distinct a.fingerprint order by a.fingerprint) from attempts a \
                  where a.result_id = r.id and a.launch_created_at = r.launch_created_at \
                    and a.fingerprint is not null), '{}') as fingerprints, \
               coalesce((select array_agg(distinct g.source || ':' || g.kind order by g.source || ':' || g.kind) \
                  from attempt_signals g where g.result_id = r.id and g.project_id = r.project_id), '{}') as signals, \
               r.started_at, r.stopped_at \
             from results r join launches l on l.id = r.launch_id \
             where r.series_id = $1 and r.project_id = $2 and r.launch_created_at >= $3 and not l.debug \
             order by r.launch_created_at desc, r.id limit $4",
        )
        .bind(series_id)
        .bind(project_id)
        .bind(since)
        .bind(max_runs.min(hints::PG_MAX_RUNS) as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    /// The recent runs of a series, oldest first: launches of the last `rawDays` days (debug
    /// launches left out), at most `max_runs` (≤ 50).
    pub async fn series_window(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        max_runs: usize,
    ) -> anyhow::Result<Vec<SeriesRun>> {
        let settings = self.project_settings(project_id).await?;
        let since = Utc::now() - Duration::days(settings.retention.raw_days as i64);
        Ok(self
            .window_rows(project_id, series_id, since, max_runs)
            .await?
            .iter()
            .map(WindowRow::series_run)
            .collect())
    }

    async fn series_meta(
        &self,
        project_id: Uuid,
        series_id: Uuid,
    ) -> anyhow::Result<Option<SeriesMeta>> {
        let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
            "select test_case_id, parameters_hash, env_key from series where id = $1 and project_id = $2",
        )
        .bind(series_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(
            row.map(|(test_case_id, parameters_hash, env_key)| SeriesMeta {
                test_case_id,
                parameters_hash,
                env_key,
            }),
        )
    }

    /// Fingerprints of the catalog whose message is a timeout or an infrastructure error.
    async fn classify_fingerprints(
        &self,
        project_id: Uuid,
        fps: &[String],
    ) -> anyhow::Result<(BTreeSet<String>, BTreeSet<String>)> {
        if fps.is_empty() {
            return Ok(Default::default());
        }
        let rows: Vec<(String, bool, bool)> = sqlx::query_as(
            "select fingerprint, message ~* $3, message ~* $4 from failure_fingerprints \
             where project_id = $1 and fingerprint = any($2)",
        )
        .bind(project_id)
        .bind(fps)
        .bind(TIMEOUT_PATTERN)
        .bind(INFRA_PATTERN)
        .fetch_all(&self.pool)
        .await?;
        let mut timeout = BTreeSet::new();
        let mut infra = BTreeSet::new();
        for (f, t, i) in rows {
            if t {
                timeout.insert(f.clone());
            }
            if i {
                infra.insert(f);
            }
        }
        Ok((timeout, infra))
    }

    /// Failing results that fell into a burst: within [`hints::BURST_SECONDS`] of them, at least
    /// [`hints::BURST_SHARE`] of the launch's results (and at least two) failed with an
    /// infrastructure error.
    async fn bursts(&self, rows: &[&WindowRow]) -> anyhow::Result<BTreeSet<Uuid>> {
        let rows: Vec<&&WindowRow> = rows
            .iter()
            .filter(|r| r.failing() && r.stopped_at.or(r.started_at).is_some())
            .collect();
        if rows.is_empty() {
            return Ok(BTreeSet::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.result_id).collect();
        let launches: Vec<Uuid> = rows.iter().map(|r| r.launch_id).collect();
        let lcas: Vec<DateTime<Utc>> = rows.iter().map(|r| r.launch_created_at).collect();
        let at: Vec<DateTime<Utc>> = rows
            .iter()
            .map(|r| r.stopped_at.or(r.started_at).unwrap_or(r.launch_created_at))
            .collect();
        let hit: Vec<Uuid> = sqlx::query_scalar(
            "select x.id from unnest($1::uuid[], $2::uuid[], $3::timestamptz[], $4::timestamptz[]) \
               as x(id, launch_id, lca, t) \
             join launches l on l.id = x.launch_id \
             where (select count(*) from results o where o.launch_id = x.launch_id and o.launch_created_at = x.lca \
                      and o.status in ('failed', 'broken') \
                      and coalesce(o.stopped_at, o.started_at) between x.t - make_interval(secs => $5) \
                        and x.t + make_interval(secs => $5) \
                      and o.message ~* $6) \
                   >= greatest(2, ceil($7 * l.count_total))",
        )
        .bind(&ids)
        .bind(&launches)
        .bind(&lcas)
        .bind(&at)
        .bind(hints::BURST_SECONDS as f64)
        .bind(INFRA_PATTERN)
        .bind(hints::BURST_SHARE)
        .fetch_all(&self.pool)
        .await?;
        Ok(hit.into_iter().collect())
    }

    /// Other tests failing in at least [`hints::ORDER_MIN_SHARED`] of the launches where this
    /// one failed, with the launches of the window where they failed.
    async fn co_failing(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        window: &[&WindowRow],
        mine: &BTreeSet<Uuid>,
    ) -> anyhow::Result<Vec<OtherSeries>> {
        if mine.len() < hints::ORDER_MIN_SHARED {
            return Ok(Vec::new());
        }
        let launches: Vec<Uuid> = window.iter().map(|r| r.launch_id).collect();
        let lcas: Vec<DateTime<Utc>> = window.iter().map(|r| r.launch_created_at).collect();
        let mine: Vec<Uuid> = mine.iter().copied().collect();
        let rows: Vec<(Uuid, String, String, Vec<Uuid>)> = sqlx::query_as(
            "with w as (select * from unnest($1::uuid[], $2::timestamptz[]) as w(launch_id, lca)), \
             f as (select o.series_id, array_agg(distinct o.launch_id) as launches, \
                     count(distinct o.launch_id) filter (where o.launch_id = any($4)) as shared \
                   from w join results o on o.launch_id = w.launch_id and o.launch_created_at = w.lca \
                   where o.project_id = $5 and o.series_id <> $3 \
                     and (o.status in ('failed', 'broken') or (o.flaky_in_run and o.status = 'passed')) \
                   group by o.series_id \
                   having count(distinct o.launch_id) filter (where o.launch_id = any($4)) >= $6 \
                   order by shared desc, o.series_id limit $7) \
             select f.series_id, s.series_key, tc.display_name, f.launches from f \
               join series s on s.id = f.series_id join test_cases tc on tc.id = s.test_case_id \
             order by f.shared desc, f.series_id",
        )
        .bind(&launches)
        .bind(&lcas)
        .bind(series_id)
        .bind(&mine)
        .bind(project_id)
        .bind(hints::ORDER_MIN_SHARED as i64)
        .bind(CO_FAILING_MAX)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, _key, name, l)| OtherSeries {
                key: id.to_string(),
                name,
                failed_launches: l.into_iter().collect(),
            })
            .collect())
    }

    /// Tests (series ids) that ran earlier on the same worker (thread label, and host when
    /// known) in the same launch, per result.
    async fn earlier_on_worker(
        &self,
        rows: &[&WindowRow],
    ) -> anyhow::Result<HashMap<Uuid, Vec<String>>> {
        let rows: Vec<&&WindowRow> = rows
            .iter()
            .filter(|r| r.thread.is_some() && r.started_at.is_some())
            .collect();
        if rows.len() < hints::ORDER_MIN_RUNS {
            return Ok(HashMap::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.result_id).collect();
        let launches: Vec<Uuid> = rows.iter().map(|r| r.launch_id).collect();
        let lcas: Vec<DateTime<Utc>> = rows.iter().map(|r| r.launch_created_at).collect();
        let started: Vec<DateTime<Utc>> = rows
            .iter()
            .map(|r| r.started_at.unwrap_or(r.launch_created_at))
            .collect();
        let threads: Vec<String> = rows
            .iter()
            .map(|r| r.thread.clone().unwrap_or_default())
            .collect();
        let hosts: Vec<Option<String>> = rows.iter().map(|r| r.host.clone()).collect();
        let out: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
            "select x.id, array(select o.series_id::text from results o \
                 where o.launch_id = x.launch_id and o.launch_created_at = x.lca and o.id <> x.id \
                   and o.started_at < x.started and o.started_at >= x.started - make_interval(mins => $7) \
                   and o.labels @> jsonb_build_array(jsonb_build_object('name', 'thread', 'value', x.thread)) \
                   and (x.host is null \
                     or o.labels @> jsonb_build_array(jsonb_build_object('name', 'host', 'value', x.host))) \
                 order by o.started_at desc limit $8) \
             from unnest($1::uuid[], $2::uuid[], $3::timestamptz[], $4::timestamptz[], $5::text[], $6::text[]) \
               as x(id, launch_id, lca, started, thread, host)",
        )
        .bind(&ids)
        .bind(&launches)
        .bind(&lcas)
        .bind(&started)
        .bind(&threads)
        .bind(&hosts)
        .bind(WORKER_LOOKBACK_MINUTES as i32)
        .bind(EARLIER_TESTS)
        .fetch_all(&self.pool)
        .await?;
        Ok(out.into_iter().collect())
    }

    async fn series_names(
        &self,
        project_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<BTreeMap<String, String>> {
        let ids: Vec<Uuid> = keys.iter().filter_map(|k| k.parse().ok()).collect();
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "select s.id, tc.display_name from series s join test_cases tc on tc.id = s.test_case_id \
             where s.project_id = $1 and s.id = any($2)",
        )
        .bind(project_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, n)| (id.to_string(), n))
            .collect())
    }

    /// Runs and failure events of the same test (same parameters) in each environment: the
    /// newest [`hints::PG_MAX_RUNS`] runs of each since `since` (the window of a series), at most
    /// [`ENV_SIBLINGS_MAX`] environments with this series' own first. Bounded by the window, not
    /// by how often the test runs.
    async fn env_siblings(
        &self,
        project_id: Uuid,
        meta: &SeriesMeta,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<EnvShare>> {
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "with sib as (select s.id, s.env_key from series s \
               where s.project_id = $1 and s.test_case_id = $2 and s.parameters_hash = $3 \
                 and s.env_key is not null \
               order by (s.env_key = $6) desc, s.id limit $7) \
             select sib.env_key, \
               count(*) filter (where w.status in ('passed', 'failed', 'broken')), \
               count(*) filter (where w.status in ('failed', 'broken') or (w.flaky_in_run and w.status = 'passed')) \
             from sib cross join lateral ( \
               select r.status, r.flaky_in_run from results r join launches l on l.id = r.launch_id and not l.debug \
               where r.series_id = sib.id and r.project_id = $1 and r.launch_created_at >= $4 \
               order by r.launch_created_at desc limit $8) w \
             group by sib.env_key order by (sib.env_key = $6) desc, 2 desc, 1 limit $5",
        )
        .bind(project_id)
        .bind(meta.test_case_id)
        .bind(&meta.parameters_hash)
        .bind(since)
        .bind(ENV_SIBLINGS_MAX)
        .bind(meta.env_key.as_deref().unwrap_or(""))
        .bind(ENV_SIBLINGS_MAX * 10)
        .bind(hints::PG_MAX_RUNS as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(value, runs, failures)| EnvShare {
                value,
                runs: runs.clamp(0, u32::MAX as i64) as u32,
                failures: failures.clamp(0, u32::MAX as i64) as u32,
            })
            .collect())
    }

    /// Computes and stores the hints of a series. `history` is the ClickHouse window (the whole
    /// history; `None` or empty: the PostgreSQL window). `None` when the series is not in the
    /// project.
    pub async fn compute_flaky_hints(
        &self,
        project_id: Uuid,
        series_id: Uuid,
        settings: &ProjectSettings,
        history: Option<Vec<SeriesRun>>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<FlakyHints>> {
        let Some(meta) = self.series_meta(project_id, series_id).await? else {
            return Ok(None);
        };
        let since = now - Duration::days(settings.retention.raw_days as i64);
        let rows = self
            .window_rows(project_id, series_id, since, hints::PG_MAX_RUNS)
            .await?;
        let history = history.filter(|h| !h.is_empty());
        let backend = if history.is_some() {
            AnalyticsBackend::Clickhouse
        } else {
            AnalyticsBackend::Postgres
        };
        let newest: Vec<&WindowRow> = rows.iter().rev().take(ORDER_RUNS).collect();
        let all: Vec<&WindowRow> = rows.iter().collect();
        let bursts = self.bursts(&all).await?;
        let earlier = self.earlier_on_worker(&newest).await?;
        let mine: BTreeSet<Uuid> = newest
            .iter()
            .filter(|r| r.failing() && !bursts.contains(&r.result_id))
            .map(|r| r.launch_id)
            .collect();
        let co_failing = self
            .co_failing(project_id, series_id, &newest, &mine)
            .await?;
        let pg_runs: Vec<Run> = rows
            .iter()
            .map(|r| Run {
                launch_id: r.launch_id,
                result_id: Some(r.result_id),
                at: r.launch_created_at,
                outcome: Outcome::of(&r.status),
                flaky_in_run: r.flaky_in_run,
                duration_ms: r.duration_ms,
                dims: dims(
                    r.env_key.as_deref(),
                    r.host.as_deref(),
                    r.ci_runner.as_deref(),
                ),
                fingerprints: r.fingerprints.clone(),
                signals: r.signals.clone(),
                burst: bursts.contains(&r.result_id),
                worker: earlier
                    .contains_key(&r.result_id)
                    .then(|| r.worker())
                    .flatten(),
                earlier: earlier.get(&r.result_id).cloned().unwrap_or_default(),
            })
            .collect();
        let runs: Vec<Run> = match &history {
            Some(h) => {
                let by_launch: HashMap<Uuid, &Run> =
                    pg_runs.iter().map(|r| (r.launch_id, r)).collect();
                h.iter()
                    .map(|c| {
                        let mut r = run_of(c);
                        if let Some(p) = by_launch.get(&c.launch_id) {
                            r.result_id = p.result_id;
                            r.burst = p.burst;
                            r.worker = p.worker.clone();
                            r.earlier = p.earlier.clone();
                            for d in &p.dims {
                                if !r.dims.iter().any(|(k, _)| *k == d.0) {
                                    r.dims.push(d.clone());
                                }
                            }
                            if r.signals.is_empty() {
                                r.signals = p.signals.clone();
                            }
                        }
                        r
                    })
                    .collect()
            }
            None => pg_runs,
        };
        let fps: Vec<String> = runs
            .iter()
            .flat_map(|r| r.fingerprints.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let (timeout_fingerprints, infra_fingerprints) =
            self.classify_fingerprints(project_id, &fps).await?;
        // Names only for the earlier tests that could name a hint (in enough failures).
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for r in runs.iter().filter(|r| r.failing()) {
            for e in &r.earlier {
                *counts.entry(e.as_str()).or_default() += 1;
            }
        }
        let keys: Vec<String> = counts
            .into_iter()
            .filter(|(_, n)| *n >= hints::ORDER_MIN_SHARED)
            .map(|(k, _)| k.to_string())
            .collect();
        let names = self.series_names(project_id, &keys).await?;
        let env_siblings = match &meta.env_key {
            Some(_) => self.env_siblings(project_id, &meta, since).await?,
            None => Vec::new(),
        };
        let ctx = Context {
            timeout_fingerprints,
            infra_fingerprints,
            co_failing,
            names,
            own_env: meta.env_key.clone(),
            env_siblings,
            long_history: backend == AnalyticsBackend::Clickhouse,
        };
        let summary = hints::classify(&runs, &ctx);
        let out = FlakyHints {
            series_id,
            computed_at: Some(now),
            window_runs: summary.window_runs as i32,
            failures: summary.failures as i32,
            infra_failures: summary.infra_failures as i32,
            infra_share: summary.infra_share(),
            backend,
            hints: summary
                .hints
                .into_iter()
                .map(|h| FlakyHint {
                    kind: h.kind,
                    confidence: h.confidence,
                    summary: h.summary,
                    evidence: h.evidence,
                    sample: h.sample,
                    actions: h.actions.into_iter().map(action).collect(),
                })
                .collect(),
        };
        self.put_flaky_hints(series_id, &out).await?;
        Ok(Some(out))
    }

    /// Stores the hints of a series (replacing the previous ones).
    pub async fn put_flaky_hints(&self, series_id: Uuid, h: &FlakyHints) -> anyhow::Result<()> {
        sqlx::query(
            "insert into flaky_hints (series_id, project_id, computed_at, window_runs, failures, infra_failures, \
               backend, hints) \
             select s.id, s.project_id, $2, $3, $4, $5, $6, $7 from series s where s.id = $1 \
             on conflict (series_id) do update set computed_at = excluded.computed_at, \
               window_runs = excluded.window_runs, failures = excluded.failures, \
               infra_failures = excluded.infra_failures, backend = excluded.backend, hints = excluded.hints",
        )
        .bind(series_id)
        .bind(h.computed_at.unwrap_or_else(Utc::now))
        .bind(h.window_runs)
        .bind(h.failures)
        .bind(h.infra_failures)
        .bind(backend_name(h.backend))
        .bind(serde_json::to_value(&h.hints)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The stored hints of a series of the project (empty, `computedAt: null`, before the first
    /// computation); 404 for a series of another project.
    pub async fn flaky_hints(
        &self,
        project_id: Uuid,
        series_id: Uuid,
    ) -> AnalysisResult<FlakyHints> {
        type Row = (
            Option<DateTime<Utc>>,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<String>,
            Option<Value>,
        );
        let row: Option<Row> = sqlx::query_as(
            "select h.computed_at, h.window_runs, h.failures, h.infra_failures, h.backend, h.hints \
             from series s left join flaky_hints h on h.series_id = s.id \
             where s.id = $1 and s.project_id = $2",
        )
        .bind(series_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TmError::Other(e.into()))?;
        let Some((computed_at, runs, failures, infra, backend, hints)) = row else {
            return Err(TmError::NotFound("series not found"));
        };
        let (failures, infra) = (failures.unwrap_or(0), infra.unwrap_or(0));
        Ok(FlakyHints {
            series_id,
            computed_at,
            window_runs: runs.unwrap_or(0),
            failures,
            infra_failures: infra,
            infra_share: share(failures, infra),
            backend: if backend.as_deref() == Some("clickhouse") {
                AnalyticsBackend::Clickhouse
            } else {
                AnalyticsBackend::Postgres
            },
            hints: hints.map(parse_hints).unwrap_or_default(),
        })
    }

    /// Briefs of the stored hints of series of the project, for the flaky list.
    pub async fn flaky_hint_briefs(
        &self,
        project_id: Uuid,
        series: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, FlakyBrief>> {
        if series.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(Uuid, i32, i32, Value)> = sqlx::query_as(
            "select series_id, failures, infra_failures, hints from flaky_hints \
             where project_id = $1 and series_id = any($2)",
        )
        .bind(project_id)
        .bind(series)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, f, i, h)| {
                (
                    id,
                    FlakyBrief {
                        hints: parse_briefs(&h),
                        infra_share: share(f, i),
                    },
                )
            })
            .collect())
    }

    /// Series of the launch whose status changed (a transition, flaky against earlier runs or
    /// within the run) and that are flaky or score at least `min_score`, at most
    /// [`LAUNCH_SERIES_MAX`]; `true` when more were left for the daily job.
    pub async fn flaky_hint_targets(
        &self,
        project_id: Uuid,
        launch: &Launch,
        min_score: f64,
    ) -> anyhow::Result<(Vec<Uuid>, bool)> {
        let mut ids: Vec<Uuid> = sqlx::query_scalar(
            "select distinct r.series_id from results r join series_stats st on st.series_id = r.series_id \
             where r.launch_id = $1 and r.launch_created_at = $2 and r.project_id = $3 \
               and (r.transition is not null or r.flaky or r.flaky_in_run) \
               and (st.flaky or coalesce(st.flaky_score, 0) >= $4) \
             order by r.series_id limit $5",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(project_id)
        .bind(min_score)
        .bind(LAUNCH_SERIES_MAX + 1)
        .fetch_all(&self.pool)
        .await?;
        let more = ids.len() as i64 > LAUNCH_SERIES_MAX;
        ids.truncate(LAUNCH_SERIES_MAX as usize);
        Ok((ids, more))
    }

    /// Flaky series of the project (flaky, or scoring at least `min_score`) whose hints are
    /// missing or older than [`STALE_HOURS`], oldest first, at most `limit`.
    pub async fn stale_flaky_hint_series(
        &self,
        project_id: Uuid,
        min_score: f64,
        now: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "select st.series_id from series_stats st left join flaky_hints h on h.series_id = st.series_id \
             where st.project_id = $1 and (st.flaky or coalesce(st.flaky_score, 0) >= $2) \
               and (h.computed_at is null or h.computed_at < $3) \
             order by h.computed_at nulls first, st.series_id limit $4",
        )
        .bind(project_id)
        .bind(min_score)
        .bind(now - Duration::hours(STALE_HOURS))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Drops the hints of series of the project that are no longer flaky (nor score
    /// `min_score`); returns how many.
    pub async fn drop_settled_flaky_hints(
        &self,
        project_id: Uuid,
        min_score: f64,
    ) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "delete from flaky_hints h where h.project_id = $1 and not exists ( \
               select 1 from series_stats st where st.series_id = h.series_id \
                 and (st.flaky or coalesce(st.flaky_score, 0) >= $2))",
        )
        .bind(project_id)
        .bind(min_score)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefs_read_leniently() {
        let v = serde_json::json!([
            {"kind": "timing", "confidence": "high", "summary": "x"},
            {"kind": "future-kind", "confidence": "high"},
            {"confidence": "low"},
            {"kind": "infra", "confidence": "low"}
        ]);
        let b = parse_briefs(&v);
        assert_eq!(b.len(), 2);
        assert_eq!(serde_json::to_value(&b).unwrap()[1]["kind"], "infra");
        assert!(parse_briefs(&serde_json::json!({})).is_empty());
        assert!(parse_hints(serde_json::json!([{"kind": "nope"}])).is_empty());
    }

    #[test]
    fn dimensions_and_shares() {
        assert_eq!(
            dims(Some("staging"), Some(""), Some("r1")),
            vec![
                ("env", "staging".to_string()),
                ("ciRunner", "r1".to_string())
            ]
        );
        assert_eq!(share(0, 0), None);
        assert_eq!(share(3, 1), Some(0.333));
    }
}
