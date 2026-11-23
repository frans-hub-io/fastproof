//! Unique errors of a launch (clusters by fingerprint) and similar failures across launches:
//! the same fingerprint first, then fingerprints whose normalized messages are similar by
//! trigrams (`pg_trgm` when installed, the same measure computed here otherwise).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Defect, Launch, Store};

/// Clusters listed per launch, at most.
pub const MAX_CLUSTERS: i64 = 1000;
/// Similar failures returned, at most.
pub const MAX_SIMILAR: i64 = 50;
/// Fingerprints compared when `pg_trgm` is missing.
pub const FALLBACK_CANDIDATES: i64 = 2000;
/// Lowest trigram similarity reported.
pub const MIN_SIMILARITY: f64 = 0.3;

/// Failed and broken counts of a cluster.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct ClusterStatuses {
    pub failed: i64,
    pub broken: i64,
}

/// A unique error of a launch.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub fingerprint: String,
    pub message: Option<String>,
    pub count: i64,
    pub statuses: ClusterStatuses,
    pub sample_result_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<Defect>,
    /// First seen in an earlier launch on this day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_since: Option<NaiveDate>,
    pub is_new: bool,
}

/// A similar failure.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimilarItem {
    pub result_id: Uuid,
    pub launch_id: Uuid,
    pub launch_number: i64,
    pub name: String,
    pub message: Option<String>,
    /// 1 for the same fingerprint, else the trigram similarity of the normalized messages.
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<Defect>,
}

fn trigrams(s: &str) -> BTreeSet<[char; 3]> {
    let mut out = BTreeSet::new();
    for word in s
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let chars: Vec<char> = "  "
            .chars()
            .chain(word.chars())
            .chain(std::iter::once(' '))
            .collect();
        for w in chars.windows(3) {
            out.insert([w[0], w[1], w[2]]);
        }
    }
    out
}

/// `pg_trgm` `similarity()`: shared trigrams over all trigrams of both strings.
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (trigrams(a), trigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let shared = x.intersection(&y).count();
    shared as f64 / (x.len() + y.len() - shared) as f64
}

fn short(m: Option<String>) -> Option<String> {
    m.map(
        |m| match m.char_indices().nth(crate::results::MESSAGE_CHARS) {
            Some((i, _)) => format!("{}…", &m[..i]),
            None => m,
        },
    )
}

fn defect_of(v: Option<serde_json::Value>) -> Option<Defect> {
    v.and_then(|d| serde_json::from_value(d).ok())
}

type ScoredRow = (
    f64,
    Uuid,
    Uuid,
    i64,
    String,
    Option<String>,
    Option<serde_json::Value>,
);
type ClusterRow = (String, i64, i64, i64, Uuid, Option<serde_json::Value>);
type SimilarRow = (
    Uuid,
    Uuid,
    i64,
    String,
    Option<String>,
    Option<serde_json::Value>,
);

