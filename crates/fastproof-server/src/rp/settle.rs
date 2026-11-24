//! Settling and emitting (design §6): finished tests wait `FASTPROOF_RP_SETTLE_MS` for late
//! log lines and files, then go to the native ingest as one attempt each (direct: persisted;
//! Kafka: produced to `ingest.results`). Finishing launches end when no test is open, or
//! after the grace period with the open ones interrupted. Pending items whose parent never
//! came become roots. Issues of callers with `results:triage` are applied as triage once
//! their result exists. A replica's watcher does this every 500 ms; claims with leases let
//! replicas share the work.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono::Utc;
use fastproof_model::ReportResult;
use fastproof_rp::assemble::{self, Attached, ErrorText, Item, Options};
use fastproof_rp::classify::{self, result_key};
use fastproof_rp::model::{ExternalIssue, ItemAttribute, ItemParameter};
use fastproof_store::Project;
use fastproof_store::analysis::triage::{TriageIssue, TriageRequest};
use fastproof_store::rp::RpItem;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{ItemRoute, LaunchInfo, PENDING_TTL};
use crate::scheduler::Scheduler;
use crate::state::AppState;

/// Items claimed per round, at most.
const CLAIM: i64 = 200;
/// How long a claim holds before another replica may take the work.
const LEASE_MS: i64 = 30_000;
/// A triage whose result does not appear within this long is given up.
const TRIAGE_GIVE_UP_SECS: f64 = 600.0;

/// A `running` result to write.
pub struct Running {
    pub project_id: Uuid,
    pub project_key: String,
    pub launch_id: Uuid,
    pub result: ReportResult,
}

/// The assembly view of a stored item.
pub fn item_of(r: &RpItem) -> Item {
    Item {
        uuid: r.uuid.clone(),
        parent_uuid: r.parent_uuid.clone(),
        name: r.name.clone(),
        code_ref: r.code_ref.clone(),
        test_case_id: r.test_case_id.clone(),
        description: r.description.clone(),
        parameters: serde_json::from_value::<Vec<ItemParameter>>(r.params.clone())
            .unwrap_or_default(),
        attributes: serde_json::from_value::<Vec<ItemAttribute>>(r.attrs.clone())
            .unwrap_or_default(),
        suite_path: serde_json::from_value(r.suite_path.clone()).unwrap_or_default(),
        start_ms: r.start_ms,
        stop_ms: r.stop_ms,
        status: r.status.clone(),
        attachments: serde_json::from_value::<Vec<Attached>>(r.attachments.clone())
            .unwrap_or_default(),
        error: r
            .error
            .clone()
            .and_then(|e| serde_json::from_value::<ErrorText>(e).ok()),
        num_id: r.num_id,
    }
}

/// Queues the `running` result of a test that started (dropped when the queue is full: the
/// final result still comes).
pub fn send_running(s: &AppState, p: &Project, info: &LaunchInfo, row: &RpItem) {
    let r = Running {
        project_id: p.id,
        project_key: p.key.clone(),
        launch_id: info.launch.id,
        result: assemble::running(&item_of(row)),
    };
    if s.rp.running.try_send(r).is_err() {
        metrics::counter!("fastproof_rp_running_dropped_total").increment(1);
    }
}

/// A test that reported `running` turned out to be a container: its live result ends with
/// the container's own status.
pub async fn end_flipped(s: &AppState, p: &Project, info: &LaunchInfo, row: &RpItem) {
    let (r, _) = assemble::assemble(&item_of(row), &[], &Options::default());
    if let Err(e) = emit(s, p.id, &p.key, info.launch.id, vec![r]).await {
        tracing::warn!(error = %format!("{e:#}"), "ending a ReportPortal container's running result failed");
    }
}

/// Writes `running` results in batches.
pub async fn running_writer(s: AppState, mut rx: mpsc::Receiver<Running>, stop: CancellationToken) {
    loop {
        let first = tokio::select! {
            _ = stop.cancelled() => return,
            r = rx.recv() => match r {
                Some(r) => r,
                None => return,
            },
        };
        let mut batch = vec![first];
        while batch.len() < 500 {
            match rx.try_recv() {
                Ok(r) => batch.push(r),
                Err(_) => break,
            }
        }
        let mut by_launch: BTreeMap<Uuid, (Uuid, String, Vec<ReportResult>)> = BTreeMap::new();
        for r in batch {
            by_launch
                .entry(r.launch_id)
                .or_insert_with(|| (r.project_id, r.project_key.clone(), Vec::new()))
                .2
                .push(r.result);
        }
        for (launch_id, (project_id, key, results)) in by_launch {
            if let Err(e) = emit(&s, project_id, &key, launch_id, results).await {
                tracing::debug!(error = %format!("{e:#}"), "writing running ReportPortal results failed");
            }
        }
    }
}

