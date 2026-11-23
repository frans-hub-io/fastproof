//! Explorer data kept beside the results: `results.label_values` (each label name with its
//! values, read by the tree instead of walking `labels`), `results.suite_tree` and
//! `suite_titled` (the suite path of the result in the tree), a cache of the explorer aggregates
//! of launches that do not run, and the background job that fills `label_values` and
//! `attachment_kinds` for results stored before they existed.
//!
//! The cache is checked against `explorer_gen`, a counter per launch that every write to the
//! launch's results raises in its own transaction ([`touch`], as the last write before the
//! commit). A reader takes the counter and the data in one snapshot and stores the counter
//! with its payload, so an entry built before a write commits is never served after it.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_model::{AttachmentRef, Step};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::ingest::AttemptDetail;

/// The `label_values` of a result: each label name with its values in their order (the SQL
/// function `fastproof_label_values` computes the same from `labels`).
pub fn label_values(labels: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for e in labels.as_array().into_iter().flatten() {
        let Some(name) = e.get("name").and_then(Value::as_str) else {
            continue;
        };
        let value = e.get("value").cloned().unwrap_or_else(|| json!(""));
        if let Value::Array(a) = m
            .entry(name.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            a.push(value);
        }
    }
    Value::Object(m)
}

/// Marks the explorer data of the launches changed. Call it in the transaction that changed
/// their results, after its other writes (the counter row stays locked until the commit).
pub async fn touch(conn: &mut PgConnection, launches: &[Uuid]) -> sqlx::Result<()> {
    if launches.is_empty() {
        return Ok(());
    }
    let mut ids = launches.to_vec();
    ids.sort();
    ids.dedup();
    sqlx::query(
        "insert into explorer_gen (launch_id) select l.id from launches l where l.id = any($1) \
         order by l.id on conflict (launch_id) do update set gen = explorer_gen.gen + 1",
    )
    .bind(&ids)
    .execute(conn)
    .await?;
    Ok(())
}

/// The counter of a launch as the caller's snapshot sees it.
pub(crate) async fn gen_of(conn: &mut PgConnection, launch: Uuid) -> sqlx::Result<i64> {
    Ok(
        sqlx::query_scalar("select gen from explorer_gen where launch_id = $1")
            .bind(launch)
            .fetch_optional(conn)
            .await?
            .unwrap_or(0),
    )
}

/// A cached payload that is still valid for the counter `generation`.
pub(crate) async fn cache_get(
    conn: &mut PgConnection,
    launch: Uuid,
    key: &str,
    generation: i64,
) -> sqlx::Result<Option<Value>> {
    sqlx::query_scalar(
        "select payload from explorer_cache where launch_id = $1 and key = $2 and gen = $3",
    )
    .bind(launch)
    .bind(key)
    .bind(generation)
    .fetch_optional(conn)
    .await
}

/// Cached entries per launch, at most (the oldest go first).
pub const MAX_CACHE_ENTRIES: i64 = 32;
/// Bytes of one cached payload, at most (larger ones are not cached).
pub const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

/// The cache key part of a free-form value (an environment key, a label list).
pub(crate) fn key_part(s: &str) -> String {
    use sha2::Digest;
    if s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.,:@/".contains(c))
    {
        return s.to_string();
    }
    format!("#{}", hex::encode(sha2::Sha256::digest(s.as_bytes())))
}

