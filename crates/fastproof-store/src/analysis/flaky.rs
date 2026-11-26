//! Flaky tests of a project: from the series statistics (all branches), or from the raw
//! results of one branch's recent launches.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use fastproof_model::Status;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::settings::ProjectSettings;
use super::stats;
use super::types::FlakyHintBrief;
use crate::{Store, no_nul};

/// Launches of a branch that the branch view reads, at most.
pub const BRANCH_LAUNCHES: i64 = 200;
/// Statuses shown per flaky test.
pub const SHOWN_STATUSES: usize = 10;

/// A flaky test.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlakyItem {
    pub test_case_id: Uuid,
    /// The series (the test in one environment); its causes are at `GET .../flaky/{seriesId}/hints`.
    pub series_id: Uuid,
    pub series_key: String,
    pub name: String,
    /// Share of status changes over the detection window (0..1).
    pub score: f32,
    /// Newest first.
    pub last_statuses: Vec<String>,
    pub last_flaky_at: Option<DateTime<Utc>>,
    /// Likely causes, most confident first (FastProof Analysis; empty without it).
    pub hints: Vec<FlakyHintBrief>,
    /// Share of the failures with infrastructure evidence (signals, bursts, infrastructure
    /// errors); `null` before the causes are computed or without failures.
    pub infra_share: Option<f64>,
}

/// Filters of the flaky list.
#[derive(Clone, Debug, Default)]
pub struct FlakyFilter {
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub limit: i64,
}

type StatsRow = (
    Uuid,
    Uuid,
    String,
    String,
    Option<f32>,
    String,
    Option<DateTime<Utc>>,
);

impl Store {
    pub async fn flaky_tests(
        &self,
        project_id: Uuid,
        settings: &ProjectSettings,
        f: &FlakyFilter,
    ) -> anyhow::Result<Vec<FlakyItem>> {
        let window = settings.flaky.depth() as usize + 1;
        let mut items = match &f.branch {
            Some(branch) => self.flaky_on_branch(project_id, branch, f, window).await?,
            None => self.flaky_overall(project_id, f).await?,
        };
        let ids: Vec<Uuid> = items.iter().map(|i| i.series_id).collect();
        let briefs = self.flaky_hint_briefs(project_id, &ids).await?;
        for i in &mut items {
            if let Some(b) = briefs.get(&i.series_id) {
                i.hints = b.hints.clone();
                i.infra_share = b.infra_share;
            }
        }
        Ok(items)
    }

    async fn flaky_overall(
        &self,
        project_id: Uuid,
        f: &FlakyFilter,
    ) -> anyhow::Result<Vec<FlakyItem>> {
        let rows: Vec<StatsRow> = sqlx::query_as(
            "select s.test_case_id, s.id, s.series_key, tc.display_name, st.flaky_score, st.last_statuses, st.last_flaky_at \
             from series_stats st join series s on s.id = st.series_id join test_cases tc on tc.id = s.test_case_id \
             where st.project_id = $1 and (st.flaky or st.last_flaky_at > now() - interval '30 days') \
               and ($2::text is null or s.env_key = $2) \
             order by st.flaky desc, st.flaky_score desc nulls last, st.last_flaky_at desc nulls last limit $3",
        )
        .bind(project_id)
        .bind(f.environment.as_deref().map(no_nul))
        .bind(f.limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(tc, series_id, key, name, score, statuses, at)| {
                let mut st = stats::decode(&statuses);
                st.reverse();
                FlakyItem {
                    test_case_id: tc,
                    series_id,
                    series_key: key,
                    name,
                    score: score.unwrap_or(0.0),
                    last_statuses: st
                        .iter()
                        .take(SHOWN_STATUSES)
                        .map(|s| s.as_str().to_string())
                        .collect(),
                    last_flaky_at: at,
                    hints: Vec::new(),
                    infra_share: None,
                }
            })
            .collect())
    }

    async fn flaky_on_branch(
        &self,
        project_id: Uuid,
        branch: &str,
        f: &FlakyFilter,
        window: usize,
    ) -> anyhow::Result<Vec<FlakyItem>> {
        let rows: Vec<(Uuid, String, DateTime<Utc>, bool)> = sqlx::query_as(
            "with ls as (select id, created_at from launches where project_id = $1 and branch = $2 and not debug \
               order by number desc limit $3), \
             fl as (select distinct r.series_id from ls join results r on r.launch_id = ls.id \
               and r.launch_created_at = ls.created_at where r.flaky limit 1000) \
             select r.series_id, r.status, r.launch_created_at, r.flaky from ls \
               join results r on r.launch_id = ls.id and r.launch_created_at = ls.created_at \
             where r.series_id in (select series_id from fl)",
        )
        .bind(project_id)
        .bind(no_nul(branch))
        .bind(BRANCH_LAUNCHES)
        .fetch_all(&self.pool)
        .await?;
        let mut by: HashMap<Uuid, Vec<(DateTime<Utc>, Status, bool)>> = HashMap::new();
        for (s, st, at, fl) in rows {
            by.entry(s)
                .or_default()
                .push((at, Status::parse_lenient(&st), fl));
        }
        if by.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = by.keys().copied().collect();
        let meta: HashMap<Uuid, (Uuid, String, String, Option<String>)> = sqlx::query_as::<
            _,
            (Uuid, Uuid, String, String, Option<String>),
        >(
            "select s.id, s.test_case_id, s.series_key, tc.display_name, s.env_key from series s \
                 join test_cases tc on tc.id = s.test_case_id where s.id = any($1)",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(id, tc, k, n, e)| (id, (tc, k, n, e)))
        .collect();
        let mut items: Vec<FlakyItem> = Vec::new();
        for (s, mut v) in by {
            let Some((tc, key, name, env)) = meta.get(&s) else {
                continue;
            };
            if let Some(e) = &f.environment
                && env.as_deref() != Some(e.as_str())
            {
                continue;
            }
            v.sort_by_key(|x| x.0);
            let statuses: Vec<Status> = v.iter().map(|x| x.1).collect();
            let last_flaky_at = v.iter().filter(|x| x.2).map(|x| x.0).max();
            items.push(FlakyItem {
                test_case_id: *tc,
                series_id: s,
                series_key: key.clone(),
                name: name.clone(),
                score: stats::flaky_score(&statuses, window).unwrap_or(0.0),
                last_statuses: statuses
                    .iter()
                    .rev()
                    .take(SHOWN_STATUSES)
                    .map(|s| s.as_str().to_string())
                    .collect(),
                last_flaky_at,
                hints: Vec::new(),
                infra_share: None,
            });
        }
        items.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(b.last_flaky_at.cmp(&a.last_flaky_at))
        });
        items.truncate(f.limit.max(0) as usize);
        Ok(items)
    }
}
