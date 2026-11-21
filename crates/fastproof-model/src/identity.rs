//! Test identity compatible with Allure 2.45+ (`Allure2Plugin`) and Allure 3
//! (`packages/core-api/src/utils/identity.ts`).

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use crate::result::{Parameter, SourceFormat};

/// Value used by Allure for a parameter without a value.
pub const UNKNOWN_PARAMETER_VALUE: &str = "#___unknown_value___#";
/// Allure 3 environment id that does not contribute to the retry hash.
pub const DEFAULT_ENVIRONMENT: &str = "default";

/// Where an explicit, writer-provided test case id came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum IdSource {
    /// `ALLURE_ID` (or legacy `AS_ID`) label.
    AllureId,
    /// Allure `testCaseId` field.
    AllureTestCaseId,
    /// CTRF `testId`.
    CtrfTestId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExplicitId {
    pub source: IdSource,
    pub value: String,
}

/// Location of the test in the source tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_ref: Option<String>,
}

/// Logical identity of a result: test case, parameters and environment.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TestIdentity {
    /// Highest-precedence explicit id: `ALLURE_ID`, then Allure `testCaseId`, then CTRF `testId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_id: Option<ExplicitId>,
    /// The id that feeds `test_case_hash` instead of the full name (Allure `testCaseId`,
    /// CTRF `testId`, or `ALLURE_ID` for non-Allure formats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_case_id: Option<String>,
    /// Canonical full name (Allure `fullName` as written; normalized for other formats).
    pub full_name: String,
    pub display_name: String,
    #[serde(default)]
    pub source: SourceRef,
    /// `md5(test_case_id || full_name)`, lowercase hex.
    pub test_case_hash: String,
    /// Allure parameters hash.
    pub parameters_hash: String,
    /// Environment dimension (Playwright project, configured environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_hash: Option<String>,
    /// Series key, equal to the Allure 3 `retryHash`.
    pub series_key: String,
    /// Allure 3 legacy test case hash (`md5("ALLURE_ID=<id>")` when an Allure id exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_test_case_hash: Option<String>,
    /// Allure `historyId` as written (used by Allure only to find old history).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_id: Option<String>,
}

/// Inputs for [`TestIdentity::build`].
#[derive(Debug, Clone, Default)]
pub struct IdentityInput<'a> {
    pub format: Option<SourceFormat>,
    pub allure_id: Option<&'a str>,
    pub allure_test_case_id: Option<&'a str>,
    pub ctrf_test_id: Option<&'a str>,
    /// Canonical full name; already normalized by the caller for non-Allure formats.
    pub full_name: &'a str,
    pub display_name: &'a str,
    pub parameters: &'a [Parameter],
    pub env_key: Option<&'a str>,
    pub source: SourceRef,
    pub history_id: Option<&'a str>,
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

impl TestIdentity {
    pub fn build(input: IdentityInput<'_>) -> TestIdentity {
        let allure_id = non_empty(input.allure_id.map(str::trim)).filter(|v| *v != "-1");
        let allure_tc = non_empty(input.allure_test_case_id);
        let ctrf_id = non_empty(input.ctrf_test_id);
        let explicit_id = allure_id
            .map(|v| (IdSource::AllureId, v))
            .or(allure_tc.map(|v| (IdSource::AllureTestCaseId, v)))
            .or(ctrf_id.map(|v| (IdSource::CtrfTestId, v)))
            .map(|(source, value)| ExplicitId {
                source,
                value: value.to_string(),
            });
        // Allure hashes testCaseId || fullName and ignores ALLURE_ID; other formats use the
        // best explicit id they have.
        let test_case_id = if input.format == Some(SourceFormat::AllureResults) {
            allure_tc
        } else {
            allure_tc.or(ctrf_id).or(allure_id)
        };
        let full_name = if input.full_name.is_empty() {
            input.display_name
        } else {
            input.full_name
        };
        let test_case_hash = test_case_hash(test_case_id, Some(full_name))
            .unwrap_or_else(|| md5_hex(input.display_name));
        let parameters_hash = parameters_hash(input.parameters);
        let env_key = input
            .env_key
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string);
        let environment_hash = environment_hash(env_key.as_deref());
        let series_key = retry_hash(
            &test_case_hash,
            &parameters_hash,
            environment_hash.as_deref(),
        );
        TestIdentity {
            explicit_id,
            test_case_id: test_case_id.map(str::to_string),
            full_name: full_name.to_string(),
            display_name: input.display_name.to_string(),
            source: input.source,
            test_case_hash,
            parameters_hash,
            env_key,
            environment_hash,
            series_key,
            legacy_test_case_hash: allure_id.map(|id| md5_hex(&format!("ALLURE_ID={id}"))),
            history_id: non_empty(input.history_id).map(str::to_string),
        }
    }
}

