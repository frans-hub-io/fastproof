//! Notifications: webhook (HMAC-SHA256 signed over a timestamp and the body), Slack incoming
//! webhook, Microsoft Teams Workflows (an Adaptive Card, see [`super::teams`]), Telegram bot API
//! and email over SMTP. Messages are built from a [`Summary`] stored with the delivery.

use std::time::Duration;

use fastproof_blob::Secret;
use fastproof_store::Counts;
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use reqwest::Method;
use ring::hmac;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::config::{SmtpConfig, SmtpTls};
use super::egress::OutboundError;
use super::gates::GateReport;
use super::secrets::{SecretPurpose, origin_of};
use super::settings::Notification;
use crate::state::AppState;

/// The launch part of a summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryLaunch {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub status: String,
    pub branch: Option<String>,
    pub environment: Option<String>,
    pub repository: Option<String>,
    pub commit: Option<String>,
    pub merge_request: Option<String>,
    pub ci_url: Option<String>,
    /// The launch page, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
    pub duration_ms: Option<i64>,
    pub counts: Counts,
}

/// One new failure with its result page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFailure {
    pub name: String,
    pub result_id: Uuid,
    /// The result page, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// New failures against the previous launch of the branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFailures {
    pub count: i64,
    pub names: Vec<String>,
    /// The same failures as `names`, with result ids and links.
    #[serde(default)]
    pub items: Vec<NewFailure>,
    pub baseline_launch_id: Option<Uuid>,
}

/// What every message says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub project: String,
    pub launch: SummaryLaunch,
    pub gate: GateReport,
    /// Percent of passed among executed results (muted ones excluded), as the gates measure it.
    #[serde(default)]
    pub pass_rate: Option<f64>,
    pub new_failures: NewFailures,
    pub events: Vec<String>,
    /// A sample sent by `POST .../notifications/{id}/test`, not a real launch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// What a launch started from a plan ran compared to the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_check: Option<fastproof_store::tm::PlanCheck>,
}

/// The outcome of one delivery attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered(Option<u16>),
    /// Try again later.
    Retry(String, Option<u16>),
    /// Try again, not before the delay the receiver asked for (`Retry-After`).
    Throttled(String, Option<u16>, Duration),
    /// Do not try again.
    Fail(String, Option<u16>),
}

impl Outcome {
    /// From an HTTP status: 2xx delivered, 408/429/5xx retried, other codes failed.
    pub fn of_status(status: u16) -> Self {
        match status {
            200..=299 => Outcome::Delivered(Some(status)),
            408 | 429 | 500..=599 => Outcome::Retry(format!("HTTP {status}"), Some(status)),
            _ => Outcome::Fail(format!("HTTP {status}"), Some(status)),
        }
    }

    /// From a response: as [`Outcome::of_status`], and a `429` or `503` with `Retry-After`
    /// (seconds or an HTTP date, at most an hour) waits at least that long.
    pub fn of_response(status: u16, headers: &reqwest::header::HeaderMap) -> Self {
        if matches!(status, 429 | 503)
            && let Some(d) = headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(retry_after)
        {
            return Outcome::Throttled(
                format!("HTTP {status} (retry after {}s)", d.as_secs()),
                Some(status),
                d,
            );
        }
        Self::of_status(status)
    }

    pub fn of_error(e: OutboundError) -> Self {
        match e {
            OutboundError::Denied(d) => Outcome::Fail(d.to_string(), None),
            other => Outcome::Retry(other.to_string(), None),
        }
    }
}

/// The delay of a `Retry-After` value: delta seconds or an HTTP date, capped at an hour.
pub fn retry_after(v: &str) -> Option<Duration> {
    let v = v.trim();
    let secs = match v.parse::<u64>() {
        Ok(n) => n,
        Err(_) => {
            let at = chrono::DateTime::parse_from_rfc2822(v).ok()?;
            (at.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64
        }
    };
    Some(Duration::from_secs(secs.min(3600)))
}

/// `sha256=<hex>` of the HMAC-SHA256 with `key` of `<timestamp>.<body>`, where `timestamp`
/// is the `X-FastProof-Timestamp` header (Unix seconds). A receiver recomputes it over the raw
/// body, compares in constant time, rejects timestamps more than a few minutes away from its
/// clock and drops repeated `X-FastProof-Delivery` ids.
pub fn signature(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&k);
    ctx.update(timestamp.to_string().as_bytes());
    ctx.update(b".");
    ctx.update(body);
    format!("sha256={}", hex::encode(ctx.sign().as_ref()))
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match flat.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

/// The headline: project, launch and outcome.
pub fn headline(s: &Summary) -> String {
    let outcome = match s.gate.status.as_str() {
        "failed" => "quality gate failed".to_string(),
        "passed" if s.gate.results.is_empty() => s.launch.status.clone(),
        "passed" => "quality gate passed".to_string(),
        other => other.to_string(),
    };
    one_line(
        &format!(
            "FastProof {} / {} #{}: {}",
            s.project, s.launch.name, s.launch.number, outcome
        ),
        300,
    )
}

/// A plain-text message.
pub fn text(s: &Summary) -> String {
    let c = &s.launch.counts;
    let mut out = vec![headline(s)];
    let mut where_ = Vec::new();
    if let Some(b) = &s.launch.branch {
        where_.push(format!("branch {}", one_line(b, 200)));
    }
    if let Some(e) = &s.launch.environment {
        where_.push(format!("environment {}", one_line(e, 200)));
    }
    if !where_.is_empty() {
        out.push(where_.join(", "));
    }
    out.push(format!(
        "{} results: {} passed, {} failed, {} broken, {} skipped, {} flaky",
        c.total, c.passed, c.failed, c.broken, c.skipped, c.flaky
    ));
    let broken_rules: Vec<String> = s
        .gate
        .results
        .iter()
        .filter(|r| !r.ok)
        .take(10)
        .map(|r| {
            format!(
                "{}: {:?} {:?} {} (actual {})",
                one_line(&r.gate, 100),
                r.rule.metric,
                r.rule.op,
                r.expected,
                r.actual
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "n/a".into())
            )
        })
        .collect();
    if !broken_rules.is_empty() {
        out.push(format!("Rules not met: {}", broken_rules.join("; ")));
    }
    if s.new_failures.count > 0 {
        let names: Vec<String> = s
            .new_failures
            .names
            .iter()
            .map(|n| one_line(n, 200))
            .collect();
        out.push(format!(
            "New failures ({}): {}",
            s.new_failures.count,
            names.join(", ")
        ));
    }
    if let Some(line) = plan_check_line(s) {
        out.push(line);
    }
    if let Some(u) = &s.launch.url {
        out.push(u.clone());
    }
    out.join("\n")
}

