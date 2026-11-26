//! The PostgreSQL side of the analytics endpoints (contract `76` §7.9): the fallback of every
//! project endpoint over at most [`ANALYTICS_PG_DAYS`] days (`window.partial` beyond), the
//! facts of the error catalog that every backend reads (MTTR, new and regressed errors,
//! aliases, labels) and the helpers both backends share (trend buckets, percentiles), so the
//! PostgreSQL and ClickHouse answers agree on the same data.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use fastproof_analysis::ANALYTICS_PG_DAYS;
use fastproof_model::Step;
use sqlx::Row;
use uuid::Uuid;

use super::types::{
    AnalyticsBackend, AnalyticsWindow, DurationRegression, DurationRegressions, FailureAnalytics,
    FailureGroup, FailureGroupBy, FailureQuery, FlakyTrend, FlakyTrendPoint, MttrBucket, MttrStats,
    RetryCost, RetryCosts, SlowStep, SlowSteps,
};
use crate::{Store, codec};

/// Days a window covers by default.
pub const DEFAULT_DAYS: i64 = 30;
/// Days a window covers at most on ClickHouse.
pub const MAX_DAYS: i64 = 365;
/// Days of the recent part of the duration comparison.
pub const RECENT_DAYS: i64 = 7;
/// Days the recent part is compared with (before it).
pub const BEFORE_DAYS: i64 = 28;
/// Runs a series needs in each part of the duration comparison.
pub const MIN_RUNS: i64 = 10;
/// Attempts whose steps the PostgreSQL fallback of slow steps decodes, at most.
pub const PG_STEP_ATTEMPTS: i64 = 2000;
/// Steps of one attempt read, at most (as the sinks flatten them).
pub const MAX_STEPS_PER_ATTEMPT: usize = 10_000;
/// Buckets of the failure trend, at most.
pub const TREND_BUCKETS: i64 = 14;
/// Upper bounds of the MTTR buckets (seconds): 1 h, 6 h, 1 d, 3 d, 7 d, 30 d, 90 d.
pub const MTTR_BUCKETS: [i64; 7] = [3600, 21_600, 86_400, 259_200, 604_800, 2_592_000, 7_776_000];
/// Reason of a window the PostgreSQL fallback cut.
pub const PG_REASON: &str =
    "PostgreSQL answers the last 30 days; longer history and the portfolio need ClickHouse";
/// Reason of a window cut to the longest one.
pub const MAX_REASON: &str = "at most 365 days";

/// The window of an answer: `to` defaults to now, `from` to 30 days before `to`; a window
/// longer than `max_days` starts `max_days` before `to` and is `partial`.
pub fn window(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    backend: AnalyticsBackend,
) -> AnalyticsWindow {
    let to = to.unwrap_or(now);
    let from = from.unwrap_or(to - Duration::days(DEFAULT_DAYS));
    let max = match backend {
        AnalyticsBackend::Postgres => ANALYTICS_PG_DAYS,
        AnalyticsBackend::Clickhouse => MAX_DAYS,
    };
    let floor = to - Duration::days(max);
    let partial = from < floor;
    AnalyticsWindow {
        backend,
        from: from.max(floor),
        to,
        partial,
        reason: partial.then(|| {
            match backend {
                AnalyticsBackend::Postgres => PG_REASON,
                AnalyticsBackend::Clickhouse => MAX_REASON,
            }
            .to_string()
        }),
    }
}

/// The trend buckets of a window: whole UTC days from the day of `from` to the day before
/// `to`, split into at most [`TREND_BUCKETS`] buckets of equal days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buckets {
    pub start: NaiveDate,
    pub days: i64,
    pub count: usize,
}