impl Store {
    /// Stores a payload computed at the counter `generation` (a newer entry is kept).
    pub(crate) async fn cache_put(
        &self,
        launch: Uuid,
        key: &str,
        generation: i64,
        payload: &Value,
    ) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(payload)?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "insert into explorer_cache (launch_id, key, gen, payload) \
             select $1, $2, $3, $4 where exists (select 1 from launches where id = $1) \
             on conflict (launch_id, key) do update set gen = excluded.gen, payload = excluded.payload, \
               created_at = now() where explorer_cache.gen <= excluded.gen",
        )
        .bind(launch)
        .bind(key)
        .bind(generation)
        .bind(crate::json_no_nul(payload.clone()))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "delete from explorer_cache where launch_id = $1 and key in (select key from explorer_cache \
             where launch_id = $1 order by created_at desc, key offset $2)",
        )
        .bind(launch)
        .bind(MAX_CACHE_ENTRIES)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes cache entries older than `age`; returns how many.
    pub async fn prune_explorer_cache(&self, age: Duration) -> anyhow::Result<u64> {
        let age = chrono::Duration::from_std(age).unwrap_or(chrono::Duration::days(7));
        Ok(sqlx::query(
            "delete from explorer_cache where ctid = any(array(select ctid from explorer_cache \
             where created_at < $1 limit 10000))",
        )
        .bind(Utc::now() - age)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}

/// Where the backfill is, in `analysis_state`.
pub const BACKFILL_STATE: &str = "explorer.results.backfill";
/// The migration that added `results.attachment_kinds`: results of launches created before
/// it was applied may have 0 there although they have attachments.
pub const ATTACHMENT_KINDS_VERSION: i64 = 600;

/// What [`Store::backfill_explorer`] did in one call, and where the whole job is.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackfillProgress {
    /// Results looked at in this call.
    pub scanned: u64,
    /// Results changed in this call.
    pub updated: u64,
    /// Results looked at since the job started.
    pub total_scanned: u64,
    /// Results changed since the job started.
    pub total_updated: u64,
    pub done: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct BackfillState {
    cursor: Option<Uuid>,
    #[serde(default)]
    scanned: u64,
    #[serde(default)]
    updated: u64,
    #[serde(default)]
    done: bool,
}

/// The attachment bits of an attempt: its attachments and those of its steps (as ingest
/// computes them; fixtures of containers are not counted there either).
fn attempt_kinds(attachments: &[AttachmentRef], steps: &[Step]) -> i32 {
    fn walk(list: &[Step], m: &mut i32) {
        for s in list {
            for a in &s.attachments {
                *m |= crate::results::kind_bit(a.kind);
            }
            walk(&s.steps, m);
        }
    }
    let mut m = attachments
        .iter()
        .fold(0, |m, a| m | crate::results::kind_bit(a.kind));
    walk(steps, &mut m);
    m
}

impl Store {
    /// Where the explorer backfill is.
    pub async fn explorer_backfill_progress(&self) -> anyhow::Result<BackfillProgress> {
        let s = self.backfill_state().await?;
        Ok(BackfillProgress {
            total_scanned: s.scanned,
            total_updated: s.updated,
            done: s.done,
            ..BackfillProgress::default()
        })
    }

    async fn backfill_state(&self) -> anyhow::Result<BackfillState> {
        let v: Option<Value> =
            sqlx::query_scalar("select value from analysis_state where name = $1")
                .bind(BACKFILL_STATE)
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    /// Fills `label_values`, `suite_tree` and, for results of launches created before migration 600,
    /// `attachment_kinds` of stored results: `batch` results per transaction in id order from
    /// a cursor kept in `analysis_state`, until `budget` is spent or every result was seen.
    /// Each transaction locks only the rows it changes. Idempotent and resumable.
    pub async fn backfill_explorer(
        &self,
        batch: i64,
        budget: Duration,
    ) -> anyhow::Result<BackfillProgress> {
        let started = Instant::now();
        let mut out = BackfillProgress::default();
        let cutoff: Option<DateTime<Utc>> = sqlx::query_scalar(
            "select installed_on from fastproof_schema_migrations where version = $1 and success",
        )
        .bind(ATTACHMENT_KINDS_VERSION)
        .fetch_optional(&self.pool)
        .await?;
        loop {
            let mut state = self.backfill_state().await?;
            if state.done {
                out.done = true;
                out.total_scanned = state.scanned;
                out.total_updated = state.updated;
                return Ok(out);
            }
            type Row = (Uuid, DateTime<Utc>, Uuid, bool, bool, bool);
            let rows: Vec<Row> = sqlx::query_as(
                "select id, launch_created_at, launch_id, label_values is null, \
                   attachment_kinds = 0 and launch_created_at <= coalesce($3, '-infinity'::timestamptz), \
                   suite_tree is null \
                 from results where ($1::uuid is null or id > $1) order by id limit $2",
            )
            .bind(state.cursor)
            .bind(batch)
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await?;
            let mut tx = self.pool.begin().await?;
            let mut updated: BTreeSet<Uuid> = BTreeSet::new();
            let mut touched: BTreeSet<Uuid> = BTreeSet::new();
            let (lid, lat): (Vec<Uuid>, Vec<DateTime<Utc>>) =
                rows.iter().filter(|r| r.3).map(|r| (r.0, r.1)).unzip();
            if !lid.is_empty() {
                let done: Vec<Uuid> = sqlx::query_scalar(
                    "update results r set label_values = fastproof_label_values(r.labels) \
                     from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                     where r.id = t.i and r.launch_created_at = t.c and r.label_values is null \
                     returning r.id",
                )
                .bind(&lid)
                .bind(&lat)
                .fetch_all(&mut *tx)
                .await?;
                updated.extend(done);
            }
            let (sid, sat): (Vec<Uuid>, Vec<DateTime<Utc>>) =
                rows.iter().filter(|r| r.5).map(|r| (r.0, r.1)).unzip();
            if !sid.is_empty() {
                type Suite = (Uuid, DateTime<Utc>, Vec<String>, Value, String);
                let found: Vec<Suite> = sqlx::query_as(
                    "select r.id, r.launch_created_at, r.suite_path, r.labels, r.full_name from results r \
                     join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                       on r.id = t.i and r.launch_created_at = t.c",
                )
                .bind(&sid)
                .bind(&sat)
                .fetch_all(&mut *tx)
                .await?;
                let mut ids = Vec::with_capacity(found.len());
                let mut ats = Vec::with_capacity(found.len());
                let mut paths = Vec::with_capacity(found.len());
                let mut titled = Vec::with_capacity(found.len());
                for (id, at, suite_path, labels, full_name) in found {
                    let (p, t) = crate::results::suite_place_json(&suite_path, &labels, &full_name);
                    ids.push(id);
                    ats.push(at);
                    paths.push(crate::json_no_nul(serde_json::to_value(p)?));
                    titled.push(t);
                }
                let done: Vec<Uuid> = sqlx::query_scalar(
                    "update results r set suite_tree = array(select jsonb_array_elements_text(t.p)), \
                       suite_titled = t.s \
                     from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[], $4::bool[]) as t(i, c, p, s) \
                     where r.id = t.i and r.launch_created_at = t.c and r.suite_tree is null \
                     returning r.id",
                )
                .bind(&ids)
                .bind(&ats)
                .bind(&paths)
                .bind(&titled)
                .fetch_all(&mut *tx)
                .await?;
                updated.extend(done);
            }
            let (kid, kat): (Vec<Uuid>, Vec<DateTime<Utc>>) =
                rows.iter().filter(|r| r.4).map(|r| (r.0, r.1)).unzip();
            if !kid.is_empty() {
                type Attempt = (Uuid, Value, Option<Vec<u8>>);
                let attempts: Vec<Attempt> = sqlx::query_as(
                    "select a.result_id, a.detail, a.steps_z from attempts a \
                     join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                       on a.result_id = t.i and a.launch_created_at = t.c",
                )
                .bind(&kid)
                .bind(&kat)
                .fetch_all(&mut *tx)
                .await?;
                let mut kinds: HashMap<Uuid, i32> = HashMap::new();
                for (result, detail, steps) in attempts {
                    let d: AttemptDetail = serde_json::from_value(detail).unwrap_or_default();
                    let steps: Vec<Step> = match steps {
                        Some(z) => crate::codec::decode(&z).unwrap_or_default(),
                        None => Vec::new(),
                    };
                    let m = attempt_kinds(&d.attachments, &steps);
                    if m != 0 {
                        *kinds.entry(result).or_default() |= m;
                    }
                }
                let at: HashMap<Uuid, DateTime<Utc>> = kid.iter().copied().zip(kat).collect();
                let mut ids = Vec::with_capacity(kinds.len());
                let mut ats = Vec::with_capacity(kinds.len());
                let mut masks = Vec::with_capacity(kinds.len());
                for (id, m) in kinds {
                    ids.push(id);
                    ats.push(at[&id]);
                    masks.push(m);
                }
                if !ids.is_empty() {
                    let done: Vec<(Uuid, Uuid)> = sqlx::query_as(
                        "update results r set attachment_kinds = r.attachment_kinds | t.k \
                         from unnest($1::uuid[], $2::timestamptz[], $3::int[]) as t(i, c, k) \
                         where r.id = t.i and r.launch_created_at = t.c \
                           and r.attachment_kinds | t.k <> r.attachment_kinds \
                         returning r.id, r.launch_id",
                    )
                    .bind(&ids)
                    .bind(&ats)
                    .bind(&masks)
                    .fetch_all(&mut *tx)
                    .await?;
                    for (id, launch) in done {
                        updated.insert(id);
                        touched.insert(launch);
                    }
                }
            }
            let touched: Vec<Uuid> = touched.into_iter().collect();
            touch(&mut tx, &touched).await?;
            state.scanned += rows.len() as u64;
            state.updated += updated.len() as u64;
            match rows.last() {
                Some(r) => state.cursor = Some(r.0),
                None => state.done = true,
            }
            sqlx::query(
                "insert into analysis_state (name, value, updated_at) values ($1, $2, now()) \
                 on conflict (name) do update set value = excluded.value, updated_at = now()",
            )
            .bind(BACKFILL_STATE)
            .bind(serde_json::to_value(&state)?)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            out.scanned += rows.len() as u64;
            out.updated += updated.len() as u64;
            out.total_scanned = state.scanned;
            out.total_updated = state.updated;
            out.done = state.done;
            metrics::counter!("fastproof_explorer_backfill_scanned_total")
                .increment(rows.len() as u64);
            metrics::counter!("fastproof_explorer_backfill_updated_total")
                .increment(updated.len() as u64);
            if state.done || started.elapsed() > budget {
                return Ok(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_values_keep_order_and_empty_values() {
        let v = label_values(&json!([
            {"name": "tag", "value": "b"},
            {"name": "owner", "value": "ann"},
            {"name": "tag", "value": ""},
            {"name": "tag", "value": "a"},
            {"value": "no name"},
            {"name": "severity"}
        ]));
        assert_eq!(
            v,
            json!({"tag": ["b", "", "a"], "owner": ["ann"], "severity": [""]})
        );
        assert_eq!(label_values(&json!("not a list")), json!({}));
    }

    #[test]
    fn key_parts_are_short_and_distinct() {
        assert_eq!(key_part("staging"), "staging");
        let a = key_part("a b");
        assert!(a.starts_with('#'));
        assert_ne!(a, key_part("a  b"));
        assert_eq!(key_part(&"x".repeat(300)).len(), 65);
    }
}
