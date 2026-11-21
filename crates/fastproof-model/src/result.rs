use serde::{Deserialize, Serialize};

use crate::attachment::AttachmentRef;
use crate::identity::TestIdentity;
use crate::status::{Stage, Status};

/// Input format a result was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SourceFormat {
    #[default]
    AllureResults,
    JunitXml,
    Ctrf,
    PlaywrightJson,
    /// The native FastProof report JSON (streaming results API).
    FastproofJson,
}

impl SourceFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceFormat::AllureResults => "allure-results",
            SourceFormat::JunitXml => "junit-xml",
            SourceFormat::Ctrf => "ctrf",
            SourceFormat::PlaywrightJson => "playwright-json",
            SourceFormat::FastproofJson => "fastproof-json",
        }
    }

    pub fn parse(value: &str) -> Option<SourceFormat> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allure" | "allure-results" | "allure2" | "allure3" => {
                Some(SourceFormat::AllureResults)
            }
            "junit" | "junit-xml" | "xunit" => Some(SourceFormat::JunitXml),
            "ctrf" => Some(SourceFormat::Ctrf),
            "playwright" | "playwright-json" => Some(SourceFormat::PlaywrightJson),
            "fastproof" | "fastproof-json" => Some(SourceFormat::FastproofJson),
            _ => None,
        }
    }
}

/// How a parameter value may be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ParameterMode {
    #[default]
    Default,
    Masked,
    Hidden,
}

impl ParameterMode {
    pub fn is_secret(self) -> bool {
        !matches!(self, ParameterMode::Default)
    }
}

/// A test or step parameter.
///
/// `value` is `None` when the writer omitted it, and also after redaction of masked and
/// hidden parameters (see [`Parameter::redact`]); identity hashes are computed before
/// redaction, so they still cover secret values.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Parameter {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub excluded: bool,
    #[serde(default)]
    pub mode: ParameterMode,
}

impl Parameter {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Parameter {
            name: name.into(),
            value: Some(value.into()),
            excluded: false,
            mode: ParameterMode::Default,
        }
    }

    /// Value safe to display: `None` for hidden, `******` for masked.
    pub fn display_value(&self) -> Option<&str> {
        match self.mode {
            ParameterMode::Default => self.value.as_deref(),
            ParameterMode::Masked => Some("******"),
            ParameterMode::Hidden => None,
        }
    }

    /// Drops the value of masked and hidden parameters.
    pub fn redact(&mut self) {
        if self.mode.is_secret() {
            self.value = None;
        }
    }
}

/// A key/value label (Allure labels, JUnit properties, CTRF tags, Playwright annotations).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Label {
    pub name: String,
    pub value: String,
}

impl Label {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Label {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// A link from a result (`type` is `link`, `issue`, `tms` or a writer-specific value).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Link {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
}

/// Error details of a result, attempt or step.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StatusDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub known: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    /// Flaky flag reported by the writer itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flaky: bool,
}

impl StatusDetails {
    pub fn is_empty(&self) -> bool {
        self == &StatusDetails::default()
    }
}

/// Description of a test: sanitized HTML and the plain/markdown text are kept separately.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Description {
    /// Plain text or markdown as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// HTML after sanitization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl Description {
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.html.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    #[default]
    Step,
    Fixture,
}

/// A step; fixtures (befores/afters) use the same shape with `kind = fixture`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Step {
    pub name: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "StatusDetails::is_empty")]
    pub status_details: StatusDetails,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
    /// Duration in milliseconds when the writer gives it without start/stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub kind: StepKind,
}

impl Step {
    /// Number of steps in this subtree including the step itself.
    pub fn count(&self) -> usize {
        1 + self.steps.iter().map(Step::count).sum::<usize>()
    }

    pub fn visit_attachments_mut(&mut self, f: &mut dyn FnMut(&mut AttachmentRef)) {
        for a in &mut self.attachments {
            f(a);
        }
        for s in &mut self.steps {
            s.visit_attachments_mut(f);
        }
    }

    pub fn visit_parameters_mut(&mut self, f: &mut dyn FnMut(&mut Parameter)) {
        for p in &mut self.parameters {
            f(p);
        }
        for s in &mut self.steps {
            s.visit_parameters_mut(f);
        }
    }
}

/// Setup and teardown fixtures attached to a result.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Fixtures {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub befores: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub afters: Vec<Step>,
}

impl Fixtures {
    pub fn is_empty(&self) -> bool {
        self.befores.is_empty() && self.afters.is_empty()
    }
}

/// Captured output, bounded by the ingest limits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BoundedText {
    pub text: String,
    /// Original size in bytes before truncation.
    pub original_bytes: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// One parsed test execution (one attempt). Several results sharing a series key within a
/// launch are attempts of one logical result; see [`crate::retry::group_retries`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ParsedResult {
    /// Writer-side uuid (Allure `uuid`, CTRF `id`, Playwright test id + retry), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uuid: Option<String>,
    pub identity: TestIdentity,
    pub name: String,
    pub full_name: String,
    pub status: Status,
    /// Status string exactly as written (e.g. `xfail`, `timedOut`, `pending`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_status: Option<String>,
    #[serde(default, skip_serializing_if = "StatusDetails::is_empty")]
    pub status_details: StatusDetails,
    #[serde(default)]
    pub stage: Stage,
    /// True when the stage is not finished or the stop time is missing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Zero-based retry number when the format states it explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Description::is_empty")]
    pub description: Description,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default, skip_serializing_if = "Fixtures::is_empty")]
    pub fixtures: Fixtures,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<BoundedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<BoundedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    pub format: SourceFormat,
    /// Upload-relative file the result was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

impl ParsedResult {
    /// All steps of the result (fixtures included), counted across nesting.
    pub fn step_count(&self) -> usize {
        self.steps
            .iter()
            .chain(self.fixtures.befores.iter())
            .chain(self.fixtures.afters.iter())
            .map(Step::count)
            .sum()
    }

    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|l| l.name == name)
            .map(|l| l.value.as_str())
    }

    pub fn visit_attachments_mut(&mut self, f: &mut dyn FnMut(&mut AttachmentRef)) {
        for a in &mut self.attachments {
            f(a);
        }
        for s in self
            .steps
            .iter_mut()
            .chain(self.fixtures.befores.iter_mut())
            .chain(self.fixtures.afters.iter_mut())
        {
            s.visit_attachments_mut(f);
        }
    }

    /// Removes masked and hidden parameter values everywhere in the result.
    pub fn redact_secret_parameters(&mut self) {
        for p in &mut self.parameters {
            p.redact();
        }
        for s in self
            .steps
            .iter_mut()
            .chain(self.fixtures.befores.iter_mut())
            .chain(self.fixtures.afters.iter_mut())
        {
            s.visit_parameters_mut(&mut |p| p.redact());
        }
    }
}

/// An Allure container: fixtures shared by its children (result uuids).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Container {
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub befores: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub afters: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

/// A run-level error (Allure globals).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GlobalError {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// Run-level errors and attachments.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Globals {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<GlobalError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
}

impl Globals {
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty() && self.attachments.is_empty()
    }
}

/// A category rule from Allure `categories.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Category {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Sanitized HTML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_statuses: Vec<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<bool>,
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}
