//! History rollup and retention.
//!
//! A launch is rolled up once: its results are added to `series_daily` (per series, day,
//! branch and environment) and `launch_daily`, and `launches.rolled_up_at` is set in the same
//! transaction. Dashboards read the aggregates of rolled-up launches and the raw results of the
//! others, so nothing is counted twice. Retention deletes launches beyond a project's window
//! (rolling them up first, keeping important ones), and queues the blobs of attachments no
//! launch references any more in `blob_deletions`. Monthly partitions older than the longest
//! raw window of all projects are dropped once every launch in them is rolled up and none is
//! kept as important; the default partition is never dropped.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use fastproof_model::Status;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::settings::ProjectSettings;
use crate::Store;
use crate::purge::{Mode, Purge};

/// Buckets of the duration histograms in `series_daily.duration_buckets`: bucket 0 holds
/// durations under 10 ms, bucket `i` holds `[10·2^(i-1), 10·2^i)` ms, the last one the rest.
pub const DURATION_BUCKETS: usize = 24;

/// Grace period before an unreferenced attachment is deleted (an upload may be linking it).
pub const ATTACHMENT_GRACE: Duration = Duration::from_secs(24 * 3600);

/// Histogram bucket of a duration.
pub fn duration_bucket(ms: i64) -> usize {
    if ms < 10 {
        return 0;
    }
    let mut i = 1;
    let mut upper = 20i64;
    while ms >= upper && i < DURATION_BUCKETS - 1 {
        i += 1;
        upper = upper.saturating_mul(2);
    }
    i
}

fn bucket_bounds(i: usize) -> (f64, f64) {
    if i == 0 {
        return (0.0, 10.0);
    }
    let lo = 10.0 * 2f64.powi(i as i32 - 1);
    (lo, lo * 2.0)
}

/// Quantile `q` (0..1) of a histogram, interpolated inside its bucket.
pub fn histogram_quantile(h: &[i64], q: f64) -> Option<i64> {
    let total: i64 = h.iter().sum();
    if total <= 0 {
        return None;
    }
    let rank = q * total as f64;
    let mut cum = 0f64;
    for (i, &c) in h.iter().enumerate() {
        if c <= 0 {
            continue;
        }
        if cum + c as f64 >= rank {
            let (lo, hi) = bucket_bounds(i);
            let frac = ((rank - cum) / c as f64).clamp(0.0, 1.0);
            return Some((lo + frac * (hi - lo)).round() as i64);
        }
        cum += c as f64;
    }
    let last = h.iter().rposition(|&c| c > 0)?;
    Some(bucket_bounds(last).1 as i64)
}

/// Aggregates of one series in one launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeriesAgg {
    pub runs: i32,
    pub passed: i32,
    pub failed: i32,
    pub broken: i32,
    pub skipped: i32,
    pub unknown: i32,
    pub flaky: i32,
    pub duration_sum: i64,
    pub duration_min: Option<i64>,
    pub duration_max: Option<i64>,
    pub buckets: [i32; DURATION_BUCKETS],
    /// productBug, automationBug, systemIssue, noDefect, toInvestigate.
    pub defects: [i32; 5],
}

impl SeriesAgg {
    pub fn add(&mut self, status: Status, flaky: bool, duration: Option<i64>, group: Option<&str>) {
        self.runs += 1;
        match status {
            Status::Passed => self.passed += 1,
            Status::Failed => self.failed += 1,
            Status::Broken => self.broken += 1,
            Status::Skipped => self.skipped += 1,
            Status::Unknown => self.unknown += 1,
        }
        if flaky {
            self.flaky += 1;
        }
        if let Some(d) = duration.filter(|d| *d >= 0) {
            self.duration_sum += d;
            self.duration_min = Some(self.duration_min.map_or(d, |m| m.min(d)));
            self.duration_max = Some(self.duration_max.map_or(d, |m| m.max(d)));
            self.buckets[duration_bucket(d)] += 1;
        }
        if let Some(i) = group.and_then(|g| super::defects::GROUPS.iter().position(|x| *x == g)) {
            self.defects[i] += 1;
        }
    }
}

fn int_array(v: &[i32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("{{{}}}", parts.join(","))
}

/// What a rollup run did.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RollupReport {
    pub launches: u64,
    pub partitions_dropped: Vec<String>,
}

/// What a retention run did.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetentionReport {
    pub launches_deleted: u64,
    pub blobs_queued: u64,
    /// Test plans older than [`super::plans::TEST_PLAN_DAYS`] that were deleted.
    #[serde(default)]
    pub test_plans_deleted: u64,
}

