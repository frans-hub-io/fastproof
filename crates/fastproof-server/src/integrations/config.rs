//! `FASTPROOF_EGRESS_ALLOW`, `FASTPROOF_SECRETS_KEY_FILE`, `FASTPROOF_SMTP_*`,
//! `FASTPROOF_TELEGRAM_API_URL`, `FASTPROOF_DELIVERY_*` and `FASTPROOF_METRICS_TOP_BRANCHES`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use fastproof_blob::Secret;

use super::egress::EgressPolicy;
use super::secrets::SecretKeys;
use crate::config::Env;

/// How the SMTP connection is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpTls {
    /// `smtps://`: TLS from the start (port 465).
    Implicit,
    /// `smtp://`: STARTTLS, required (port 587).
    StartTls,
    /// `smtp+insecure://`: no TLS (only for a relay on a trusted network or tests).
    None,
}

/// The mail relay of email notifications.
#[derive(Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    pub username: Option<String>,
    pub password: Option<Secret>,
    pub from: String,
}

impl fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from", &self.from)
            .finish()
    }
}

/// Settings of integrations.
#[derive(Clone, Debug)]
pub struct IntegrationsConfig {
    pub egress: Arc<EgressPolicy>,
    pub secrets: Option<Arc<SecretKeys>>,
    pub smtp: Option<SmtpConfig>,
    pub telegram_api: url::Url,
    pub delivery_max_attempts: i32,
    /// First retry delay; doubled per attempt, at most one hour.
    pub delivery_backoff: Duration,
    /// How often the dispatcher looks for work.
    pub poll: Duration,
    /// Age of outbox events before they are consumed (lets earlier transactions commit).
    pub settle: Duration,
    /// Branches per project with their own `branch` label in quality metrics.
    pub metrics_top_branches: usize,
    /// Environments per project with their own `environment` label.
    pub metrics_top_environments: usize,
}

impl Default for IntegrationsConfig {
    fn default() -> Self {
        Self {
            egress: Arc::new(EgressPolicy::default()),
            secrets: None,
            smtp: None,
            telegram_api: url::Url::parse("https://api.telegram.org").expect("static URL"),
            delivery_max_attempts: 6,
            delivery_backoff: Duration::from_secs(30),
            poll: Duration::from_secs(2),
            settle: Duration::from_secs(3),
            metrics_top_branches: 10,
            metrics_top_environments: 5,
        }
    }
}

fn number<T: std::str::FromStr>(env: &Env, k: &str, default: T) -> anyhow::Result<T>
where
    T::Err: fmt::Display,
{
    match env.var(k) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|e| anyhow!("{k}: invalid value {:?}: {e}", v.trim())),
    }
}

