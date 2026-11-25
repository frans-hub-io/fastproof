//! Log rules of the analysis role: the `log_rules` step and the `log_rule.apply` task
//! (contract §7.6). Rules that read logs or signals or mark a pattern never run at ingest;
//! this step evaluates them once the launch has finished and settled, so late log lines (the
//! ReportPortal agents that batch them) are read too.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use fastproof_store::analysis::rules::{CompiledRule, RulePhase, UNREADABLE_RULE, enabled_rules};
use fastproof_store::analysis::tasks::ApplyParams;
use fastproof_store::analysis::types::{ClaimedTask, LaunchCtx, StepOutcome};

use super::pipeline::LaunchJob;
use crate::state::AppState;

/// Whether this build evaluates log rules (`AnalysisFeatures.logPatterns`).
pub const AVAILABLE: bool = true;
/// Wall time one `log_rule.apply` task spends, at most; the launches it did not reach are
/// reported (`partial`).
pub const RETRO_TASK_BUDGET: Duration = Duration::from_secs(300);
/// Log lines asked from ClickHouse for one retroactive application, at most.
pub const RETRO_CLICKHOUSE_MATCHES: usize = 50_000;

/// Step `log_rules`: the project's enabled after-launch rules on the launch's failed and
/// broken attempts, within `log_cpu_ms`.
pub async fn on_launch(s: &AppState, job: &LaunchJob) -> anyhow::Result<StepOutcome> {
    let rules = {
        let mut conn = s.store.pool().acquire().await?;
        enabled_rules(&mut conn, job.project.id, RulePhase::AfterLaunch).await?
    };
    if rules.is_empty() {
        return Ok(StepOutcome::skipped("no log rules"));
    }
    let ctx = LaunchCtx {
        project_id: job.project.id,
        project_key: &job.project.key,
        launch: &job.launch,
        settings: &job.settings,
        limits: &job.limits,
        now: Utc::now(),
    };
    s.store
        .evaluate_log_rules(&ctx, &rules, Duration::from_millis(job.limits.log_cpu_ms))
        .await
}

/// Task kind `log_rule.apply`: one after-launch rule on the launches the task lists that are
/// within the project's raw retention, as the person who queued it. ClickHouse answers first
/// when it holds the logs; otherwise each launch is read from PostgreSQL within `log_cpu_ms`,
/// and the whole task within [`RETRO_TASK_BUDGET`].
pub async fn run_retro_task(s: &AppState, task: &ClaimedTask) -> anyhow::Result<serde_json::Value> {
    let p: ApplyParams = serde_json::from_value(task.params.clone())?;
    let Some(key) = s.store.project_key(task.project_id).await? else {
        anyhow::bail!("the project was deleted");
    };
    let Some(project) = s.store.project(&key).await? else {
        anyhow::bail!("the project was deleted");
    };
    let Some(rule) = s.store.rule(task.project_id, p.rule_id).await? else {
        anyhow::bail!("the rule was deleted");
    };
    if !rule.readable() {
        anyhow::bail!("{UNREADABLE_RULE}");
    }
    if rule.phase() != RulePhase::AfterLaunch {
        anyhow::bail!("the rule no longer reads logs; apply it again");
    }
    let settings = s.store.project_settings(task.project_id).await?;
    let limits = s.cfg.analysis.limits.clone();
    let since = Utc::now() - chrono::Duration::days(i64::from(settings.retention.raw_days));
    let compiled = Arc::new(CompiledRule::compile(rule.clone()).map_err(|e| anyhow::anyhow!(e))?);
    let by = task.created_by.as_str();
    if rule.matcher.log.is_some()
        && let Some(matches) = super::clickhouse::retro_log_matches(
            s,
            &project,
            &rule,
            since,
            RETRO_CLICKHOUSE_MATCHES,
        )
        .await?
    {
        let launches: std::collections::HashSet<uuid::Uuid> =
            p.launch_ids.iter().copied().collect();
        let matches: Vec<_> = matches
            .into_iter()
            .filter(|m| launches.contains(&m.launch_id))
            .collect();
        let r = s
            .store
            .apply_log_matches(task.project_id, &compiled, &matches, by)
            .await?;
        return Ok(
            serde_json::json!({"backend": "clickhouse", "launches": p.launch_ids.len(),
            "of": p.launch_ids.len(), "matched": r.matched, "marked": r.writes.marked,
            "updated": r.writes.updated, "partial": r.partial}),
        );
    }
    let started = Instant::now();
    let (mut done, mut matched, mut marked, mut updated, mut old, mut partial) =
        (0usize, 0u64, 0u64, 0u64, 0u64, false);
    let of = p.launch_ids.len();
    for l in &p.launch_ids {
        if started.elapsed() >= RETRO_TASK_BUDGET {
            partial = true;
            break;
        }
        let Some(launch) = s.store.launch(task.project_id, *l).await? else {
            done += 1;
            continue;
        };
        if launch.created_at < since {
            old += 1;
            done += 1;
            continue;
        }
        let r = s
            .store
            .apply_log_rule_to_launch(
                task.project_id,
                launch.id,
                &compiled,
                &limits,
                Duration::from_millis(limits.log_cpu_ms),
                by,
            )
            .await?;
        matched += r.matched;
        marked += r.writes.marked;
        updated += r.writes.updated;
        partial |= r.partial;
        done += 1;
        s.store
            .task_progress(
                task,
                serde_json::json!({"launches": done, "of": of, "matched": matched,
                    "marked": marked, "updated": updated}),
                super::tasks::LEASE,
            )
            .await?;
    }
    Ok(
        serde_json::json!({"backend": "postgres", "launches": done, "of": of,
        "matched": matched, "marked": marked, "updated": updated, "outsideRetention": old,
        "partial": partial || done < of}),
    )
}