/// One line when the launch ran tests outside its plan or nothing of it.
pub fn plan_check_line(s: &Summary) -> Option<String> {
    let p = s.plan_check.as_ref().filter(|p| p.warning)?;
    let mut line = format!(
        "Plan check: {} of {} plan cases ran, {} missing, {} not in the plan",
        p.ran, p.in_plan, p.missing, p.extra
    );
    if !p.extra_names.is_empty() {
        let names: Vec<String> = p
            .extra_names
            .iter()
            .take(5)
            .map(|n| one_line(n, 100))
            .collect();
        line.push_str(&format!(" ({})", names.join(", ")));
    }
    Some(line)
}

fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn secret(
    st: &AppState,
    project_id: Uuid,
    name: &Option<String>,
    purpose: SecretPurpose,
    destination: Option<&str>,
) -> Result<Option<Secret>, Outcome> {
    match name {
        None => Ok(None),
        Some(n) => st
            .project_secret(project_id, n, purpose, destination)
            .await
            .map(Some)
            .map_err(|e| Outcome::Fail(e.to_string(), None)),
    }
}

/// The URL of a webhook or Slack notification: `target`, or the value of `targetSecretRef`.
async fn target_url(
    st: &AppState,
    project_id: Uuid,
    n: &Notification,
    purpose: SecretPurpose,
) -> Result<String, Outcome> {
    match secret(st, project_id, &n.target_secret_ref, purpose, None).await? {
        Some(v) => {
            let u = v.expose().trim().to_string();
            if origin_of(&u).is_none() {
                return Err(Outcome::Fail(
                    "the targetSecretRef secret is not an http(s) URL".into(),
                    None,
                ));
            }
            Ok(u)
        }
        None => Ok(n.target.clone()),
    }
}

async fn post_json(st: &AppState, url: &str, body: Vec<u8>, extra: &[(&str, String)]) -> Outcome {
    let rb = match st.egress.request(Method::POST, url) {
        Ok(rb) => rb,
        Err(e) => return Outcome::of_error(e),
    };
    let mut rb = rb
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    for (k, v) in extra {
        rb = rb.header(*k, v.as_str());
    }
    match st.egress.send(rb, 64 * 1024).await {
        Ok(r) => Outcome::of_response(r.status, &r.headers),
        Err(e) => Outcome::of_error(e),
    }
}

/// Posts a card to a Teams workflow. The URL is the credential, so redirects are not followed.
async fn post_teams(st: &AppState, url: &str, body: Vec<u8>) -> Outcome {
    let rb = match st.egress.request(Method::POST, url) {
        Ok(rb) => rb
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body),
        Err(e) => return Outcome::of_error(e),
    };
    match st.egress.send_no_redirect(rb, 64 * 1024).await {
        Ok(r) => match Outcome::of_response(r.status, &r.headers) {
            Outcome::Fail(_, c) if matches!(r.status, 401 | 403) => Outcome::Fail(
                format!(
                    "HTTP {} (the workflow trigger must allow Anyone to trigger it)",
                    r.status
                ),
                c,
            ),
            Outcome::Fail(_, c) if r.status == 404 => Outcome::Fail(
                "HTTP 404 (the workflow was deleted, turned off or got a new URL)".into(),
                c,
            ),
            Outcome::Fail(_, c) if (300..400).contains(&r.status) => Outcome::Fail(
                format!("HTTP {} (redirects are not followed for Teams)", r.status),
                c,
            ),
            other => other,
        },
        Err(e) => Outcome::of_error(e),
    }
}

/// The JSON body of a webhook.
pub fn webhook_body(delivery: Uuid, s: &Summary) -> Vec<u8> {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "event".into(),
            serde_json::json!(s.events.first().cloned().unwrap_or_default()),
        );
        o.insert("deliveryId".into(), serde_json::json!(delivery));
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

