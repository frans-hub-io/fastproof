//! The committed JSON Schema of the native report format.

/// Path of the schema relative to the repository root.
pub const REPORT_SCHEMA_PATH: &str = "schemas/fastproof-report-v1.json";

/// The schema as committed: pretty JSON with a trailing newline.
pub fn report_schema_text() -> String {
    let mut s = serde_json::to_string_pretty(&fastproof_model::report::report_json_schema())
        .unwrap_or_default();
    s.push('\n');
    s
}