impl Store {
    /// Whether `pg_trgm` is installed in this database.
    pub async fn trigram_installed(&self) -> anyhow::Result<bool> {
        Ok(sqlx::query_scalar(
            "select exists(select 1 from pg_extension where extname = 'pg_trgm')",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// Failed and broken results of a launch grouped by fingerprint, largest first.
    pub async fn clusters(
        &self,
        launch: &Launch,
        env: Option<&crate::results::EnvFilter>,
    ) -> anyhow::Result<Vec<Cluster>> {
        let (mode, key) = crate::results::EnvFilter::args(env);
        let rows: Vec<ClusterRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "select fingerprint, count(*)::bigint, count(*) filter (where status = 'failed')::bigint, \
               count(*) filter (where status = 'broken')::bigint, (array_agg(id order by full_name, id))[1], \
               (array_agg(defect order by (defect->>'source' in ('user', 'ai')) desc, \
                 (defect->>'ruleId') is not null desc, full_name, id) filter (where defect is not null))[1] \
             from results where launch_id = $1 and launch_created_at = $2 and status in ('failed', 'broken') \
               and fingerprint is not null \
               and ($4::int = 0 or exists (select 1 from series se where se.id = results.series_id and {})) \
             group by fingerprint order by 2 desc, 1 limit $3",
            crate::results::ENV_MATCH
                .replace("$m", "$4")
                .replace("$k", "$5::text")
        )))
        .bind(launch.id)
        .bind(launch.created_at)
        .bind(MAX_CLUSTERS)
        .bind(mode)
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let samples: Vec<Uuid> = rows.iter().map(|r| r.4).collect();
        let messages: HashMap<Uuid, Option<String>> = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select id, message from results where id = any($1) and launch_created_at = $2",
        )
        .bind(&samples)
        .bind(launch.created_at)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        let fps: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let known: HashMap<String, (DateTime<Utc>, Option<Uuid>)> =
            sqlx::query_as::<_, (String, DateTime<Utc>, Option<Uuid>)>(
                "select fingerprint, first_seen_at, first_launch_id from failure_fingerprints \
                 where project_id = $1 and fingerprint = any($2)",
            )
            .bind(launch.project_id)
            .bind(&fps)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|(f, at, l)| (f, (at, l)))
            .collect();
        Ok(rows
            .into_iter()
            .map(|(fp, count, failed, broken, sample, defect)| {
                let known_since = known
                    .get(&fp)
                    .filter(|(at, l)| *l != Some(launch.id) && *at < launch.created_at)
                    .map(|(at, _)| at.date_naive());
                Cluster {
                    message: short(messages.get(&sample).cloned().flatten()),
                    count,
                    statuses: ClusterStatuses { failed, broken },
                    sample_result_id: sample,
                    defect: defect_of(defect),
                    is_new: known_since.is_none(),
                    known_since,
                    fingerprint: fp,
                }
            })
            .collect())
    }

    /// Failures similar to a result; `None` when the result does not exist. `trigram` forces
    /// (or avoids) `pg_trgm`; by default it is used when installed.
    pub async fn similar_results(
        &self,
        project_id: Uuid,
        result_id: Uuid,
        limit: i64,
        trigram: Option<bool>,
    ) -> anyhow::Result<Option<Vec<SimilarItem>>> {
        let target: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "select fingerprint, message from results where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(result_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((fp, message)) = target else {
            return Ok(None);
        };
        let Some(fp) = fp else {
            return Ok(Some(Vec::new()));
        };
        let limit = limit.clamp(1, MAX_SIMILAR);
        let same: Vec<SimilarRow> = sqlx::query_as(
            "select r.id, r.launch_id, l.number, r.name, r.message, r.defect from results r \
             join launches l on l.id = r.launch_id \
             where r.project_id = $1 and r.fingerprint = $2 and r.id <> $3 \
             order by r.launch_created_at desc, r.id limit $4",
        )
        .bind(project_id)
        .bind(&fp)
        .bind(result_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut items: Vec<SimilarItem> = same
            .into_iter()
            .map(|r| SimilarItem {
                result_id: r.0,
                launch_id: r.1,
                launch_number: r.2,
                name: r.3,
                message: short(r.4),
                score: 1.0,
                defect: defect_of(r.5),
            })
            .collect();
        let rest = limit - items.len() as i64;
        if rest <= 0 {
            return Ok(Some(items));
        }
        let normalized: String = sqlx::query_scalar::<_, String>(
            "select message from failure_fingerprints where project_id = $1 and fingerprint = $2",
        )
        .bind(project_id)
        .bind(&fp)
        .fetch_optional(&self.pool)
        .await?
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| {
            fastproof_model::fingerprint::normalize_message(message.as_deref().unwrap_or(""))
        });
        if normalized.is_empty() {
            return Ok(Some(items));
        }
        let use_trgm = match trigram {
            Some(t) => t,
            None => self.trigram_installed().await?,
        };
        let candidates: Vec<(String, f64)> = if use_trgm {
            sqlx::query_as(
                "select fingerprint, similarity(message, $3)::float8 as score from failure_fingerprints \
                 where project_id = $1 and fingerprint <> $2 and message % $3 \
                 order by score desc, last_seen_at desc limit $4",
            )
            .bind(project_id)
            .bind(&fp)
            .bind(&normalized)
            .bind(rest)
            .fetch_all(&self.pool)
            .await?
        } else {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "select fingerprint, message from failure_fingerprints \
                 where project_id = $1 and fingerprint <> $2 order by last_seen_at desc limit $3",
            )
            .bind(project_id)
            .bind(&fp)
            .bind(FALLBACK_CANDIDATES)
            .fetch_all(&self.pool)
            .await?;
            let mut scored: Vec<(String, f64)> = rows
                .into_iter()
                .map(|(f, m)| {
                    let s = trigram_similarity(&m, &normalized);
                    (f, s)
                })
                .filter(|(_, s)| *s >= MIN_SIMILARITY)
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1));
            scored.truncate(rest as usize);
            scored
        };
        if candidates.is_empty() {
            return Ok(Some(items));
        }
        let fps: Vec<String> = candidates.iter().map(|c| c.0.clone()).collect();
        let scores: Vec<f64> = candidates.iter().map(|c| c.1).collect();
        let rows: Vec<ScoredRow> =
            sqlx::query_as(
                "select f.score, x.id, x.launch_id, x.number, x.name, x.message, x.defect \
                 from unnest($2::text[], $3::float8[]) as f(fp, score) \
                 cross join lateral (select r.id, r.launch_id, l.number, r.name, r.message, r.defect \
                   from results r join launches l on l.id = r.launch_id \
                   where r.project_id = $1 and r.fingerprint = f.fp \
                   order by r.launch_created_at desc limit 1) x",
            )
            .bind(project_id)
            .bind(&fps)
            .bind(&scores)
            .fetch_all(&self.pool)
            .await?;
        let mut more: Vec<SimilarItem> = rows
            .into_iter()
            .map(|r| SimilarItem {
                score: (r.0 * 1000.0).round() / 1000.0,
                result_id: r.1,
                launch_id: r.2,
                launch_number: r.3,
                name: r.4,
                message: short(r.5),
                defect: defect_of(r.6),
            })
            .collect();
        more.sort_by(|a, b| b.score.total_cmp(&a.score));
        items.extend(more);
        Ok(Some(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigram_similarity_matches_pg_trgm() {
        assert_eq!(trigram_similarity("word", "word"), 1.0);
        assert_eq!(trigram_similarity("", "word"), 0.0);
        // pg_trgm: similarity('word', 'two words') = 0.36363637
        assert!((trigram_similarity("word", "two words") - 0.363_636_37).abs() < 1e-6);
        let a = "Connection refused: connect to <tmp> port <n>";
        let b = "Connection refused: connect to db port <n>";
        assert!(trigram_similarity(a, b) > 0.5);
        assert!(trigram_similarity(a, "expected: <<n>> but was: <<n>>") < 0.3);
    }
}