/// Sends one notification.
pub async fn send(
    st: &AppState,
    project_id: Uuid,
    delivery: Uuid,
    n: &Notification,
    s: &Summary,
) -> Outcome {
    match n.kind.as_str() {
        "webhook" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Webhook).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let key = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Webhook,
                Some(&url),
            )
            .await
            {
                Ok(k) => k,
                Err(o) => return o,
            };
            let body = webhook_body(delivery, s);
            let ts = chrono::Utc::now().timestamp();
            let mut headers = vec![
                (
                    "X-FastProof-Event",
                    s.events.first().cloned().unwrap_or_default(),
                ),
                ("X-FastProof-Delivery", delivery.to_string()),
                ("X-FastProof-Timestamp", ts.to_string()),
            ];
            if let Some(k) = &key {
                headers.push((
                    "X-FastProof-Signature",
                    signature(k.expose().as_bytes(), ts, &body),
                ));
            }
            post_json(st, &url, body, &headers).await
        }
        "teams" => {
            if n.target_secret_ref.is_none() {
                return Outcome::Fail("teams needs targetSecretRef".into(), None);
            }
            let url = match target_url(st, project_id, n, SecretPurpose::Teams).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            if let Err(e) = super::secrets::check_teams_url(&url) {
                return Outcome::Fail(e, None);
            }
            post_teams(st, &url, super::teams::body(s)).await
        }
        "slack" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Slack).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let body = serde_json::to_vec(&serde_json::json!({"text": slack_escape(&text(s))}))
                .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "telegram" => {
            let base = st
                .cfg
                .integrations
                .telegram_api
                .as_str()
                .trim_end_matches('/');
            let token = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Telegram,
                Some(base),
            )
            .await
            {
                Ok(Some(t)) => t,
                Ok(None) => return Outcome::Fail("telegram needs secretRef".into(), None),
                Err(o) => return o,
            };
            let t = token.expose().trim();
            if t.is_empty()
                || t.len() > 128
                || !t
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            {
                return Outcome::Fail("the telegram bot token is malformed".into(), None);
            }
            let url = format!("{base}/bot{t}/sendMessage");
            let body = serde_json::to_vec(&serde_json::json!({
                "chat_id": n.target.trim(),
                "text": text(s),
                "disable_web_page_preview": true,
            }))
            .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "email" => match &st.cfg.integrations.smtp {
            None => Outcome::Fail("email needs FASTPROOF_SMTP_URL".into(), None),
            Some(smtp) => send_email(st, smtp, &n.target, s).await,
        },
        other => Outcome::Fail(format!("unknown notification kind {other}"), None),
    }
}

/// The event of review requests.
pub const REVIEW_EVENT: &str = "caseReviewRequested";
/// Least time between two `caseReviewRequested` deliveries of one notification.
pub const REVIEW_THROTTLE: Duration = Duration::from_secs(300);

/// One object waiting for review in a [`ReviewDigest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDigestItem {
    /// `case` or `sharedStep`.
    pub kind: String,
    /// `TC-<n>` or `SS-<n>`.
    pub key: String,
    pub name: String,
    pub author: Option<String>,
    /// The page of the object, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// What a `caseReviewRequested` message says: objects of a project that became pending since
/// the previous message of the notification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDigest {
    pub project: String,
    pub event: String,
    /// Objects waiting for review now.
    pub waiting: i64,
    /// Objects that became pending since the previous message.
    pub new: i64,
    /// The newest of them (at most 10).
    pub items: Vec<ReviewDigestItem>,
    /// The review queue page, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// The headline of a review request.
pub fn review_headline(d: &ReviewDigest) -> String {
    one_line(
        &format!(
            "FastProof {}: {} new for review, {} waiting",
            d.project, d.new, d.waiting
        ),
        300,
    )
}

/// The plain-text message of a review request.
pub fn review_text(d: &ReviewDigest) -> String {
    let mut out = vec![review_headline(d)];
    for i in &d.items {
        let by = i
            .author
            .as_deref()
            .map(|a| format!(" (by {})", one_line(a, 100)))
            .unwrap_or_default();
        out.push(format!("{} {}{by}", i.key, one_line(&i.name, 200)));
    }
    if d.new > d.items.len() as i64 {
        out.push(format!("and {} more", d.new - d.items.len() as i64));
    }
    if let Some(u) = &d.url {
        out.push(u.clone());
    }
    out.join("\n")
}

