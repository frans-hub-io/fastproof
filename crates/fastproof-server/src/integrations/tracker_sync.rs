//! Reads the state of tracked issues back from their trackers: open issues every
//! `FASTPROOF_TRACKER_POLL_SECONDS` (`sync: poll`), on a webhook from the tracker
//! (`sync: webhook`, plus a daily read), closed issues daily (they may be reopened). Failed
//! reads back off exponentially up to six hours; a throttled tracker is left alone for as long
//! as it asked.
//!
//! A webhook only says "read this issue now": its payload is never trusted for the state,
//! which always comes from the tracker's API with FastProof's token.

use std::collections::HashMap;
use std::time::Duration;

use axum::http::HeaderMap;
use base64::Engine as _;
use chrono::Utc;
use fastproof_store::trackers::{SyncJob, SyncOutcome};
use ring::hmac;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::settings::{IntegrationSettings, IssueTracker, TrackerApi};
use super::trackers::{Client, TrackerError};
use crate::state::AppState;

/// Issues read per round, at most.
pub const BATCH: i64 = 20;
/// Lease of a claimed issue.
const LEASE: Duration = Duration::from_secs(120);
/// Reads of closed issues, and of webhook-synced ones without a webhook.
pub const DAILY: Duration = Duration::from_secs(24 * 3600);
/// The longest delay after failed reads.
const MAX_BACKOFF: Duration = Duration::from_secs(6 * 3600);
/// Least time between two reads of one issue asked for by a user or a webhook.
pub const MIN_GAP: Duration = Duration::from_secs(5);

/// When to read an issue again after a successful read; `None` with `sync: off`.
pub fn next_read(api: &TrackerApi, closed: bool, poll: Duration) -> Option<Duration> {
    match api.sync_mode() {
        "off" => None,
        _ if closed => Some(DAILY),
        "webhook" => Some(DAILY),
        _ => Some(poll),
    }
}

/// The delay after `attempts` failed reads in a row: a minute, doubled, at most six hours.
pub fn backoff(attempts: i32) -> Duration {
    let n = attempts.clamp(0, 12) as u32;
    Duration::from_secs(60u64 << n).min(MAX_BACKOFF)
}

fn at(d: Duration) -> chrono::DateTime<Utc> {
    Utc::now() + chrono::Duration::from_std(d).unwrap_or_default()
}

/// Reads one issue and stores what the tracker says. Returns whether it was closed by this
/// read.
pub async fn sync_one(st: &AppState, tracker: Option<&IssueTracker>, job: &SyncJob) -> bool {
    let poll = st.cfg.integrations.tracker_poll;
    let outcome = match tracker.and_then(|t| t.api.as_ref().map(|a| (t, a))) {
        None => SyncOutcome::Failed(
            "the tracker is no longer in the project settings".into(),
            Some(at(DAILY)),
        ),
        Some((t, api)) => match Client::new(st, job.project_id, t).await {
            Err(e) => SyncOutcome::Failed(
                e.to_string(),
                Some(at(backoff(job.attempts).max(DAILY / 24))),
            ),
            Ok(c) => match c
                .get(&job.key, job.remote_id.as_deref(), job.etag.as_deref())
                .await
            {
                Ok(Some((s, etag))) => {
                    let next = next_read(api, s.closed, poll).map(at);
                    return finish(st, job, &SyncOutcome::Read(s, etag), next).await;
                }
                Ok(None) => {
                    let next = next_read(api, job.closed, poll).map(at);
                    return finish(st, job, &SyncOutcome::Unchanged, next).await;
                }
                Err(TrackerError::Throttled(d)) => {
                    SyncOutcome::Failed(TrackerError::Throttled(d).to_string(), Some(at(d)))
                }
                Err(e @ TrackerError::NotFound) => {
                    SyncOutcome::Failed(e.to_string(), Some(at(DAILY)))
                }
                Err(e) => SyncOutcome::Failed(e.to_string(), Some(at(backoff(job.attempts)))),
            },
        },
    };
    if let SyncOutcome::Failed(e, _) = &outcome {
        tracing::warn!(project = %job.project_key, issue = %job.id, tracker = %job.tracker_id,
            attempt = job.attempts + 1, error = %e, "reading a tracker issue failed");
    }
    finish(st, job, &outcome, None).await
}

