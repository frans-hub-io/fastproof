//! `series_stats`: a rolling status string per series (one character per launch, oldest first),
//! a flakiness score and the latest flaky verdict. It survives raw partitions being dropped,
//! and serves the flaky list and the history fallback.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fastproof_model::Status;
use sqlx::PgConnection;
use uuid::Uuid;

use super::settings::ProjectSettings;
use crate::ingest::LaunchRef;

/// Launches remembered per series.
pub const STATUS_CHARS: usize = 64;

/// The stored state of a series.
#[derive(Clone, Debug, Default)]
pub struct SeriesStatsRow {
    pub last_statuses: String,
    pub last_launch_id: Option<Uuid>,
    pub runs_tracked: i32,
}

pub fn encode(s: Status) -> char {
    match s {
        Status::Passed => 'P',
        Status::Failed => 'F',
        Status::Broken => 'B',
        Status::Skipped => 'S',
        Status::Unknown => 'U',
    }
}

/// Statuses of a status string, oldest first.
pub fn decode(s: &str) -> Vec<Status> {
    s.chars()
        .map(|c| match c {
            'P' => Status::Passed,
            'F' => Status::Failed,
            'B' => Status::Broken,
            'S' => Status::Skipped,
            _ => Status::Unknown,
        })
        .collect()
}

/// Share of status changes among the last `window` significant statuses (oldest first input);
/// `None` with fewer than two.
pub fn flaky_score(statuses: &[Status], window: usize) -> Option<f32> {
    let sig: Vec<Status> = statuses
        .iter()
        .copied()
        .filter(|s| s.is_significant())
        .collect();
    let sig = &sig[sig.len().saturating_sub(window.max(2))..];
    if sig.len() < 2 {
        return None;
    }
    let flips = sig.windows(2).filter(|w| w[0] != w[1]).count();
    Some(flips as f32 / (sig.len() - 1) as f32)
}

/// Locks and reads the statistics rows of series (sorted ids, so concurrent launches lock in
/// the same order).
pub(crate) async fn lock_rows(
    conn: &mut PgConnection,
    series_sorted: &[Uuid],
) -> anyhow::Result<HashMap<Uuid, SeriesStatsRow>> {
    let rows: Vec<(Uuid, String, Option<Uuid>, i32)> = sqlx::query_as(
        "select series_id, last_statuses, last_launch_id, runs_tracked from series_stats \
         where series_id = any($1) order by series_id for update",
    )
    .bind(series_sorted)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(s, l, id, n)| {
            (
                s,
                SeriesStatsRow {
                    last_statuses: l,
                    last_launch_id: id,
                    runs_tracked: n,
                },
            )
        })
        .collect())
}

/// The status string after a launch's result: appended, or replacing the launch's earlier one.
pub fn push_status(row: Option<&SeriesStatsRow>, launch: Uuid, status: Status) -> (String, i32) {
    let mut s: Vec<char> = row
        .map(|r| r.last_statuses.chars().collect())
        .unwrap_or_default();
    let mut runs = row.map(|r| r.runs_tracked).unwrap_or(0);
    if row.is_some_and(|r| r.last_launch_id == Some(launch)) && !s.is_empty() {
        s.pop();
    } else {
        runs += 1;
    }
    s.push(encode(status));
    if s.len() > STATUS_CHARS {
        s.drain(..s.len() - STATUS_CHARS);
    }
    (s.into_iter().collect(), runs)
}