/// The Teams card of a review request (plain text runs only).
pub fn review_card(d: &ReviewDigest) -> serde_json::Value {
    use super::teams::plain;
    let mut body = vec![
        serde_json::json!({"type": "TextBlock", "text": plain(&review_headline(d), 300),
        "weight": "Bolder", "wrap": true, "size": "Medium"}),
    ];
    for i in &d.items {
        let by = i
            .author
            .as_deref()
            .map(|a| format!(" (by {})", plain(a, 100)))
            .unwrap_or_default();
        body.push(
            serde_json::json!({"type": "RichTextBlock", "spacing": "Small", "inlines": [
            {"type": "TextRun", "text": format!("{} ", i.key), "weight": "Bolder"},
            {"type": "TextRun", "text": format!("{}{by}", plain(&i.name, 200))}]}),
        );
    }
    let mut card = serde_json::json!({"type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "version": "1.5", "body": body});
    if let Some(u) = &d.url {
        card["actions"] =
            serde_json::json!([{"type": "Action.OpenUrl", "title": "Open reviews", "url": u}]);
    }
    super::teams::message(card)
}

/// The JSON body of a review request webhook.
pub fn review_webhook_body(delivery: Uuid, d: &ReviewDigest) -> Vec<u8> {
    let mut v = serde_json::to_value(d).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("deliveryId".into(), serde_json::json!(delivery));
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

/// Whether every recipient of an email notification is one of `actors` (the authors of the
/// requested versions): such a message would reach the actor alone and is not sent.
pub fn only_actors(n: &Notification, actors: &[String]) -> bool {
    if n.kind != "email" {
        return false;
    }
    let to: Vec<String> = n
        .target
        .split(',')
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .collect();
    !to.is_empty()
        && to
            .iter()
            .all(|a| actors.iter().any(|x| x.trim().eq_ignore_ascii_case(a)))
}

/// Sends one review request.
pub async fn send_review(
    st: &AppState,
    project_id: Uuid,
    delivery: Uuid,
    n: &Notification,
    d: &ReviewDigest,
) -> Outcome {
    match n.kind.as_str() {
        "webhook" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Webhook).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let key = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Webhook,
                Some(&url),
            )
            .await
            {
                Ok(k) => k,
                Err(o) => return o,
            };
            let body = review_webhook_body(delivery, d);
            let ts = chrono::Utc::now().timestamp();
            let mut headers = vec![
                ("X-FastProof-Event", REVIEW_EVENT.to_string()),
                ("X-FastProof-Delivery", delivery.to_string()),
                ("X-FastProof-Timestamp", ts.to_string()),
            ];
            if let Some(k) = &key {
                headers.push((
                    "X-FastProof-Signature",
                    signature(k.expose().as_bytes(), ts, &body),
                ));
            }
            post_json(st, &url, body, &headers).await
        }
        "teams" => {
            if n.target_secret_ref.is_none() {
                return Outcome::Fail("teams needs targetSecretRef".into(), None);
            }
            let url = match target_url(st, project_id, n, SecretPurpose::Teams).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            if let Err(e) = super::secrets::check_teams_url(&url) {
                return Outcome::Fail(e, None);
            }
            post_teams(
                st,
                &url,
                serde_json::to_vec(&review_card(d)).unwrap_or_default(),
            )
            .await
        }
        "slack" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Slack).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let body =
                serde_json::to_vec(&serde_json::json!({"text": slack_escape(&review_text(d))}))
                    .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "telegram" => {
            let base = st
                .cfg
                .integrations
                .telegram_api
                .as_str()
                .trim_end_matches('/');
            let token = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Telegram,
                Some(base),
            )
            .await
            {
                Ok(Some(t)) => t,
                Ok(None) => return Outcome::Fail("telegram needs secretRef".into(), None),
                Err(o) => return o,
            };
            let t = token.expose().trim();
            if t.is_empty()
                || t.len() > 128
                || !t
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            {
                return Outcome::Fail("the telegram bot token is malformed".into(), None);
            }
            let url = format!("{base}/bot{t}/sendMessage");
            let body = serde_json::to_vec(&serde_json::json!({
                "chat_id": n.target.trim(),
                "text": review_text(d),
                "disable_web_page_preview": true,
            }))
            .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "email" => match &st.cfg.integrations.smtp {
            None => Outcome::Fail("email needs FASTPROOF_SMTP_URL".into(), None),
            Some(smtp) => {
                send_email_text(st, smtp, &n.target, review_headline(d), review_text(d)).await
            }
        },
        other => Outcome::Fail(format!("unknown notification kind {other}"), None),
    }
}

/// Least time between two test sends of one notification (per replica).
pub const TEST_INTERVAL: Duration = Duration::from_secs(5);

/// Whether a test of the notification may be sent now; records the send when it may.
pub fn test_allowed(project_id: Uuid, id: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static LAST: OnceLock<Mutex<HashMap<(Uuid, String), Instant>>> = OnceLock::new();
    let mut m = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if m.len() > 10_000 {
        m.retain(|_, t| now.duration_since(*t) < TEST_INTERVAL);
    }
    let key = (project_id, id.to_string());
    if m.get(&key)
        .is_some_and(|t| now.duration_since(*t) < TEST_INTERVAL)
    {
        return false;
    }
    m.insert(key, now);
    true
}

/// The summary of a test notification: a made-up launch of the project with every part of a
/// message (failed gate, counts, new failures), linking to the project page.
pub fn sample_summary(project: &str, project_url: Option<String>, n: &Notification) -> Summary {
    use super::gates::{GateStatus, RuleResult};
    use super::settings::{GateRule, Metric, Op};
    let rule = |metric, op, value: f64, actual: f64, ok| RuleResult {
        gate: "default".into(),
        rule: GateRule {
            metric,
            op,
            value,
            baseline: None,
        },
        actual: Some(actual),
        expected: value,
        ok,
        baseline_launch_id: None,
        note: None,
    };
    let names = [
        "sample.checkout.pays_with_card",
        "sample.cart.applies_discount",
    ];
    Summary {
        project: project.to_string(),
        launch: SummaryLaunch {
            id: Uuid::nil(),
            number: 0,
            name: "Sample launch".into(),
            status: "finished".into(),
            branch: Some("main".into()),
            environment: Some("staging".into()),
            repository: None,
            commit: None,
            merge_request: None,
            ci_url: None,
            url: project_url,
            duration_ms: Some(754_000),
            counts: Counts {
                total: 120,
                passed: 112,
                failed: 3,
                broken: 1,
                skipped: 4,
                flaky: 2,
                ..Default::default()
            },
        },
        gate: GateReport {
            status: GateStatus::Failed,
            results: vec![
                rule(Metric::Failed, Op::Lte, 0.0, 3.0, false),
                rule(Metric::PassRate, Op::Gte, 95.0, 96.55, true),
            ],
            launch_id: Uuid::nil(),
            evaluated_at: chrono::Utc::now(),
        },
        pass_rate: Some(112.0 * 100.0 / 116.0),
        new_failures: NewFailures {
            count: 2,
            names: names.iter().map(|x| x.to_string()).collect(),
            items: vec![],
            baseline_launch_id: None,
        },
        events: n.on.clone(),
        test: true,
        plan_check: None,
    }
}

async fn send_email(st: &AppState, smtp: &SmtpConfig, to: &str, s: &Summary) -> Outcome {
    send_email_text(st, smtp, to, headline(s), text(s)).await
}

