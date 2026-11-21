use serde::{Deserialize, Serialize};

/// A key/value pair whose order matters (environment properties, attributes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        KeyValue {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// CI system that produced the launch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CiInfo {
    /// `gitlab`, `github`, `jenkins`, ... as written by the executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_order: Option<i64>,
}

impl CiInfo {
    pub fn is_empty(&self) -> bool {
        self == &CiInfo::default()
    }
}

/// Version control information of the launch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VcsInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl VcsInfo {
    pub fn is_empty(&self) -> bool {
        self == &VcsInfo::default()
    }
}

/// Allure `executor.json` as written.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Executor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub executor_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_url: Option<String>,
}

/// Launch-level metadata found in an upload. The server merges it with what the client
/// sent when creating the launch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LaunchMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "CiInfo::is_empty")]
    pub ci: CiInfo,
    #[serde(default, skip_serializing_if = "VcsInfo::is_empty")]
    pub vcs: VcsInfo,
    /// Named environment (series dimension), when the upload states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_key: Option<String>,
    /// Environment properties (`environment.properties`, CTRF `environment`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<Executor>,
    /// Other launch attributes (tool name and version, report ids, shard).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
}

impl LaunchMeta {
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| kv.value.as_str())
    }

    pub fn set_attribute(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(kv) = self.attributes.iter_mut().find(|kv| kv.key == key) {
            kv.value = value;
        } else {
            self.attributes.push(KeyValue::new(key, value));
        }
    }
}
