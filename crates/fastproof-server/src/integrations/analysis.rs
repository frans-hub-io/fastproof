//! The analysis operations the MCP tools call (clusters, similar failures, flaky tests,
//! launch comparison, triage, rule drafts). The analysis module of the server implements them
//! fully; [`Fallback`] answers from the stage B tables so the MCP endpoint works on its own.

use async_trait::async_trait;
use chrono::Utc;
use fastproof_store::{Launch, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::state::AppState;

/// Largest number of results one triage call changes by id.
pub const MAX_TRIAGE_IDS: usize = 500;

/// An issue link of a triage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageIssue {
    /// Issue URL; built from the project's issue trackers when only `key` is given.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

/// A mute of a triage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageMute {
    /// RFC 3339; muted until changed when absent.
    #[serde(default)]
    pub until: Option<String>,
    pub reason: String,
}

/// The body of `POST /api/v1/projects/{key}/triage`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriageRequest {
    #[serde(default)]
    pub result_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub launch_id: Option<Uuid>,
    /// With `launchId`: every result with this fingerprint in the launch.
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub defect_type_id: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub issue: Option<TriageIssue>,
    #[serde(default)]
    pub mute: Option<TriageMute>,
    #[serde(default)]
    pub unmute: Option<bool>,
    /// `user` or `ai`; the MCP tool always sends `ai`.
    #[serde(default)]
    pub source: Option<String>,
}

