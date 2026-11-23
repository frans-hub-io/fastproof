//! A cluster-wide scheduler: periodic jobs run on any replica, at most once per due time. A
//! replica claims a due job in `scheduled_jobs` with a lease (no other replica can claim it
//! while the lease holds), renews the lease while the job runs, and sets the next due time when
//! it finishes.
//!
//! ```ignore
//! let mut sched = Scheduler::new();
//! sched.register("partitions.ensure", "6h", |s: AppState| async move {
//!     s.store.ensure_partitions(1, 3).await.map(|_| ())
//! })?;
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, Timelike, Utc};
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

type JobFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
type JobFn = Arc<dyn Fn(AppState) -> JobFuture + Send + Sync>;

/// When a job is due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// A fixed interval after the previous run finished.
    Every(Duration),
    /// A five-field cron expression in UTC (`minute hour day month weekday`) with `*`, `*/n`,
    /// `a-b` and lists.
    Cron(Box<CronSpec>),
}

/// Parsed cron fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronSpec {
    minutes: Vec<bool>,
    hours: Vec<bool>,
    days: Vec<bool>,
    months: Vec<bool>,
    weekdays: Vec<bool>,
}

fn cron_field(s: &str, min: u32, max: u32, what: &str) -> anyhow::Result<Vec<bool>> {
    let mut set = vec![false; (max + 1) as usize];
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (
                r,
                st.parse::<u32>()
                    .with_context(|| format!("{what}: bad step"))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            bail!("{what}: step 0");
        }
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse::<u32>()?, b.parse::<u32>()?)
        } else {
            let v = range
                .parse::<u32>()
                .with_context(|| format!("{what}: bad value {range:?}"))?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            bail!("{what}: {part:?} out of range {min}-{max}");
        }
        let mut v = lo;
        while v <= hi {
            set[v as usize] = true;
            v += step;
        }
    }
    Ok(set)
}

impl Schedule {
    /// `30s`, `5m`, `1h`, `1d` (also `every 5m`) or a five-field cron expression.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let t = s.trim();
        let t = t.strip_prefix("every ").unwrap_or(t).trim();
        let fields: Vec<&str> = t.split_whitespace().collect();
        if fields.len() == 5 {
            return Ok(Schedule::Cron(Box::new(CronSpec {
                minutes: cron_field(fields[0], 0, 59, "minute")?,
                hours: cron_field(fields[1], 0, 23, "hour")?,
                days: cron_field(fields[2], 1, 31, "day")?,
                months: cron_field(fields[3], 1, 12, "month")?,
                weekdays: cron_field(&fields[4].replace('7', "0"), 0, 6, "weekday")?,
            })));
        }
        let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
        let n: u64 = num
            .parse()
            .with_context(|| format!("invalid schedule {s:?}"))?;
        let secs = match unit {
            "s" => n,
            "m" => n * 60,
            "h" => n * 3600,
            "d" => n * 86400,
            _ => bail!("invalid schedule {s:?}: use 30s, 5m, 1h, 1d or a cron expression"),
        };
        if secs == 0 {
            bail!("invalid schedule {s:?}: zero interval");
        }
        Ok(Schedule::Every(Duration::from_secs(secs)))
    }

    /// The next due time after `now`.
    pub fn next_after(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Schedule::Every(d) => {
                now + chrono::Duration::from_std(*d).unwrap_or(chrono::Duration::hours(1))
            }
            Schedule::Cron(c) => {
                let mut t = now
                    .with_second(0)
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(now)
                    + chrono::Duration::minutes(1);
                for _ in 0..(366 * 24 * 60) {
                    if c.minutes[t.minute() as usize]
                        && c.hours[t.hour() as usize]
                        && c.days[t.day() as usize]
                        && c.months[t.month() as usize]
                        && c.weekdays[t.weekday().num_days_from_sunday() as usize]
                    {
                        return t;
                    }
                    t += chrono::Duration::minutes(1);
                }
                now + chrono::Duration::days(1)
            }
        }
    }
}