impl Buckets {
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        let start = from.date_naive();
        let end = (to - Duration::milliseconds(1)).date_naive().max(start);
        let span = (end - start).num_days() + 1;
        let days = (span + TREND_BUCKETS - 1) / TREND_BUCKETS;
        Self {
            start,
            days,
            count: ((span + days - 1) / days) as usize,
        }
    }

    /// The bucket of a day (`None` outside the window).
    pub fn index(&self, day: NaiveDate) -> Option<usize> {
        let d = (day - self.start).num_days();
        if d < 0 {
            return None;
        }
        let i = (d / self.days) as usize;
        (i < self.count).then_some(i)
    }

    /// Trends per key from `(key, day, count)` rows.
    pub fn trends(&self, rows: &[(String, NaiveDate, i64)]) -> HashMap<String, Vec<i64>> {
        let mut out: HashMap<String, Vec<i64>> = HashMap::new();
        for (k, d, n) in rows {
            if let Some(i) = self.index(*d) {
                out.entry(k.clone()).or_insert_with(|| vec![0; self.count])[i] += n;
            }
        }
        out
    }
}

/// The value at quantile `q` of sorted values as ClickHouse's `quantileExactLow` gives it
/// (the lower median for 0.5; otherwise the value at `floor(q * n)`), `None` when empty.
pub fn quantile_low(sorted: &[i64], q: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let i = if q == 0.5 {
        n.div_ceil(2) - 1
    } else {
        ((q * n as f64).floor() as usize).min(n - 1)
    };
    Some(sorted[i])
}

/// The label of a failure group key.
pub fn group_label(by: FailureGroupBy, key: &str) -> String {
    match by {
        FailureGroupBy::DefectGroup => match key {
            "productBug" => "Product bug",
            "automationBug" => "Automation bug",
            "systemIssue" => "System issue",
            "noDefect" => "No defect",
            "toInvestigate" => "To investigate",
            other => other,
        }
        .to_string(),
        FailureGroupBy::Suite if key.is_empty() => "(no suite)".into(),
        FailureGroupBy::Env if key.is_empty() => "(no environment)".into(),
        _ => key.to_string(),
    }
}

/// Builds the failure groups of an answer from counts, day rows and labels.
pub fn failure_groups(
    by: FailureGroupBy,
    total: i64,
    counts: Vec<(String, i64, i64, i64)>,
    days: &[(String, NaiveDate, i64)],
    buckets: &Buckets,
    labels: &HashMap<String, String>,
) -> Vec<FailureGroup> {
    let trends = buckets.trends(days);
    counts
        .into_iter()
        .map(|(key, failures, tests, launches)| FailureGroup {
            label: labels
                .get(&key)
                .cloned()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| group_label(by, &key)),
            trend: trends
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vec![0; buckets.count]),
            share: if total > 0 {
                ((failures as f64 / total as f64) * 1000.0).round() / 1000.0
            } else {
                0.0
            },
            key,
            failures,
            tests,
            launches,
        })
        .collect()
}

/// MTTR statistics from the seconds each error took to resolve.
pub fn mttr_stats(window: AnalyticsWindow, mut seconds: Vec<i64>) -> MttrStats {
    seconds.sort_unstable();
    MttrStats {
        window,
        resolved: seconds.len() as i64,
        p50_seconds: quantile_low(&seconds, 0.5),
        p90_seconds: quantile_low(&seconds, 0.9),
        buckets: MTTR_BUCKETS
            .iter()
            .map(|le| MttrBucket {
                le: *le,
                count: seconds.iter().filter(|s| **s <= *le).count() as i64,
            })
            .collect(),
    }
}

/// Step names and durations in the order the sinks flatten them (depth first, at most
/// [`MAX_STEPS_PER_ATTEMPT`]); steps without a duration are left out.
pub fn step_durations(steps: &[Step], out: &mut Vec<(String, i64)>) {
    fn walk(steps: &[Step], seen: &mut usize, out: &mut Vec<(String, i64)>) {
        for s in steps {
            if *seen >= MAX_STEPS_PER_ATTEMPT {
                return;
            }
            *seen += 1;
            let d = s.duration_ms.or(match (s.start, s.stop) {
                (Some(a), Some(b)) if b >= a => Some(b - a),
                _ => None,
            });
            if let Some(d) = d {
                out.push((s.name.clone(), d));
            }
            walk(&s.steps, seen, out);
        }
    }
    let mut seen = 0;
    walk(steps, &mut seen, out);
}

