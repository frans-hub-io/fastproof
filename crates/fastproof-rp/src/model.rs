//! Request bodies of the ReportPortal reporting API as agents send them. Parsing is lenient:
//! unknown fields are ignored, `null` stands for a missing value, and scalar strings may come
//! as numbers or booleans (agents differ). Sizes are bounded later by the callers.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// `null` (or a missing field) as the default value.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A string from a string, number or boolean; `null` is none; anything else is an error.
fn lenient_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    use serde::de::Error;
    match Value::deserialize(d)? {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        _ => Err(D::Error::custom("expected a string")),
    }
}

/// A boolean from a boolean or the strings `true`/`false`; `null` is false.
fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    })
}

/// `hasStats`: a missing value or `null` is true.
fn has_stats<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => !s.eq_ignore_ascii_case("false"),
        _ => true,
    })
}

fn yes() -> bool {
    true
}

/// `{key, value, system}` of launches and items; a missing key makes a tag.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ItemAttribute {
    #[serde(deserialize_with = "lenient_string")]
    pub key: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub value: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    pub system: bool,
}

/// `{key, value}` of a test item.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(default)]
pub struct ItemParameter {
    #[serde(deserialize_with = "lenient_string")]
    pub key: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub value: Option<String>,
}

/// `StartLaunchRQ`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StartLaunch {
    #[serde(deserialize_with = "lenient_string")]
    pub name: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub attributes: Vec<ItemAttribute>,
    pub start_time: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub mode: Option<String>,
    #[serde(deserialize_with = "lenient_bool")]
    pub rerun: bool,
    #[serde(deserialize_with = "lenient_string")]
    pub rerun_of: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub uuid: Option<String>,
}

/// `FinishExecutionRQ` of a launch.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FinishLaunch {
    pub end_time: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub status: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub attributes: Vec<ItemAttribute>,
}

/// `UpdateLaunchRQ`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateLaunch {
    #[serde(deserialize_with = "lenient_string")]
    pub mode: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    pub attributes: Option<Vec<ItemAttribute>>,
}

/// `StartTestItemRQ`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StartItem {
    #[serde(deserialize_with = "lenient_string")]
    pub name: Option<String>,
    #[serde(rename = "type", deserialize_with = "lenient_string")]
    pub item_type: Option<String>,
    pub start_time: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub launch_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub attributes: Vec<ItemAttribute>,
    #[serde(deserialize_with = "nullable")]
    pub parameters: Vec<ItemParameter>,
    #[serde(deserialize_with = "lenient_string")]
    pub code_ref: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub test_case_id: Option<String>,
    #[serde(deserialize_with = "has_stats")]
    pub has_stats: bool,
    #[serde(deserialize_with = "lenient_bool")]
    pub retry: bool,
    #[serde(alias = "retry_of", deserialize_with = "lenient_string")]
    pub retry_of: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub parent_uuid: Option<String>,
}

impl Default for StartItem {
    fn default() -> Self {
        Self {
            name: None,
            item_type: None,
            start_time: Value::Null,
            launch_uuid: None,
            uuid: None,
            description: None,
            attributes: Vec::new(),
            parameters: Vec::new(),
            code_ref: None,
            test_case_id: None,
            has_stats: yes(),
            retry: false,
            retry_of: None,
            parent_uuid: None,
        }
    }
}

/// `externalSystemIssues[]` of an issue.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExternalIssue {
    #[serde(deserialize_with = "lenient_string")]
    pub ticket_id: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub url: Option<String>,
}

/// `Issue` of a finished item.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Issue {
    #[serde(deserialize_with = "lenient_string")]
    pub issue_type: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub comment: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub external_system_issues: Vec<ExternalIssue>,
}

/// `FinishTestItemRQ`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FinishItem {
    pub end_time: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub status: Option<String>,
    pub issue: Option<Issue>,
    #[serde(deserialize_with = "lenient_string")]
    pub launch_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    pub attributes: Option<Vec<ItemAttribute>>,
    #[serde(deserialize_with = "lenient_bool")]
    pub retry: bool,
    #[serde(alias = "retry_of", deserialize_with = "lenient_string")]
    pub retry_of: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub test_case_id: Option<String>,
}