async fn send_email_text(
    st: &AppState,
    smtp: &SmtpConfig,
    to: &str,
    subject: String,
    body: String,
) -> Outcome {
    let from: Mailbox = match smtp.from.parse() {
        Ok(m) => m,
        Err(_) => return Outcome::Fail("FASTPROOF_SMTP_FROM is invalid".into(), None),
    };
    let mut b = Message::builder()
        .from(from)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN);
    for a in to.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        match a.parse::<Mailbox>() {
            Ok(m) => b = b.to(m),
            Err(_) => return Outcome::Fail(format!("invalid address {a:?}"), None),
        }
    }
    let msg = match b.body(body) {
        Ok(m) => m,
        Err(e) => return Outcome::Fail(format!("building the message failed: {e}"), None),
    };
    let addrs = match st.egress.policy().resolve(&smtp.host, smtp.port).await {
        Ok(a) => a,
        Err(e) => return Outcome::Fail(e.to_string(), None),
    };
    let mut last = String::from("no address");
    for addr in addrs.iter().take(3) {
        let mut t = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(addr.ip().to_string())
            .port(addr.port())
            .timeout(Some(Duration::from_secs(20)));
        let tls = match smtp.tls {
            SmtpTls::None => Tls::None,
            mode => match TlsParameters::new(smtp.host.clone()) {
                Ok(p) if mode == SmtpTls::Implicit => Tls::Wrapper(p),
                Ok(p) => Tls::Required(p),
                Err(e) => return Outcome::Fail(format!("TLS settings: {e}"), None),
            },
        };
        t = t.tls(tls);
        if let (Some(u), Some(p)) = (&smtp.username, &smtp.password) {
            t = t.credentials(Credentials::new(u.clone(), p.expose().to_string()));
        }
        match t.build().send(msg.clone()).await {
            Ok(r) if r.is_positive() => return Outcome::Delivered(Some(r.code().into())),
            Ok(r) => last = format!("SMTP {}", r.code()),
            Err(e) if e.is_permanent() => {
                return Outcome::Fail(
                    format!(
                        "SMTP rejected the message ({})",
                        e.status().map(|c| c.to_string()).unwrap_or_default()
                    ),
                    None,
                );
            }
            Err(e) => {
                last = format!(
                    "SMTP failed{}",
                    e.status()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_else(|| " (connection or TLS)".into())
                )
            }
        }
    }
    Outcome::Retry(last, None)
}

/// The event of regressed unique errors.
pub const ERROR_EVENT: &str = "errorRegressed";

/// One regressed error in an [`ErrorDigest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDigestItem {
    pub fingerprint: String,
    pub message: String,
    /// The error page, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// The launch an [`ErrorDigest`] is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDigestLaunch {
    pub id: Uuid,
    pub number: i64,
    pub name: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// The launch page, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// What an `errorRegressed` message says: errors that came back after they were resolved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDigest {
    pub project: String,
    pub event: String,
    pub launch: ErrorDigestLaunch,
    /// Errors that regressed in the launch.
    pub count: i64,
    /// The first of them (at most 10).
    pub items: Vec<ErrorDigestItem>,
    /// The unique errors page filtered to regressed errors, when `FASTPROOF_PUBLIC_URL` is set.
    pub url: Option<String>,
}

/// The headline of an `errorRegressed` message.
pub fn error_headline(d: &ErrorDigest) -> String {
    let branch = d
        .launch
        .branch
        .as_deref()
        .map(|b| format!(" on {}", one_line(b, 100)))
        .unwrap_or_default();
    let what = if d.count == 1 {
        "1 error regressed".to_string()
    } else {
        format!("{} errors regressed", d.count)
    };
    one_line(
        &format!(
            "FastProof {}: {what} in launch #{}{branch}",
            d.project, d.launch.number
        ),
        300,
    )
}

/// The plain-text message of regressed errors.
pub fn error_text(d: &ErrorDigest) -> String {
    let mut out = vec![error_headline(d)];
    for i in &d.items {
        out.push(format!("{} {}", i.fingerprint, one_line(&i.message, 200)));
    }
    if d.count > d.items.len() as i64 {
        out.push(format!("and {} more", d.count - d.items.len() as i64));
    }
    if let Some(u) = d.url.as_ref().or(d.launch.url.as_ref()) {
        out.push(u.clone());
    }
    out.join("\n")
}

