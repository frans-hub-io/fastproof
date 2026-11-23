//! Parsers of test case imports (`40-test-management.md` §3.7): generic CSV with a column
//! mapping, TestRail CSV, Qase CSV (V1 and V2) and JSON, Allure TestOps CSV (the generic
//! layout of the `tom` migration script) and the FastProof NDJSON export.
//!
//! Pure parsing, no database: a parser reads one file and hands normalised [`ImportedCase`]s
//! (with their external keys and folder paths) and row errors to a visitor. Every input is
//! bounded by [`ParseLimits`]: bytes, rows, columns, cell bytes, JSON depth, steps. CSV text is
//! UTF-8 or UTF-16 with a byte order mark; formulas are never interpreted. HTML that the
//! source tools store in text fields becomes Markdown on an allowlist ([`html::to_markdown`]).

pub mod csv;
pub mod detect;
pub mod export;
pub mod generic;
pub mod html;
pub mod ndjson;
pub mod qase;
pub mod steps;
pub mod testops;
pub mod testrail;
pub mod text;

use std::collections::BTreeMap;
use std::io::Read;

use serde::{Deserialize, Serialize};

pub use detect::{Preview, detect, preview};

/// Format of an import file (the `ImportFormat` of the API).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    #[serde(rename = "generic-csv")]
    GenericCsv,
    #[serde(rename = "testrail-csv")]
    TestrailCsv,
    #[serde(rename = "qase-csv")]
    QaseCsv,
    #[serde(rename = "qase-json")]
    QaseJson,
    #[serde(rename = "testops-csv")]
    TestopsCsv,
    #[serde(rename = "fastproof-ndjson")]
    FastproofNdjson,
}

impl Format {
    pub const ALL: &'static [Format] = &[
        Format::GenericCsv,
        Format::TestrailCsv,
        Format::QaseCsv,
        Format::QaseJson,
        Format::TestopsCsv,
        Format::FastproofNdjson,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Format::GenericCsv => "generic-csv",
            Format::TestrailCsv => "testrail-csv",
            Format::QaseCsv => "qase-csv",
            Format::QaseJson => "qase-json",
            Format::TestopsCsv => "testops-csv",
            Format::FastproofNdjson => "fastproof-ndjson",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.as_str() == s)
    }

    /// Whether the format is CSV (steps layout and column mapping apply).
    pub fn is_csv(self) -> bool {
        !matches!(self, Format::QaseJson | Format::FastproofNdjson)
    }

    /// The `case_keys` kind of the source's ids.
    pub fn key_kind(self) -> &'static str {
        match self {
            Format::TestrailCsv => "testrail",
            Format::QaseCsv | Format::QaseJson => "qase",
            Format::TestopsCsv => "testops",
            Format::GenericCsv | Format::FastproofNdjson => "external",
        }
    }
}

/// How the steps of a CSV are laid out: one row per step (`rows`) or all steps of a case in
/// one cell as a numbered list (`cells`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepsLayout {
    Rows,
    Cells,
}

/// An external key of a case: `kind` is a `case_keys` kind (`testrail`, `qase`, `testops`,
/// `external`, `allure_id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedKey {
    pub kind: String,
    pub value: String,
}

/// One step; text is Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportedStep {
    /// Kept when it is a FastProof step id (NDJSON), else empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub action: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub expected: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// Attachment names (zip entries, or `testops:<id>`) of the step.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ImportedStep>,
}

/// A link of a case: `kind` is `link`, `issue` or `tms`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedLink {
    #[serde(default)]
    pub name: String,
    pub url: String,
    #[serde(default = "link_kind", rename = "type")]
    pub kind: String,
}

fn link_kind() -> String {
    "link".into()
}

/// A field value as the source gives it; the importer maps it to the field definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ImportedValue {
    Text(String),
    Many(Vec<String>),
    Number(f64),
}

/// An attachment of a case: `name` as shown, `source` the zip entry name or `testops:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedAttachment {
    pub name: String,
    pub source: String,
    /// Id the attachment had in the exporting FastProof project (NDJSON), so `attachment:<id>`
    /// references in Markdown can be rewritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_id: Option<String>,
}

