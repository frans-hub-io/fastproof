//! The native FastProof report format, version 1 (`schemas/fastproof-report-v1.json`).
//!
//! One [`ReportResult`] is one attempt of a test. It maps field by field onto
//! [`crate::ParsedResult`]; identity (test case hash, parameters hash, series key) is computed
//! by the server exactly as for the other formats. Attachments are uploaded first and
//! referenced by the id the server returned.

use serde::{Deserialize, Serialize};

use crate::result::{Label, Link, Parameter};
use crate::status::Status;

/// Schema version written in `$id` of the JSON Schema.
pub const REPORT_SCHEMA_ID: &str = "https://fastproof.franshub.io/schemas/fastproof-report-v1.json";

/// Error details of a result or step.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
}

/// A reference to an attachment uploaded with `POST .../attachments`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportAttachment {
    /// Attachment id returned by the server.
    pub id: String,
    /// Name to show; defaults to the name given at upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Content type to show; defaults to the one given at upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// Source location of a test.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// A step; nested steps form a tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportStep {
    pub name: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReportError>,
    /// Start, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Stop, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ReportAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub steps: Vec<ReportStep>,
}

/// Status of a reported result: a final status, or `running` for a result opened before its
/// test finished (sent again later with the same `id` and a final status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Passed,
    Failed,
    Broken,
    Skipped,
    #[default]
    Unknown,
    Running,
}

impl ReportStatus {
    /// The final status (`unknown` while running).
    pub fn status(self) -> Status {
        match self {
            ReportStatus::Passed => Status::Passed,
            ReportStatus::Failed => Status::Failed,
            ReportStatus::Broken => Status::Broken,
            ReportStatus::Skipped => Status::Skipped,
            ReportStatus::Unknown | ReportStatus::Running => Status::Unknown,
        }
    }

    pub fn is_running(self) -> bool {
        self == ReportStatus::Running
    }
}

impl From<Status> for ReportStatus {
    fn from(s: Status) -> Self {
        match s {
            Status::Passed => ReportStatus::Passed,
            Status::Failed => ReportStatus::Failed,
            Status::Broken => ReportStatus::Broken,
            Status::Skipped => ReportStatus::Skipped,
            Status::Unknown => ReportStatus::Unknown,
        }
    }
}

/// One attempt of one test, the unit of `POST /api/v1/projects/{key}/launches/{id}/results`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReportResult {
    /// Client id of this attempt, unique within the launch: sending it again is a no-op.
    pub id: String,
    /// Short name of the test.
    pub name: String,
    /// Stable full name (for example `com.example.CheckoutTest#pays`); identity is computed
    /// from it unless `testCaseId` is given. Defaults to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// Explicit stable test case id (takes precedence over the full name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_case_id: Option<String>,
    /// A final status, or `running` while the test runs.
    pub status: ReportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReportError>,
    /// Start, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    /// Stop, milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Zero-based retry number, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    /// Plain text or Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// HTML; sanitized by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_html: Option<String>,
    /// Parameters; `mode` `masked` or `hidden` values count for identity but are never shown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ReportStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ReportAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// W3C `traceparent` of the test, for correlation with traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ReportSource>,
}

/// The JSON Schema (draft 2020-12) of [`ReportResult`], generated from the Rust types.
#[cfg(feature = "openapi")]
pub fn report_json_schema() -> serde_json::Value {
    use utoipa::openapi::ComponentsBuilder;
    use utoipa::{PartialSchema, ToSchema};

    let mut schemas = Vec::new();
    ReportResult::schemas(&mut schemas);
    let mut components = ComponentsBuilder::new();
    for (name, schema) in schemas {
        components = components.schema(name, schema);
    }
    components = components.schema(ReportResult::name(), ReportResult::schema());
    let components = components.build();
    let mut defs = serde_json::to_value(&components.schemas).unwrap_or_default();
    rewrite_refs(&mut defs);
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": REPORT_SCHEMA_ID,
        "title": "FastProof report result, version 1",
        "description": "One attempt of one test. Send one object, an array of objects, or NDJSON (one object per line).",
        "$ref": "#/$defs/ReportResult",
        "$defs": defs,
    })
}

#[cfg(feature = "openapi")]
fn rewrite_refs(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                if k == "$ref"
                    && let serde_json::Value::String(s) = val
                    && let Some(rest) = s.strip_prefix("#/components/schemas/")
                {
                    *s = format!("#/$defs/{rest}");
                } else {
                    rewrite_refs(val);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}
