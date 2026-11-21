//! The project dashboard: pass rate, durations, flaky and most failing tests, defect groups
//! and launch counts per day or week. Rolled-up launches come from `series_daily` and
//! `launch_daily`, the others from raw results, so any window can be shown.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::flaky::{FlakyFilter, FlakyItem};
use super::rollup::{DURATION_BUCKETS, histogram_quantile};
use super::settings::ProjectSettings;
use crate::{Store, no_nul};

/// Tests listed in the top lists.
pub const TOP: i64 = 10;

/// Bucket size of the series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    Day,
    Week,
}

impl Bucket {
    fn sql(self) -> &'static str {
        match self {
            Bucket::Day => "day",
            Bucket::Week => "week",
        }
    }
}

/// Filters of the dashboard; `from` and `to` are inclusive UTC days.
#[derive(Clone, Debug)]
pub struct DashboardQuery {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub bucket: Bucket,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PassRatePoint {
    pub t: NaiveDate,
    /// passed / (total − skipped); `null` when nothing but skipped results ran.
    pub pass_rate: Option<f64>,
    pub total: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DurationPoint {
    pub t: NaiveDate,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailingTest {
    pub test_case_id: Uuid,
    pub name: String,
    pub failures: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DefectGroupsPoint {
    pub t: NaiveDate,
    pub product_bug: i64,
    pub automation_bug: i64,
    pub system_issue: i64,
    pub no_defect: i64,
    pub to_investigate: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LaunchCountPoint {
    pub t: NaiveDate,
    pub count: i64,
}

/// The dashboard.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub pass_rate: Vec<PassRatePoint>,
    pub durations: Vec<DurationPoint>,
    pub flaky_top: Vec<FlakyItem>,
    pub most_failing: Vec<FailingTest>,
    pub defect_groups: Vec<DefectGroupsPoint>,
    pub launches: Vec<LaunchCountPoint>,
}

#[derive(Default)]
struct Acc {
    total: i64,
    passed: i64,
    skipped: i64,
    defects: [i64; 5],
    hist_agg: Option<Vec<i64>>,
    hist_raw: Vec<i64>,
    raw_p50: Option<f64>,
    raw_p95: Option<f64>,
    launches: i64,
}

fn midnight(d: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
}

type AggRow = (NaiveDate, i64, i64, i64, i64, i64, i64, i64, i64);
type RawRow = (
    NaiveDate,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<f64>,
    Option<f64>,
);

const RAW_FROM: &str = "from launches l join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
    where l.project_id = $2 and not l.debug and l.rolled_up_at is null and l.created_at >= $3 and l.created_at < $4 \
      and ($5::text is null or l.branch = $5) and ($6::text is null or l.env_key = $6)";
const AGG_WHERE: &str = "where sd.project_id = $2 and sd.day >= $3 and sd.day <= $4 \
      and ($5::text is null or sd.branch = $5) and ($6::text is null or sd.env_key = $6)";
const BUCKET_EXPR: &str = "case when r.duration_ms < 10 then 0 \
    else least(23, floor(log(2, r.duration_ms / 10.0))::int + 1) end";

impl Store {
    pub async fn dashboard(
        &self,
        project_id: Uuid,
        settings: &ProjectSettings,
        q: &DashboardQuery,
    ) -> anyhow::Result<Dashboard> {
        let b = q.bucket.sql();
        let branch = q.branch.as_deref().map(no_nul);
        let env = q.environment.as_deref().map(no_nul);
        let (from_ts, to_ts) = (midnight(q.from), midnight(q.to + chrono::Duration::days(1)));
        let mut acc: BTreeMap<NaiveDate, Acc> = BTreeMap::new();

        let agg: Vec<AggRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select date_trunc($1, sd.day::timestamp)::date, sum(sd.runs)::bigint, sum(sd.passed)::bigint, sum(sd.skipped)::bigint, \
               sum(sd.product_bug)::bigint, sum(sd.automation_bug)::bigint, sum(sd.system_issue)::bigint, \
               sum(sd.no_defect)::bigint, sum(sd.to_investigate)::bigint \
             from series_daily sd {AGG_WHERE} group by 1"
        )))
        .bind(b)
        .bind(project_id)
        .bind(q.from)
        .bind(q.to)
        .bind(&branch)
        .bind(&env)
        .fetch_all(&self.pool)
        .await?;
        for (t, total, passed, skipped, pb, ab, si, nd, ti) in agg {
            let a = acc.entry(t).or_default();
            a.total += total;
            a.passed += passed;
            a.skipped += skipped;
            for (i, v) in [pb, ab, si, nd, ti].into_iter().enumerate() {
                a.defects[i] += v;
            }
        }
        let hist: Vec<(NaiveDate, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select date_trunc($1, sd.day::timestamp)::date, b.i, sum(b.c)::bigint from series_daily sd \
             cross join lateral unnest(sd.duration_buckets) with ordinality as b(c, i) {AGG_WHERE} group by 1, 2"
        )))
        .bind(b)
        .bind(project_id)
        .bind(q.from)
        .bind(q.to)
        .bind(&branch)
        .bind(&env)
        .fetch_all(&self.pool)
        .await?;
        for (t, i, c) in hist {
            let a = acc.entry(t).or_default();
            let h = a.hist_agg.get_or_insert_with(|| vec![0; DURATION_BUCKETS]);
            if (1..=DURATION_BUCKETS as i64).contains(&i) {
                h[i as usize - 1] += c;
            }
        }
        let launches: Vec<(NaiveDate, i64)> = sqlx::query_as(
            "select date_trunc($1, day::timestamp)::date, sum(launches)::bigint from launch_daily \
             where project_id = $2 and day >= $3 and day <= $4 and ($5::text is null or branch = $5) \
               and ($6::text is null or env_key = $6) group by 1",
        )
        .bind(b)
        .bind(project_id)
        .bind(q.from)
        .bind(q.to)
        .bind(&branch)
        .bind(&env)
        .fetch_all(&self.pool)
        .await?;
        for (t, n) in launches {
            acc.entry(t).or_default().launches += n;
        }

