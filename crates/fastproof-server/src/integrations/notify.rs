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
    if let Some(u) = &s.launch.url {
        out.push(u.clone());
    }
    out.join("\n")
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
    }
}

async fn send_email(st: &AppState, smtp: &SmtpConfig, to: &str, s: &Summary) -> Outcome {
    let from: Mailbox = match smtp.from.parse() {
        Ok(m) => m,
        Err(_) => return Outcome::Fail("FASTPROOF_SMTP_FROM is invalid".into(), None),
    };
    let mut b = Message::builder()
        .from(from)
        .subject(headline(s))
        .header(ContentType::TEXT_PLAIN);
    for a in to.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        match a.parse::<Mailbox>() {
            Ok(m) => b = b.to(m),
            Err(_) => return Outcome::Fail(format!("invalid address {a:?}"), None),
        }
    }
    let msg = match b.body(text(s)) {
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

    #[test]
    fn outcomes() {
        assert_eq!(Outcome::of_status(204), Outcome::Delivered(Some(204)));
        assert!(matches!(Outcome::of_status(503), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(429), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(404), Outcome::Fail(..)));
    }
}
