//! Notifications: webhook (HMAC-SHA256 signed), Slack incoming webhook, Telegram bot API and
//! email over SMTP. Messages are built from a [`Summary`] stored with the delivery.

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

/// New failures against the previous launch of the branch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFailures {
    pub count: i64,
    pub names: Vec<String>,
    pub baseline_launch_id: Option<Uuid>,
}

/// What every message says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub project: String,
    pub launch: SummaryLaunch,
    pub gate: GateReport,
    pub new_failures: NewFailures,
    pub events: Vec<String>,
}

/// The outcome of one delivery attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered(Option<u16>),
    /// Try again later.
    Retry(String, Option<u16>),
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

    pub fn of_error(e: OutboundError) -> Self {
        match e {
            OutboundError::Denied(d) => Outcome::Fail(d.to_string(), None),
            other => Outcome::Retry(other.to_string(), None),
        }
    }
}

/// `sha256=<hex>` of the HMAC-SHA256 of `body` with `key`.
pub fn signature(key: &[u8], body: &[u8]) -> String {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    format!("sha256={}", hex::encode(hmac::sign(&k, body).as_ref()))
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
) -> Result<Option<Secret>, Outcome> {
    match name {
        None => Ok(None),
        Some(n) => st
            .project_secret(project_id, n)
            .await
            .map(Some)
            .map_err(|e| Outcome::Fail(e.to_string(), None)),
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
        Ok(r) => Outcome::of_status(r.status),
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
    let key = match secret(st, project_id, &n.secret_ref).await {
        Ok(k) => k,
        Err(o) => return o,
    };
    match n.kind.as_str() {
        "webhook" => {
            let body = webhook_body(delivery, s);
            let mut headers = vec![
                (
                    "X-FastProof-Event",
                    s.events.first().cloned().unwrap_or_default(),
                ),
                ("X-FastProof-Delivery", delivery.to_string()),
            ];
            if let Some(k) = &key {
                headers.push((
                    "X-FastProof-Signature",
                    signature(k.expose().as_bytes(), &body),
                ));
            }
            post_json(st, &n.target, body, &headers).await
        }
        "slack" => {
            let url = key
                .as_ref()
                .map(|k| k.expose().trim().to_string())
                .unwrap_or_else(|| n.target.clone());
            let body = serde_json::to_vec(&serde_json::json!({"text": slack_escape(&text(s))}))
                .unwrap_or_default();
            post_json(st, &url, body, &[]).await
        }
        "telegram" => {
            let Some(token) = key else {
                return Outcome::Fail("telegram needs secretRef".into(), None);
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
            let base = st
                .cfg
                .integrations
                .telegram_api
                .as_str()
                .trim_end_matches('/');
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
    let addrs = match st
        .egress
        .policy()
        .resolve(&smtp.host, Some(smtp.port))
        .await
    {
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
            new_failures: NewFailures {
                count: 1,
                names: vec!["checkout <pays>\nnow".into()],
                baseline_launch_id: None,
            },
            events: vec!["gateFailed".into()],
        }
    }

    #[test]
    fn signing_and_text() {
        assert_eq!(
            signature(b"key", b"The quick brown fox jumps over the lazy dog"),
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
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
    fn outcomes() {
        assert_eq!(Outcome::of_status(204), Outcome::Delivered(Some(204)));
        assert!(matches!(Outcome::of_status(503), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(429), Outcome::Retry(..)));
        assert!(matches!(Outcome::of_status(404), Outcome::Fail(..)));
    }
}