/// `UpdateTestItemRQ`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateItem {
    #[serde(deserialize_with = "lenient_string")]
    pub description: Option<String>,
    pub attributes: Option<Vec<ItemAttribute>>,
}

/// `file` of a log entry: the name of the multipart part with the content.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct LogFile {
    #[serde(deserialize_with = "lenient_string")]
    pub name: Option<String>,
}

/// `SaveLogRQ`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveLog {
    #[serde(deserialize_with = "lenient_string")]
    pub launch_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub item_uuid: Option<String>,
    #[serde(deserialize_with = "lenient_string")]
    pub uuid: Option<String>,
    pub time: Value,
    #[serde(deserialize_with = "lenient_string")]
    pub message: Option<String>,
    pub level: Value,
    pub file: Option<LogFile>,
}

/// Cleans attributes: drops empty values, bounds sizes and the count.
pub fn clean_attributes(a: &[ItemAttribute]) -> Vec<ItemAttribute> {
    a.iter()
        .filter_map(|x| {
            let value = crate::clean_name(x.value.as_deref()?);
            if value.is_empty() {
                return None;
            }
            let key = x
                .key
                .as_deref()
                .map(|k| crate::cut(&crate::clean_name(k), crate::MAX_ATTRIBUTE_KEY_BYTES))
                .filter(|k| !k.is_empty());
            Some(ItemAttribute {
                key,
                value: Some(crate::cut(&value, crate::MAX_ATTRIBUTE_VALUE_BYTES)),
                system: x.system,
            })
        })
        .take(crate::MAX_ATTRIBUTES)
        .collect()
}

/// Cleans parameters: a parameter needs a key or a value; sizes and the count are bounded.
pub fn clean_parameters(p: &[ItemParameter]) -> Vec<ItemParameter> {
    p.iter()
        .filter_map(|x| {
            let key = x
                .key
                .as_deref()
                .map(|k| crate::cut(&crate::clean_name(k), crate::MAX_ATTRIBUTE_KEY_BYTES))
                .filter(|k| !k.is_empty());
            let value = x
                .value
                .as_deref()
                .map(|v| crate::cut(&crate::clean_text(v), crate::MAX_ATTRIBUTE_VALUE_BYTES));
            (key.is_some() || value.is_some()).then_some(ItemParameter { key, value })
        })
        .take(crate::MAX_PARAMETERS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_item_is_lenient() {
        let v: StartItem = serde_json::from_str(
            r#"{"name":"t","type":"step","startTime":"1700000000000","launchUuid":"l",
                "attributes":null,"parameters":[{"key":"n","value":3}],"hasStats":null,
                "unknownField":{"x":1},"retry":"true","retry_of":"r0"}"#,
        )
        .unwrap();
        assert!(v.has_stats);
        assert!(v.retry);
        assert_eq!(v.retry_of.as_deref(), Some("r0"));
        assert!(v.attributes.is_empty());
        assert_eq!(v.parameters[0].value.as_deref(), Some("3"));
        let v: StartItem = serde_json::from_str(r#"{"hasStats":false}"#).unwrap();
        assert!(!v.has_stats);
        let v: StartItem = serde_json::from_str("{}").unwrap();
        assert!(v.has_stats);
    }

    #[test]
    fn attributes_are_bounded() {
        let many: Vec<ItemAttribute> = (0..300)
            .map(|i| ItemAttribute {
                key: Some(format!("k{i}")),
                value: Some("v".repeat(5000)),
                system: false,
            })
            .collect();
        let c = clean_attributes(&many);
        assert_eq!(c.len(), crate::MAX_ATTRIBUTES);
        assert_eq!(
            c[0].value.as_ref().unwrap().len(),
            crate::MAX_ATTRIBUTE_VALUE_BYTES
        );
        let c = clean_attributes(&[ItemAttribute {
            key: Some("k".into()),
            value: Some(" ".into()),
            system: false,
        }]);
        assert!(c.is_empty());
    }

    #[test]
    fn save_log_takes_numeric_levels() {
        let v: SaveLog = serde_json::from_str(
            r#"{"launchUuid":"l","itemUuid":"i","time":1700000000000,"message":"m","level":40000,
                "file":{"name":"a.png"}}"#,
        )
        .unwrap();
        assert_eq!(v.level, serde_json::json!(40000));
        assert_eq!(v.file.unwrap().name.as_deref(), Some("a.png"));
    }
}