/// Hands results of a launch to the native ingest: validated and persisted (direct), or
/// validated and produced to `ingest.results` (Kafka), like `POST .../results`.
pub async fn emit(
    s: &AppState,
    project_id: Uuid,
    project_key: &str,
    launch_id: Uuid,
    results: Vec<ReportResult>,
) -> anyhow::Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let Some(launch) = s.store.launch(project_id, launch_id).await? else {
        return Ok(());
    };
    let launch = s.store.follow_merged(launch).await?;
    let lref = crate::api::results::launch_ref(project_key, &launch);
    let env_key = launch.environment.as_ref().and_then(|e| e.key.clone());
    let originals: Option<Vec<ReportResult>> = s.kafka_gateway().then(|| results.clone());
    let pending: Vec<(u64, ReportResult)> = results
        .into_iter()
        .enumerate()
        .map(|(i, r)| (i as u64, r))
        .collect();
    let prep =
        crate::api::results::prepare_reports(s, project_id, env_key.as_deref(), pending).await?;
    for r in &prep.rejected {
        tracing::warn!(project = project_key, launch = %launch_id, reason = %r.message, "a ReportPortal result was rejected");
        metrics::counter!("fastproof_rp_rejected_total").increment(1);
    }
    if let Some(originals) = originals {
        crate::stream::produce_results(
            s,
            &lref,
            prep.accepted
                .iter()
                .map(|(pos, index, a)| {
                    (
                        *index,
                        originals[*pos].clone(),
                        a.result.identity.series_key.clone(),
                    )
                })
                .collect(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }
    let attempts: Vec<_> = prep.accepted.into_iter().map(|(_, _, a)| a).collect();
    s.store.link_attachments(&prep.used, launch.id).await?;
    let out = s.store.persist_attempts(&lref, None, attempts).await?;
    metrics::counter!("fastproof_results_ingested_total", "source" => "rp").increment(out.accepted);
    Ok(())
}

fn issue_parts(row: &RpItem) -> (Option<String>, Vec<ExternalIssue>) {
    let Some(i) = &row.issue else {
        return (None, Vec::new());
    };
    let group = i.get("group").and_then(|g| g.as_str()).map(str::to_string);
    let ext = i
        .get("externalSystemIssues")
        .cloned()
        .and_then(|v| serde_json::from_value::<Vec<ExternalIssue>>(v).ok())
        .unwrap_or_default();
    (group, ext)
}

/// Assembles and emits claimed items (`interrupted`: they never finished), then marks them
/// emitted. Items stay claimed when emitting fails, so another round retries them.
pub async fn emit_items(s: &AppState, rows: Vec<RpItem>, interrupted: bool) -> anyhow::Result<()> {
    let mut by_project: BTreeMap<Uuid, Vec<RpItem>> = BTreeMap::new();
    for r in rows {
        by_project.entry(r.project_id).or_default().push(r);
    }
    for (project_id, rows) in by_project {
        let Some(key) = s.store.rp_project_key(project_id).await? else {
            continue;
        };
        let owners: Vec<String> = rows.iter().map(|r| r.uuid.clone()).collect();
        let mut steps: HashMap<String, Vec<Item>> = HashMap::new();
        for st in s.store.rp_steps_of(project_id, &owners).await? {
            if let Some(o) = &st.owner_uuid {
                steps.entry(o.clone()).or_default().push(item_of(&st));
            }
        }
        let originals: Vec<String> = rows
            .iter()
            .filter(|r| r.code_ref.is_none() && r.test_case_id.is_none())
            .filter_map(|r| r.retry_of.clone())
            .collect();
        let originals: HashMap<String, RpItem> = s
            .store
            .rp_items(project_id, &originals)
            .await?
            .into_iter()
            .map(|r| (r.uuid.clone(), r))
            .collect();
        let mut by_launch: BTreeMap<Uuid, (Vec<ReportResult>, Vec<String>)> = BTreeMap::new();
        for r in &rows {
            let (group, external) = issue_parts(r);
            let opts = Options {
                interrupted,
                fixture_of: (r.role == "fixture").then(|| {
                    serde_json::from_value::<Vec<String>>(r.suite_path.clone())
                        .ok()
                        .and_then(|p| p.last().cloned())
                        .unwrap_or_default()
                }),
                issue_label: group.filter(|_| r.triage_by.is_none()),
                external_issues: external,
                retry_of: r.retry_of.as_ref().and_then(|o| originals.get(o)).map(|o| {
                    (
                        o.code_ref.clone(),
                        o.test_case_id.clone(),
                        serde_json::from_value(o.suite_path.clone()).unwrap_or_default(),
                    )
                }),
            };
            let none = Vec::new();
            let (result, warnings) =
                assemble::assemble(&item_of(r), steps.get(&r.uuid).unwrap_or(&none), &opts);
            for w in warnings {
                tracing::warn!(project = %key, item = %r.uuid, "{w}");
            }
            let e = by_launch.entry(r.launch_id).or_default();
            e.0.push(result);
            e.1.push(r.uuid.clone());
        }
        for (launch_id, (results, uuids)) in by_launch {
            emit(s, project_id, &key, launch_id, results).await?;
            s.store.rp_mark_emitted(project_id, &uuids).await?;
        }
    }
    Ok(())
}

async fn settle_round(s: &AppState) -> anyhow::Result<usize> {
    let claimed = s.store.rp_claim_settled(CLAIM, LEASE_MS).await?;
    let n = claimed.len();
    if n == 0 {
        return Ok(0);
    }
    let mut rows = Vec::with_capacity(n);
    for (r, lag) in claimed {
        metrics::histogram!("fastproof_rp_settle_lag_seconds").record(lag.max(0.0));
        rows.push(r);
    }
    emit_items(s, rows, false).await?;
    Ok(n)
}

/// Finishes the launches whose finish was requested once nothing is open in them.
async fn finish_round(s: &AppState) -> anyhow::Result<()> {
    let grace = chrono::Duration::from_std(s.cfg.rp.finish_grace).unwrap_or_default();
    for f in s.store.rp_claim_finishing(20, LEASE_MS).await? {
        let interrupted = f.force || fastproof_rp::status::launch_interrupted(f.status.as_deref());
        if f.force || f.requested_at + grace <= Utc::now() {
            for _ in 0..1000 {
                let rows = s.store.rp_interrupt_open(f.launch_id, 1000).await?;
                if rows.is_empty() {
                    break;
                }
                emit_items(s, rows, true).await?;
            }
        }
        if s.store.rp_open_items(f.launch_id).await? > 0 {
            s.store.rp_release_finishing(f.project_id, &f.uuid).await?;
            continue;
        }
        if let Some(l) = s.store.launch(f.project_id, f.launch_id).await?
            && l.finished_at.is_none()
        {
            let status = if interrupted {
                "interrupted"
            } else {
                "finished"
            };
            s.store
                .finish_launch(f.project_id, &f.project_key, f.launch_id, status)
                .await?;
            tracing::info!(project = %f.project_key, launch = %f.launch_id, status, "ReportPortal launch finished");
        }
        s.store.rp_mark_done(f.project_id, &f.uuid).await?;
        s.rp.launches.remove(&(f.project_id, f.uuid.clone()));
    }
    Ok(())
}

/// Pending items whose parent did not come within [`PENDING_TTL`] become roots.
async fn pending_round(s: &AppState) -> anyhow::Result<()> {
    let settle = s.cfg.rp.settle.as_millis() as i64;
    for r in s
        .store
        .rp_expired_pending(PENDING_TTL.as_millis() as i64, 100)
        .await?
    {
        let pl = classify::place(&r.uuid, &r.name, &r.item_type, r.has_stats, None);
        if let Some(row) = s
            .store
            .rp_resolve_pending(
                r.project_id,
                &r.uuid,
                &super::item::stored(&pl),
                true,
                settle,
            )
            .await?
        {
            metrics::counter!("fastproof_rp_late_total", "kind" => "orphan").increment(1);
            tracing::warn!(item = %r.uuid, "a ReportPortal item's parent never came; it became a root");
            s.rp.items
                .put((r.project_id, r.uuid.clone()), ItemRoute::of(&row));
            if let Some(key) = s.store.rp_project_key(r.project_id).await?
                && let Some(p) = s.store.project(&key).await?
            {
                super::item::resolve_pending(s, &p, &row).await?;
            }
        }
    }
    Ok(())
}

/// Applies issues of callers with `results:triage` once their results exist.
async fn triage_round(s: &AppState) -> anyhow::Result<()> {
    for (row, key) in s.store.rp_claim_triage(50, LEASE_MS).await? {
        let by = row.triage_by.clone().unwrap_or_default();
        let Some(result_id) = s
            .store
            .result_of_client(row.launch_id, &result_key(&row.uuid))
            .await?
        else {
            if s.store.rp_emitted_age(row.project_id, &row.uuid).await? > TRIAGE_GIVE_UP_SECS {
                tracing::warn!(item = %row.uuid, "the result of a ReportPortal issue never appeared; triage given up");
                s.store.rp_triage_done(row.project_id, &row.uuid).await?;
            }
            continue;
        };
        let Some(project) = s.store.project(&key).await? else {
            s.store.rp_triage_done(row.project_id, &row.uuid).await?;
            continue;
        };
        let issue = row.issue.clone().unwrap_or_default();
        let group = issue
            .get("group")
            .and_then(|g| g.as_str())
            .unwrap_or_default();
        let custom = issue
            .get("custom")
            .and_then(|c| c.as_str())
            .and_then(|c| c.parse::<Uuid>().ok());
        let types = s.store.defect_types(project.id).await?;
        let defect = custom
            .filter(|c| types.iter().any(|t| t.id == *c))
            .or_else(|| {
                types
                    .iter()
                    .find(|t| t.built_in && t.group == group)
                    .map(|t| t.id)
            });
        let (_, external) = issue_parts(&row);
        let req = TriageRequest {
            result_ids: Some(vec![result_id]),
            defect_type_id: defect,
            comment: issue
                .get("comment")
                .and_then(|c| c.as_str())
                .map(str::to_string)
                .filter(|c| !c.is_empty()),
            issue: external
                .into_iter()
                .find(|x| {
                    x.url
                        .as_deref()
                        .is_some_and(|u| u.starts_with("https://") || u.starts_with("http://"))
                })
                .map(|x| TriageIssue {
                    url: x.url,
                    key: x.ticket_id,
                }),
            ..Default::default()
        };
        if defect.is_some() {
            match crate::api::analysis::apply_triage(s, &project, &by, req).await {
                Ok((out, body)) => {
                    let mut ev = franshub_access::AuditEvent::anonymous(
                        &by,
                        Some("token"),
                        "results.triage",
                        serde_json::json!({"updated": out.updated, "resultIds": [result_id],
                            "defectTypeId": body.defect_type_id, "comment": body.comment.is_some(),
                            "issue": body.issue.as_ref().map(|i| &i.url), "source": "reporter"}),
                    );
                    ev.scope = Some(project.key.clone());
                    s.platform.audit(ev).await;
                }
                Err(e) => {
                    tracing::warn!(item = %row.uuid, error = %e, "applying a ReportPortal issue failed")
                }
            }
        }
        s.store.rp_triage_done(row.project_id, &row.uuid).await?;
    }
    Ok(())
}

/// The replica's watcher: settles every 500 ms (at once when woken), finishes launches,
/// places orphans and applies issues.
pub async fn watcher(s: AppState, stop: CancellationToken) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut round = 0u64;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tick.tick() => {}
            _ = s.rp.wake.notified() => {}
        }
        for _ in 0..20 {
            match settle_round(&s).await {
                Ok(n) if n as i64 == CLAIM => continue,
                Ok(_) => break,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "settling ReportPortal results failed");
                    break;
                }
            }
        }
        if let Err(e) = finish_round(&s).await {
            tracing::warn!(error = %format!("{e:#}"), "finishing ReportPortal launches failed");
        }
        if round.is_multiple_of(4) {
            if let Err(e) = pending_round(&s).await {
                tracing::warn!(error = %format!("{e:#}"), "placing ReportPortal orphans failed");
            }
            if let Err(e) = triage_round(&s).await {
                tracing::warn!(error = %format!("{e:#}"), "applying ReportPortal issues failed");
            }
        }
        round += 1;
    }
}

