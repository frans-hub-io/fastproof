//! The integrations worker of each replica: consumes `launch.finished` from the outbox (one
//! replica at a time), evaluates gates once the launch's uploads are processed, queues
//! notifications and VCS updates in the same transaction as the gate result, sends due
//! deliveries with exponential backoff, refreshes quality metrics, prunes old deliveries and
//! re-encrypts secrets after a key rotation.

use std::time::{Duration, Instant};

use chrono::Utc;
use fastproof_store::integrations::{DeliveryJob, NewDelivery};
use fastproof_store::{Launch, Project};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::gates::{self, Evaluation, GateStatus};
use super::notify::{self, NewFailure, NewFailures, Outcome, Summary, SummaryLaunch};
use super::settings::{IntegrationSettings, Notification, VcsSettings};
use super::{quality, vcs};
use crate::state::AppState;

/// Name of the outbox cursor.
pub const CONSUMER: &str = "integrations";
/// How long a launch with unprocessed uploads waits before its gate is evaluated anyway.
pub const MAX_UPLOAD_WAIT: Duration = Duration::from_secs(1800);
/// Deliveries sent per round.
pub const BATCH: i64 = 10;
/// Days finished deliveries are kept.
pub const KEEP_DAYS: i64 = 30;

/// The launch page.
pub fn launch_url(s: &AppState, project: &str, id: Uuid) -> Option<String> {
    page_url(s, &format!("p/{project}/launches/{id}"))
}

/// A page of the UI under `FASTPROOF_PUBLIC_URL`.
pub fn page_url(s: &AppState, path: &str) -> Option<String> {
    let base = s.cfg.public_url.as_ref()?;
    base.join(path).ok().map(|u| u.to_string())
}

/// The summary messages are built from.
pub fn summary(
    s: &AppState,
    project: &Project,
    launch: &Launch,
    ev: &Evaluation,
    events: Vec<String>,
) -> Summary {
    let vcs = launch.vcs.clone().unwrap_or_default();
    let ci = launch.ci.clone().unwrap_or_default();
    Summary {
        project: project.key.clone(),
        launch: SummaryLaunch {
            id: launch.id,
            number: launch.number,
            name: launch.name.clone(),
            status: launch.status.clone(),
            branch: vcs.branch,
            environment: gates::env_of(launch).map(str::to_string),
            repository: vcs.repository,
            commit: vcs.commit,
            merge_request: ci.merge_request,
            ci_url: ci.url,
            url: launch_url(s, &project.key, launch.id),
            duration_ms: launch.duration_ms,
            counts: launch.counts.clone(),
        },
        gate: ev.report.clone(),
        pass_rate: ev.facts.pass_rate(),
        new_failures: NewFailures {
            count: ev.new_failures,
            names: ev
                .new_failure_sample
                .iter()
                .map(|(_, n)| n.clone())
                .collect(),
            items: ev
                .new_failure_sample
                .iter()
                .map(|(id, n)| NewFailure {
                    name: n.clone(),
                    result_id: *id,
                    url: page_url(s, &format!("p/{}/results/{id}", project.key)),
                })
                .collect(),
            baseline_launch_id: ev.previous_launch_id,
        },
        events,
        test: false,
    }
}

/// The deliveries of a finished launch.
pub fn deliveries(
    settings: &IntegrationSettings,
    launch: &Launch,
    summary: &Summary,
    max_attempts: i32,
) -> Vec<NewDelivery> {
    let mut out = Vec::new();
    if launch.debug {
        return out;
    }
    let attributes: Vec<(String, String)> = launch
        .attributes
        .iter()
        .map(|a| (a.key.clone(), a.value.clone()))
        .collect();
    for n in &settings.notifications {
        if let Some(f) = &n.filter
            && !f.matches(
                gates::branch_of(launch),
                gates::env_of(launch),
                &launch.name,
                &attributes,
            )
        {
            continue;
        }
        let events: Vec<String> = summary
            .events
            .iter()
            .filter(|e| n.on.contains(e))
            .cloned()
            .collect();
        if events.is_empty() {
            continue;
        }
        let mut sm = summary.clone();
        sm.events = events.clone();
        out.push(NewDelivery {
            channel: n.kind.clone(),
            target_id: n.id.clone(),
            events,
            payload: json!({"summary": sm, "notification": n}),
            max_attempts,
        });
    }
    if let Some(v) = &settings.vcs
        && vcs::applies(v, summary)
    {
        out.push(NewDelivery {
            channel: v.provider.clone(),
            target_id: "vcs".into(),
            events: summary.events.clone(),
            payload: json!({"summary": summary, "vcs": v}),
            max_attempts,
        });
    }
    out
}