/// Slow steps from step durations: the `limit` names with the largest 95th percentile.
pub fn slow_steps_of(durations: Vec<(String, i64)>, limit: usize) -> Vec<SlowStep> {
    let mut by_name: HashMap<String, Vec<i64>> = HashMap::new();
    for (n, d) in durations {
        by_name.entry(n).or_default().push(d);
    }
    let mut items: Vec<SlowStep> = by_name
        .into_iter()
        .map(|(name, mut ds)| {
            ds.sort_unstable();
            SlowStep {
                p50_ms: quantile_low(&ds, 0.5).unwrap_or(0),
                p95_ms: quantile_low(&ds, 0.95).unwrap_or(0),
                count: ds.len() as i64,
                name,
            }
        })
        .collect();
    items.sort_by(|a, b| b.p95_ms.cmp(&a.p95_ms).then_with(|| a.name.cmp(&b.name)));
    items.truncate(limit);
    items
}

/// The conditions of a window over launches `l` (`$1` project, `$2` from, `$3` to, `$4`
/// branch, `$5` environment; branch and environment compare with `""` for none).
const SCOPE: &str = "l.project_id = $1 and l.created_at >= $2 and l.created_at < $3 \
    and ($4::text is null or coalesce(l.branch, '') = $4) \
    and ($5::text is null or coalesce(l.env_key, '') = $5)";

fn key_sql(by: FailureGroupBy) -> &'static str {
    match by {
        FailureGroupBy::Fingerprint => "coalesce(ff.alias_of, r.fingerprint)",
        FailureGroupBy::DefectGroup => "coalesce(nullif(r.defect->>'group', ''), 'toInvestigate')",
        FailureGroupBy::Suite => "array_to_string(r.suite_path, ' / ')",
        FailureGroupBy::Env => "coalesce(l.env_key, '')",
    }
}

/// A PostgreSQL window from a query.
fn pg_window(q: &FailureQuery) -> AnalyticsWindow {
    window(q.from, q.to, Utc::now(), AnalyticsBackend::Postgres)
}

impl Store {
    /// The fingerprint and the fingerprints merged into it (an unknown one: itself only).
    pub async fn error_members(
        &self,
        project_id: Uuid,
        fingerprint: &str,
    ) -> anyhow::Result<Vec<String>> {
        let mut out: Vec<String> = sqlx::query_scalar(
            "select fingerprint from failure_fingerprints where project_id = $1 and alias_of = $2 \
             order by fingerprint limit 10000",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_all(&self.pool)
        .await?;
        out.insert(0, fingerprint.to_string());
        Ok(out)
    }

    /// Merged fingerprints of a project (`fingerprint` → the error it is merged into), at
    /// most `limit + 1` (the caller sees that there are more).
    pub async fn alias_map(
        &self,
        project_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "select fingerprint, alias_of from failure_fingerprints \
             where project_id = $1 and alias_of is not null order by fingerprint limit $2",
        )
        .bind(project_id)
        .bind(limit as i64 + 1)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Catalog messages of fingerprints (the labels of fingerprint groups), at most 200
    /// characters each.
    pub async fn fingerprint_labels(
        &self,
        project_id: Uuid,
        fingerprints: &[String],
    ) -> anyhow::Result<HashMap<String, String>> {
        if fingerprints.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "select fingerprint, left(message, 200) from failure_fingerprints \
             where project_id = $1 and fingerprint = any($2)",
        )
        .bind(project_id)
        .bind(fingerprints)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Existing defect type ids of a project among `ids`.
    pub async fn existing_defect_types(
        &self,
        project_id: Uuid,
        ids: &[Uuid],
    ) -> anyhow::Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(
            sqlx::query_scalar(
                "select id from defect_types where id = any($2) and project_id = $1",
            )
            .bind(project_id)
            .bind(ids)
            .fetch_all(&self.pool)
            .await?,
        )
    }