/// Cluster-wide jobs: idle launches finish as interrupted, the working set of launches done a
/// day ago goes, and the item gauges are set.
pub fn register_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("rp.maintain", "1m", |s: AppState| async move {
        if !s.cfg.rp.enabled {
            return Ok(());
        }
        let idle = s
            .store
            .rp_expire_idle(s.cfg.rp.idle_timeout.as_secs() as i64)
            .await?;
        if idle > 0 {
            tracing::warn!(
                launches = idle,
                "idle ReportPortal launches finish as interrupted"
            );
            metrics::counter!("fastproof_rp_idle_launches_total").increment(idle);
        }
        for _ in 0..20 {
            if s.store
                .rp_cleanup(super::KEEP_AFTER_DONE.as_secs() as i64, 5000)
                .await?
                < 5000
            {
                break;
            }
        }
        let mut pending = 0;
        for state in ["pending", "started", "finished", "emitting", "emitted"] {
            metrics::gauge!("fastproof_rp_items", "state" => state).set(0.0);
        }
        for (state, n) in s.store.rp_item_counts().await? {
            if state == "pending" {
                pending = n;
            }
            metrics::gauge!("fastproof_rp_items", "state" => state).set(n as f64);
        }
        metrics::gauge!("fastproof_rp_pending_parents").set(pending as f64);
        Ok(())
    })
}