struct Registered {
    name: String,
    text: String,
    schedule: Schedule,
    run: JobFn,
}

/// The registered jobs of this replica.
#[derive(Default)]
pub struct Scheduler {
    jobs: Vec<Registered>,
    registered: bool,
}

/// Lease of a running job; renewed every third of it.
pub const LEASE: Duration = Duration::from_secs(120);
/// How often due jobs are looked for.
pub const TICK: Duration = Duration::from_secs(5);

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a job. `schedule` is an interval (`30s`, `5m`, `1h`, `1d`) or a cron expression.
    pub fn register<F, Fut>(&mut self, name: &str, schedule: &str, f: F) -> anyhow::Result<()>
    where
        F: Fn(AppState) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        if self.jobs.iter().any(|j| j.name == name) {
            bail!("scheduled job {name} registered twice");
        }
        self.jobs.push(Registered {
            name: name.to_string(),
            text: schedule.to_string(),
            schedule: Schedule::parse(schedule).with_context(|| format!("scheduled job {name}"))?,
            run: Arc::new(move |s| Box::pin(f(s))),
        });
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        self.jobs.iter().map(|j| j.name.clone()).collect()
    }

    /// Upserts every job into `scheduled_jobs`, so they are listed before the first tick.
    pub async fn register_all(&mut self, store: &fastproof_store::Store) -> anyhow::Result<()> {
        for j in &self.jobs {
            store
                .register_scheduled_job(&j.name, &j.text, Utc::now())
                .await
                .with_context(|| format!("registering scheduled job {}", j.name))?;
        }
        self.registered = true;
        Ok(())
    }

    /// Registers the jobs in the database (unless [`Scheduler::register_all`] did) and runs them
    /// when due, until `stop`.
    pub async fn run(mut self, s: AppState, stop: CancellationToken) {
        while !self.registered {
            if let Err(e) = self.register_all(&s.store).await {
                tracing::warn!(error = %format!("{e:#}"), "registering scheduled jobs failed");
                tokio::select! {
                    _ = stop.cancelled() => return,
                    _ = tokio::time::sleep(TICK) => {}
                }
            }
        }
        let jobs: Arc<Vec<Registered>> = Arc::new(self.jobs);
        let names: Vec<String> = jobs.iter().map(|j| j.name.clone()).collect();
        let owner = format!("{}-sched", s.instance);
        loop {
            match s.store.claim_scheduled_jobs(&names, &owner, LEASE).await {
                Ok(claimed) => {
                    for name in claimed {
                        if let Some(idx) = jobs.iter().position(|j| j.name == name) {
                            tokio::spawn(run_one(s.clone(), jobs.clone(), idx, owner.clone()));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "claiming scheduled jobs failed")
                }
            }
            let jitter = Duration::from_millis(rand::random::<u64>() % 1000);
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tokio::time::sleep(TICK + jitter) => {}
            }
        }
    }
}

async fn run_one(s: AppState, jobs: Arc<Vec<Registered>>, idx: usize, owner: String) {
    let job = &jobs[idx];
    let started = Instant::now();
    let hb = {
        let s = s.clone();
        let name = job.name.clone();
        let owner = owner.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEASE / 3).await;
                if let Ok(false) = s.store.heartbeat_scheduled_job(&name, &owner, LEASE).await {
                    tracing::warn!(job = %name, "scheduled job lease lost");
                    return;
                }
            }
        })
    };
    let result = (job.run)(s.clone()).await;
    hb.abort();
    let elapsed = started.elapsed();
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    let outcome = if error.is_some() { "failed" } else { "ok" };
    if let Some(e) = &error {
        tracing::warn!(job = %job.name, error = %e, "scheduled job failed");
    } else {
        tracing::debug!(job = %job.name, ms = elapsed.as_millis() as u64, "scheduled job finished");
    }
    metrics::counter!("fastproof_scheduler_runs_total", "job" => job.name.clone(), "outcome" => outcome).increment(1);
    metrics::histogram!("fastproof_scheduler_duration_seconds", "job" => job.name.clone())
        .record(elapsed.as_secs_f64());
    let next = job.schedule.next_after(Utc::now());
    if let Err(e) = s
        .store
        .finish_scheduled_job(&job.name, &owner, error.as_deref(), elapsed, next)
        .await
    {
        tracing::warn!(job = %job.name, error = %format!("{e:#}"), "recording a scheduled run failed");
    }
}

