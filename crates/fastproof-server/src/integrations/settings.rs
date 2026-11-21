//! The integration parts of the project settings document (`gates`, `notifications`, `vcs`,
//! `issueTrackers`, `fastmock`) with their validation. The settings API (`PUT .../settings`)
//! calls [`validate`] before storing a document.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::secrets::valid_name;

pub const MAX_GATES: usize = 20;
pub const MAX_RULES: usize = 20;
pub const MAX_BASELINES: usize = 5;
pub const MAX_GLOBS: usize = 50;
pub const MAX_NOTIFICATIONS: usize = 20;
pub const MAX_TRACKERS: usize = 10;
pub const MAX_EMAIL_RECIPIENTS: usize = 20;

/// A metric a gate rule checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    /// Failed results (muted ones excluded).
    Failed,
    /// Broken results (muted ones excluded).
    Broken,
    /// Failed or broken results whose series was not failing in the baseline launch.
    NewFailures,
    /// Percent of passed results among executed (total minus skipped), 0 to 100.
    PassRate,
    /// Results flaky within the run.
    Flaky,
    /// Launch duration in seconds.
    Duration,
}

/// A comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Op {
    pub fn holds(self, actual: f64, expected: f64) -> bool {
        match self {
            Op::Gt => actual > expected,
            Op::Gte => actual >= expected,
            Op::Lt => actual < expected,
            Op::Lte => actual <= expected,
        }
    }
}

/// A rule: it holds when `metric op value`. With `baseline`, `newFailures` compares with the
/// newest finished launch of that branch and the other metrics become the difference to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateRule {
    pub metric: Metric,
    pub op: Op,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

/// A quality gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gate {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Branch globs (`*` matches any text); every branch when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<String>>,
    pub rules: Vec<GateRule>,
}

fn yes() -> bool {
    true
}

/// Which launches a notification is about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<String>>,
}

/// A notification channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Notification {
    pub id: String,
    /// `webhook`, `slack`, `email` or `telegram`.
    pub kind: String,
    /// `launchFinished`, `gateFailed`, `newFailures`.
    pub on: Vec<String>,
    /// Webhook or Slack URL, Telegram chat id, or comma-separated email addresses. A Slack
    /// webhook URL is better kept in a secret (`secretRef`), as settings are readable.
    #[serde(default)]
    pub target: String,
    /// Webhook: HMAC key; Slack: the webhook URL; Telegram: the bot token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<NotificationFilter>,
}

/// Merge request comments and commit statuses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VcsSettings {
    /// `gitlab` or `github`.
    pub provider: String,
    /// GitLab base URL (`https://gitlab.example.com`) or GitHub API URL
    /// (`https://api.github.com`, `https://github.example.com/api/v3`).
    pub url: String,
    pub token_secret_ref: String,
    #[serde(default = "yes")]
    pub comment: bool,
    #[serde(default = "yes")]
    pub status: bool,
}

/// An issue tracker for links from issue keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueTracker {
    /// `jira`, `gitlab` or `github`.
    #[serde(rename = "type")]
    pub tracker: String,
    /// URL with `{key}`, e.g. `https://jira.example.com/browse/{key}`.
    pub url_template: String,
    /// Keys this tracker owns, e.g. `[A-Z][A-Z0-9]+-[0-9]+` (matched in full).
    pub key_regex: String,
}

/// The FastMock instance whose journal is correlated with results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FastMockSettings {
    /// Admin base URL, e.g. `https://fastmock.example.com:9090`.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_secret_ref: Option<String>,
}

/// The integration keys of the settings document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IntegrationSettings {
    pub gates: Vec<Gate>,
    pub notifications: Vec<Notification>,
    pub vcs: Option<VcsSettings>,
    pub issue_trackers: Vec<IssueTracker>,
    pub fastmock: Option<FastMockSettings>,
}

pub const EVENTS: [&str; 3] = ["launchFinished", "gateFailed", "newFailures"];