/// The Teams card of regressed errors (plain text runs only).
pub fn error_card(d: &ErrorDigest) -> serde_json::Value {
    use super::teams::plain;
    let mut body = vec![serde_json::json!({"type": "RichTextBlock", "inlines": [
        {"type": "TextRun", "text": plain(&error_headline(d), 300), "weight": "Bolder", "size": "Medium"}]})];
    for i in &d.items {
        body.push(
            serde_json::json!({"type": "RichTextBlock", "spacing": "Small", "inlines": [
            {"type": "TextRun", "text": format!("{} ", i.fingerprint), "fontType": "Monospace"},
            {"type": "TextRun", "text": plain(&i.message, 200)}]}),
        );
    }
    let mut card = serde_json::json!({"type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "version": "1.5", "body": body});
    if let Some(u) = d.url.as_ref().or(d.launch.url.as_ref()) {
        card["actions"] =
            serde_json::json!([{"type": "Action.OpenUrl", "title": "Open errors", "url": u}]);
    }
    super::teams::message(card)
}

/// The JSON body of an `errorRegressed` webhook.
pub fn error_webhook_body(delivery: Uuid, d: &ErrorDigest) -> Vec<u8> {
    let mut v = serde_json::to_value(d).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.insert("deliveryId".into(), serde_json::json!(delivery));
    }
    serde_json::to_vec(&v).unwrap_or_default()
}

/// Sends one `errorRegressed` message.
pub async fn send_errors(
    st: &AppState,
    project_id: Uuid,
    delivery: Uuid,
    n: &Notification,
    d: &ErrorDigest,
) -> Outcome {
    match n.kind.as_str() {
        "webhook" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Webhook).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let key = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Webhook,
                Some(&url),
            )
            .await
            {
                Ok(k) => k,
                Err(o) => return o,
            };
            let body = error_webhook_body(delivery, d);
            let ts = chrono::Utc::now().timestamp();
            let mut headers = vec![
                ("X-FastProof-Event", ERROR_EVENT.to_string()),
                ("X-FastProof-Delivery", delivery.to_string()),
                ("X-FastProof-Timestamp", ts.to_string()),
            ];
            if let Some(k) = &key {
                headers.push((
                    "X-FastProof-Signature",
                    signature(k.expose().as_bytes(), ts, &body),
                ));
            }
            post_json(st, &url, body, &headers).await
        }
        "teams" => {
            if n.target_secret_ref.is_none() {
                return Outcome::Fail("teams needs targetSecretRef".into(), None);
            }
            let url = match target_url(st, project_id, n, SecretPurpose::Teams).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            if let Err(e) = super::secrets::check_teams_url(&url) {
                return Outcome::Fail(e, None);
            }
            post_teams(
                st,
                &url,
                serde_json::to_vec(&error_card(d)).unwrap_or_default(),
            )
            .await
        }
        "slack" => {
            let url = match target_url(st, project_id, n, SecretPurpose::Slack).await {
                Ok(u) => u,
                Err(o) => return o,
            };
            let body =
                serde_json::to_vec(&serde_json::json!({"text": slack_escape(&error_text(d))}))
                    .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "telegram" => {
            let base = st
                .cfg
                .integrations
                .telegram_api
                .as_str()
                .trim_end_matches('/');
            let token = match secret(
                st,
                project_id,
                &n.secret_ref,
                SecretPurpose::Telegram,
                Some(base),
            )
            .await
            {
                Ok(Some(t)) => t,
                Ok(None) => return Outcome::Fail("telegram needs secretRef".into(), None),
                Err(o) => return o,
            };
            let t = token.expose().trim();
            if t.is_empty()
                || t.len() > 128
                || !t
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
            {
                return Outcome::Fail("the telegram bot token is malformed".into(), None);
            }
            let url = format!("{base}/bot{t}/sendMessage");
            let body = serde_json::to_vec(&serde_json::json!({
                "chat_id": n.target.trim(),
                "text": error_text(d),
                "disable_web_page_preview": true,
            }))
            .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "email" => match &st.cfg.integrations.smtp {
            None => Outcome::Fail("email needs FASTPROOF_SMTP_URL".into(), None),
            Some(smtp) => {
                send_email_text(st, smtp, &n.target, error_headline(d), error_text(d)).await
            }
        },
        other => Outcome::Fail(format!("unknown notification kind {other}"), None),
    }
}

/// The `errorRegressed` message of a launch's changes; `None` without regressions. Only
/// transitions from an evaluated state count (a first evaluation after Analysis is turned on
/// is not news).
pub fn error_digest(
    s: &AppState,
    project: &fastproof_store::Project,
    launch: &fastproof_store::Launch,
    changes: &[fastproof_store::analysis::types::ErrorStateChange],
) -> Option<ErrorDigest> {
    use fastproof_store::analysis::types::ErrorState;
    let regressed: Vec<&fastproof_store::analysis::types::ErrorStateChange> = changes
        .iter()
        .filter(|c| {
            c.to == ErrorState::Regressed && c.from.is_some_and(|f| f != ErrorState::Regressed)
        })
        .collect();
    if regressed.is_empty() {
        return None;
    }
    let key = &project.key;
    let vcs = launch.vcs.clone().unwrap_or_default();
    Some(ErrorDigest {
        project: key.clone(),
        event: ERROR_EVENT.into(),
        launch: ErrorDigestLaunch {
            id: launch.id,
            number: launch.number,
            name: launch.name.clone(),
            branch: vcs.branch,
            commit: vcs.commit,
            url: super::dispatch::launch_url(s, key, launch.id),
        },
        count: regressed.len() as i64,
        items: regressed
            .iter()
            .take(fastproof_store::analysis::errors::EVENT_ITEMS)
            .map(|c| ErrorDigestItem {
                fingerprint: c.fingerprint.clone(),
                message: one_line(&c.message, 500),
                url: super::dispatch::page_url(s, &format!("p/{key}/errors/{}", c.fingerprint)),
            })
            .collect(),
        url: super::dispatch::page_url(s, &format!("p/{key}/errors?state=regressed")),
    })
}