/// A normalised case. Field names are the source's (column header, custom field name or
/// FastProof field key); the importer maps them to field definitions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportedCase {
    /// Row (CSV record, 1 = the header) or line (NDJSON) or position (JSON) of the case.
    pub row: u64,
    pub keys: Vec<ImportedKey>,
    /// The number the case had in the source (TestOps AllureID, FastProof number).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    pub name: String,
    /// Folder path from the root.
    pub folder: Vec<String>,
    /// `draft`, `active` or `outdated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub description: String,
    pub preconditions: String,
    pub steps: Vec<ImportedStep>,
    pub links: Vec<ImportedLink>,
    pub tags: Vec<String>,
    pub fields: BTreeMap<String, ImportedValue>,
    pub automated: bool,
    /// Full names of code identities (TestOps automated cases, NDJSON).
    pub full_names: Vec<String>,
    pub attachments: Vec<ImportedAttachment>,
}

/// A field definition carried by the source (FastProof NDJSON header).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedFieldDef {
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
}

/// An error of one row; the import goes on without it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowError {
    pub row: u64,
    pub code: String,
    pub message: String,
}

impl RowError {
    pub fn new(row: u64, code: &str, message: impl Into<String>) -> Self {
        Self {
            row,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What a parser hands to its visitor.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Case(Box<ImportedCase>),
    Error(RowError),
    FieldDefs(Vec<ImportedFieldDef>),
}

fn steps_bytes(s: &[ImportedStep]) -> usize {
    s.iter()
        .map(|x| {
            std::mem::size_of::<ImportedStep>()
                + x.id.len()
                + x.action.len()
                + x.expected.len()
                + x.data.len()
                + strings_bytes(&x.attachments)
                + steps_bytes(&x.steps)
        })
        .sum()
}

fn strings_bytes(v: &[String]) -> usize {
    v.iter()
        .map(|x| std::mem::size_of::<String>() + x.len())
        .sum()
}

impl Item {
    /// About how much memory the item holds (text plus the size of its parts), for bounding
    /// the queue between the parser and the importer.
    pub fn approx_bytes(&self) -> usize {
        match self {
            Item::Case(c) => {
                let fields: usize = c
                    .fields
                    .iter()
                    .map(|(k, v)| {
                        64 + k.len()
                            + match v {
                                ImportedValue::Text(t) => t.len(),
                                ImportedValue::Many(m) => strings_bytes(m),
                                ImportedValue::Number(_) => 8,
                            }
                    })
                    .sum();
                std::mem::size_of::<ImportedCase>()
                    + c.keys
                        .iter()
                        .map(|k| 48 + k.kind.len() + k.value.len())
                        .sum::<usize>()
                    + c.name.len()
                    + strings_bytes(&c.folder)
                    + c.status.as_ref().map_or(0, String::len)
                    + c.description.len()
                    + c.preconditions.len()
                    + steps_bytes(&c.steps)
                    + c.links
                        .iter()
                        .map(|l| 72 + l.name.len() + l.url.len() + l.kind.len())
                        .sum::<usize>()
                    + strings_bytes(&c.tags)
                    + fields
                    + strings_bytes(&c.full_names)
                    + c.attachments
                        .iter()
                        .map(|a| {
                            72 + a.name.len()
                                + a.source.len()
                                + a.old_id.as_ref().map_or(0, String::len)
                        })
                        .sum::<usize>()
            }
            Item::Error(e) => std::mem::size_of::<RowError>() + e.code.len() + e.message.len(),
            Item::FieldDefs(d) => d
                .iter()
                .map(|f| {
                    std::mem::size_of::<ImportedFieldDef>()
                        + f.key.len()
                        + f.name.len()
                        + f.field_type.len()
                        + strings_bytes(&f.options)
                        + f.source_label.as_ref().map_or(0, String::len)
                })
                .sum(),
        }
    }
}

/// A folder (suite) name cut to at most one character past `max` bytes (see
/// [`ParseLimits::max_name_bytes`]).
pub(crate) fn cap_name(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .find(|&e| e > max)
        .unwrap_or(s.len());
    s[..end].to_string()
}

/// An error that stops the import.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// A limit of [`ParseLimits`]; `limit` is the camelCase name of the `TmLimits` field (or
    /// `maxColumns`, `maxJsonDepth`).
    #[error("{limit} ({max}) exceeded")]
    Limit { limit: &'static str, max: u64 },
    /// The file is not in the format.
    #[error("{0}")]
    Invalid(String),
    /// The visitor stopped the parse.
    #[error("stopped")]
    Stopped,
}

impl ImportError {
    pub fn limit(limit: &'static str, max: impl TryInto<u64>) -> Self {
        Self::Limit {
            limit,
            max: max.try_into().unwrap_or(u64::MAX),
        }
    }

    pub fn invalid(m: impl Into<String>) -> Self {
        Self::Invalid(m.into())
    }

    /// The API error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Limit { .. } => "limit_reached",
            Self::Invalid(_) => "invalid_file",
            Self::Stopped => "cancelled",
        }
    }
}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        Self::Invalid(format!("reading the file failed: {e}"))
    }
}