/// Writes the statistics of series after a launch's results (series, status, flaky).
pub(crate) async fn record(
    conn: &mut PgConnection,
    launch: &LaunchRef,
    updates: &[(Uuid, Status, bool)],
    existing: &HashMap<Uuid, SeriesStatsRow>,
    settings: &ProjectSettings,
    _now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let mut updates = updates.to_vec();
    updates.sort_by_key(|u| u.0);
    updates.dedup_by_key(|u| u.0);
    let window = settings.flaky.depth() as usize + 1;
    let at = launch.created_at;
    let n = updates.len();
    let mut ids = Vec::with_capacity(n);
    let mut strings = Vec::with_capacity(n);
    let mut bits = Vec::with_capacity(n);
    let mut runs = Vec::with_capacity(n);
    let mut scores: Vec<Option<f32>> = Vec::with_capacity(n);
    let mut last = Vec::with_capacity(n);
    let mut passed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut failed: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    let mut flaky = Vec::with_capacity(n);
    let mut flaky_at: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
    for (series, status, is_flaky) in updates {
        let (s, r) = push_status(existing.get(&series), launch.id, status);
        let statuses = decode(&s);
        let mut b: i64 = 0;
        for (i, st) in statuses.iter().rev().take(63).enumerate() {
            if st.is_failure() {
                b |= 1 << i;
            }
        }
        ids.push(series);
        scores.push(flaky_score(&statuses, window));
        strings.push(s);
        bits.push(b);
        runs.push(r);
        last.push(status.as_str().to_string());
        passed.push((status == Status::Passed).then_some(at));
        failed.push(status.is_failure().then_some(at));
        flaky.push(is_flaky);
        flaky_at.push(is_flaky.then_some(at));
    }
    sqlx::query(
        "insert into series_stats (series_id, project_id, last_statuses, status_bits, runs_tracked, flaky_score, \
           last_status, last_passed_at, last_failed_at, first_seen_at, last_seen_at, last_launch_id, flaky, \
           last_flaky_at, updated_at) \
         select s, $1, st, b, r, sc, ls, pa, fa, $11, $11, $12, fl, fla, now() \
         from unnest($2::uuid[], $3::text[], $4::bigint[], $5::int[], $6::real[], $7::text[], \
           $8::timestamptz[], $9::timestamptz[], $10::bool[], $13::timestamptz[]) \
           as t(s, st, b, r, sc, ls, pa, fa, fl, fla) \
         on conflict (series_id) do update set last_statuses = excluded.last_statuses, \
           status_bits = excluded.status_bits, runs_tracked = excluded.runs_tracked, \
           flaky_score = excluded.flaky_score, last_status = excluded.last_status, \
           last_passed_at = coalesce(excluded.last_passed_at, series_stats.last_passed_at), \
           last_failed_at = coalesce(excluded.last_failed_at, series_stats.last_failed_at), \
           first_seen_at = coalesce(series_stats.first_seen_at, excluded.first_seen_at), \
           last_seen_at = greatest(series_stats.last_seen_at, excluded.last_seen_at), \
           last_launch_id = excluded.last_launch_id, flaky = excluded.flaky, \
           last_flaky_at = coalesce(excluded.last_flaky_at, series_stats.last_flaky_at), updated_at = now()",
    )
    .bind(launch.project_id)
    .bind(&ids)
    .bind(&strings)
    .bind(&bits)
    .bind(&runs)
    .bind(&scores)
    .bind(&last)
    .bind(&passed)
    .bind(&failed)
    .bind(&flaky)
    .bind(at)
    .bind(launch.id)
    .bind(&flaky_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_string() {
        let l1 = Uuid::now_v7();
        let l2 = Uuid::now_v7();
        let (s, r) = push_status(None, l1, Status::Passed);
        assert_eq!((s.as_str(), r), ("P", 1));
        let row = SeriesStatsRow {
            last_statuses: s,
            last_launch_id: Some(l1),
            runs_tracked: r,
        };
        assert_eq!(push_status(Some(&row), l1, Status::Failed), ("F".into(), 1));
        assert_eq!(
            push_status(Some(&row), l2, Status::Failed),
            ("PF".into(), 2)
        );
        let long = SeriesStatsRow {
            last_statuses: "P".repeat(STATUS_CHARS),
            last_launch_id: Some(l1),
            runs_tracked: 100,
        };
        let (s, _) = push_status(Some(&long), l2, Status::Broken);
        assert_eq!(s.len(), STATUS_CHARS);
        assert!(s.ends_with('B'));
        assert_eq!(decode("PFBSU").len(), 5);
        assert_eq!(flaky_score(&decode("PPPP"), 10), Some(0.0));
        assert_eq!(flaky_score(&decode("PFPF"), 10), Some(1.0));
        assert_eq!(flaky_score(&decode("P"), 10), None);
        assert_eq!(flaky_score(&decode("FFFFFPFPF"), 3), Some(1.0));
    }
}
