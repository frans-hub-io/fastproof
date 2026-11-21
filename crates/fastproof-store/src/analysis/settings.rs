//! Project settings (`projects.settings`): flaky detection, retention, rollup, and the
//! documents of gates, notifications and issue trackers.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{Store, json_no_nul};

/// Largest settings document, in bytes of JSON.
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
/// Gates or notifications per project, at most.
pub const MAX_SETTINGS_ITEMS: usize = 50;
/// Issue trackers per project, at most.
pub const MAX_ISSUE_TRACKERS: usize = 10;

/// Flaky detection across runs (Allure 3 `flakyDetection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct FlakySettings {
    /// Past runs that are compared: `-1` disables detection, `0` uses all (up to 50).
    pub history_depth: i32,
    /// Identical outcomes in a row that reset earlier instability.
    pub stabilization_period: u32,
    /// Also mark passed tests as flaky.
    pub include_passed_tests: bool,
}

impl Default for FlakySettings {
    fn default() -> Self {
        Self {
            history_depth: 10,
            stabilization_period: 5,
            include_passed_tests: false,
        }
    }
}

impl FlakySettings {
    pub fn options(&self) -> fastproof_model::FlakyOptions {
        fastproof_model::FlakyOptions {
            history_depth: self.history_depth,
            stabilization_period: self.stabilization_period.max(1),
            include_passed_tests: self.include_passed_tests,
        }
    }

    /// Past results to read for detection.
    pub fn depth(&self) -> i64 {
        match self.history_depth {
            d if d <= 0 => 50,
            d => (d as i64).min(100),
        }
    }
}

/// How long raw results are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct RetentionSettings {
    /// Launches older than this many days are deleted (after their history is rolled up).
    pub raw_days: u32,
    /// Keep at most this many launches (important ones are not counted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launches_keep: Option<u32>,
    /// Never delete launches marked important.
    pub keep_important: bool,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            raw_days: 90,
            launches_keep: None,
            keep_important: true,
        }
    }
}

/// When launches are aggregated into daily history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct RollupSettings {
    pub after_days: u32,
}

impl Default for RollupSettings {
    fn default() -> Self {
        Self { after_days: 30 }
    }
}

/// Settings of a project. The analysis parts (`flaky`, `retention`, `rollup`) are typed here;
/// the integration parts (`gates`, `notifications`, `issueTrackers`, `fastmock`, and any other
/// key such as `vcs`) are kept as given and validated by the integrations feature.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectSettings {
    pub flaky: FlakySettings,
    pub retention: RetentionSettings,
    pub rollup: RollupSettings,
    /// Quality gates.
    #[schema(value_type = Vec<Object>)]
    pub gates: Vec<serde_json::Value>,
    /// Notification targets.
    #[schema(value_type = Vec<Object>)]
    pub notifications: Vec<serde_json::Value>,
    /// Issue trackers `{type: jira|gitlab|github, urlTemplate, keyRegex}`; triage builds issue
    /// URLs from them when only a key is given.
    #[schema(value_type = Vec<Object>)]
    pub issue_trackers: Vec<serde_json::Value>,
    /// FastMock integration `{url, tokenSecretRef?}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub fastmock: Option<serde_json::Value>,
    /// Other parts of the document (e.g. `vcs`), kept unchanged.
    #[serde(flatten)]
    #[schema(ignore)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl ProjectSettings {
    /// The issue URL for a key, from the first tracker whose `keyRegex` matches the whole key.
    pub fn issue_url(&self, key: &str) -> Option<String> {
        self.issue_trackers.iter().find_map(|t| {
            let template = t.get("urlTemplate")?.as_str()?;
            let re =
                super::rules::compile_regex(&format!("^(?:{})$", t.get("keyRegex")?.as_str()?))
                    .ok()?;
            re.is_match(key)
                .then(|| template.replace("{key}", &url_component(key)))
        })
    }
}