async fn finish(
    st: &AppState,
    job: &SyncJob,
    outcome: &SyncOutcome,
    next: Option<chrono::DateTime<Utc>>,
) -> bool {
    let label = match outcome {
        SyncOutcome::Read(..) => "read",
        SyncOutcome::Unchanged => "unchanged",
        SyncOutcome::Failed(..) => "failed",
    };
    metrics::counter!("fastproof_tracker_syncs_total", "tracker" => job.tracker_type.clone(), "outcome" => label)
        .increment(1);
    match st.store.finish_tracker_sync(job, outcome, next).await {
        Ok(closed) => {
            if closed {
                tracing::info!(project = %job.project_key, issue = %job.id, key = %job.key, "a tracked issue was closed");
            }
            closed
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "recording a tracker read failed");
            false
        }
    }
}

/// One round: claims due issues and reads them.
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    let jobs = st.store.claim_tracker_syncs(BATCH, LEASE).await?;
    let n = jobs.len();
    let mut settings: HashMap<Uuid, IntegrationSettings> = HashMap::new();
    for job in &jobs {
        if let std::collections::hash_map::Entry::Vacant(e) = settings.entry(job.project_id) {
            let doc = st.store.project_settings_json(job.project_id).await?;
            e.insert(IntegrationSettings::parse(&doc).unwrap_or_default());
        }
    }
    let reads = jobs.iter().map(|job| {
        let trackers = settings
            .get(&job.project_id)
            .map(|s| s.issue_trackers.as_slice())
            .unwrap_or_default();
        let t = IssueTracker::connected(trackers, &job.tracker_id);
        sync_one(st, t, job)
    });
    futures::future::join_all(reads).await;
    Ok(n)
}

/// Runs until `stop`.
pub async fn run(st: AppState, stop: CancellationToken) {
    loop {
        let busy = match tick(&st).await {
            Ok(n) => n as i64 >= BATCH,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "tracker sync round failed");
                false
            }
        };
        let wait = if busy {
            Duration::from_millis(200)
        } else {
            st.cfg.integrations.tracker_tick
        };
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    let k = hmac::Key::new(hmac::HMAC_SHA256, b"fastproof-webhook-compare");
    let tag = hmac::sign(&k, a);
    hmac::verify(&k, b, tag.as_ref()).is_ok()
}

fn signed(secret: &str, body: &[u8], sig: Option<&str>) -> bool {
    let Some(hexsig) = sig.and_then(|s| s.strip_prefix("sha256=")) else {
        return false;
    };
    let Ok(raw) = hex::decode(hexsig) else {
        return false;
    };
    let k = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    hmac::verify(&k, body, &raw).is_ok()
}

/// Whether a webhook request proves the shared secret: GitHub `X-Hub-Signature-256` and Jira
/// `X-Hub-Signature` (HMAC-SHA256 of the body), GitLab `X-Gitlab-Token`, Azure DevOps basic
/// authentication with the secret as the password.
pub fn verify(tracker: &str, h: &HeaderMap, body: &[u8], secret: &str) -> bool {
    match tracker {
        "github" => signed(secret, body, header(h, "x-hub-signature-256")),
        "jira" => signed(secret, body, header(h, "x-hub-signature")),
        "gitlab" => {
            header(h, "x-gitlab-token").is_some_and(|t| same(t.as_bytes(), secret.as_bytes()))
        }
        "azure" => header(h, "authorization")
            .and_then(|a| a.strip_prefix("Basic "))
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .and_then(|raw| String::from_utf8(raw).ok())
            .and_then(|up| up.split_once(':').map(|(_, p)| p.to_string()))
            .is_some_and(|p| same(p.as_bytes(), secret.as_bytes())),
        _ => false,
    }
}

