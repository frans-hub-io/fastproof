//! Background analysis work: queued tasks (retroactive rule application), the rule dry run,
//! the fingerprint backfill of rows stored before fingerprints existed, and mute expiry.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fastproof_model::{Status, fingerprint};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::hook::{is_protected, record_fingerprints, rule_defect, type_refs, upsert_mutes};
use super::rules::{self, Budget, CompiledRule, MATCH_INPUT_BYTES, Rule, Subject};
use super::triage::recount_muted;
use crate::{Defect, Store, json_no_nul, no_nul};

/// Launches one retroactive application covers, at most.
pub const MAX_APPLY_LAUNCHES: i64 = 500;
/// Result ids returned by a dry run, at most.
pub const MAX_DRY_RUN_IDS: usize = 1000;
/// Queued or running tasks per project, at most.
pub const MAX_OPEN_TASKS: i64 = 20;
/// Task kinds the standard `analysis.tasks` job runs (an older replica claimed every kind and
/// failed the ones it did not know).
pub const STANDARD_TASK_KINDS: [&str; 1] = ["rule.apply"];

/// A background task.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: Uuid,
    pub kind: String,
    /// `queued`, `running`, `done` or `failed`.
    pub status: String,
    #[schema(value_type = Object)]
    pub progress: serde_json::Value,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Result of a dry run.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DryRun {
    pub matches: Vec<Uuid>,
    /// Results the rule was evaluated on.
    pub evaluated: u64,
    /// More matches than listed, or the evaluation budget ran out.
    pub truncated: bool,
}

/// Parameters of a `rule.apply` task.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyParams {
    pub rule_id: Uuid,
    pub launch_ids: Vec<Uuid>,
}

type TaskRow = (
    Uuid,
    String,
    String,
    serde_json::Value,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);
/// Fingerprints per (project, launch, launch time): message and count.
type Catalog = BTreeMap<(Uuid, Uuid, DateTime<Utc>), BTreeMap<String, (String, i64)>>;
/// Rows read and evaluated together (off the async threads).
const EVAL_BATCH: usize = 500;

/// A candidate row: id, launch time, series, test case, status, message and trace (both cut
/// to the rule input size in SQL), fingerprint, defect.
type Candidate = (
    Uuid,
    DateTime<Utc>,
    Uuid,
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<serde_json::Value>,
    Option<String>,
);

/// A matching row, without its texts.
struct Matched {
    id: Uuid,
    created: DateTime<Utc>,
    series: Uuid,
    status: String,
    defect: Option<serde_json::Value>,
}

/// Outcome of evaluating a rule on a launch.
struct Evaluated {
    matched: Vec<Matched>,
    evaluated: u64,
    exhausted: bool,
}

/// Evaluates a batch of rows; returns the matches and whether the budget ran out.
fn evaluate_batch(
    rule: &CompiledRule,
    rows: Vec<Candidate>,
    env: Option<&str>,
    budget: &mut Budget,
    evaluated: &mut u64,
    matched: &mut Vec<Matched>,
) -> bool {
    for r in rows {
        let s = Subject {
            status: &r.4,
            message: r.5.as_deref(),
            trace: r.8.as_deref(),
            test_case_id: r.3,
            fingerprint: r.6.as_deref(),
            environment: env,
        };
        match rule.matches(&s, budget) {
            None => return false,
            Some(m) => {
                *evaluated += 1;
                if m {
                    matched.push(Matched {
                        id: r.0,
                        created: r.1,
                        series: r.2,
                        status: r.4,
                        defect: r.7,
                    });
                }
            }
        }
    }
    true
}

