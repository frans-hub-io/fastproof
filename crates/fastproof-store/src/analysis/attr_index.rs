//! An index of launch attributes for the cumulative trend widget: one row per distinct
//! (key, value) of a launch, kept by a bounded job. A launch is indexed when its
//! `attrs_indexed_at` is empty (new launches, and launches whose attributes were patched);
//! the widget groups the rows by value and sums the counters the launches already hold, so
//! counters and triage stay current without a second copy.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::boards::MAX_DAYS;
use crate::{Store, no_nul};

/// Attributes of one launch that are indexed, at most.
pub const MAX_ATTRS_PER_LAUNCH: usize = 20;
/// Longest key and value that are indexed, in characters.
pub const MAX_ATTR_KEY: usize = 64;
pub const MAX_ATTR_VALUE: usize = 200;
/// Launches one run of the job indexes, at most.
pub const INDEX_BATCH: i64 = 500;

type Pending = (Uuid, Uuid, DateTime<Utc>);

/// The distinct (key, value) pairs of a launch's `attributes` that fit the index limits.
pub fn indexable(attributes: &serde_json::Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for a in attributes.as_array().into_iter().flatten() {
        let (Some(k), Some(v)) = (
            a.get("key").and_then(|k| k.as_str()),
            a.get("value").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty()
            || v.is_empty()
            || k.chars().count() > MAX_ATTR_KEY
            || v.chars().count() > MAX_ATTR_VALUE
            || k.contains('\0')
            || v.contains('\0')
        {
            continue;
        }
        let pair = (k.to_string(), v.to_string());
        if !out.contains(&pair) {
            out.push(pair);
        }
        if out.len() >= MAX_ATTRS_PER_LAUNCH {
            break;
        }
    }
    out
}

/// Launches of one project one round takes, at most: a round gives every project the same
/// share, so one project that makes many launches cannot starve the others.
pub const PER_PROJECT: i64 = 50;
/// Rounds one run makes, at most.
const ROUNDS: usize = 10;
/// Launches older than the widget window that one run marks as not to be indexed.
const SWEEP: i64 = 2000;

impl Store {
    /// Marks launches older than the widget window as indexed without indexing them (they
    /// are never read), so the pending index drains.
    async fn sweep_old_attribute_launches(&self) -> anyhow::Result<u64> {
        Ok(sqlx::query(
            "update launches set attrs_indexed_at = now() where id in ( \
               select l.id from projects p cross join lateral ( \
                 select id from launches where project_id = p.id and attrs_indexed_at is null and not debug \
                   and created_at < now() - make_interval(days => $1) order by created_at limit 200) l \
               where p.deleted_at is null limit $2)",
        )
        .bind(MAX_DAYS as i32)
        .bind(SWEEP)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Indexes launches whose attributes are not indexed yet in rounds of [`PER_PROJECT`] per
    /// project (newest first within a project), at most [`INDEX_BATCH`] in all and until
    /// `budget` is spent. Returns how many were indexed.
    pub async fn index_launch_attributes(&self, budget: Duration) -> anyhow::Result<u64> {
        let started = Instant::now();
        self.sweep_old_attribute_launches().await?;
        let mut done = 0u64;
        for _ in 0..ROUNDS {
            let pending: Vec<Pending> = sqlx::query_as(
                "select l.id, l.project_id, l.created_at from projects p cross join lateral ( \
                   select id, project_id, created_at from launches \
                   where project_id = p.id and attrs_indexed_at is null and not debug \
                     and created_at >= now() - make_interval(days => $1) \
                   order by created_at desc limit $2) l \
                 where p.deleted_at is null limit $3",
            )
            .bind(MAX_DAYS as i32)
            .bind(PER_PROJECT)
            .bind(INDEX_BATCH)
            .fetch_all(&self.pool)
            .await?;
            let mut round = 0;
            for (id, project_id, created) in pending {
                if started.elapsed() > budget || done >= INDEX_BATCH as u64 {
                    return Ok(done);
                }
                if self.index_one(id, project_id, created).await? {
                    round += 1;
                    done += 1;
                }
            }
            if round == 0 {
                break;
            }
        }
        Ok(done)
    }

    async fn index_one(
        &self,
        id: Uuid,
        project_id: Uuid,
        created: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let row: Option<serde_json::Value> = sqlx::query_scalar(
            "select attributes from launches where id = $1 and attrs_indexed_at is null for update skip locked",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(attrs) = row else { return Ok(false) };
        sqlx::query("delete from launch_attr_index where launch_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let pairs = indexable(&attrs);
        if !pairs.is_empty() {
            let keys: Vec<String> = pairs.iter().map(|p| no_nul(&p.0)).collect();
            let values: Vec<String> = pairs.iter().map(|p| no_nul(&p.1)).collect();
            sqlx::query(
                "insert into launch_attr_index (project_id, launch_id, key, value, launch_created_at) \
                 select $1, $2, k, v, $3 from unnest($4::text[], $5::text[]) as t(k, v) \
                 on conflict do nothing",
            )
            .bind(project_id)
            .bind(id)
            .bind(created)
            .bind(&keys)
            .bind(&values)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("update launches set attrs_indexed_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attributes_are_bounded_and_distinct() {
        let mut many: Vec<serde_json::Value> = (0..40)
            .map(|i| json!({"key": format!("k{i}"), "value": "v"}))
            .collect();
        many.push(json!({"key": "k0", "value": "v"}));
        assert_eq!(indexable(&json!(many)).len(), MAX_ATTRS_PER_LAUNCH);
        let odd = json!([
            {"key": "", "value": "x"},
            {"key": "build", "value": ""},
            {"key": "a".repeat(65), "value": "x"},
            {"key": "build", "value": "b".repeat(201)},
            {"key": "bu\u{0}ild", "value": "x"},
            {"key": " build ", "value": " 7 "},
            {"key": "build", "value": "7"},
            {"key": 1, "value": "x"},
        ]);
        assert_eq!(
            indexable(&odd),
            vec![("build".to_string(), "7".to_string())]
        );
        assert!(indexable(&json!({"not": "a list"})).is_empty());
    }
}