/// Launches a retention run would delete.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPreview {
    pub launches: i64,
    pub results: i64,
    /// Important launches kept although beyond the window.
    pub important_kept: i64,
    pub oldest: Option<DateTime<Utc>>,
    /// The first launches that would go (oldest first, at most 50).
    pub launch_ids: Vec<Uuid>,
}

fn month_of(name: &str, parent: &str) -> Option<NaiveDate> {
    let rest = name.strip_prefix(parent)?.strip_prefix("_p")?;
    if rest.len() != 6 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::from_ymd_opt(rest[..4].parse().ok()?, rest[4..].parse().ok()?, 1)
}

fn next_month(d: NaiveDate) -> NaiveDate {
    if d.month() == 12 {
        NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).unwrap_or(d)
    } else {
        NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1).unwrap_or(d)
    }
}

fn at_midnight(d: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
}

type AggInputRow = (Uuid, String, bool, Option<i64>, Option<String>);
type LaunchRow = (
    Uuid,
    DateTime<Utc>,
    Option<String>,
    Option<String>,
    bool,
    Option<DateTime<Utc>>,
);

impl Store {
    /// Adds a launch to the daily aggregates (once). Returns false when it was rolled up
    /// before or does not exist. Debug launches are marked without aggregating.
    pub async fn rollup_launch(&self, launch_id: Uuid) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let l: Option<LaunchRow> = sqlx::query_as(
            "select project_id, created_at, branch, env_key, debug, rolled_up_at from launches \
             where id = $1 for update",
        )
        .bind(launch_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((project_id, created, branch, env, debug, rolled)) = l else {
            return Ok(false);
        };
        if rolled.is_some() {
            return Ok(false);
        }
        if !debug {
            let day = created.date_naive();
            let rows: Vec<AggInputRow> = sqlx::query_as(
                "select series_id, status, flaky_in_run or flaky, duration_ms, defect->>'group' from results \
                 where launch_id = $1 and launch_created_at = $2",
            )
            .bind(launch_id)
            .bind(created)
            .fetch_all(&mut *tx)
            .await?;
            let mut by: BTreeMap<Uuid, SeriesAgg> = BTreeMap::new();
            for (s, st, fl, d, g) in &rows {
                by.entry(*s)
                    .or_default()
                    .add(Status::parse_lenient(st), *fl, *d, g.as_deref());
            }
            if !by.is_empty() {
                let ids: Vec<Uuid> = by.keys().copied().collect();
                let col =
                    |f: &dyn Fn(&SeriesAgg) -> i32| -> Vec<i32> { by.values().map(f).collect() };
                let sums: Vec<i64> = by.values().map(|a| a.duration_sum).collect();
                let mins: Vec<Option<i64>> = by.values().map(|a| a.duration_min).collect();
                let maxs: Vec<Option<i64>> = by.values().map(|a| a.duration_max).collect();
                let buckets: Vec<String> = by.values().map(|a| int_array(&a.buckets)).collect();
                sqlx::query(
                    "insert into series_daily (project_id, series_id, day, branch, env_key, runs, passed, failed, \
                       broken, skipped, unknown, flaky, duration_sum_ms, duration_min_ms, duration_max_ms, \
                       duration_buckets, product_bug, automation_bug, system_issue, no_defect, to_investigate, updated_at) \
                     select $1, s, $2, $3, $4, ru, pa, fa, br, sk, un, fl, ds, dn, dx, bk::int[], pb, ab, si, nd, ti, now() \
                     from unnest($5::uuid[], $6::int[], $7::int[], $8::int[], $9::int[], $10::int[], $11::int[], \
                       $12::int[], $13::bigint[], $14::bigint[], $15::bigint[], $16::text[], $17::int[], $18::int[], \
                       $19::int[], $20::int[], $21::int[]) \
                       as t(s, ru, pa, fa, br, sk, un, fl, ds, dn, dx, bk, pb, ab, si, nd, ti) \
                     on conflict (project_id, series_id, day, branch, env_key) do update set \
                       runs = series_daily.runs + excluded.runs, passed = series_daily.passed + excluded.passed, \
                       failed = series_daily.failed + excluded.failed, broken = series_daily.broken + excluded.broken, \
                       skipped = series_daily.skipped + excluded.skipped, unknown = series_daily.unknown + excluded.unknown, \
                       flaky = series_daily.flaky + excluded.flaky, \
                       duration_sum_ms = series_daily.duration_sum_ms + excluded.duration_sum_ms, \
                       duration_min_ms = least(series_daily.duration_min_ms, excluded.duration_min_ms), \
                       duration_max_ms = greatest(series_daily.duration_max_ms, excluded.duration_max_ms), \
                       duration_buckets = fastproof_int_array_add(series_daily.duration_buckets, excluded.duration_buckets), \
                       product_bug = series_daily.product_bug + excluded.product_bug, \
                       automation_bug = series_daily.automation_bug + excluded.automation_bug, \
                       system_issue = series_daily.system_issue + excluded.system_issue, \
                       no_defect = series_daily.no_defect + excluded.no_defect, \
                       to_investigate = series_daily.to_investigate + excluded.to_investigate, updated_at = now()",
                )
                .bind(project_id)
                .bind(day)
                .bind(branch.clone().unwrap_or_default())
                .bind(env.clone().unwrap_or_default())
                .bind(&ids)
                .bind(col(&|a| a.runs))
                .bind(col(&|a| a.passed))
                .bind(col(&|a| a.failed))
                .bind(col(&|a| a.broken))
                .bind(col(&|a| a.skipped))
                .bind(col(&|a| a.unknown))
                .bind(col(&|a| a.flaky))
                .bind(&sums)
                .bind(&mins)
                .bind(&maxs)
                .bind(&buckets)
                .bind(col(&|a| a.defects[0]))
                .bind(col(&|a| a.defects[1]))
                .bind(col(&|a| a.defects[2]))
                .bind(col(&|a| a.defects[3]))
                .bind(col(&|a| a.defects[4]))
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "insert into launch_daily (project_id, day, branch, env_key, launches) values ($1, $2, $3, $4, 1) \
                 on conflict (project_id, day, branch, env_key) do update set launches = launch_daily.launches + 1",
            )
            .bind(project_id)
            .bind(day)
            .bind(branch.unwrap_or_default())
            .bind(env.unwrap_or_default())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("update launches set rolled_up_at = now() where id = $1")
            .bind(launch_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Rolls up launches older than each project's `rollup.afterDays`, then drops raw
    /// partitions that are no longer needed. Stops starting new work after `budget`.
    pub async fn rollup_due(&self, budget: Duration) -> anyhow::Result<RollupReport> {
        let started = Instant::now();
        let mut report = RollupReport::default();
        let projects = self.all_project_settings().await?;
        'projects: for (id, _, s) in &projects {
            loop {
                let due: Vec<Uuid> = sqlx::query_scalar(
                    "select id from launches where project_id = $1 and rolled_up_at is null \
                     and manual_state is distinct from 'open' \
                     and created_at < now() - make_interval(days => $2) order by created_at limit 200",
                )
                .bind(id)
                .bind(s.rollup.after_days as i32)
                .fetch_all(&self.pool)
                .await?;
                for l in &due {
                    if started.elapsed() > budget {
                        break 'projects;
                    }
                    if self.rollup_launch(*l).await? {
                        report.launches += 1;
                    }
                }
                if due.len() < 200 {
                    break;
                }
            }
        }
        if started.elapsed() <= budget {
            report.partitions_dropped = self.drop_rolled_partitions(&projects).await?;
        }
        Ok(report)
    }

    /// Drops monthly partitions (results and attempts) that end before the longest raw window
    /// of all projects, when every launch in them is rolled up, none is kept as important and
    /// no manual run in them is open. Remaining launch rows of such a month are deleted with
    /// them, except launches with a manual run: those stay (with their run items, marked
    /// rolled up) and their run items lose the `result_id` of the dropped results. The rows
    /// under the deleted launches (uploads, containers, links, log chunks, ...) go first in
    /// bounded steps, and the launch rows in chunks, so no statement cascades through a month.
    pub async fn drop_rolled_partitions(
        &self,
        projects: &[(Uuid, String, ProjectSettings)],
    ) -> anyhow::Result<Vec<String>> {
        let max_raw = projects
            .iter()
            .map(|p| p.2.retention.raw_days)
            .max()
            .unwrap_or(super::settings::RetentionSettings::default().raw_days);
        let cutoff = Utc::now().date_naive() - chrono::Duration::days(max_raw as i64);
        let mut dropped = Vec::new();
        for name in self.partitions("results").await? {
            let Some(start) = month_of(&name, "results") else {
                continue;
            };
            let end = next_month(start);
            if end > cutoff {
                continue;
            }
            let blockers: i64 = sqlx::query_scalar(
                "select count(*) from launches l join projects p on p.id = l.project_id \
                 where l.created_at >= $1 and l.created_at < $2 and p.deleted_at is null and \
                   ((l.rolled_up_at is null and not l.debug) or l.manual_state = 'open' or \
                    (l.important and coalesce((p.settings->'retention'->>'keepImportant')::boolean, true)))",
            )
            .bind(at_midnight(start))
            .bind(at_midnight(end))
            .fetch_one(&self.pool)
            .await?;
            if blockers > 0 {
                continue;
            }
            let gone: Vec<Uuid> = sqlx::query_scalar(
                "select id from launches where created_at >= $1 and created_at < $2 \
                 and manual_state is null order by id",
            )
            .bind(at_midnight(start))
            .bind(at_midnight(end))
            .fetch_all(&self.pool)
            .await?;
            let n = self.purge_batch;
            let emptied = async {
                let mut conn = self.pool.acquire().await?;
                let mut p = Purge::new(&mut conn, n, Mode::Steps);
                for chunk in gone.chunks(n as usize) {
                    p.launch_contents(chunk, false).await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(e) = emptied {
                tracing::warn!(partition = %name, error = %e, "emptying the launches of a rolled-up partition failed; retried on the next run");
                continue;
            }
            let suffix = &name["results".len()..];
            let attempts = format!("attempts{suffix}");
            let mut tx = self.pool.begin().await?;
            sqlx::query("set local lock_timeout = '5s'")
                .execute(&mut *tx)
                .await?;
            let r = async {
                for t in [&attempts, &name] {
                    sqlx::query(sqlx::AssertSqlSafe(format!("drop table if exists {t}")))
                        .execute(&mut *tx)
                        .await?;
                }
                for chunk in gone.chunks(n as usize) {
                    sqlx::query(
                        "delete from launches where id = any($1) and created_at >= $2 and created_at < $3 \
                         and manual_state is null",
                    )
                    .bind(chunk)
                    .bind(at_midnight(start))
                    .bind(at_midnight(end))
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query(
                    "delete from launches where created_at >= $1 and created_at < $2 \
                     and manual_state is null",
                )
                .bind(at_midnight(start))
                .bind(at_midnight(end))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "update run_items set result_id = null where result_id is not null and launch_id in \
                     (select id from launches where created_at >= $1 and created_at < $2 \
                      and manual_state is not null)",
                )
                .bind(at_midnight(start))
                .bind(at_midnight(end))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "update launches set rolled_up_at = coalesce(rolled_up_at, now()) \
                     where created_at >= $1 and created_at < $2 and manual_state is not null",
                )
                .bind(at_midnight(start))
                .bind(at_midnight(end))
                .execute(&mut *tx)
                .await?;
                Ok::<_, sqlx::Error>(())
            }
            .await;
            match r {
                Ok(()) => {
                    tx.commit().await?;
                    tracing::info!(partition = %name, "rolled-up raw partition dropped");
                    dropped.push(name);
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    tracing::warn!(partition = %name, error = %e, "dropping a rolled-up partition failed; retried on the next run");
                }
            }
        }
        Ok(dropped)
    }

    /// Launches beyond retention: without a manual run, older than `rawDays` or beyond
    /// `launchesKeep`; with a closed manual run, older than `manualDays` when it is set.
    fn retention_candidates_sql() -> &'static str {
        "select id, created_at, count_total from launches where project_id = $1 and merged_into is null \
           and not (important and $2) and ((manual_state is null and \
             (created_at < now() - make_interval(days => $3) \
             or ($4::bigint is not null and id in (select id from launches where project_id = $1 \
               and merged_into is null and not (important and $2) and manual_state is null \
               order by number desc offset $4)))) \
             or (manual_state = 'closed' and $6 > 0 and created_at < now() - make_interval(days => $6))) \
         order by created_at, number limit $5"
    }

    /// What retention would delete in a project now.
    pub async fn retention_preview(
        &self,
        project_id: Uuid,
        s: &ProjectSettings,
    ) -> anyhow::Result<RetentionPreview> {
        let r = &s.retention;
        let rows: Vec<(Uuid, DateTime<Utc>, i32)> =
            sqlx::query_as(Self::retention_candidates_sql())
                .bind(project_id)
                .bind(r.keep_important)
                .bind(r.raw_days as i32)
                .bind(r.launches_keep.map(|k| k as i64))
                .bind(100_000i64)
                .bind(r.manual_days as i32)
                .fetch_all(&self.pool)
                .await?;
        let important_kept: i64 = if r.keep_important {
            sqlx::query_scalar(
                "select count(*) from launches where project_id = $1 and important \
                 and created_at < now() - make_interval(days => $2)",
            )
            .bind(project_id)
            .bind(r.raw_days as i32)
            .fetch_one(&self.pool)
            .await?
        } else {
            0
        };
        Ok(RetentionPreview {
            launches: rows.len() as i64,
            results: rows.iter().map(|r| r.2 as i64).sum(),
            important_kept,
            oldest: rows.first().map(|r| r.1),
            launch_ids: rows.iter().take(50).map(|r| r.0).collect(),
        })
    }

    /// Deletes launches beyond each project's retention (rolled up first), then queues the
    /// blobs of unreferenced attachments. Stops starting new work after `budget`.
    pub async fn retention_apply(&self, budget: Duration) -> anyhow::Result<RetentionReport> {
        let started = Instant::now();
        let mut report = RetentionReport {
            test_plans_deleted: self.delete_old_test_plans(10_000).await?,
            ..RetentionReport::default()
        };
        for (project_id, key, s) in self.all_project_settings().await? {
            let r = &s.retention;
            loop {
                if started.elapsed() > budget {
                    return Ok(report);
                }
                let rows: Vec<(Uuid, DateTime<Utc>, i32)> =
                    sqlx::query_as(Self::retention_candidates_sql())
                        .bind(project_id)
                        .bind(r.keep_important)
                        .bind(r.raw_days as i32)
                        .bind(r.launches_keep.map(|k| k as i64))
                        .bind(100i64)
                        .bind(r.manual_days as i32)
                        .fetch_all(&self.pool)
                        .await?;
                for (id, _, _) in &rows {
                    self.rollup_launch(*id).await?;
                    if self.delete_launch(project_id, &key, *id).await? {
                        report.launches_deleted += 1;
                    }
                }
                if rows.len() < 100 {
                    break;
                }
            }
            loop {
                let n = self
                    .queue_unreferenced_attachments(project_id, ATTACHMENT_GRACE, 1000)
                    .await?;
                report.blobs_queued += n;
                if n < 1000 || started.elapsed() > budget {
                    break;
                }
            }
        }
        Ok(report)
    }

    /// Deletes attachment rows of a project that no launch links, no case holds and no manual
    /// step result references, and that were not seen for `grace`, and queues their blobs (with
    /// their derived blobs) in `blob_deletions` (same transaction).
    pub async fn queue_unreferenced_attachments(
        &self,
        project_id: Uuid,
        grace: Duration,
        batch: i64,
    ) -> anyhow::Result<u64> {
        let r = sqlx::query(
            "with gone as (delete from attachments where id in (select a.id from attachments a \
               where a.project_id = $1 and a.last_seen_at < now() - make_interval(secs => $2) \
                 and not exists (select 1 from attachment_links l where l.attachment_id = a.id) \
                 and not exists (select 1 from case_attachments c where c.attachment_id = a.id) \
                 and not exists (select 1 from run_step_results s where s.attachment_ids @> array[a.id]) \
                 and not exists (select 1 from attachment_refs r join run_step_results s \
                   on s.attachment_ids @> array[r.id] where r.attachment_id = a.id) \
               order by a.last_seen_at limit $3 for update skip locked) returning id, blob_key), \
             derived as (insert into blob_deletions (prefix, project_id) select m.blob_key, null \
               from attachment_media m join gone g on g.id = m.attachment_id where m.blob_key is not null) \
             insert into blob_deletions (prefix, project_id) select blob_key, null from gone",
        )
        .bind(project_id)
        .bind(grace.as_secs_f64())
        .bind(batch)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_quantiles() {
        assert_eq!(duration_bucket(0), 0);
        assert_eq!(duration_bucket(9), 0);
        assert_eq!(duration_bucket(10), 1);
        assert_eq!(duration_bucket(19), 1);
        assert_eq!(duration_bucket(20), 2);
        assert_eq!(duration_bucket(1000), 7);
        assert_eq!(duration_bucket(i64::MAX), DURATION_BUCKETS - 1);
        let mut h = vec![0i64; DURATION_BUCKETS];
        for d in [100, 120, 150, 900, 1000] {
            h[duration_bucket(d)] += 1;
        }
        let p50 = histogram_quantile(&h, 0.5).unwrap();
        assert!((80..=160).contains(&p50), "{p50}");
        let p95 = histogram_quantile(&h, 0.95).unwrap();
        assert!((640..=1280).contains(&p95), "{p95}");
        assert_eq!(histogram_quantile(&[0; 3], 0.5), None);
        assert_eq!(
            month_of("results_p202611", "results"),
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );
        assert_eq!(month_of("results_default", "results"), None);
        assert_eq!(month_of("results_p2026111", "results"), None);
        assert_eq!(int_array(&[1, 2]), "{1,2}");
    }
}