        let raw: Vec<RawRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select date_trunc($1, l.created_at at time zone 'UTC')::date, count(*), \
               count(*) filter (where r.status = 'passed'), count(*) filter (where r.status = 'skipped'), \
               count(*) filter (where r.defect->>'group' = 'productBug'), \
               count(*) filter (where r.defect->>'group' = 'automationBug'), \
               count(*) filter (where r.defect->>'group' = 'systemIssue'), \
               count(*) filter (where r.defect->>'group' = 'noDefect'), \
               count(*) filter (where r.defect->>'group' = 'toInvestigate'), \
               percentile_cont(0.5) within group (order by r.duration_ms), \
               percentile_cont(0.95) within group (order by r.duration_ms) \
             {RAW_FROM} group by 1"
        )))
        .bind(b)
        .bind(project_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(&branch)
        .bind(&env)
        .fetch_all(&self.pool)
        .await?;
        for (t, total, passed, skipped, pb, ab, si, nd, ti, p50, p95) in raw {
            let a = acc.entry(t).or_default();
            a.total += total;
            a.passed += passed;
            a.skipped += skipped;
            for (i, v) in [pb, ab, si, nd, ti].into_iter().enumerate() {
                a.defects[i] += v;
            }
            a.raw_p50 = p50;
            a.raw_p95 = p95;
        }
        if acc
            .values()
            .any(|a| a.hist_agg.is_some() && a.raw_p50.is_some())
        {
            let rh: Vec<(NaiveDate, i32, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select date_trunc($1, l.created_at at time zone 'UTC')::date, {BUCKET_EXPR}, count(*) \
                 {RAW_FROM} and r.duration_ms >= 0 group by 1, 2"
            )))
            .bind(b)
            .bind(project_id)
            .bind(from_ts)
            .bind(to_ts)
            .bind(&branch)
            .bind(&env)
            .fetch_all(&self.pool)
            .await?;
            for (t, i, c) in rh {
                let a = acc.entry(t).or_default();
                if a.hist_raw.is_empty() {
                    a.hist_raw = vec![0; DURATION_BUCKETS];
                }
                a.hist_raw[(i.max(0) as usize).min(DURATION_BUCKETS - 1)] += c;
            }
        }
        let raw_launches: Vec<(NaiveDate, i64)> = sqlx::query_as(
            "select date_trunc($1, l.created_at at time zone 'UTC')::date, count(*) from launches l \
             where l.project_id = $2 and not l.debug and l.rolled_up_at is null and l.created_at >= $3 \
               and l.created_at < $4 and ($5::text is null or l.branch = $5) \
               and ($6::text is null or l.env_key = $6) group by 1",
        )
        .bind(b)
        .bind(project_id)
        .bind(from_ts)
        .bind(to_ts)
        .bind(&branch)
        .bind(&env)
        .fetch_all(&self.pool)
        .await?;
        for (t, n) in raw_launches {
            acc.entry(t).or_default().launches += n;
        }

        let failing: Vec<(Uuid, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select tc, sum(f)::bigint from ( \
               select s.test_case_id as tc, sum(sd.failed + sd.broken)::bigint as f from series_daily sd \
                 join series s on s.id = sd.series_id \
                 where sd.project_id = $1 and sd.day >= $2 and sd.day <= $3 \
                   and ($4::text is null or sd.branch = $4) and ($5::text is null or sd.env_key = $5) group by 1 \
               union all \
               select r.test_case_id, count(*) from launches l \
                 join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
                 where l.project_id = $1 and not l.debug and l.rolled_up_at is null and l.created_at >= $6 \
                   and l.created_at < $7 and ($4::text is null or l.branch = $4) \
                   and ($5::text is null or l.env_key = $5) and r.status in ('failed', 'broken') group by 1 \
             ) x where f > 0 group by tc order by 2 desc, 1 limit {TOP}"
        )))
        .bind(project_id)
        .bind(q.from)
        .bind(q.to)
        .bind(&branch)
        .bind(&env)
        .bind(from_ts)
        .bind(to_ts)
        .fetch_all(&self.pool)
        .await?;
        let names: HashMap<Uuid, String> = if failing.is_empty() {
            HashMap::new()
        } else {
            let ids: Vec<Uuid> = failing.iter().map(|f| f.0).collect();
            sqlx::query_as::<_, (Uuid, String)>(
                "select id, display_name from test_cases where id = any($1)",
            )
            .bind(&ids)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .collect()
        };
        let most_failing = failing
            .into_iter()
            .map(|(id, n)| FailingTest {
                test_case_id: id,
                name: names.get(&id).cloned().unwrap_or_default(),
                failures: n,
            })
            .collect();
        let flaky_top = self
            .flaky_tests(
                project_id,
                settings,
                &FlakyFilter {
                    branch: q.branch.clone(),
                    environment: q.environment.clone(),
                    limit: TOP,
                },
            )
            .await?;

        let mut d = Dashboard {
            pass_rate: Vec::new(),
            durations: Vec::new(),
            flaky_top,
            most_failing,
            defect_groups: Vec::new(),
            launches: Vec::new(),
        };
        for (t, a) in acc {
            if a.total > 0 {
                let denom = a.total - a.skipped;
                d.pass_rate.push(PassRatePoint {
                    t,
                    pass_rate: (denom > 0)
                        .then(|| ((a.passed as f64 / denom as f64) * 10_000.0).round() / 10_000.0),
                    total: a.total,
                });
                d.defect_groups.push(DefectGroupsPoint {
                    t,
                    product_bug: a.defects[0],
                    automation_bug: a.defects[1],
                    system_issue: a.defects[2],
                    no_defect: a.defects[3],
                    to_investigate: a.defects[4],
                });
            }
            let (p50, p95) = match &a.hist_agg {
                Some(h) => {
                    let mut h = h.clone();
                    for (i, c) in a.hist_raw.iter().enumerate() {
                        h[i] += c;
                    }
                    (histogram_quantile(&h, 0.5), histogram_quantile(&h, 0.95))
                }
                None => (
                    a.raw_p50.map(|v| v.round() as i64),
                    a.raw_p95.map(|v| v.round() as i64),
                ),
            };
            if p50.is_some() || p95.is_some() {
                d.durations.push(DurationPoint {
                    t,
                    p50_ms: p50,
                    p95_ms: p95,
                });
            }
            if a.launches > 0 {
                d.launches.push(LaunchCountPoint {
                    t,
                    count: a.launches,
                });
            }
        }
        Ok(d)
    }
}