    /// `GET .../analytics/failures` on PostgreSQL.
    pub async fn analytics_failures_pg(
        &self,
        project_id: Uuid,
        q: &FailureQuery,
    ) -> anyhow::Result<FailureAnalytics> {
        let w = pg_window(q);
        let by = q.group_by;
        let key = key_sql(by);
        let only_fp = if by == FailureGroupBy::Fingerprint {
            " and r.fingerprint is not null and r.fingerprint <> ''"
        } else {
            ""
        };
        let base = format!(
            "from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             left join failure_fingerprints ff on ff.project_id = r.project_id and ff.fingerprint = r.fingerprint \
             where {SCOPE} and r.status in ('failed', 'broken')"
        );
        let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select count(*)::bigint {base}"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .fetch_one(&self.pool)
        .await?;
        let counts: Vec<(String, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select {key} as k, count(*)::bigint, count(distinct r.test_case_id)::bigint, \
               count(distinct r.launch_id)::bigint {base}{only_fp} \
             group by k order by 2 desc, ({key}) collate \"C\" limit $6"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .bind(q.limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        let keys: Vec<String> = counts.iter().map(|c| c.0.clone()).collect();
        let days: Vec<(String, NaiveDate, i64)> = if keys.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select {key} as k, (l.created_at at time zone 'UTC')::date as d, count(*)::bigint \
                 {base}{only_fp} and {key} = any($6) group by k, d"
            )))
            .bind(project_id)
            .bind(w.from)
            .bind(w.to)
            .bind(q.branch.as_deref())
            .bind(q.env.as_deref())
            .bind(&keys)
            .fetch_all(&self.pool)
            .await?
        };
        let labels = if by == FailureGroupBy::Fingerprint {
            self.fingerprint_labels(project_id, &keys).await?
        } else {
            HashMap::new()
        };
        let buckets = Buckets::new(w.from, w.to);
        Ok(FailureAnalytics {
            items: failure_groups(by, total, counts, &days, &buckets, &labels),
            window: w,
            group_by: by,
            total,
        })
    }

    /// `GET .../analytics/durations` on PostgreSQL: the last [`RECENT_DAYS`] days against the
    /// rest of the 30-day window (ClickHouse compares with the [`BEFORE_DAYS`] days before).
    pub async fn analytics_durations_pg(
        &self,
        project_id: Uuid,
        q: &FailureQuery,
    ) -> anyhow::Result<DurationRegressions> {
        let to = q.to.unwrap_or_else(Utc::now);
        let mut w = window(
            Some(to - Duration::days(RECENT_DAYS + BEFORE_DAYS)),
            Some(to),
            Utc::now(),
            AnalyticsBackend::Postgres,
        );
        if w.partial {
            w.reason = Some(format!(
                "PostgreSQL compares the last {RECENT_DAYS} days with the {} days before; \
                 ClickHouse uses {BEFORE_DAYS}",
                ANALYTICS_PG_DAYS - RECENT_DAYS
            ));
        }
        let mid = to - Duration::days(RECENT_DAYS);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select * from (select r.series_id, (array_agg(r.test_case_id))[1] as tc, \
               (array_agg(r.name order by r.launch_created_at desc))[1] as name, \
               percentile_disc(0.5) within group (order by r.duration_ms) filter (where r.launch_created_at >= $6) as p50r, \
               percentile_disc(0.5) within group (order by r.duration_ms) filter (where r.launch_created_at < $6) as p50b, \
               count(*) filter (where r.launch_created_at >= $6) as rr, \
               count(*) filter (where r.launch_created_at < $6) as rb, count(*) as runs \
             from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             where {SCOPE} and r.status in ('passed', 'failed', 'broken') and r.duration_ms is not null \
             group by r.series_id) s \
             where rr >= $7 and rb >= $7 and p50b > 0 and p50r * 10 >= p50b * 13 \
             order by p50r::float8 / p50b desc, series_id limit $8"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .bind(mid)
        .bind(MIN_RUNS)
        .bind(q.limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        let mut items = Vec::with_capacity(rows.len());
        for r in rows {
            let p50r: i64 = r.try_get("p50r")?;
            let p50b: i64 = r.try_get("p50b")?;
            items.push(DurationRegression {
                series_id: r.try_get("series_id")?,
                test_case_id: r.try_get("tc")?,
                name: r.try_get("name")?,
                p50_recent_ms: p50r,
                p50_before_ms: p50b,
                ratio: ((p50r as f64 / p50b as f64) * 1000.0).round() / 1000.0,
                runs: r.try_get("runs")?,
            });
        }
        Ok(DurationRegressions { window: w, items })
    }

    /// `GET .../analytics/retries` on PostgreSQL.
    pub async fn analytics_retries_pg(
        &self,
        project_id: Uuid,
        q: &FailureQuery,
    ) -> anyhow::Result<RetryCosts> {
        let w = pg_window(q);
        let base = format!(
            "with a as (select r.id, array_to_string(r.suite_path, ' / ') as suite, r.test_case_id, \
               (select sum(coalesce(x.duration_ms, 0)) - (array_agg(coalesce(x.duration_ms, 0) order by x.seq desc))[1] \
                  from attempts x where x.result_id = r.id and x.launch_created_at = r.launch_created_at)::bigint as wasted, \
               (select count(*) - 1 from attempts x where x.result_id = r.id \
                  and x.launch_created_at = r.launch_created_at)::bigint as retries \
             from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             where {SCOPE} and r.attempt_count > 1) "
        );
        let total: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "{base} select sum(wasted)::bigint from a where retries > 0"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .fetch_one(&self.pool)
        .await?;
        let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{base} select suite, sum(wasted)::bigint, sum(retries)::bigint, count(distinct test_case_id)::bigint \
             from a where retries > 0 group by suite order by 2 desc, suite collate \"C\" limit $6"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .bind(q.limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        Ok(RetryCosts {
            window: w,
            total_wasted_ms: total.unwrap_or(0),
            items: rows
                .into_iter()
                .map(|(suite, wasted_ms, retries, tests)| RetryCost {
                    suite,
                    wasted_ms,
                    retries,
                    tests,
                })
                .collect(),
        })
    }

    /// `GET .../analytics/flaky-trend` on PostgreSQL.
    pub async fn analytics_flaky_trend_pg(
        &self,
        project_id: Uuid,
        q: &FailureQuery,
    ) -> anyhow::Result<FlakyTrend> {
        let w = pg_window(q);
        let rows: Vec<(NaiveDate, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select (l.created_at at time zone 'UTC')::date as d, \
               count(*) filter (where r.flaky_in_run)::bigint, \
               count(*) filter (where r.status not in ('skipped', 'running'))::bigint \
             from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             where {SCOPE} group by d"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .fetch_all(&self.pool)
        .await?;
        let by_day: HashMap<NaiveDate, (i64, i64)> =
            rows.into_iter().map(|(d, f, r)| (d, (f, r))).collect();
        let mut items = Vec::new();
        let mut d = w.from.date_naive();
        let last = (w.to - Duration::milliseconds(1)).date_naive();
        while d <= last {
            let (flaky, runs) = by_day.get(&d).copied().unwrap_or((0, 0));
            items.push(FlakyTrendPoint {
                day: d,
                flaky,
                runs,
            });
            d += Duration::days(1);
        }
        Ok(FlakyTrend { window: w, items })
    }

    /// `GET .../analytics/slow-steps` on PostgreSQL: the steps of the newest
    /// [`PG_STEP_ATTEMPTS`] attempts of the window (`partial` when there are more).
    pub async fn analytics_slow_steps_pg(
        &self,
        project_id: Uuid,
        q: &FailureQuery,
    ) -> anyhow::Result<SlowSteps> {
        let mut w = pg_window(q);
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select a.steps_z from launches l join attempts a on a.launch_id = l.id \
               and a.launch_created_at = l.created_at \
             where {SCOPE} and a.steps_z is not null order by l.created_at desc, a.id limit $6"
        )))
        .bind(project_id)
        .bind(w.from)
        .bind(w.to)
        .bind(q.branch.as_deref())
        .bind(q.env.as_deref())
        .bind(PG_STEP_ATTEMPTS + 1)
        .fetch_all(&self.pool)
        .await?;
        if rows.len() as i64 > PG_STEP_ATTEMPTS {
            w.partial = true;
            w.reason = Some(format!(
                "PostgreSQL reads the steps of the newest {PG_STEP_ATTEMPTS} attempts; all of them need ClickHouse"
            ));
        }
        let mut durations = Vec::new();
        for z in rows.iter().take(PG_STEP_ATTEMPTS as usize) {
            let steps: Vec<Step> = codec::decode(z).unwrap_or_default();
            step_durations(&steps, &mut durations);
        }
        Ok(SlowSteps {
            window: w,
            items: slow_steps_of(durations, q.limit.clamp(1, 1000) as usize),
        })
    }

    /// Seconds each error of the project took to resolve, for errors resolved in
    /// `[from, to)` (from the error catalog, the same on both backends; merged errors count
    /// in their target).
    pub async fn resolved_seconds(
        &self,
        project_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<i64>> {
        Ok(sqlx::query_scalar(
            "select greatest(0, extract(epoch from (resolved_at - first_seen_at)))::bigint \
             from failure_fingerprints where project_id = $1 and alias_of is null and state = 'resolved' \
               and resolved_at >= $2 and resolved_at < $3 limit 1000000",
        )
        .bind(project_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?)
    }

    /// `GET .../analytics/mttr` (the window as the caller resolved it).
    pub async fn analytics_mttr(
        &self,
        project_id: Uuid,
        window: AnalyticsWindow,
    ) -> anyhow::Result<MttrStats> {
        let s = self
            .resolved_seconds(project_id, window.from, window.to)
            .await?;
        Ok(mttr_stats(window, s))
    }

    /// Per project: errors first seen in `[from, to)`, errors that regressed in it, and the
    /// median seconds to resolve of errors resolved in it.
    pub async fn portfolio_errors(
        &self,
        project_ids: &[Uuid],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<HashMap<Uuid, (i64, i64, Option<i64>)>> {
        if project_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "select project_id, \
               count(*) filter (where first_seen_at >= $2 and first_seen_at < $3)::bigint as new_errors, \
               count(*) filter (where state = 'regressed' and state_at >= $2 and state_at < $3)::bigint as regressed, \
               array_remove(array_agg(case when state = 'resolved' and resolved_at >= $2 and resolved_at < $3 \
                 then greatest(0, extract(epoch from (resolved_at - first_seen_at)))::bigint end), null) as resolved \
             from failure_fingerprints where project_id = any($1) and alias_of is null group by project_id",
        )
        .bind(project_ids)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        let mut out = HashMap::new();
        for r in rows {
            let mut s: Vec<i64> = r.try_get("resolved")?;
            s.sort_unstable();
            out.insert(
                r.try_get("project_id")?,
                (
                    r.try_get("new_errors")?,
                    r.try_get("regressed")?,
                    quantile_low(&s, 0.5),
                ),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap()
    }

    #[test]
    fn windows_default_and_cap() {
        let now = at(31, 0);
        let w = window(None, None, now, AnalyticsBackend::Postgres);
        assert_eq!((w.from, w.to, w.partial), (at(1, 0), now, false));
        let w = window(
            Some(at(1, 0) - Duration::days(60)),
            None,
            now,
            AnalyticsBackend::Postgres,
        );
        assert_eq!((w.from, w.partial), (at(1, 0), true));
        assert_eq!(w.reason.as_deref(), Some(PG_REASON));
        let w = window(
            Some(at(1, 0) - Duration::days(60)),
            None,
            now,
            AnalyticsBackend::Clickhouse,
        );
        assert!(!w.partial);
        let w = window(
            Some(now - Duration::days(400)),
            None,
            now,
            AnalyticsBackend::Clickhouse,
        );
        assert_eq!((w.from, w.partial), (now - Duration::days(365), true));
    }

    #[test]
    fn trend_buckets_cover_whole_days() {
        let b = Buckets::new(at(1, 12), at(31, 0));
        assert_eq!((b.days, b.count), (3, 10));
        assert_eq!(b.index(at(1, 0).date_naive()), Some(0));
        assert_eq!(b.index(at(4, 0).date_naive()), Some(1));
        assert_eq!(b.index(at(30, 0).date_naive()), Some(9));
        assert_eq!(b.index(at(31, 0).date_naive()), None);
        let b = Buckets::new(at(1, 0), at(8, 0));
        assert_eq!((b.days, b.count), (1, 7));
        let b = Buckets::new(at(5, 1), at(5, 2));
        assert_eq!((b.days, b.count), (1, 1));
        let t = b.trends(&[
            ("k".into(), at(5, 0).date_naive(), 3),
            ("k".into(), at(9, 0).date_naive(), 1),
        ]);
        assert_eq!(t["k"], vec![3]);
    }

    #[test]
    fn quantiles_match_clickhouse() {
        let v: Vec<i64> = (1..=40).collect();
        assert_eq!(quantile_low(&v, 0.5), Some(20));
        assert_eq!(quantile_low(&v, 0.95), Some(39));
        let v: Vec<i64> = (1..=21).collect();
        assert_eq!(
            (quantile_low(&v, 0.5), quantile_low(&v, 0.95)),
            (Some(11), Some(20))
        );
        assert_eq!(quantile_low(&[7], 0.9), Some(7));
        assert_eq!(quantile_low(&[], 0.5), None);
        let m = mttr_stats(
            window(None, None, at(31, 0), AnalyticsBackend::Postgres),
            vec![100, 7200, 90_000],
        );
        assert_eq!(
            (m.resolved, m.p50_seconds, m.p90_seconds),
            (3, Some(7200), Some(90_000))
        );
        assert_eq!(m.buckets[0].count, 1);
        assert_eq!(m.buckets[2].count, 2);
        assert_eq!(m.buckets[6].count, 3);
    }

    #[test]
    fn steps_flatten_like_the_sinks() {
        let leaf = Step {
            name: "click".into(),
            duration_ms: Some(5),
            ..Default::default()
        };
        let root = Step {
            name: "open".into(),
            start: Some(10),
            stop: Some(30),
            steps: vec![
                leaf.clone(),
                Step {
                    name: "wait".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut out = Vec::new();
        step_durations(&[root], &mut out);
        assert_eq!(out, vec![("open".into(), 20), ("click".into(), 5)]);
        let s = slow_steps_of(vec![("a".into(), 1), ("b".into(), 9), ("a".into(), 3)], 1);
        assert_eq!((s[0].name.as_str(), s[0].p95_ms, s.len()), ("b", 9, 1));
        assert_eq!(
            group_label(FailureGroupBy::DefectGroup, "productBug"),
            "Product bug"
        );
        assert_eq!(group_label(FailureGroupBy::Suite, ""), "(no suite)");
    }
}