/// The issue a verified webhook names, as keys or tracker ids (at most two).
pub fn refs(tracker: &str, body: &[u8]) -> Vec<String> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let num = |p: &str| v.pointer(p).and_then(Value::as_u64);
    let mut out = Vec::new();
    match tracker {
        "github" => out.extend(num("/issue/number").map(|n| format!("#{n}"))),
        "gitlab" => out.extend(num("/object_attributes/iid").map(|n| format!("#{n}"))),
        "jira" => {
            out.extend(
                v.pointer("/issue/key")
                    .and_then(Value::as_str)
                    .and_then(|k| super::trackers::parse_key("jira", k)),
            );
            out.extend(
                v.pointer("/issue/id")
                    .and_then(Value::as_str)
                    .filter(|s| s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()))
                    .map(str::to_string),
            );
        }
        "azure" => out.extend(
            num("/resource/workItemId")
                .or_else(|| num("/resource/id"))
                .map(|n| n.to_string()),
        ),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn sig(secret: &str, body: &[u8]) -> String {
        let k = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        format!("sha256={}", hex::encode(hmac::sign(&k, body).as_ref()))
    }

    #[test]
    fn webhooks_are_verified() {
        let body = br#"{"action":"closed","issue":{"number":7}}"#;
        let mut h = HeaderMap::new();
        h.insert(
            "x-hub-signature-256",
            HeaderValue::from_str(&sig("s3cret", body)).unwrap(),
        );
        assert!(verify("github", &h, body, "s3cret"));
        assert!(!verify("github", &h, body, "other"));
        assert!(!verify("github", &h, b"{}", "s3cret"));
        assert!(
            !verify("jira", &h, body, "s3cret"),
            "jira reads its own header"
        );
        assert_eq!(refs("github", body), vec!["#7"]);

        let mut gl = HeaderMap::new();
        gl.insert("x-gitlab-token", HeaderValue::from_static("s3cret"));
        assert!(verify("gitlab", &gl, b"{}", "s3cret"));
        assert!(!verify("gitlab", &gl, b"{}", "s3cret2"));
        assert_eq!(
            refs(
                "gitlab",
                br#"{"object_kind":"issue","object_attributes":{"iid":3}}"#
            ),
            vec!["#3"]
        );

        let mut az = HeaderMap::new();
        let b = base64::engine::general_purpose::STANDARD.encode("hook:s3cret");
        az.insert(
            "authorization",
            HeaderValue::from_str(&format!("Basic {b}")).unwrap(),
        );
        assert!(verify("azure", &az, b"{}", "s3cret"));
        assert!(!verify("azure", &HeaderMap::new(), b"{}", "s3cret"));
        assert_eq!(
            refs(
                "azure",
                br#"{"eventType":"workitem.updated","resource":{"workItemId":42}}"#
            ),
            vec!["42"]
        );

        let jb = br#"{"webhookEvent":"jira:issue_updated","issue":{"id":"10002","key":"SHOP-2"}}"#;
        let mut jh = HeaderMap::new();
        jh.insert(
            "x-hub-signature",
            HeaderValue::from_str(&sig("k", jb)).unwrap(),
        );
        assert!(verify("jira", &jh, jb, "k"));
        assert_eq!(refs("jira", jb), vec!["SHOP-2", "10002"]);
        assert!(refs("jira", b"not json").is_empty());
    }

    #[test]
    fn schedule() {
        assert_eq!(backoff(0), Duration::from_secs(60));
        assert_eq!(backoff(3), Duration::from_secs(480));
        assert_eq!(backoff(50), MAX_BACKOFF);
        let api: TrackerApi = serde_json::from_value(serde_json::json!({
            "url": "https://gitlab.example.com", "tokenSecretRef": "t", "project": "g/a"}))
        .unwrap();
        let poll = Duration::from_secs(600);
        assert_eq!(next_read(&api, false, poll), Some(poll));
        assert_eq!(next_read(&api, true, poll), Some(DAILY));
        let mut off = api.clone();
        off.sync = Some("off".into());
        assert_eq!(next_read(&off, false, poll), None);
    }
}