fn smtp(env: &Env) -> anyhow::Result<Option<SmtpConfig>> {
    let Some(raw) = env.var("FASTPROOF_SMTP_URL") else {
        return Ok(None);
    };
    let u = url::Url::parse(raw.trim()).map_err(|_| anyhow!("FASTPROOF_SMTP_URL: not a URL"))?;
    if !u.username().is_empty() || u.password().is_some() {
        bail!(
            "FASTPROOF_SMTP_URL: put credentials in FASTPROOF_SMTP_USERNAME and FASTPROOF_SMTP_PASSWORD(_FILE)"
        );
    }
    let (tls, default_port) = match u.scheme() {
        "smtps" => (SmtpTls::Implicit, 465),
        "smtp" => (SmtpTls::StartTls, 587),
        "smtp+insecure" => (SmtpTls::None, 25),
        s => bail!("FASTPROOF_SMTP_URL: scheme {s:?}: expected smtps, smtp or smtp+insecure"),
    };
    let host = u
        .host_str()
        .ok_or_else(|| anyhow!("FASTPROOF_SMTP_URL: no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let from = env
        .var("FASTPROOF_SMTP_FROM")
        .ok_or_else(|| anyhow!("FASTPROOF_SMTP_FROM is required with FASTPROOF_SMTP_URL"))?
        .trim()
        .to_string();
    from.parse::<lettre::message::Mailbox>()
        .map_err(|e| anyhow!("FASTPROOF_SMTP_FROM: {e}"))?;
    let password = env.secret("FASTPROOF_SMTP_PASSWORD")?;
    let username = env
        .var("FASTPROOF_SMTP_USERNAME")
        .map(|v| v.trim().to_string());
    if password.is_some() != username.is_some() {
        bail!("FASTPROOF_SMTP_USERNAME and FASTPROOF_SMTP_PASSWORD(_FILE) must be set together");
    }
    Ok(Some(SmtpConfig {
        host,
        port: u.port().unwrap_or(default_port),
        tls,
        username,
        password,
        from,
    }))
}

impl IntegrationsConfig {
    pub fn from_env(env: &Env) -> anyhow::Result<Self> {
        let d = Self::default();
        let egress = EgressPolicy::parse(&env.var("FASTPROOF_EGRESS_ALLOW").unwrap_or_default())
            .map_err(|e| anyhow!("FASTPROOF_EGRESS_ALLOW: {e}"))?;
        let secrets = match env.var("FASTPROOF_SECRETS_KEY_FILE") {
            None => None,
            Some(p) => Some(Arc::new(
                SecretKeys::load(std::path::Path::new(p.trim()))
                    .context("FASTPROOF_SECRETS_KEY_FILE")?,
            )),
        };
        let telegram_api = match env.var("FASTPROOF_TELEGRAM_API_URL") {
            None => d.telegram_api.clone(),
            Some(v) => {
                let u = url::Url::parse(v.trim())
                    .map_err(|_| anyhow!("FASTPROOF_TELEGRAM_API_URL: not a URL"))?;
                if !matches!(u.scheme(), "http" | "https") {
                    bail!("FASTPROOF_TELEGRAM_API_URL: expected an http(s) URL");
                }
                u
            }
        };
        let delivery_max_attempts: i32 = number(env, "FASTPROOF_DELIVERY_MAX_ATTEMPTS", 6)?;
        if !(1..=20).contains(&delivery_max_attempts) {
            bail!("FASTPROOF_DELIVERY_MAX_ATTEMPTS: 1 to 20");
        }
        let backoff: u64 = number(env, "FASTPROOF_DELIVERY_BACKOFF_SECONDS", 30)?;
        if backoff > 3600 {
            bail!("FASTPROOF_DELIVERY_BACKOFF_SECONDS: at most 3600");
        }
        let top: usize = number(env, "FASTPROOF_METRICS_TOP_BRANCHES", 10)?;
        if !(1..=100).contains(&top) {
            bail!("FASTPROOF_METRICS_TOP_BRANCHES: 1 to 100");
        }
        Ok(Self {
            egress: Arc::new(egress),
            secrets,
            smtp: smtp(env)?,
            telegram_api,
            delivery_max_attempts,
            delivery_backoff: Duration::from_secs(backoff),
            metrics_top_branches: top,
            ..d
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn cfg(vars: &[(&str, &str)]) -> anyhow::Result<IntegrationsConfig> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let get = |k: &str| m.get(k).cloned();
        IntegrationsConfig::from_env(&Env::new(&get))
    }

    #[test]
    fn parses_and_names_variables() {
        let c = cfg(&[]).unwrap();
        assert!(c.egress.is_empty() && c.secrets.is_none() && c.smtp.is_none());
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "smtp-pass-123\n").unwrap();
        let c = cfg(&[
            ("FASTPROOF_EGRESS_ALLOW", "hooks.example.com,*.example.org"),
            ("FASTPROOF_SMTP_URL", "smtp://smtp.example.com"),
            ("FASTPROOF_SMTP_FROM", "FastProof <fastproof@example.com>"),
            ("FASTPROOF_SMTP_USERNAME", "fastproof"),
            ("FASTPROOF_SMTP_PASSWORD_FILE", pw.to_str().unwrap()),
        ])
        .unwrap();
        let s = c.smtp.as_ref().unwrap();
        assert_eq!((s.port, s.tls), (587, SmtpTls::StartTls));
        assert!(!format!("{c:?}").contains("smtp-pass-123"));
        for (k, v) in [
            ("FASTPROOF_EGRESS_ALLOW", "a*b.example.com"),
            ("FASTPROOF_SECRETS_KEY_FILE", "/nonexistent/fastproof-key"),
            ("FASTPROOF_SMTP_URL", "imap://x.example.com"),
            ("FASTPROOF_TELEGRAM_API_URL", "ftp://x"),
            ("FASTPROOF_DELIVERY_MAX_ATTEMPTS", "0"),
            ("FASTPROOF_METRICS_TOP_BRANCHES", "x"),
        ] {
            let e = format!("{:#}", cfg(&[(k, v)]).unwrap_err());
            assert!(e.contains(k), "{k}: {e}");
        }
        let e = format!(
            "{:#}",
            cfg(&[("FASTPROOF_SMTP_URL", "smtps://u:p@smtp.example.com")]).unwrap_err()
        );
        assert!(e.contains("FASTPROOF_SMTP_USERNAME"), "{e}");
        let e = format!(
            "{:#}",
            cfg(&[("FASTPROOF_SMTP_URL", "smtps://smtp.example.com")]).unwrap_err()
        );
        assert!(e.contains("FASTPROOF_SMTP_FROM"), "{e}");
    }
}