fn field<T: serde::de::DeserializeOwned>(
    doc: &serde_json::Value,
    key: &str,
) -> Result<Option<T>, String> {
    match doc.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("{key}: {e}")),
    }
}

fn http_url(what: &str, u: &str) -> Result<url::Url, String> {
    let p = url::Url::parse(u).map_err(|_| format!("{what} must be a URL"))?;
    if !matches!(p.scheme(), "http" | "https") || p.host_str().is_none() || u.len() > 2048 {
        return Err(format!("{what} must be an http(s) URL with a host"));
    }
    Ok(p)
}

fn globs(what: &str, g: &Option<Vec<String>>) -> Result<(), String> {
    if let Some(g) = g {
        if g.len() > MAX_GLOBS {
            return Err(format!("{what}: at most {MAX_GLOBS} entries"));
        }
        if g.iter().any(|x| x.is_empty() || x.len() > 256) {
            return Err(format!("{what}: entries have 1 to 256 bytes"));
        }
    }
    Ok(())
}

fn secret_ref(what: &str, r: &Option<String>) -> Result<(), String> {
    match r {
        Some(n) if !valid_name(n) => Err(format!(
            "{what} must name a secret (^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$)"
        )),
        _ => Ok(()),
    }
}

/// Compiles a tracker key regex with size limits, anchored to match whole keys.
pub fn key_regex(re: &str) -> Result<regex::Regex, String> {
    if re.is_empty() || re.len() > 256 {
        return Err("keyRegex must have 1 to 256 bytes".into());
    }
    regex::RegexBuilder::new(&format!("^(?:{re})$"))
        .size_limit(64 * 1024)
        .dfa_size_limit(64 * 1024)
        .build()
        .map_err(|e| format!("keyRegex: {e}"))
}

impl IntegrationSettings {
    /// Reads and validates the integration keys of a settings document.
    pub fn parse(doc: &serde_json::Value) -> Result<Self, String> {
        let s = Self {
            gates: field(doc, "gates")?.unwrap_or_default(),
            notifications: field(doc, "notifications")?.unwrap_or_default(),
            vcs: field(doc, "vcs")?,
            issue_trackers: field(doc, "issueTrackers")?.unwrap_or_default(),
            fastmock: field(doc, "fastmock")?,
        };
        s.check()?;
        Ok(s)
    }