/// The notification events of an evaluation. The first evaluation of a launch sends
/// `launchFinished` (plus `gateFailed`, `newFailures` and, for a manual run closed with the
/// launch, `manualRunClosed`); a re-evaluation after a manual run closed on an already finished
/// launch sends `manualRunClosed` only, so subscribers do not hear about the same finish twice.
pub fn evaluation_events(
    reevaluated: bool,
    gate_failed: bool,
    new_failures: bool,
    manual_closed: bool,
) -> Vec<String> {
    if reevaluated {
        return if manual_closed {
            vec!["manualRunClosed".to_string()]
        } else {
            Vec::new()
        };
    }
    let mut events = vec!["launchFinished".to_string()];
    if gate_failed {
        events.push("gateFailed".into());
    }
    if new_failures {
        events.push("newFailures".into());
    }
    if manual_closed {
        events.push("manualRunClosed".into());
    }
    events
}

async fn process_launch(
    s: &AppState,
    project_id: Uuid,
    launch_id: Uuid,
    queued_at: chrono::DateTime<Utc>,
    reevaluated: bool,
) -> anyhow::Result<()> {
    let Some(key) = s.store.project_key(project_id).await? else {
        return Ok(());
    };
    let (Some(project), Some(launch)) = (
        s.store.project(&key).await?,
        s.store.launch(project_id, launch_id).await?,
    ) else {
        return Ok(());
    };
    let waited = (Utc::now() - queued_at).to_std().unwrap_or_default();
    if gates::is_pending(&launch) && waited < MAX_UPLOAD_WAIT {
        return Ok(());
    }
    let doc = s.store.project_settings_json(project_id).await?;
    let settings = IntegrationSettings::parse(&doc).unwrap_or_else(|e| {
        tracing::warn!(project = %key, error = %e, "integration settings are invalid; skipping gates and notifications");
        IntegrationSettings::default()
    });
    let ev = gates::evaluate_launch(s, &project, &launch, &settings.gates).await?;
    let events = evaluation_events(
        reevaluated,
        ev.report.status == GateStatus::Failed,
        ev.new_failures > 0,
        launch
            .manual
            .is_some_and(|m| m.state == fastproof_store::tm::ManualState::Closed),
    );
    let sm = summary(s, &project, &launch, &ev, events);
    let ds = deliveries(
        &settings,
        &launch,
        &sm,
        s.cfg.integrations.delivery_max_attempts,
    );
    if s.store
        .complete_gate(
            project_id,
            launch_id,
            ev.report.status.as_str(),
            serde_json::to_value(&ev.report)?,
            &ds,
        )
        .await?
    {
        if !launch.debug && !reevaluated {
            quality::launch_finished(&key, launch.duration_ms);
        }
        tracing::info!(project = %key, launch = %launch_id, gate = ev.report.status.as_str(), deliveries = ds.len(), "launch evaluated");
    }
    Ok(())
}

async fn send(s: &AppState, job: &DeliveryJob) -> Outcome {
    let Ok(sm) = serde_json::from_value::<Summary>(job.payload["summary"].clone()) else {
        return Outcome::Fail("the delivery payload is unreadable".into(), None);
    };
    match job.channel.as_str() {
        "gitlab" | "github" => {
            match serde_json::from_value::<VcsSettings>(job.payload["vcs"].clone()) {
                Ok(v) => vcs::deliver(s, job.project_id, &v, &sm).await,
                Err(_) => Outcome::Fail("the delivery payload is unreadable".into(), None),
            }
        }
        _ => match serde_json::from_value::<Notification>(job.payload["notification"].clone()) {
            Ok(n) => notify::send(s, job.project_id, job.id, &n, &sm).await,
            Err(_) => Outcome::Fail("the delivery payload is unreadable".into(), None),
        },
    }
}

/// The longest delay between two attempts.
pub const MAX_DELAY: Duration = Duration::from_secs(3600);

/// The delay before attempt `attempts + 1`.
pub fn backoff(base: Duration, attempts: i32) -> Duration {
    let exp = attempts.clamp(1, 16) as u32 - 1;
    base.saturating_mul(1u32 << exp.min(12)).min(MAX_DELAY)
}