/// Queues the `errorRegressed` deliveries of the error-state changes of a launch: one per
/// channel that has the event and whose filter matches the launch, at most one per channel and
/// project every 10 minutes (a regression inside that window is on the errors page, not in a
/// second message), listing up to 10 errors. Returns how many were queued.
pub async fn error_events(
    s: &AppState,
    project: &fastproof_store::Project,
    launch: &fastproof_store::Launch,
    changes: &[fastproof_store::analysis::types::ErrorStateChange],
) -> anyhow::Result<u64> {
    if launch.debug {
        return Ok(0);
    }
    let Some(digest) = error_digest(s, project, launch, changes) else {
        return Ok(0);
    };
    let doc = s.store.project_settings_json(project.id).await?;
    let Ok(settings) = super::settings::IntegrationSettings::parse(&doc) else {
        return Ok(0);
    };
    let attributes: Vec<(String, String)> = launch
        .attributes
        .iter()
        .map(|a| (a.key.clone(), a.value.clone()))
        .collect();
    let mut queued = 0;
    for n in settings
        .notifications
        .iter()
        .filter(|n| n.on.iter().any(|e| e == ERROR_EVENT))
    {
        if let Some(f) = &n.filter
            && !f.matches(
                super::gates::branch_of(launch),
                super::gates::env_of(launch),
                &launch.name,
                &attributes,
            )
        {
            continue;
        }
        let sent = s
            .store
            .queue_error_delivery(
                &fastproof_store::analysis::errors::ErrorDelivery {
                    project_id: project.id,
                    launch_id: launch.id,
                    channel: &n.kind,
                    target_id: &n.id,
                    payload: &serde_json::json!({"errors": digest, "notification": n}),
                    max_attempts: s.cfg.integrations.delivery_max_attempts,
                },
                fastproof_store::analysis::errors::EVENT_THROTTLE,
            )
            .await?;
        if sent {
            queued += 1;
        } else {
            metrics::counter!("fastproof_deliveries_throttled_total", "event" => ERROR_EVENT)
                .increment(1);
        }
    }
    Ok(queued)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::super::gates::GateStatus;
    use super::*;

    pub fn summary() -> Summary {
        Summary {
            project: "shop".into(),
            launch: SummaryLaunch {
                id: Uuid::nil(),
                number: 7,
                name: "nightly".into(),
                status: "finished".into(),
                branch: Some("main".into()),
                environment: None,
                repository: None,
                commit: None,
                merge_request: None,
                ci_url: None,
                url: Some("https://fastproof.example.com/p/shop/launches/x".into()),
                duration_ms: Some(1000),
                counts: Counts {
                    total: 3,
                    passed: 1,
                    failed: 2,
                    ..Default::default()
                },
            },
            gate: GateReport {
                status: GateStatus::Failed,
                results: vec![],
                launch_id: Uuid::nil(),
                evaluated_at: Utc::now(),
            },
            pass_rate: Some(33.3),
            new_failures: NewFailures {
                count: 1,
                names: vec!["checkout <pays>\nnow".into()],
                items: vec![],
                baseline_launch_id: None,
            },
            events: vec!["gateFailed".into()],
            test: false,
            plan_check: None,
        }
    }

    #[test]
    fn signing_and_text() {
        assert_eq!(
            signature(b"key", 1_795_249_079, b"{}"),
            format!(
                "sha256={}",
                hex::encode(
                    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, b"key"), b"1795249079.{}")
                        .as_ref()
                )
            ),
            "the signature covers <timestamp>.<body>"
        );
        assert_ne!(
            signature(b"key", 1, b"{}"),
            signature(b"key", 2, b"{}"),
            "a replayed body with a new timestamp needs a new signature"
        );
        let s = summary();
        let t = text(&s);
        assert!(t.starts_with("FastProof shop / nightly #7: quality gate failed"));
        assert!(t.contains("New failures (1): checkout <pays> now"));
        assert!(slack_escape(&t).contains("&lt;pays&gt;"));
        let body = webhook_body(Uuid::nil(), &s);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event"], "gateFailed");
        assert_eq!(v["launch"]["counts"]["failed"], 2);
    }

    #[test]
    fn plan_check_line_only_on_a_warning() {
        let mut s = summary();
        assert!(!text(&s).contains("Plan check"));
        let check = fastproof_store::tm::PlanCheck {
            plan_id: Uuid::nil(),
            in_plan: 10,
            ran: 8,
            extra: 2,
            missing: 2,
            extra_names: vec!["shop.Extra#one".into(), "shop.Extra#two".into()],
            warning: true,
        };
        s.plan_check = Some(fastproof_store::tm::PlanCheck {
            warning: false,
            ..check.clone()
        });
        assert!(!text(&s).contains("Plan check"));
        assert!(serde_json::to_value(&s).unwrap()["planCheck"]["inPlan"] == 10);
        s.plan_check = Some(check);
        let t = text(&s);
        assert!(
            t.contains("Plan check: 8 of 10 plan cases ran, 2 missing, 2 not in the plan (shop.Extra#one, shop.Extra#two)"),
            "{t}"
        );
        s.plan_check = None;
        assert!(serde_json::to_value(&s).unwrap().get("planCheck").is_none());
    }

    #[test]
    fn retry_after_is_honoured_and_capped() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(RETRY_AFTER, HeaderValue::from_str(v).unwrap());
            m
        };
        assert_eq!(
            Outcome::of_response(429, &h("7")),
            Outcome::Throttled(
                "HTTP 429 (retry after 7s)".into(),
                Some(429),
                Duration::from_secs(7)
            )
        );
        assert!(matches!(
            Outcome::of_response(429, &h("999999")),
            Outcome::Throttled(_, _, d) if d == Duration::from_secs(3600)
        ));
        let later = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc2822();
        match Outcome::of_response(503, &h(&later)) {
            Outcome::Throttled(_, _, d) => assert!((100..=121).contains(&d.as_secs()), "{d:?}"),
            o => panic!("{o:?}"),
        }
        assert_eq!(
            Outcome::of_response(429, &HeaderMap::new()),
            Outcome::Retry("HTTP 429".into(), Some(429))
        );
        assert_eq!(
            Outcome::of_response(429, &h("soon")),
            Outcome::Retry("HTTP 429".into(), Some(429))
        );
        assert_eq!(
            Outcome::of_response(400, &h("5")),
            Outcome::Fail("HTTP 400".into(), Some(400))
        );
        assert_eq!(
            Outcome::of_response(202, &HeaderMap::new()),
            Outcome::Delivered(Some(202))
        );
    }

    #[test]
    fn sample_summary_is_marked_as_a_test() {
        let n = Notification {
            id: "t".into(),
            kind: "teams".into(),
            on: vec!["gateFailed".into(), "newFailures".into()],
            target: String::new(),
            target_secret_ref: Some("teams-url".into()),
            secret_ref: None,
            filter: None,
        };
        let s = sample_summary(
            "shop",
            Some("https://fastproof.example.com/p/shop".into()),
            &n,
        );
        assert!(s.test);
        assert_eq!(s.events, n.on);
        assert_eq!(s.gate.status, super::super::gates::GateStatus::Failed);
        let body = webhook_body(Uuid::nil(), &s);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["test"], true);
        assert!(headline(&s).contains("shop / Sample launch #0"));
        let real = webhook_body(Uuid::nil(), &summary());
        let v: serde_json::Value = serde_json::from_slice(&real).unwrap();
        assert!(
            v.get("test").is_none(),
            "real deliveries carry no test flag"
        );
    }

    fn digest() -> ReviewDigest {
        ReviewDigest {
            project: "shop".into(),
            event: REVIEW_EVENT.into(),
            waiting: 4,
            new: 3,
            items: vec![
                ReviewDigestItem {
                    kind: "case".into(),
                    key: "TC-7".into(),
                    name: "Pay <with> card\nnow".into(),
                    author: Some("alice".into()),
                    url: None,
                },
                ReviewDigestItem {
                    kind: "sharedStep".into(),
                    key: "SS-2".into(),
                    name: "Log in".into(),
                    author: None,
                    url: None,
                },
            ],
            url: Some("https://fastproof.example.com/p/shop/reviews".into()),
        }
    }

    #[test]
    fn review_messages() {
        let d = digest();
        let t = review_text(&d);
        assert!(
            t.starts_with("FastProof shop: 3 new for review, 4 waiting"),
            "{t}"
        );
        assert!(t.contains("TC-7 Pay <with> card now (by alice)"), "{t}");
        assert!(t.contains("SS-2 Log in\n"), "{t}");
        assert!(t.contains("and 1 more"), "{t}");
        assert!(slack_escape(&t).contains("&lt;with&gt;"));
        let v: serde_json::Value =
            serde_json::from_slice(&review_webhook_body(Uuid::nil(), &d)).unwrap();
        assert_eq!(v["event"], REVIEW_EVENT);
        assert_eq!(v["items"][0]["key"], "TC-7");
        let card = review_card(&d);
        let s = card.to_string();
        assert!(
            s.contains("AdaptiveCard") && s.contains("Open reviews"),
            "{s}"
        );
        assert!(
            !s.contains("Pay <with> card\\n"),
            "control characters become spaces"
        );
    }

    #[test]
    fn review_requests_never_reach_the_actor_alone() {
        let n = |kind: &str, target: &str| Notification {
            id: "r".into(),
            kind: kind.into(),
            on: vec![REVIEW_EVENT.into()],
            target: target.into(),
            target_secret_ref: None,
            secret_ref: None,
            filter: None,
        };
        let actors = vec!["Alice@example.com".to_string()];
        assert!(only_actors(&n("email", "alice@example.com"), &actors));
        assert!(only_actors(&n("email", " alice@example.com , "), &actors));
        assert!(!only_actors(
            &n("email", "alice@example.com, bob@example.com"),
            &actors
        ));
        assert!(!only_actors(
            &n("slack", "https://hooks.example.com/x"),
            &actors
        ));
        assert!(!only_actors(&n("email", "team@example.com"), &actors));
    }

    #[test]
    fn outcomes() {
        assert_eq!(Outcome::of_status(204), Outcome::Delivered(Some(204)));
        assert!(matches!(Outcome::of_status(503), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(429), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(404), Outcome::Fail(..)));
    }

    #[test]
    fn error_digests_read_well_everywhere() {
        let d = ErrorDigest {
            project: "shop".into(),
            event: ERROR_EVENT.into(),
            launch: ErrorDigestLaunch {
                id: Uuid::nil(),
                number: 42,
                name: "nightly".into(),
                branch: Some("main".into()),
                commit: Some("d4e5f6".into()),
                url: Some("https://fastproof.example.com/p/shop/launches/x".into()),
            },
            count: 12,
            items: (0..10)
                .map(|i| ErrorDigestItem {
                    fingerprint: format!("{i:016x}"),
                    message: "Connection <refused>\nagain".into(),
                    url: None,
                })
                .collect(),
            url: Some("https://fastproof.example.com/p/shop/errors?state=regressed".into()),
        };
        assert_eq!(
            error_headline(&d),
            "FastProof shop: 12 errors regressed in launch #42 on main"
        );
        let t = error_text(&d);
        assert!(
            t.contains("0000000000000000 Connection <refused> again"),
            "{t}"
        );
        assert!(t.contains("and 2 more"));
        assert!(t.ends_with("errors?state=regressed"));
        let card = serde_json::to_string(&error_card(&d)).unwrap();
        assert!(card.contains("Open errors"));
        // Launch text (the branch is in the headline) is never in a Markdown TextBlock.
        let mut md = d.clone();
        md.launch.branch = Some("**x** [y](https://evil.example)".into());
        let card = error_card(&md);
        let blocks = card["attachments"][0]["content"]["body"]
            .as_array()
            .unwrap();
        assert!(
            blocks.iter().all(|b| b["type"] == "RichTextBlock"),
            "{card}"
        );
        assert!(
            blocks[0]["inlines"][0]["text"]
                .as_str()
                .unwrap()
                .contains("**x**")
        );
        let body: serde_json::Value =
            serde_json::from_slice(&error_webhook_body(Uuid::nil(), &d)).unwrap();
        assert_eq!(body["event"], ERROR_EVENT);
        assert_eq!(body["items"].as_array().unwrap().len(), 10);
        assert!(body["deliveryId"].is_string());
    }
}