impl Store {
    /// Streams the candidate rows of a launch (texts cut in SQL) and evaluates them in batches
    /// on a blocking thread until the launch budget runs out; only the matches are kept.
    async fn evaluate_rule_on_launch(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        rule: &Arc<CompiledRule>,
    ) -> anyhow::Result<Option<Evaluated>> {
        let l: Option<(DateTime<Utc>, Option<String>)> = sqlx::query_as(
            "select created_at, env_key from launches where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(launch_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((created, env)) = l else {
            return Ok(None);
        };
        let statuses: Vec<String> = rule
            .rule
            .matcher
            .statuses
            .clone()
            .unwrap_or_else(|| vec!["failed".into(), "broken".into()]);
        let cut = MATCH_INPUT_BYTES as i32;
        let mut rows = sqlx::query_as::<_, Candidate>(
            "select r.id, r.launch_created_at, r.series_id, r.test_case_id, r.status, left(r.message, $5), r.fingerprint, \
               r.defect, la.trace from results r \
             left join lateral (select case when $4 then left(a.trace, $5) end as trace from attempts a \
               where a.result_id = r.id and a.launch_created_at = r.launch_created_at \
               order by (a.started_at is not null) desc, a.started_at desc nulls last, a.seq desc limit 1) la on true \
             where r.launch_id = $1 and r.launch_created_at = $2 and r.status = any($3) order by left(r.full_name, 600), r.full_name, r.id \
             limit $6",
        )
        .bind(launch_id)
        .bind(created)
        .bind(&statuses)
        .bind(rule.needs_trace())
        .bind(cut)
        .bind(rules::LAUNCH_EVALUATIONS as i64 + 1)
        .fetch(&self.pool);
        let mut budget = Budget::launch();
        let mut matched = Vec::new();
        let mut evaluated = 0u64;
        let mut batch = Vec::with_capacity(EVAL_BATCH);
        let mut more = true;
        while more {
            batch.clear();
            while batch.len() < EVAL_BATCH {
                match rows.try_next().await? {
                    Some(r) => batch.push(r),
                    None => {
                        more = false;
                        break;
                    }
                }
            }
            if batch.is_empty() {
                break;
            }
            let rule = rule.clone();
            let env = env.clone();
            let work = std::mem::replace(&mut batch, Vec::with_capacity(EVAL_BATCH));
            let (b, e, m, go_on) = tokio::task::spawn_blocking(move || {
                let mut m = Vec::new();
                let mut e = 0;
                let go_on =
                    evaluate_batch(&rule, work, env.as_deref(), &mut budget, &mut e, &mut m);
                (budget, e, m, go_on)
            })
            .await?;
            budget = b;
            evaluated += e;
            matched.extend(m);
            if !go_on {
                break;
            }
        }
        Ok(Some(Evaluated {
            matched,
            evaluated,
            exhausted: budget.exhausted,
        }))
    }

    /// Evaluates a rule on a launch without changing anything; `None` when the launch does
    /// not exist.
    pub async fn rule_dry_run(
        &self,
        project_id: Uuid,
        launch_id: Uuid,
        rule: Rule,
    ) -> anyhow::Result<Option<Result<DryRun, String>>> {
        let compiled = match CompiledRule::compile(rule) {
            Ok(c) => Arc::new(c),
            Err(e) => return Ok(Some(Err(e))),
        };
        let Some(ev) = self
            .evaluate_rule_on_launch(project_id, launch_id, &compiled)
            .await?
        else {
            return Ok(None);
        };
        let truncated = ev.exhausted || ev.matched.len() > MAX_DRY_RUN_IDS;
        Ok(Some(Ok(DryRun {
            matches: ev
                .matched
                .iter()
                .take(MAX_DRY_RUN_IDS)
                .map(|m| m.id)
                .collect(),
            evaluated: ev.evaluated,
            truncated,
        })))
    }

    /// Applies a rule to the results of one launch that no person triaged; returns how many
    /// results it changed. Journaled as a `retro` batch by [`super::hook::SYSTEM`].
    pub async fn apply_rule_to_launch(
        &self,
        project_id: Uuid,
        project_key: &str,
        launch_id: Uuid,
        rule: &Arc<CompiledRule>,
    ) -> anyhow::Result<u64> {
        self.apply_rule_to_launch_by(
            project_id,
            project_key,
            launch_id,
            rule,
            super::hook::SYSTEM,
        )
        .await
    }