fn url_component(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl ProjectSettings {
    /// Checks ranges and sizes; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        let f = &self.flaky;
        if !(-1..=100).contains(&f.history_depth) {
            return Err("flaky.historyDepth must be -1..=100".into());
        }
        if !(1..=100).contains(&f.stabilization_period) {
            return Err("flaky.stabilizationPeriod must be 1..=100".into());
        }
        let r = &self.retention;
        if !(1..=36_500).contains(&r.raw_days) {
            return Err("retention.rawDays must be 1..=36500".into());
        }
        if let Some(k) = r.launches_keep
            && !(1..=10_000_000).contains(&k)
        {
            return Err("retention.launchesKeep must be 1..=10000000".into());
        }
        if !(1..=36_500).contains(&self.rollup.after_days) {
            return Err("rollup.afterDays must be 1..=36500".into());
        }
        if self.gates.len() > MAX_SETTINGS_ITEMS || self.notifications.len() > MAX_SETTINGS_ITEMS {
            return Err(format!(
                "at most {MAX_SETTINGS_ITEMS} gates and {MAX_SETTINGS_ITEMS} notifications"
            ));
        }
        if self
            .gates
            .iter()
            .chain(&self.notifications)
            .any(|v| !v.is_object())
        {
            return Err("gates and notifications are objects".into());
        }
        if self.issue_trackers.len() > MAX_ISSUE_TRACKERS {
            return Err(format!("at most {MAX_ISSUE_TRACKERS} issue trackers"));
        }
        if self.issue_trackers.iter().any(|v| !v.is_object())
            || self.fastmock.as_ref().is_some_and(|v| !v.is_object())
        {
            return Err("issueTrackers and fastmock are objects".into());
        }
        let size = serde_json::to_vec(self)
            .map(|v| v.len())
            .unwrap_or(usize::MAX);
        if size > MAX_SETTINGS_BYTES {
            return Err(format!(
                "settings must have at most {MAX_SETTINGS_BYTES} bytes"
            ));
        }
        Ok(())
    }

    /// Settings from the stored document; invalid parts fall back to defaults.
    pub fn from_value(v: serde_json::Value) -> Self {
        let mut s: ProjectSettings = serde_json::from_value(v.clone()).unwrap_or_else(|_| {
            let mut d = ProjectSettings::default();
            if let Some(o) = v.as_object() {
                for (k, slot) in [("flaky", 0), ("retention", 1), ("rollup", 2)] {
                    if let Some(x) = o.get(k) {
                        match slot {
                            0 => d.flaky = serde_json::from_value(x.clone()).unwrap_or_default(),
                            1 => {
                                d.retention = serde_json::from_value(x.clone()).unwrap_or_default()
                            }
                            _ => d.rollup = serde_json::from_value(x.clone()).unwrap_or_default(),
                        }
                    }
                }
            }
            d
        });
        if s.flaky.stabilization_period == 0 {
            s.flaky.stabilization_period = 1;
        }
        if s.retention.raw_days == 0 {
            s.retention.raw_days = RetentionSettings::default().raw_days;
        }
        if s.rollup.after_days == 0 {
            s.rollup.after_days = RollupSettings::default().after_days;
        }
        s
    }
}

impl Store {
    pub async fn project_settings(&self, project_id: Uuid) -> anyhow::Result<ProjectSettings> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("select settings from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(ProjectSettings::from_value(v.unwrap_or_default()))
    }

    /// Replaces the settings document (validate it first).
    pub async fn put_project_settings(
        &self,
        project_id: Uuid,
        s: &ProjectSettings,
    ) -> anyhow::Result<()> {
        sqlx::query("update projects set settings = $2, updated_at = now() where id = $1")
            .bind(project_id)
            .bind(json_no_nul(serde_json::to_value(s)?))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Settings of every project that is not deleted.
    pub async fn all_project_settings(
        &self,
    ) -> anyhow::Result<Vec<(Uuid, String, ProjectSettings)>> {
        let rows: Vec<(Uuid, String, serde_json::Value)> = sqlx::query_as(
            "select id, key, settings from projects where deleted_at is null order by key limit 100000",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, key, v)| (id, key, ProjectSettings::from_value(v)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        let s = ProjectSettings::from_value(serde_json::json!({}));
        assert_eq!(s.flaky.history_depth, 10);
        assert_eq!(s.retention.raw_days, 90);
        assert!(s.retention.keep_important);
        assert_eq!(s.rollup.after_days, 30);
        assert!(s.validate().is_ok());
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["flaky"]["stabilizationPeriod"], 5);
        let mut bad = s.clone();
        bad.flaky.history_depth = 1000;
        assert!(bad.validate().is_err());
        let mut with = s.clone();
        with.issue_trackers.push(serde_json::json!({"type": "jira",
            "urlTemplate": "https://issues.example.com/browse/{key}", "keyRegex": "[A-Z]+-\\d+"}));
        assert_eq!(
            with.issue_url("OPS-12").as_deref(),
            Some("https://issues.example.com/browse/OPS-12")
        );
        assert_eq!(with.issue_url("ops 12"), None);
        let kept = ProjectSettings::from_value(serde_json::json!({"vcs": {"comments": true}}));
        assert_eq!(
            serde_json::to_value(&kept).unwrap()["vcs"]["comments"],
            true
        );
        let partly = ProjectSettings::from_value(
            serde_json::json!({"flaky": {"historyDepth": 3}, "gates": 5}),
        );
        assert_eq!(partly.flaky.history_depth, 3);
    }
}