/// Columns of a CSV, at most.
pub const MAX_COLUMNS: usize = 500;
/// Nesting of a JSON document, at most.
pub const MAX_JSON_DEPTH: u32 = 64;

/// Limits of one parse (from `TmLimits`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    /// Bytes of the file (after decoding).
    pub max_bytes: u64,
    /// Rows (CSV records) or cases (JSON, NDJSON).
    pub max_rows: usize,
    /// Bytes of one cell or text value.
    pub max_cell_bytes: usize,
    /// Columns of a CSV.
    pub max_columns: usize,
    /// Steps of one case on all levels.
    pub max_steps: usize,
    /// Nesting of steps.
    pub max_step_depth: usize,
    /// Tags of one case.
    pub max_tags: usize,
    /// Bytes of a folder (suite) name. Longer names are cut while reading to one character
    /// past this, so the importer still reports the row, and memory stays bounded.
    pub max_name_bytes: usize,
    /// Levels of a folder path. Suite parent chains are followed at most one level past it
    /// (the importer reports deeper paths).
    pub max_folder_depth: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_bytes: 100 * 1024 * 1024,
            max_rows: 200_000,
            max_cell_bytes: 65_536,
            max_columns: MAX_COLUMNS,
            max_steps: 500,
            max_step_depth: 3,
            max_tags: 50,
            max_name_bytes: 500,
            max_folder_depth: 10,
        }
    }
}

/// Options of one parse.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Column → target (see [`generic::Target`]); for the tool formats only overrides of
    /// custom columns.
    pub mapping: Option<BTreeMap<String, String>>,
    /// The steps layout of a CSV; detected when absent.
    pub steps_layout: Option<StepsLayout>,
    pub limits: ParseLimits,
}

/// What the visitor answers: `false` stops the parse with [`ImportError::Stopped`].
pub type Visit<'a> = dyn FnMut(Item) -> bool + 'a;

/// Parses a whole file of `format` and hands every case and row error to `visit`.
pub fn parse(
    format: Format,
    input: impl Read,
    opts: &ParseOptions,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let r = text::decode(input, opts.limits.max_bytes)?;
    match format {
        Format::GenericCsv => generic::parse(r, opts, visit),
        Format::TestrailCsv => testrail::parse(r, opts, visit),
        Format::QaseCsv => qase::parse_csv(r, opts, visit),
        Format::QaseJson => qase::parse_json(r, opts, visit),
        Format::TestopsCsv => testops::parse(r, opts, visit),
        Format::FastproofNdjson => ndjson::parse(r, opts, visit),
    }
}

/// Collects every item of a parse (tests and small files).
pub fn parse_all(
    format: Format,
    input: impl Read,
    opts: &ParseOptions,
) -> Result<(Vec<ImportedCase>, Vec<RowError>), ImportError> {
    let mut cases = Vec::new();
    let mut errors = Vec::new();
    parse(format, input, opts, &mut |item| {
        match item {
            Item::Case(c) => cases.push(*c),
            Item::Error(e) => errors.push(e),
            Item::FieldDefs(_) => {}
        }
        true
    })?;
    Ok((cases, errors))
}