    fn check(&self) -> Result<(), String> {
        if self.gates.len() > MAX_GATES {
            return Err(format!("gates: at most {MAX_GATES}"));
        }
        let mut baselines = BTreeSet::new();
        let mut names = BTreeSet::new();
        for g in &self.gates {
            if g.name.trim().is_empty() || g.name.len() > 100 {
                return Err("gates: name must have 1 to 100 bytes".into());
            }
            if !names.insert(g.name.as_str()) {
                return Err(format!("gates: duplicate name {:?}", g.name));
            }
            globs("gates.branches", &g.branches)?;
            if g.rules.is_empty() || g.rules.len() > MAX_RULES {
                return Err(format!("gates.rules: 1 to {MAX_RULES} rules per gate"));
            }
            for r in &g.rules {
                if !r.value.is_finite() {
                    return Err("gates.rules.value must be a number".into());
                }
                if let Some(b) = &r.baseline {
                    if b.is_empty() || b.len() > 256 {
                        return Err("gates.rules.baseline must have 1 to 256 bytes".into());
                    }
                    baselines.insert(b.as_str());
                }
            }
        }
        if baselines.len() > MAX_BASELINES {
            return Err(format!("gates: at most {MAX_BASELINES} distinct baselines"));
        }
        if self.notifications.len() > MAX_NOTIFICATIONS {
            return Err(format!("notifications: at most {MAX_NOTIFICATIONS}"));
        }
        let mut ids = BTreeSet::new();
        for n in &self.notifications {
            if !valid_name(&n.id) {
                return Err("notifications.id must match ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$".into());
            }
            if !ids.insert(n.id.as_str()) {
                return Err(format!("notifications: duplicate id {:?}", n.id));
            }
            if n.on.is_empty() || n.on.iter().any(|e| !EVENTS.contains(&e.as_str())) {
                return Err(
                    "notifications.on: one or more of launchFinished, gateFailed, newFailures"
                        .into(),
                );
            }
            secret_ref("notifications.secretRef", &n.secret_ref)?;
            if let Some(f) = &n.filter {
                globs("notifications.filter.branches", &f.branches)?;
                globs("notifications.filter.environments", &f.environments)?;
            }
            match n.kind.as_str() {
                "webhook" => {
                    http_url("notifications.target", &n.target)?;
                }
                "slack" => {
                    if n.secret_ref.is_none() {
                        http_url("notifications.target", &n.target)?;
                    }
                }
                "telegram" => {
                    if n.secret_ref.is_none() {
                        return Err("telegram notifications need secretRef (the bot token)".into());
                    }
                    let t = n.target.trim();
                    let ok = !t.is_empty()
                        && t.len() <= 64
                        && (t
                            .trim_start_matches('-')
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                            || (t.starts_with('@')
                                && t[1..]
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')));
                    if !ok {
                        return Err(
                            "telegram notifications: target is a chat id or @channel".into()
                        );
                    }
                }
                "email" => {
                    let list: Vec<&str> = n.target.split(',').map(str::trim).collect();
                    if list.is_empty() || list.len() > MAX_EMAIL_RECIPIENTS {
                        return Err(format!(
                            "email notifications: 1 to {MAX_EMAIL_RECIPIENTS} addresses"
                        ));
                    }
                    for a in list {
                        if a.parse::<lettre::Address>().is_err() {
                            return Err(format!("email notifications: invalid address {a:?}"));
                        }
                    }
                }
                _ => {
                    return Err(
                        "notifications.kind must be webhook, slack, email or telegram".into(),
                    );
                }
            }
        }
        if let Some(v) = &self.vcs {
            if !matches!(v.provider.as_str(), "gitlab" | "github") {
                return Err("vcs.provider must be gitlab or github".into());
            }
            http_url("vcs.url", &v.url)?;
            secret_ref("vcs.tokenSecretRef", &Some(v.token_secret_ref.clone()))?;
        }
        if self.issue_trackers.len() > MAX_TRACKERS {
            return Err(format!("issueTrackers: at most {MAX_TRACKERS}"));
        }
        for t in &self.issue_trackers {
            if !matches!(t.tracker.as_str(), "jira" | "gitlab" | "github") {
                return Err("issueTrackers.type must be jira, gitlab or github".into());
            }
            if !t.url_template.contains("{key}") {
                return Err("issueTrackers.urlTemplate must contain {key}".into());
            }
            http_url(
                "issueTrackers.urlTemplate",
                &t.url_template.replace("{key}", "KEY-1"),
            )?;
            key_regex(&t.key_regex)?;
        }
        if let Some(f) = &self.fastmock {
            http_url("fastmock.url", &f.url)?;
            secret_ref("fastmock.tokenSecretRef", &f.token_secret_ref)?;
        }
        Ok(())
    }
}

/// Validates the integration keys of a settings document (for the settings API).
pub fn validate(doc: &serde_json::Value) -> Result<(), String> {
    IntegrationSettings::parse(doc).map(|_| ())
}

/// `*` matches any text (including none); everything else literally.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether a value matches any of the globs (`None`: no restriction).
pub fn matches_any(globs: &Option<Vec<String>>, value: Option<&str>) -> bool {
    match globs {
        None => true,
        Some(g) => value.is_some_and(|v| g.iter().any(|p| glob_match(p, v))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn globs_match() {
        assert!(glob_match("*", ""));
        assert!(glob_match("release/*", "release/1.2"));
        assert!(!glob_match("release/*", "main"));
        assert!(glob_match("*-hotfix", "x-hotfix"));
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("a*b*c", "aXbY"));
        assert!(glob_match("main", "main"));
        assert!(matches_any(&None, None));
        assert!(!matches_any(&Some(vec!["main".into()]), None));
    }

    #[test]
    fn parses_and_rejects() {
        let doc = json!({
            "flaky": {"historyDepth": 10},
            "gates": [{"name": "default", "branches": ["main", "release/*"], "rules": [
                {"metric": "failed", "op": "lte", "value": 0},
                {"metric": "newFailures", "op": "lte", "value": 0, "baseline": "main"},
                {"metric": "passRate", "op": "gte", "value": 95}
            ]}],
            "notifications": [
                {"id": "hook", "kind": "webhook", "on": ["launchFinished"], "target": "https://hooks.example.com/fp", "secretRef": "hook-key"},
                {"id": "chat", "kind": "telegram", "on": ["gateFailed"], "target": "-100123", "secretRef": "tg"},
                {"id": "mail", "kind": "email", "on": ["newFailures"], "target": "qa@example.com, dev@example.com"},
                {"id": "sl", "kind": "slack", "on": ["gateFailed"], "secretRef": "slack-url"}
            ],
            "vcs": {"provider": "gitlab", "url": "https://gitlab.example.com", "tokenSecretRef": "gl"},
            "issueTrackers": [{"type": "jira", "urlTemplate": "https://jira.example.com/browse/{key}", "keyRegex": "[A-Z]+-[0-9]+"}],
            "fastmock": {"url": "https://fastmock.example.com:9090", "tokenSecretRef": "fm"}
        });
        let s = IntegrationSettings::parse(&doc).unwrap();
        assert_eq!(s.gates[0].rules.len(), 3);
        assert!(s.gates[0].enabled);
        assert!(s.vcs.as_ref().unwrap().comment);
        for (k, v) in [
            ("gates", json!([{"name": "g", "rules": []}])),
            (
                "gates",
                json!([{"name": "g", "rules": [{"metric": "speed", "op": "gt", "value": 1}]}]),
            ),
            (
                "gates",
                json!([{"name": "g", "rules": [{"metric": "failed", "op": "gt", "value": 1, "x": 1}]}]),
            ),
            (
                "notifications",
                json!([{"id": "a", "kind": "pager", "on": ["launchFinished"], "target": "x"}]),
            ),
            (
                "notifications",
                json!([{"id": "a", "kind": "webhook", "on": ["started"], "target": "https://x.example.com"}]),
            ),
            (
                "notifications",
                json!([{"id": "a", "kind": "webhook", "on": ["launchFinished"], "target": "file:///etc/passwd"}]),
            ),
            (
                "notifications",
                json!([{"id": "a", "kind": "telegram", "on": ["launchFinished"], "target": "1"}]),
            ),
            (
                "notifications",
                json!([{"id": "a", "kind": "email", "on": ["launchFinished"], "target": "not-an-address"}]),
            ),
            (
                "vcs",
                json!({"provider": "bitbucket", "url": "https://x.example.com", "tokenSecretRef": "t"}),
            ),
            (
                "issueTrackers",
                json!([{"type": "jira", "urlTemplate": "https://j.example.com/", "keyRegex": "X"}]),
            ),
            (
                "issueTrackers",
                json!([{"type": "jira", "urlTemplate": "https://j.example.com/{key}", "keyRegex": "("}]),
            ),
            ("fastmock", json!({"url": "fastmock:9090"})),
        ] {
            let mut d = doc.clone();
            d[k] = v.clone();
            assert!(validate(&d).is_err(), "{k}: {v}");
        }
        let many: Vec<_> = (0..6)
            .map(|i| json!({"metric": "failed", "op": "lte", "value": 0, "baseline": format!("b{i}")}))
            .collect();
        assert!(validate(&json!({"gates": [{"name": "g", "rules": many}]})).is_err());
        assert_eq!(
            IntegrationSettings::parse(&json!({})).unwrap(),
            IntegrationSettings::default()
        );
    }
}