/// Lowercase hex md5 of the UTF-8 bytes.
pub fn md5_hex(value: &str) -> String {
    let digest = Md5::digest(value.as_bytes());
    let mut out = String::with_capacity(32);
    for b in digest.iter() {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// `md5(testCaseId || fullName)`; `None` when both are empty (as in Allure).
pub fn test_case_hash(test_case_id: Option<&str>, full_name: Option<&str>) -> Option<String> {
    non_empty(test_case_id)
        .or(non_empty(full_name))
        .map(md5_hex)
}

/// The `name:value,…` string hashed by Allure: non-excluded parameters with a non-empty
/// name, missing values as [`UNKNOWN_PARAMETER_VALUE`], sorted by UTF-8 bytes of name then
/// value, duplicates removed. Masked and hidden values are included.
pub fn stringify_parameters(parameters: &[Parameter]) -> String {
    let mut items: Vec<(&str, &str)> = parameters
        .iter()
        .filter(|p| !p.name.is_empty() && !p.excluded)
        .map(|p| {
            (
                p.name.as_str(),
                p.value.as_deref().unwrap_or(UNKNOWN_PARAMETER_VALUE),
            )
        })
        .collect();
    // Rust string ordering is UTF-8 byte ordering.
    items.sort_unstable();
    items.dedup();
    let mut out = String::new();
    for (i, (name, value)) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(name);
        out.push(':');
        out.push_str(value);
    }
    out
}

pub fn parameters_hash(parameters: &[Parameter]) -> String {
    md5_hex(&stringify_parameters(parameters))
}

/// `md5(environment)` unless the environment is absent or `default`.
pub fn environment_hash(env: Option<&str>) -> Option<String> {
    match env {
        None => None,
        Some(DEFAULT_ENVIRONMENT) => None,
        Some(e) => Some(md5_hex(e)),
    }
}

/// `testCaseHash.parametersHash[.environmentHash]`.
pub fn retry_hash(
    test_case_hash: &str,
    parameters_hash: &str,
    environment_hash: Option<&str>,
) -> String {
    match environment_hash {
        Some(e) if !e.is_empty() => format!("{test_case_hash}.{parameters_hash}.{e}"),
        _ => format!("{test_case_hash}.{parameters_hash}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_of_empty() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn explicit_id_precedence() {
        let id = TestIdentity::build(IdentityInput {
            format: Some(SourceFormat::AllureResults),
            allure_id: Some("1001"),
            allure_test_case_id: Some("tc"),
            full_name: "a.b#c",
            display_name: "c",
            ..Default::default()
        });
        assert_eq!(id.explicit_id.as_ref().unwrap().source, IdSource::AllureId);
        assert_eq!(id.test_case_id.as_deref(), Some("tc"));
        assert_eq!(id.test_case_hash, md5_hex("tc"));
        assert_eq!(id.legacy_test_case_hash, Some(md5_hex("ALLURE_ID=1001")));

        let id = TestIdentity::build(IdentityInput {
            format: Some(SourceFormat::JunitXml),
            allure_id: Some("1001"),
            full_name: "a.b#c",
            display_name: "c",
            ..Default::default()
        });
        assert_eq!(id.test_case_hash, md5_hex("1001"));
    }
}