/// The jobs of stage B.
pub fn standard_jobs() -> anyhow::Result<Scheduler> {
    let mut sch = Scheduler::new();
    sch.register("partitions.ensure", "6h", |s: AppState| async move {
        let n = s.store.ensure_partitions(1, 3).await?;
        if n > 0 {
            tracing::info!(created = n, "result partitions created");
        }
        Ok(())
    })?;
    sch.register("jobs.reap", "30s", |s: AppState| async move {
        let n = s.store.reap_jobs().await?;
        if n > 0 {
            tracing::warn!(
                failed = n,
                "uploads failed after repeated interrupted processing"
            );
        }
        Ok(())
    })?;
    sch.register("uploads.cleanup", "15m", |s: AppState| async move {
        if s.cfg.retain_uploads {
            return Ok(());
        }
        let keys = s
            .blobs
            .list(&fastproof_blob::BlobKey::uploads_prefix())
            .await?;
        let ids: Vec<uuid::Uuid> = keys
            .iter()
            .filter(|(_, at)| Utc::now() - *at > chrono::Duration::hours(1))
            .filter_map(|(k, _)| k.strip_prefix("uploads/").and_then(|i| i.parse().ok()))
            .collect();
        for chunk in ids.chunks(1000) {
            for id in s
                .store
                .deletable_upload_blobs(chunk, Duration::from_secs(3600))
                .await?
            {
                s.blobs
                    .delete(&fastproof_blob::BlobKey::upload(&id.to_string())?)
                    .await?;
            }
        }
        Ok(())
    })?;
    sch.register("outbox.prune", "1h", |s: AppState| async move {
        loop {
            let n = s
                .store
                .prune_outbox(s.cfg.outbox_retention_days, 10_000)
                .await?;
            if n < 10_000 {
                return Ok(());
            }
        }
    })?;
    sch.register("attachments.gc", "1h", |s: AppState| async move {
        loop {
            let n = s
                .store
                .collect_attachments(Duration::from_secs(86_400), 1000)
                .await?;
            if n < 1000 {
                return Ok(());
            }
        }
    })?;
    sch.register("projects.purge", "1m", |s: AppState| async move {
        purge_blobs(&s).await
    })?;
    analysis_jobs(&mut sch)?;
    case_jobs(&mut sch)?;
    media_jobs(&mut sch)?;
    direct_upload_jobs(&mut sch)?;
    Ok(sch)
}