/// The operations of the analysis module used by MCP tools.
#[async_trait]
pub trait AnalysisOps: Send + Sync {
    /// Failure clusters of a launch (`GET .../launches/{id}/clusters`).
    async fn clusters(
        &self,
        s: &AppState,
        project: &Project,
        launch: &Launch,
    ) -> anyhow::Result<Value>;
    /// Results similar to one (`GET .../results/{id}/similar`).
    async fn similar(
        &self,
        s: &AppState,
        project: &Project,
        result_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Value>;
    /// Flaky tests (`GET .../flaky`).
    async fn flaky(
        &self,
        s: &AppState,
        project: &Project,
        branch: Option<&str>,
        environment: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Value>;
    /// Compares two launches (`GET .../launches/{a}/compare/{b}`).
    async fn compare(
        &self,
        s: &AppState,
        project: &Project,
        a: &Launch,
        b: &Launch,
    ) -> anyhow::Result<Value>;
    /// Applies a triage (`POST .../triage`); returns `{updated}`.
    async fn triage(
        &self,
        s: &AppState,
        project: &Project,
        by: &str,
        req: &TriageRequest,
    ) -> anyhow::Result<Value>;
    /// Creates a disabled rule; returns it.
    async fn create_rule_draft(
        &self,
        s: &AppState,
        project: &Project,
        by: &str,
        rule: &Value,
    ) -> anyhow::Result<Value>;
}

/// Answers from the stage B tables (fingerprints as stored at ingest, `flaky_in_run`, the
/// `defect` and `muted` columns of results).
pub struct Fallback;

#[async_trait]
impl AnalysisOps for Fallback {
    async fn clusters(
        &self,
        s: &AppState,
        _project: &Project,
        launch: &Launch,
    ) -> anyhow::Result<Value> {
        let rows: Vec<(Option<String>, Option<String>, i64, i64, i64, uuid::Uuid)> = sqlx::query_as(
            "select fingerprint, min(left(message, 500)), count(*), count(*) filter (where status = 'failed'), \
             count(*) filter (where status = 'broken'), (array_agg(id order by full_name))[1] \
             from results where launch_id = $1 and launch_created_at = $2 and status in ('failed', 'broken') \
             group by coalesce(fingerprint, left(coalesce(message, ''), 200)), fingerprint \
             order by count(*) desc limit 200",
        )
        .bind(launch.id)
        .bind(launch.created_at)
        .fetch_all(s.store.pool())
        .await?;
        let items: Vec<Value> = rows
            .into_iter()
            .map(|(fp, msg, n, failed, broken, sample)| {
                json!({"fingerprint": fp, "message": msg, "count": n,
                       "statuses": {"failed": failed, "broken": broken}, "sampleResultId": sample})
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn similar(
        &self,
        s: &AppState,
        project: &Project,
        result_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Value> {
        let rows: Vec<(Uuid, Uuid, i64, String, Option<String>)> = sqlx::query_as(
            "select r.id, r.launch_id, l.number, r.full_name, left(r.message, 500) from results r \
             join launches l on l.id = r.launch_id \
             where r.project_id = $1 and r.id <> $2 and r.fingerprint is not null and r.fingerprint = \
             (select fingerprint from results where project_id = $1 and id = $2) \
             order by r.launch_created_at desc limit $3",
        )
        .bind(project.id)
        .bind(result_id)
        .bind(limit)
        .fetch_all(s.store.pool())
        .await?;
        let items: Vec<Value> = rows
            .into_iter()
            .map(|(id, l, n, name, msg)| {
                json!({"resultId": id, "launchId": l, "launchNumber": n, "name": name, "message": msg, "score": 1.0})
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn flaky(
        &self,
        s: &AppState,
        project: &Project,
        branch: Option<&str>,
        environment: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Value> {
        let rows: Vec<(Uuid, String, String, i64)> = sqlx::query_as(
            "select r.test_case_id, s.series_key, min(r.full_name), count(*) from \
             (select id, created_at from launches where project_id = $1 and not debug and status <> 'running' \
             and ($2::text is null or branch = $2) and ($3::text is null or env_key = $3) \
             order by number desc limit 20) l \
             join results r on r.launch_id = l.id and r.launch_created_at = l.created_at \
             join series s on s.id = r.series_id where r.flaky_in_run \
             group by r.test_case_id, s.series_key order by count(*) desc limit $4",
        )
        .bind(project.id)
        .bind(branch)
        .bind(environment)
        .bind(limit)
        .fetch_all(s.store.pool())
        .await?;
        let items: Vec<Value> = rows
            .into_iter()
            .map(|(tc, sk, name, n)| json!({"testCaseId": tc, "seriesKey": sk, "name": name, "score": n as f64 / 20.0}))
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn compare(
        &self,
        s: &AppState,
        _project: &Project,
        a: &Launch,
        b: &Launch,
    ) -> anyhow::Result<Value> {
        let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "select coalesce(x.full_name, y.full_name), x.status, y.status from \
             (select series_id, full_name, status from results where launch_id = $1 and launch_created_at = $2) x \
             full join (select series_id, full_name, status from results where launch_id = $3 and launch_created_at = $4) y \
             on x.series_id = y.series_id limit 100000",
        )
        .bind(a.id)
        .bind(a.created_at)
        .bind(b.id)
        .bind(b.created_at)
        .fetch_all(s.store.pool())
        .await?;
        let bad = |st: &Option<String>| matches!(st.as_deref(), Some("failed" | "broken"));
        let (mut new_failures, mut fixed, mut still, mut new_tests, mut missing) =
            (vec![], vec![], vec![], vec![], vec![]);
        for (name, sa, sb) in rows {
            match (&sa, &sb) {
                (Some(_), None) => missing.push(name),
                (None, Some(_)) => {
                    if bad(&sb) {
                        new_failures.push(name.clone());
                    }
                    new_tests.push(name)
                }
                _ if bad(&sb) && !bad(&sa) => new_failures.push(name),
                _ if bad(&sa) && !bad(&sb) => fixed.push(name),
                _ if bad(&sa) && bad(&sb) => still.push(name),
                _ => {}
            }
        }
        let cap = |v: Vec<String>| v.into_iter().take(200).collect::<Vec<_>>();
        Ok(
            json!({"newFailures": cap(new_failures), "fixed": cap(fixed), "stillFailing": cap(still),
                  "newTests": cap(new_tests), "missing": cap(missing)}),
        )
    }

    async fn triage(
        &self,
        s: &AppState,
        project: &Project,
        by: &str,
        req: &TriageRequest,
    ) -> anyhow::Result<Value> {
        let defect = json!({
            "type": req.defect_type_id.clone().unwrap_or_else(|| "toInvestigate".into()),
            "typeId": req.defect_type_id,
            "comment": req.comment,
            "issueUrl": req.issue.as_ref().and_then(|i| i.url.clone()),
            "issueKey": req.issue.as_ref().and_then(|i| i.key.clone()),
            "source": req.source.clone().unwrap_or_else(|| "user".into()),
            "at": Utc::now(),
            "by": by,
        });
        let muted: Option<bool> = if req.unmute == Some(true) {
            Some(false)
        } else if req.mute.is_some() {
            Some(true)
        } else {
            None
        };
        let ids: Vec<Uuid> = req.result_ids.clone().unwrap_or_default();
        let mut attempt = 1;
        loop {
            match fallback_triage(s, project, &ids, req, &defect, muted).await {
                Err(e)
                    if attempt < fastproof_store::ingest::TX_ATTEMPTS
                        && fastproof_store::ingest::retryable(&e).is_some() =>
                {
                    fastproof_store::ingest::retry_pause("triage", &e, attempt).await;
                    attempt += 1;
                }
                r => return r.map(|n| json!({"updated": n})),
            }
        }
    }

    async fn create_rule_draft(
        &self,
        _s: &AppState,
        _p: &Project,
        _by: &str,
        _rule: &Value,
    ) -> anyhow::Result<Value> {
        anyhow::bail!("rules are not available in this build")
    }
}

/// One transaction of [`Fallback::triage`]: the results, then `count_muted` of their launches
/// recounted (as every change of the `muted` flag does), so the counter never drifts.
async fn fallback_triage(
    s: &AppState,
    project: &Project,
    ids: &[Uuid],
    req: &TriageRequest,
    defect: &Value,
    muted: Option<bool>,
) -> anyhow::Result<u64> {
    let mut tx = s.store.pool().begin().await?;
    let launches: Vec<Uuid> = sqlx::query_scalar(
        "update results set defect = $4, muted = coalesce($5, muted), updated_at = now() \
         where project_id = $1 and ((cardinality($2::uuid[]) > 0 and id = any($2)) or \
         ($3::uuid is not null and $6::text is not null and launch_id = $3 and fingerprint = $6)) \
         returning launch_id",
    )
    .bind(project.id)
    .bind(ids)
    .bind(req.launch_id)
    .bind(defect)
    .bind(muted)
    .bind(req.fingerprint.as_deref())
    .fetch_all(&mut *tx)
    .await?;
    let updated = launches.len() as u64;
    if muted.is_some() {
        fastproof_store::analysis::triage::recount_muted(&mut tx, &launches).await?;
    }
    tx.commit().await?;
    Ok(updated)
}

/// The analysis module of the server: the same functions the HTTP API uses.
pub struct Full;

#[async_trait]
impl AnalysisOps for Full {
    async fn clusters(
        &self,
        s: &AppState,
        _project: &Project,
        launch: &Launch,
    ) -> anyhow::Result<Value> {
        Ok(json!({ "items": s.store.clusters(launch).await? }))
    }

    async fn similar(
        &self,
        s: &AppState,
        project: &Project,
        result_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<Value> {
        let items = s
            .store
            .similar_results(project.id, result_id, limit, None)
            .await?
            .unwrap_or_default();
        Ok(json!({ "items": items }))
    }

    async fn flaky(
        &self,
        s: &AppState,
        project: &Project,
        branch: Option<&str>,
        environment: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Value> {
        let settings = s.store.project_settings(project.id).await?;
        let f = fastproof_store::analysis::flaky::FlakyFilter {
            branch: branch.map(str::to_owned),
            environment: environment.map(str::to_owned),
            limit,
        };
        Ok(json!({ "items": s.store.flaky_tests(project.id, &settings, &f).await? }))
    }

    async fn compare(
        &self,
        s: &AppState,
        _project: &Project,
        a: &Launch,
        b: &Launch,
    ) -> anyhow::Result<Value> {
        Ok(serde_json::to_value(s.store.compare(a, b).await?)?)
    }

    async fn triage(
        &self,
        s: &AppState,
        project: &Project,
        by: &str,
        req: &TriageRequest,
    ) -> anyhow::Result<Value> {
        let req: fastproof_store::analysis::triage::TriageRequest =
            serde_json::from_value(serde_json::to_value(req)?)?;
        let (out, _) = crate::api::analysis::apply_triage(s, project, by, req).await?;
        Ok(json!({ "updated": out.updated }))
    }

    async fn create_rule_draft(
        &self,
        s: &AppState,
        project: &Project,
        by: &str,
        rule: &Value,
    ) -> anyhow::Result<Value> {
        let r = crate::api::analysis::create_rule_draft(s, project, by, rule.clone()).await?;
        Ok(serde_json::to_value(r)?)
    }
}