async fn deliver(s: &AppState, job: DeliveryJob) {
    let outcome = send(s, &job).await;
    let (ok, err, code, retry) = match outcome {
        Outcome::Delivered(c) => (true, None, c, None),
        Outcome::Retry(e, c) if job.attempts < job.max_attempts => {
            let d = backoff(s.cfg.integrations.delivery_backoff, job.attempts);
            (
                false,
                Some(e),
                c,
                Some(Utc::now() + chrono::Duration::from_std(d).unwrap_or_default()),
            )
        }
        Outcome::Throttled(e, c, after) if job.attempts < job.max_attempts => {
            let d = backoff(s.cfg.integrations.delivery_backoff, job.attempts)
                .max(after)
                .min(MAX_DELAY);
            (
                false,
                Some(e),
                c,
                Some(Utc::now() + chrono::Duration::from_std(d).unwrap_or_default()),
            )
        }
        Outcome::Retry(e, c) | Outcome::Throttled(e, c, _) | Outcome::Fail(e, c) => {
            (false, Some(e), c, None)
        }
    };
    let label = if ok {
        "delivered"
    } else if retry.is_some() {
        "retry"
    } else {
        "failed"
    };
    metrics::counter!("fastproof_deliveries_total", "kind" => job.channel.clone(), "outcome" => label)
        .increment(1);
    if !ok {
        tracing::warn!(project = %job.project_key, delivery = %job.id, kind = %job.channel, target = %job.target_id,
            attempt = job.attempts, error = err.as_deref().unwrap_or_default(), "delivery failed");
    }
    if let Err(e) = s
        .store
        .finish_delivery(job.id, ok, err.as_deref(), code.map(i32::from), retry)
        .await
    {
        tracing::error!(error = %format!("{e:#}"), "recording a delivery failed");
    }
}

/// One round: outbox, waiting launches, due deliveries.
pub async fn tick(s: &AppState) -> anyhow::Result<()> {
    let cfg = &s.cfg.integrations;
    s.store
        .consume_finished_launches(CONSUMER, cfg.settle, 500)
        .await?;
    for (p, l, queued, reevaluated) in s.store.waiting_gates(50).await? {
        if let Err(e) = process_launch(s, p, l, queued, reevaluated).await {
            tracing::warn!(launch = %l, error = %format!("{e:#}"), "evaluating a launch failed");
        }
    }
    for _ in 0..5 {
        let jobs = s
            .store
            .claim_deliveries(BATCH, Duration::from_secs(120))
            .await?;
        if jobs.is_empty() {
            break;
        }
        futures::future::join_all(jobs.into_iter().map(|j| deliver(s, j))).await;
    }
    Ok(())
}

async fn reseal(s: &AppState) {
    let Some(keys) = s.cfg.integrations.secrets.clone() else {
        return;
    };
    let mut done = 0u64;
    loop {
        let rows = match s.store.secrets_not_under_key(keys.active_id(), 100).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "listing secrets to re-encrypt failed");
                return;
            }
        };
        let mut progressed = false;
        for (p, name, sealed) in &rows {
            let Ok(v) = keys.open(*p, name, sealed) else {
                continue;
            };
            let Ok(new) = keys.seal(
                *p,
                name,
                sealed.purpose.as_deref(),
                sealed.origin.as_deref(),
                v.expose(),
            ) else {
                continue;
            };
            if s.store
                .reseal_secret(*p, name, &sealed.key_id, &new)
                .await
                .unwrap_or(false)
            {
                done += 1;
                progressed = true;
            }
        }
        if !progressed {
            if !rows.is_empty() {
                tracing::warn!(
                    count = rows.len(),
                    "some secrets use a key that is not in FASTPROOF_SECRETS_KEY_FILE"
                );
            }
            break;
        }
    }
    if done > 0 {
        tracing::info!(
            count = done,
            key = keys.active_id(),
            "re-encrypted secrets with the active key"
        );
    }
}

/// Runs until `stop`.
pub async fn run(s: AppState, stop: CancellationToken) {
    reseal(&s).await;
    let mut last_metrics: Option<Instant> = None;
    let mut last_prune: Option<Instant> = None;
    loop {
        if let Err(e) = tick(&s).await {
            tracing::warn!(error = %format!("{e:#}"), "integrations round failed");
        }
        if last_metrics.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            last_metrics = Some(Instant::now());
            if let Err(e) = quality::refresh(&s).await {
                tracing::warn!(error = %format!("{e:#}"), "refreshing quality metrics failed");
            }
        }
        if last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(6 * 3600)) {
            last_prune = Some(Instant::now());
            let _ = s.store.prune_deliveries(KEEP_DAYS).await;
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(s.cfg.integrations.poll) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let b = Duration::from_secs(30);
        assert_eq!(backoff(b, 1), Duration::from_secs(30));
        assert_eq!(backoff(b, 2), Duration::from_secs(60));
        assert_eq!(backoff(b, 4), Duration::from_secs(240));
        assert_eq!(backoff(b, 20), Duration::from_secs(3600));
    }

    #[test]
    fn a_closed_manual_run_on_a_finished_launch_sends_only_its_own_event() {
        assert_eq!(
            evaluation_events(false, true, true, false),
            vec!["launchFinished", "gateFailed", "newFailures"]
        );
        assert_eq!(
            evaluation_events(false, false, false, true),
            vec!["launchFinished", "manualRunClosed"],
            "a purely manual launch finishes when its run closes"
        );
        assert_eq!(
            evaluation_events(true, true, true, true),
            vec!["manualRunClosed"]
        );
        assert!(evaluation_events(true, true, false, false).is_empty());
    }
}