/// Runs queued blob deletions: single blobs (collected attachments) are deleted while their
/// queue entry is held, so an upload of the same content either cancels the deletion or waits
/// for it and stores the blob again; deleted projects lose their prefix, their rows and their
/// analytics rows (through the `project.deleted` event).
pub async fn purge_blobs(s: &AppState) -> anyhow::Result<()> {
    for d in s.store.pending_blob_deletions(500).await? {
        let r = async {
            if d.project_id.is_none() {
                if let Ok(k) = fastproof_blob::BlobKey::parse(&d.prefix) {
                    s.store
                        .delete_blob_locked(&d, || async { s.blobs.delete(&k).await })
                        .await?;
                } else {
                    s.store.finish_blob_deletion(&d).await?;
                }
                return Ok(());
            }
            let key = d.prefix.strip_prefix("projects/").unwrap_or_default();
            let n = s
                .blobs
                .delete_prefix(&fastproof_blob::BlobKey::project_prefix(key)?)
                .await?;
            s.store.finish_blob_deletion(&d).await?;
            tracing::info!(prefix = %d.prefix, blobs = n, "deleted project purged");
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = r {
            s.store.fail_blob_deletion(&d, &format!("{e:#}")).await?;
        }
    }
    Ok(())
}

/// Jobs of the analysis features (stage C).
pub fn analysis_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("analysis.indexes", "6h", |s: AppState| async move {
        let pool = crate::db::unbounded(s.store.pool()).await?;
        let n = fastproof_store::analysis::indexes::ensure_analysis_indexes(&pool).await;
        pool.close().await;
        let n = n?;
        if n > 0 {
            tracing::info!(partitions = n, "fingerprint indexes built");
        }
        Ok(())
    })?;
    sch.register("fingerprints.backfill", "1m", |s: AppState| async move {
        let n = s
            .store
            .backfill_fingerprints(5000, Duration::from_secs(30))
            .await?;
        if n > 0 {
            tracing::info!(attempts = n, "fingerprints filled in");
        }
        Ok(())
    })?;
    sch.register("mutes.expire", "1m", |s: AppState| async move {
        while s.store.expire_mutes().await? >= 1000 {}
        Ok(())
    })?;
    sch.register("analysis.tasks", "10s", |s: AppState| async move {
        let owner = format!("{}-tasks", s.instance);
        s.store.run_tasks(&owner, Duration::from_secs(90)).await?;
        Ok(())
    })?;
    sch.register("history.rollup", "1h", |s: AppState| async move {
        let r = s.store.rollup_due(Duration::from_secs(600)).await?;
        if r.launches > 0 || !r.partitions_dropped.is_empty() {
            tracing::info!(launches = r.launches, dropped = ?r.partitions_dropped, "history rolled up");
        }
        Ok(())
    })?;
    sch.register("retention.apply", "1h", |s: AppState| async move {
        let r = s.store.retention_apply(Duration::from_secs(600)).await?;
        if r.launches_deleted > 0 || r.blobs_queued > 0 {
            tracing::info!(
                launches = r.launches_deleted,
                blobs = r.blobs_queued,
                "retention applied"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Seekable copies of video attachments (new uploads and existing ones).
pub fn media_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("media.prepare", "15s", |s: AppState| async move {
        let r = crate::media_prepare::run(&s, Duration::from_secs(60)).await?;
        if r != crate::media_prepare::RunStats::default() {
            tracing::info!(
                ready = r.ready,
                native = r.native,
                skipped = r.skipped,
                failed = r.failed,
                "videos prepared"
            );
        }
        Ok(())
    })
}

/// Expiry and cleanup of direct uploads (`crate::direct_uploads`).
pub fn direct_upload_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("direct_uploads.sweep", "5m", |s: AppState| async move {
        if s.blobs.backend() != "s3" {
            return Ok(());
        }
        let st = crate::direct_uploads::sweep(&s, crate::direct_uploads::ORPHAN_AGE).await?;
        if st != crate::direct_uploads::SweepStats::default() {
            tracing::info!(
                expired = st.expired,
                orphans = st.orphans,
                pruned = st.pruned,
                "direct uploads swept"
            );
        }
        Ok(())
    })
}

/// Jobs of test-management cases: code sync of finished launches and the backfill of cases
/// from existing tests.
pub fn case_jobs(sch: &mut Scheduler) -> anyhow::Result<()> {
    sch.register("cases.sync", "10s", |s: AppState| async move {
        case_sync(&s, Duration::from_secs(60)).await?;
        Ok(())
    })?;
    sch.register("cases.jobs", "10s", |s: AppState| async move {
        crate::tm_jobs::tick(&s).await
    })?;
    sch.register("cases.backfill", "1m", |s: AppState| async move {
        case_backfill(&s, Duration::from_secs(50)).await?;
        Ok(())
    })?;
    sch.register("cases.versions.prune", "1h", |s: AppState| async move {
        let hour = (chrono::Utc::now().timestamp() / 3600) as usize;
        let versions = s
            .store
            .prune_case_versions(Duration::from_secs(300), 1000, hour)
            .await;
        let people = s.store.prune_people().await;
        if let Ok(n) = &versions
            && *n > 0
        {
            tracing::info!(versions = n, "case versions pruned");
        }
        versions?;
        people?;
        Ok(())
    })?;
    Ok(())
}

/// One round of the `cases.sync` job: code sync of queued launches, then one audit record
/// `case.linkIdentity` (`source: code`) per identity its `ALLURE_ID` moved to another case.
pub async fn case_sync(
    s: &AppState,
    budget: Duration,
) -> anyhow::Result<fastproof_store::tm::sync::SyncStats> {
    let owner = format!("{}-cases-sync", s.instance);
    let r = s.store.run_case_sync(&owner, &s.cfg.tm, budget).await?;
    if r.created + r.linked + r.updated + r.relinked > 0 {
        tracing::info!(
            created = r.created,
            linked = r.linked,
            updated = r.updated,
            relinked = r.relinked,
            "cases synced from code"
        );
    }
    audit_relinks(s, &r.relinks).await;
    Ok(r)
}

/// One round of the `cases.backfill` job: decides the upgrade backfills that are due, then
/// works on one job.
pub async fn case_backfill(
    s: &AppState,
    budget: Duration,
) -> anyhow::Result<fastproof_store::tm::backfill::BackfillRun> {
    s.store
        .enqueue_upgrade_backfills(s.cfg.tm_backfill_delay)
        .await?;
    let owner = format!("{}-cases-backfill", s.instance);
    let r = s.store.run_case_backfill(&owner, &s.cfg.tm, budget).await?;
    if let Some(job) = r.job {
        tracing::info!(job = %job, done = r.done, scanned = r.scanned, created = r.stats.created,
            linked = r.stats.linked, "case backfill ran");
    }
    audit_relinks(s, &r.stats.relinks).await;
    Ok(r)
}

async fn audit_relinks(s: &AppState, relinks: &[fastproof_store::tm::sync::IdentityRelink]) {
    for r in relinks {
        let mut e = franshub_access::AuditEvent::anonymous(
            fastproof_store::tm::sync::SYNC_ACTOR,
            Some("system"),
            "case.linkIdentity",
            serde_json::json!({"number": r.to_number, "testCaseId": r.test_case_id,
                "previousNumber": r.from_number, "archivedDuplicate": r.archived_duplicate,
                "launchId": r.launch_id, "source": "code"}),
        );
        e.scope = Some(r.project_key.clone());
        s.platform.audit(e).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_and_cron() {
        assert_eq!(
            Schedule::parse("30s").unwrap(),
            Schedule::Every(Duration::from_secs(30))
        );
        assert_eq!(
            Schedule::parse("every 5m").unwrap(),
            Schedule::Every(Duration::from_secs(300))
        );
        assert!(Schedule::parse("0s").is_err());
        assert!(Schedule::parse("5 weeks").is_err());
        assert!(Schedule::parse("61 * * * *").is_err());
        let now = DateTime::parse_from_rfc3339("2026-11-21T10:17:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = Schedule::parse("*/15 * * * *").unwrap();
        assert_eq!(c.next_after(now).to_rfc3339(), "2026-11-21T10:30:00+00:00");
        let c = Schedule::parse("0 3 * * 1").unwrap();
        assert_eq!(c.next_after(now).to_rfc3339(), "2026-11-23T03:00:00+00:00");
        let c = Schedule::parse("5 4 1,15 * *").unwrap();
        assert_eq!(c.next_after(now).to_rfc3339(), "2026-12-01T04:05:00+00:00");
    }
}