    /// [`Self::apply_rule_to_launch`] for the task created by `by`: the changes are one
    /// journal batch (`via = retro`, `source = rule`, `by`) with a `triage.decided` event.
    pub async fn apply_rule_to_launch_by(
        &self,
        project_id: Uuid,
        project_key: &str,
        launch_id: Uuid,
        rule: &Arc<CompiledRule>,
        by: &str,
    ) -> anyhow::Result<u64> {
        let Some(ev) = self
            .evaluate_rule_on_launch(project_id, launch_id, rule)
            .await?
        else {
            return Ok(0);
        };
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        // The order every writer of results takes (`triage::lock_launches`): the launch row
        // share-locked, the result rows in id order, the launch row for the counter at the end.
        let live: Option<i32> =
            sqlx::query_scalar("select 1 from launches where id = $1 for key share")
                .bind(launch_id)
                .fetch_optional(&mut *tx)
                .await?;
        if live.is_none() {
            return Ok(0);
        }
        let (by_id, by_group) = type_refs(&mut tx, project_id).await?;
        let mut ids = Vec::new();
        let mut created = Vec::new();
        let mut defects = Vec::new();
        let mut series = BTreeSet::new();
        for r in ev.matched {
            let existing: Option<Defect> = r.defect.and_then(|d| serde_json::from_value(d).ok());
            if is_protected(existing.as_ref()) {
                continue;
            }
            let d = rule_defect(
                rule,
                Status::parse_lenient(&r.status),
                existing.as_ref(),
                &by_id,
                &by_group,
                now,
            );
            ids.push(r.id);
            created.push(r.created);
            defects.push(d.map(serde_json::to_value).transpose()?.map(json_no_nul));
            series.insert(r.series);
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let mut order: Vec<(Uuid, DateTime<Utc>)> =
            ids.iter().copied().zip(created.iter().copied()).collect();
        order.sort();
        sqlx::query(
            "select 1 from results r join unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
               on r.id = t.i and r.launch_created_at = t.c order by r.id for no key update of r",
        )
        .bind(order.iter().map(|o| o.0).collect::<Vec<_>>())
        .bind(order.iter().map(|o| o.1).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await?;
        let before = super::decisions::read_states(&mut tx, &order).await?;
        sqlx::query(
            "update results r set defect = t.d, updated_at = now() \
             from unnest($1::uuid[], $2::timestamptz[], $3::jsonb[]) as t(i, c, d) \
             where r.id = t.i and r.launch_created_at = t.c",
        )
        .bind(&ids)
        .bind(&created)
        .bind(&defects)
        .execute(&mut *tx)
        .await?;
        if let Some(m) = &rule.rule.action.mute {
            let until = m.days.map(|d| now + chrono::Duration::days(d as i64));
            let mutes: BTreeMap<Uuid, (Option<DateTime<Utc>>, String)> = series
                .iter()
                .map(|s| (*s, (until, format!("rule {}", rule.rule.name))))
                .collect();
            upsert_mutes(&mut tx, project_id, &mutes, super::hook::SYSTEM, false).await?;
            sqlx::query(
                "update results r set muted = true, \
                   mute_until = (select m.until from series_mutes m where m.series_id = r.series_id), updated_at = now() \
                 from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                 where r.id = t.i and r.launch_created_at = t.c",
            )
            .bind(&ids)
            .bind(&created)
            .execute(&mut *tx)
            .await?;
            recount_muted(&mut tx, &[launch_id]).await?;
        }
        let after = super::decisions::read_states(&mut tx, &order).await?;
        super::decisions::record(
            &mut tx,
            &super::decisions::Batch {
                project_id,
                project_key,
                via: super::types::DecisionVia::Retro,
                source: super::types::DecisionSource::Rule,
                by,
                suggestion_id: None,
                reverts: None,
            },
            &before,
            &after,
        )
        .await?;
        let mut hits = HashMap::new();
        hits.insert(rule.rule.id, ids.len() as i64);
        rules::add_hits(&mut tx, &hits).await?;
        crate::events::notify_launch(&mut tx, project_key, launch_id).await?;
        tx.commit().await?;
        Ok(ids.len() as u64)
    }

    /// Queues a task; `None` when the project has too many open tasks.
    pub async fn enqueue_task(
        &self,
        project_id: Uuid,
        kind: &str,
        params: serde_json::Value,
        by: &str,
    ) -> anyhow::Result<Option<Task>> {
        let open: i64 = sqlx::query_scalar(
            "select count(*) from analysis_tasks where project_id = $1 and status in ('queued', 'running')",
        )
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?;
        if open >= MAX_OPEN_TASKS {
            return Ok(None);
        }
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into analysis_tasks (id, project_id, kind, params, created_by) values ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(project_id)
        .bind(kind)
        .bind(json_no_nul(params))
        .bind(no_nul(by))
        .execute(&self.pool)
        .await?;
        self.task(project_id, id).await
    }

    pub async fn task(&self, project_id: Uuid, id: Uuid) -> anyhow::Result<Option<Task>> {
        let r: Option<TaskRow> = sqlx::query_as(
            "select id, kind, status, progress, error, created_at, finished_at from analysis_tasks \
                 where project_id = $1 and id = $2",
        )
        .bind(project_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|r| Task {
            id: r.0,
            kind: r.1,
            status: r.2,
            progress: r.3,
            error: r.4,
            created_at: r.5,
            finished_at: r.6,
        }))
    }

    /// Runs queued tasks of the kinds of [`STANDARD_TASK_KINDS`] (one at a time, leased) until
    /// none is left or `budget` is spent. Other kinds (`backtest`, `log_rule.apply`) belong to
    /// the analysis role.
    pub async fn run_tasks(&self, owner: &str, budget: Duration) -> anyhow::Result<u64> {
        let started = Instant::now();
        let mut n = 0;
        while started.elapsed() < budget {
            let claimed: Option<(Uuid, Uuid, String, serde_json::Value, Option<String>)> = sqlx::query_as(
                "update analysis_tasks set status = 'running', lease_owner = $1, \
                   lease_until = now() + interval '10 minutes', started_at = coalesce(started_at, now()) \
                 where id = (select id from analysis_tasks where kind = any($2) and (status = 'queued' \
                   or (status = 'running' and lease_until < now())) order by created_at limit 1 for update skip locked) \
                 returning id, project_id, kind, params, created_by",
            )
            .bind(owner)
            .bind(STANDARD_TASK_KINDS.map(String::from).to_vec())
            .fetch_optional(&self.pool)
            .await?;
            let Some((id, project_id, kind, params, created_by)) = claimed else {
                break;
            };
            let by = created_by.unwrap_or_else(|| super::hook::SYSTEM.to_string());
            let r = match kind.as_str() {
                "rule.apply" => self.run_rule_apply(id, project_id, params, &by).await,
                other => Err(anyhow::anyhow!("unknown task kind {other}")),
            };
            let (status, error, progress) = match r {
                Ok(p) => ("done", None, p),
                Err(e) => ("failed", Some(format!("{e:#}")), serde_json::json!({})),
            };
            sqlx::query(
                "update analysis_tasks set status = $2, error = $3, \
                   progress = case when $2 = 'done' then $4 else progress end, finished_at = now(), \
                   lease_owner = null, lease_until = null where id = $1",
            )
            .bind(id)
            .bind(status)
            .bind(error.as_deref().map(no_nul))
            .bind(progress)
            .execute(&self.pool)
            .await?;
            n += 1;
        }
        Ok(n)
    }

    async fn run_rule_apply(
        &self,
        task_id: Uuid,
        project_id: Uuid,
        params: serde_json::Value,
        by: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let p: ApplyParams = serde_json::from_value(params)?;
        let key: String = sqlx::query_scalar("select key from projects where id = $1")
            .bind(project_id)
            .fetch_one(&self.pool)
            .await?;
        let Some(rule) = self.rule(project_id, p.rule_id).await? else {
            anyhow::bail!("the rule was deleted");
        };
        if !rule.readable() {
            anyhow::bail!("{}", rules::UNREADABLE_RULE);
        }
        let compiled = Arc::new(CompiledRule::compile(rule).map_err(|e| anyhow::anyhow!(e))?);
        let mut updated = 0u64;
        for (i, l) in p.launch_ids.iter().enumerate() {
            updated += self
                .apply_rule_to_launch_by(project_id, &key, *l, &compiled, by)
                .await?;
            sqlx::query(
                "update analysis_tasks set progress = $2, lease_until = now() + interval '10 minutes' where id = $1",
            )
            .bind(task_id)
            .bind(serde_json::json!({"launches": i + 1, "of": p.launch_ids.len(), "updated": updated}))
            .execute(&self.pool)
            .await?;
        }
        Ok(
            serde_json::json!({"launches": p.launch_ids.len(), "of": p.launch_ids.len(), "updated": updated}),
        )
    }

    /// Launches a retroactive application covers: the given one, or the newest of the last
    /// 30 days.
    pub async fn apply_targets(
        &self,
        project_id: Uuid,
        launch_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<Uuid>> {
        Ok(match launch_id {
            Some(l) => sqlx::query_scalar("select id from launches where project_id = $1 and id = $2")
                .bind(project_id)
                .bind(l)
                .fetch_all(&self.pool)
                .await?,
            None => sqlx::query_scalar(
                "select id from launches where project_id = $1 and created_at > now() - interval '30 days' \
                 order by number desc limit $2",
            )
            .bind(project_id)
            .bind(MAX_APPLY_LAUNCHES)
            .fetch_all(&self.pool)
            .await?,
        })
    }

    /// Lifts mutes that ended: their rows go and the results of those series are unmuted.
    /// Returns how many mutes ended (up to 1000 per project and call).
    ///
    /// One project per transaction, which waits for rows held elsewhere for a short while
    /// only: a project whose results or launch rows another transaction holds (a merge, a
    /// manual run) is left for the next run, instead of waiting for it while the rows of other
    /// projects already locked here hold up their writers.
    pub async fn expire_mutes(&self) -> anyhow::Result<u64> {
        Ok(self.expire_mutes_round(&mut HashSet::new()).await?.0)
    }

    /// The mute expiry job: lifts every ended mute, a project at a time in batches of 1000,
    /// and leaves a project whose rows are held for the next run without waiting for it again
    /// in this one. Returns how many mutes ended.
    pub async fn expire_all_mutes(&self) -> anyhow::Result<u64> {
        let mut busy = HashSet::new();
        let mut total = 0;
        loop {
            let (n, more) = self.expire_mutes_round(&mut busy).await?;
            total += n;
            if !more {
                return Ok(total);
            }
        }
    }

    /// One batch per project not in `busy` (busy ones are added to it). Returns the mutes that
    /// ended and whether a project may have more.
    async fn expire_mutes_round(&self, busy: &mut HashSet<Uuid>) -> anyhow::Result<(u64, bool)> {
        let skip: Vec<Uuid> = busy.iter().copied().collect();
        let projects: Vec<Uuid> = sqlx::query_scalar(
            "select distinct project_id from series_mutes where until is not null and until <= now() \
               and project_id <> all($1) limit 1000",
        )
        .bind(&skip)
        .fetch_all(&self.pool)
        .await?;
        let mut expired = 0;
        let mut more = false;
        for project_id in projects {
            match self.expire_mutes_of(project_id).await {
                Ok(n) => {
                    expired += n;
                    more |= n >= 1000;
                }
                Err(e) if crate::merges::lock_timeout(&e) => {
                    metrics::counter!("fastproof_mute_expiry_busy_total").increment(1);
                    tracing::debug!(project = %project_id, "rows held elsewhere; mutes end at the next run");
                    busy.insert(project_id);
                }
                Err(e) => return Err(e),
            }
        }
        Ok((expired, more))
    }

    async fn expire_mutes_of(&self, project_id: Uuid) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("set local lock_timeout = '2s'")
            .execute(&mut *tx)
            .await?;
        let ending: Vec<Uuid> = sqlx::query_scalar(
            "select series_id from series_mutes where project_id = $1 and until is not null \
             and until <= now() order by series_id limit 1000",
        )
        .bind(project_id)
        .fetch_all(&mut *tx)
        .await?;
        if ending.is_empty() {
            return Ok(0);
        }
        // The order every writer of results takes (`triage::lock_launches`): result rows in
        // id order, then the mutes, the launch rows at the end.
        let muted: Vec<(Uuid, DateTime<Utc>, Uuid)> = sqlx::query_as(
            "select id, launch_created_at, series_id from results \
             where series_id = any($1) and muted order by id for no key update",
        )
        .bind(&ending)
        .fetch_all(&mut *tx)
        .await?;
        let expired: Vec<Uuid> = sqlx::query_scalar(
            "delete from series_mutes where series_id = any($1) and until is not null and until <= now() \
             returning series_id",
        )
        .bind(&ending)
        .fetch_all(&mut *tx)
        .await?;
        if expired.is_empty() {
            return Ok(0);
        }
        let expired_set: BTreeSet<Uuid> = expired.iter().copied().collect();
        let (ids, created): (Vec<Uuid>, Vec<DateTime<Utc>>) = muted
            .iter()
            .filter(|r| expired_set.contains(&r.2))
            .map(|r| (r.0, r.1))
            .unzip();
        // The launches of the rows actually unmuted, whatever launch they belong to.
        let launches: Vec<Uuid> = sqlx::query_scalar(
            "update results r set muted = false, mute_until = null, updated_at = now() \
             from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
             where r.id = t.i and r.launch_created_at = t.c and r.muted returning r.launch_id",
        )
        .bind(&ids)
        .bind(&created)
        .fetch_all(&mut *tx)
        .await?;
        let launches: Vec<Uuid> = launches
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        recount_muted(&mut tx, &launches).await?;
        tx.commit().await?;
        Ok(expired.len() as u64)
    }

    /// Fills fingerprints of failed attempts stored without one (rows of earlier versions,
    /// or written by an older replica during an upgrade), with their results and the
    /// fingerprint catalog. Walks attempts by id from a stored cursor; returns rows updated.
    /// Imported history is skipped: an import runs no analysis.
    pub async fn backfill_fingerprints(&self, batch: i64, budget: Duration) -> anyhow::Result<u64> {
        const STATE: &str = "fingerprints.backfill";
        let started = Instant::now();
        let mut updated = 0u64;
        loop {
            let cursor: Option<Uuid> = sqlx::query_scalar::<_, serde_json::Value>(
                "select value from analysis_state where name = $1",
            )
            .bind(STATE)
            .fetch_optional(&self.pool)
            .await?
            .and_then(|v| v["cursor"].as_str().and_then(|c| c.parse().ok()));
            type Row = (
                Uuid,
                DateTime<Utc>,
                Uuid,
                Uuid,
                Uuid,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            );
            let rows: Vec<Row> = sqlx::query_as(
                "select id, launch_created_at, launch_id, project_id, result_id, status, message, \
                   left(trace, 65536), fingerprint from attempts \
                 where ($1::uuid is null or id > $1) order by id limit $2",
            )
            .bind(cursor)
            .bind(batch)
            .fetch_all(&self.pool)
            .await?;
            let Some(last) = rows.last().map(|r| r.0) else {
                return Ok(updated);
            };
            let mut ids = Vec::new();
            let mut created = Vec::new();
            let mut fps = Vec::new();
            let mut results: BTreeSet<(Uuid, DateTime<Utc>)> = BTreeSet::new();
            let mut catalog: Catalog = BTreeMap::new();
            let batch_launches: Vec<Uuid> = rows
                .iter()
                .map(|r| r.2)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let imported: HashSet<Uuid> = sqlx::query_scalar(
                "select id from launches where id = any($1) and attributes @> $2::jsonb",
            )
            .bind(&batch_launches)
            .bind(serde_json::json!([{
                "key": crate::history_import::IMPORT_ATTRIBUTE.0,
                "value": crate::history_import::IMPORT_ATTRIBUTE.1,
            }]))
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .collect();
            for r in &rows {
                if r.8.is_some()
                    || !Status::parse_lenient(&r.5).is_failure()
                    || imported.contains(&r.2)
                {
                    continue;
                }
                let Some(fp) = fingerprint(r.6.as_deref(), r.7.as_deref()) else {
                    continue;
                };
                ids.push(r.0);
                created.push(r.1);
                results.insert((r.4, r.1));
                let e = catalog
                    .entry((r.3, r.2, r.1))
                    .or_default()
                    .entry(fp.hash.clone())
                    .or_insert((fp.normalized_message, 0));
                e.1 += 1;
                fps.push(fp.hash);
            }
            let mut tx = self.pool.begin().await?;
            if !ids.is_empty() {
                sqlx::query(
                    "update attempts a set fingerprint = t.f \
                     from unnest($1::uuid[], $2::timestamptz[], $3::text[]) as t(i, c, f) \
                     where a.id = t.i and a.launch_created_at = t.c and a.fingerprint is null",
                )
                .bind(&ids)
                .bind(&created)
                .bind(&fps)
                .execute(&mut *tx)
                .await?;
                let rids: Vec<Uuid> = results.iter().map(|r| r.0).collect();
                let rcs: Vec<DateTime<Utc>> = results.iter().map(|r| r.1).collect();
                sqlx::query(
                    "update results r set fingerprint = la.fingerprint \
                     from unnest($1::uuid[], $2::timestamptz[]) as t(i, c) \
                     cross join lateral (select a.fingerprint from attempts a \
                       where a.result_id = t.i and a.launch_created_at = t.c \
                       order by (a.started_at is not null) desc, a.started_at desc nulls last, a.seq desc limit 1) la \
                     where r.id = t.i and r.launch_created_at = t.c and r.fingerprint is null \
                       and r.status in ('failed', 'broken')",
                )
                .bind(&rids)
                .bind(&rcs)
                .execute(&mut *tx)
                .await?;
                for ((project, launch, at), m) in &catalog {
                    let f: Vec<String> = m.keys().cloned().collect();
                    let msg: Vec<String> = m.values().map(|v| v.0.clone()).collect();
                    let n: Vec<i64> = m.values().map(|v| v.1).collect();
                    record_fingerprints(&mut tx, *project, *launch, *at, &f, &msg, &n).await?;
                }
                updated += ids.len() as u64;
            }
            sqlx::query(
                "insert into analysis_state (name, value, updated_at) values ($1, $2, now()) \
                 on conflict (name) do update set value = excluded.value, updated_at = now()",
            )
            .bind(STATE)
            .bind(serde_json::json!({"cursor": last}))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            if (rows.len() as i64) < batch || started.elapsed() > budget {
                return Ok(updated);
            }
        }
    }
}