/// Bounds a case after parsing: steps (count and depth) and tags; the excess becomes a row
/// error and the case is dropped.
pub fn check_case(c: &ImportedCase, l: &ParseLimits) -> Result<(), RowError> {
    fn walk(steps: &[ImportedStep], depth: usize, count: &mut usize, l: &ParseLimits) -> bool {
        if steps.is_empty() {
            return true;
        }
        if depth > l.max_step_depth {
            return false;
        }
        for s in steps {
            *count += 1;
            if *count > l.max_steps || !walk(&s.steps, depth + 1, count, l) {
                return false;
            }
        }
        true
    }
    let mut count = 0;
    if !walk(&c.steps, 1, &mut count, l) {
        return Err(RowError::new(
            c.row,
            "limit_reached",
            format!(
                "more than {} steps or deeper than {} levels",
                l.max_steps, l.max_step_depth
            ),
        ));
    }
    if c.tags.len() > l.max_tags {
        return Err(RowError::new(
            c.row,
            "limit_reached",
            format!("more than {} tags", l.max_tags),
        ));
    }
    if c.name.trim().is_empty() {
        return Err(RowError::new(c.row, "invalid_case", "the case has no name"));
    }
    Ok(())
}

/// Hands a case to the visitor after [`check_case`] (a failed check is a row error).
pub(crate) fn emit(
    c: ImportedCase,
    l: &ParseLimits,
    visit: &mut Visit<'_>,
) -> Result<(), ImportError> {
    let item = match check_case(&c, l) {
        Ok(()) => Item::Case(Box::new(c)),
        Err(e) => Item::Error(e),
    };
    if visit(item) {
        Ok(())
    } else {
        Err(ImportError::Stopped)
    }
}

pub(crate) fn emit_error(e: RowError, visit: &mut Visit<'_>) -> Result<(), ImportError> {
    if visit(Item::Error(e)) {
        Ok(())
    } else {
        Err(ImportError::Stopped)
    }
}

/// Splits a list cell (`a, b; c` or one per line) into trimmed non-empty items.
pub fn split_list(s: &str) -> Vec<String> {
    s.split([',', ';', '\n'])
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Lowercase letters and digits of a header (for matching column names).
pub fn norm_header(h: &str) -> String {
    h.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Maps a status of a source tool to `draft`, `active` or `outdated`.
pub fn map_status(s: &str) -> Option<String> {
    let v = s.trim().to_lowercase();
    let out = match v.as_str() {
        "draft" | "new" | "in progress" | "review" | "needs review" => "draft",
        "active" | "actual" | "ready" | "approved" | "done" | "0" => "active",
        "outdated" | "deprecated" | "obsolete" | "archived" | "2" => "outdated",
        "1" => "draft",
        _ => return None,
    };
    Some(out.to_string())
}

/// Text of a source field: HTML becomes Markdown, the rest is kept.
pub fn rich(s: &str) -> String {
    let t = s.trim_matches(|c| c == '\r' || c == '\n');
    if html::looks_like_html(t) {
        html::to_markdown(t)
    } else {
        t.replace("\r\n", "\n")
    }
}

/// Splits a folder path (`A > B > C`, `A / B`, or `A\tB`) into names.
pub fn folder_path(s: &str, sep: &str) -> Vec<String> {
    s.split(sep)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_bytes_grows_with_the_text_of_a_case() {
        let small = Item::Case(Box::default());
        let mut c = ImportedCase {
            description: "d".repeat(10_000),
            folder: vec!["f".repeat(1000); 3],
            ..Default::default()
        };
        c.steps.push(ImportedStep {
            action: "a".repeat(5000),
            steps: vec![ImportedStep {
                expected: "e".repeat(2000),
                ..Default::default()
            }],
            ..Default::default()
        });
        let big = Item::Case(Box::new(c)).approx_bytes();
        assert!(big >= small.approx_bytes() + 20_000, "{big}");
        assert!(big < small.approx_bytes() + 21_000, "{big}");
    }

    #[test]
    fn cap_name_keeps_one_character_past_the_limit() {
        assert_eq!(cap_name("abc", 3), "abc");
        assert_eq!(cap_name("abcd", 2), "abc");
        assert_eq!(cap_name("ééé", 3), "éé");
    }
}
