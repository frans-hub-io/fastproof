//! Project settings of the ReportPortal endpoint: the `rp` area of the settings document.
//!
//! ```json
//! {"rp": {"aliases": ["default_personal", "Team_A"],
//!         "attributeMap": {"branch": "vcs.branch", "build": "ci.buildOrder"},
//!         "environmentAttribute": "env"}}
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Aliases of one project, at most.
pub const MAX_ALIASES: usize = 20;
/// Entries of `attributeMap`, at most.
pub const MAX_ATTRIBUTE_MAP: usize = 50;

/// Launch fields an attribute may fill.
pub const TARGETS: [&str; 8] = [
    "vcs.branch",
    "vcs.commit",
    "vcs.repository",
    "ci.url",
    "ci.buildOrder",
    "ci.pipelineId",
    "ci.jobId",
    "ci.provider",
];

/// The `rp` settings area.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpSettings {
    /// ReportPortal project names that mean this project (compared without case).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Launch attribute key to launch field; missing means [`default_attribute_map`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute_map: Option<BTreeMap<String, String>>,
    /// The launch attribute whose value is the environment key of the launch (part of
    /// every series identity). Off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_attribute: Option<String>,
}

/// The attribute map used when a project sets none.
pub fn default_attribute_map() -> BTreeMap<String, String> {
    [
        ("branch", "vcs.branch"),
        ("commit", "vcs.commit"),
        ("revision", "vcs.commit"),
        ("ci.url", "ci.url"),
        ("build", "ci.buildOrder"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// A project alias: 1 to 128 bytes of letters, digits, `.`, `_` and `-`.
pub fn valid_alias(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 128
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl RpSettings {
    /// The `rp` area of a settings document (absent: defaults).
    pub fn from_doc(doc: &serde_json::Value) -> Result<Self, String> {
        match doc.get("rp") {
            None | Some(serde_json::Value::Null) => Ok(Self::default()),
            Some(v) => {
                let s: Self = serde_json::from_value(v.clone()).map_err(|e| format!("rp: {e}"))?;
                s.validate()?;
                Ok(s)
            }
        }
    }

    /// Checks sizes, characters and targets; the message names the field.
    pub fn validate(&self) -> Result<(), String> {
        if self.aliases.len() > MAX_ALIASES {
            return Err(format!("rp.aliases: at most {MAX_ALIASES}"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in &self.aliases {
            if !valid_alias(a) {
                return Err(
                    "rp.aliases: 1 to 128 characters of letters, digits, '.', '_' and '-'".into(),
                );
            }
            if !seen.insert(a.to_ascii_lowercase()) {
                return Err(format!("rp.aliases: {a:?} is listed twice"));
            }
        }
        if let Some(m) = &self.attribute_map {
            if m.len() > MAX_ATTRIBUTE_MAP {
                return Err(format!(
                    "rp.attributeMap: at most {MAX_ATTRIBUTE_MAP} entries"
                ));
            }
            for (k, v) in m {
                if k.trim().is_empty() || k.len() > 256 {
                    return Err("rp.attributeMap: keys have 1 to 256 bytes".into());
                }
                if !TARGETS.contains(&v.as_str()) {
                    return Err(format!(
                        "rp.attributeMap: {v:?} is not one of {}",
                        TARGETS.join(", ")
                    ));
                }
            }
        }
        if let Some(e) = &self.environment_attribute
            && (e.trim().is_empty() || e.len() > 256)
        {
            return Err("rp.environmentAttribute must have 1 to 256 bytes".into());
        }
        Ok(())
    }

    /// The attribute map in effect.
    pub fn attribute_map(&self) -> BTreeMap<String, String> {
        self.attribute_map
            .clone()
            .unwrap_or_else(default_attribute_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_and_validates() {
        let s = RpSettings::from_doc(&json!({"rp": {"aliases": ["default_personal", "Team_A"],
            "attributeMap": {"build": "ci.buildOrder"}, "environmentAttribute": "env"}}))
        .unwrap();
        assert_eq!(s.aliases.len(), 2);
        assert_eq!(s.attribute_map().len(), 1);
        assert_eq!(
            RpSettings::from_doc(&json!({})).unwrap(),
            RpSettings::default()
        );
        assert_eq!(
            RpSettings::default().attribute_map()["revision"],
            "vcs.commit"
        );
        for bad in [
            json!({"rp": {"aliases": ["a/b"]}}),
            json!({"rp": {"aliases": ["A", "a"]}}),
            json!({"rp": {"aliases": (0..21).map(|i| format!("p{i}")).collect::<Vec<_>>()}}),
            json!({"rp": {"attributeMap": {"x": "launch.name"}}}),
            json!({"rp": {"environmentAttribute": ""}}),
            json!({"rp": {"unknown": 1}}),
        ] {
            assert!(RpSettings::from_doc(&bad).is_err(), "{bad}");
        }
    }
}
