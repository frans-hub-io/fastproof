use fastproof_model::{AttachmentRef, Label, Link, ParsedResult, TestIdentity};
use serde::{Deserialize, Serialize};

use crate::IngestOptions;
use crate::error::IngestError;
use crate::limits::{Budget, Limits};

const MAX_WARNINGS: usize = 1000;

/// A non-fatal problem found while parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

/// Attachment body that was inline in a report (Playwright `body`).
pub(crate) struct InlineAttachment {
    pub key: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

pub(crate) struct Ctx<'a> {
    pub limits: &'a Limits,
    pub options: &'a IngestOptions,
    pub budget: Budget,
    pub warnings: Vec<Warning>,
    pub suppressed_warnings: u64,
    pub inline: Vec<InlineAttachment>,
    /// Decoded bytes held in `inline`.
    pub inline_bytes: u64,
    /// Attachment references created by the parsers so far.
    pub attachment_refs: u64,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(options: &'a IngestOptions, budget: Budget) -> Ctx<'a> {
        Ctx {
            limits: &options.limits,
            options,
            budget,
            warnings: Vec::new(),
            suppressed_warnings: 0,
            inline: Vec::new(),
            inline_bytes: 0,
            attachment_refs: 0,
        }
    }

    pub(crate) fn warn(&mut self, file: Option<&str>, message: impl Into<String>) {
        if self.warnings.len() >= MAX_WARNINGS {
            self.suppressed_warnings += 1;
            return;
        }
        self.warnings.push(Warning {
            file: file.map(str::to_string),
            message: message.into(),
        });
    }

    pub(crate) fn steps(&self, file: &str) -> StepBudget {
        StepBudget {
            count: 0,
            max: self.limits.max_steps_per_result,
            attachments: 0,
            max_attachments: self.limits.max_attachments_per_result,
            file: file.to_string(),
        }
    }

    /// Charges `n` bytes of a copy made by a parser (shared parts of a test repeated per
    /// attempt, per test of a spec, and so on) to the upload byte budget.
    pub(crate) fn charge_copy(&self, n: u64, file: &str) -> Result<(), IngestError> {
        self.budget.charge(n, file)
    }

    /// Counts one attachment reference of the upload.
    pub(crate) fn count_ref(&mut self, file: &str) -> Result<(), IngestError> {
        self.add_refs(1, file)
    }

    /// Counts `n` attachment references of the upload.
    pub(crate) fn add_refs(&mut self, n: u64, file: &str) -> Result<(), IngestError> {
        self.attachment_refs += n;
        if self.attachment_refs > self.limits.max_attachments {
            return Err(IngestError::limit_at(
                "max_attachments",
                self.limits.max_attachments,
                file.to_string(),
            ));
        }
        Ok(())
    }

    /// Fails when one test has more attempts than allowed.
    pub(crate) fn check_attempts(&self, n: usize, file: &str) -> Result<(), IngestError> {
        if n as u64 > self.limits.max_attempts_per_result {
            return Err(IngestError::limit_at(
                "max_attempts_per_result",
                self.limits.max_attempts_per_result,
                file.to_string(),
            ));
        }
        Ok(())
    }

    /// Fails when one result has more labels than allowed.
    pub(crate) fn check_labels(&self, n: usize, file: &str) -> Result<(), IngestError> {
        if n as u64 > self.limits.max_labels_per_result {
            return Err(IngestError::limit_at(
                "max_labels_per_result",
                self.limits.max_labels_per_result,
                file.to_string(),
            ));
        }
        Ok(())
    }

    /// Truncates a test name or full name to `max_name_bytes`, with a warning.
    pub(crate) fn name(&mut self, mut s: String, what: &str, file: &str) -> String {
        let max = self.limits.max_name_bytes;
        if s.len() as u64 > max {
            let original = s.len();
            truncate_with_ellipsis(&mut s, max as usize);
            self.warn(
                Some(file),
                format!("{what} of {original} bytes truncated to max_name_bytes ({max})"),
            );
        }
        s
    }

    /// Stores an inline attachment body and returns a reference to it.
    pub(crate) fn add_inline(
        &mut self,
        name: &str,
        content_type: Option<String>,
        bytes: Vec<u8>,
        file: &str,
    ) -> Result<AttachmentRef, IngestError> {
        self.budget.charge(bytes.len() as u64, file)?;
        self.count_ref(file)?;
        self.inline_bytes += bytes.len() as u64;
        if self.inline_bytes > self.limits.max_inline_attachment_bytes {
            return Err(IngestError::limit_at(
                "max_inline_attachment_bytes",
                self.limits.max_inline_attachment_bytes,
                file.to_string(),
            ));
        }
        let key = format!("inline/{}/{}", self.inline.len(), sanitize_name(name));
        let mut r = AttachmentRef::new(name, key.clone(), content_type.clone());
        r.size = Some(bytes.len() as u64);
        r.resolved_path = Some(key.clone());
        self.inline.push(InlineAttachment {
            key,
            content_type,
            bytes,
        });
        Ok(r)
    }

    pub(crate) fn env_key(&self, format_env: Option<&str>) -> Option<String> {
        let format_env = format_env.map(str::trim).filter(|s| !s.is_empty());
        let configured = self
            .options
            .environment_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (configured, format_env) {
            (Some(c), Some(f)) => Some(format!("{c}:{f}")),
            (Some(c), None) => Some(c.to_string()),
            (None, Some(f)) => Some(f.to_string()),
            (None, None) => None,
        }
    }

    /// Applies the post-identity steps shared by all formats.
    pub(crate) fn finish(&self, result: &mut ParsedResult) {
        if self.options.redact_secret_parameters {
            result.redact_secret_parameters();
        }
    }
}

/// Cuts `s` to at most `max` bytes on a char boundary, ending with an ellipsis.
pub(crate) fn truncate_with_ellipsis(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    const ELLIPSIS: char = '\u{2026}';
    let mut end = max.saturating_sub(ELLIPSIS.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    if max >= ELLIPSIS.len_utf8() {
        s.push(ELLIPSIS);
    }
}

fn sanitize_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() { "attachment".into() } else { s }
}

/// Counts steps and attachment references of one result across nesting.
pub(crate) struct StepBudget {
    pub count: u64,
    max: u64,
    attachments: u64,
    max_attachments: u64,
    file: String,
}

impl StepBudget {
    pub(crate) fn tick(&mut self) -> Result<(), IngestError> {
        self.count += 1;
        if self.count > self.max {
            return Err(IngestError::limit_at(
                "max_steps_per_result",
                self.max,
                self.file.clone(),
            ));
        }
        Ok(())
    }

    pub(crate) fn attachments(&self) -> u64 {
        self.attachments
    }

    /// Counts one attachment reference of the result.
    pub(crate) fn attachment(&mut self) -> Result<(), IngestError> {
        self.attachments += 1;
        if self.attachments > self.max_attachments {
            return Err(IngestError::limit_at(
                "max_attachments_per_result",
                self.max_attachments,
                self.file.clone(),
            ));
        }
        Ok(())
    }
}

fn opt_len(s: &Option<String>) -> usize {
    s.as_ref().map_or(0, |s| s.len())
}

/// Approximate heap size of a test identity.
pub(crate) fn identity_bytes(id: &TestIdentity) -> u64 {
    (id.full_name.len()
        + id.display_name.len()
        + opt_len(&id.source.file)
        + opt_len(&id.source.code_ref)
        + opt_len(&id.test_case_id)
        + opt_len(&id.env_key)
        + opt_len(&id.history_id)
        + id.explicit_id.as_ref().map_or(0, |e| e.value.len())
        + 4 * 64) as u64
}

/// Approximate heap size of labels.
pub(crate) fn labels_bytes(labels: &[Label]) -> u64 {
    labels
        .iter()
        .map(|l| (l.name.len() + l.value.len() + 48) as u64)
        .sum()
}

/// Approximate heap size of links.
pub(crate) fn links_bytes(links: &[Link]) -> u64 {
    links
        .iter()
        .map(|l| (opt_len(&l.name) + l.url.len() + opt_len(&l.link_type) + 72) as u64)
        .sum()
}

/// Approximate heap size of a result, used to charge copies to the byte budget.
pub(crate) fn result_bytes(r: &ParsedResult) -> u64 {
    let d = &r.status_details;
    let details =
        opt_len(&d.message) + opt_len(&d.trace) + opt_len(&d.actual) + opt_len(&d.expected);
    let params: usize = r
        .parameters
        .iter()
        .map(|p| p.name.len() + opt_len(&p.value) + 56)
        .sum();
    let atts: usize = r
        .attachments
        .iter()
        .map(|a| a.name.len() + a.source.len() + opt_len(&a.resolved_path) + 160)
        .sum();
    let logs = r.stdout.as_ref().map_or(0, |t| t.text.len())
        + r.stderr.as_ref().map_or(0, |t| t.text.len());
    let steps: u64 = r
        .steps
        .iter()
        .chain(r.fixtures.befores.iter())
        .chain(r.fixtures.afters.iter())
        .map(crate::allure::step_bytes)
        .sum();
    (512 + r.name.len()
        + r.full_name.len()
        + opt_len(&r.source_uuid)
        + opt_len(&r.raw_status)
        + opt_len(&r.source_file)
        + opt_len(&r.description.text)
        + opt_len(&r.description.html)
        + details
        + params
        + atts
        + logs) as u64
        + identity_bytes(&r.identity)
        + labels_bytes(&r.labels)
        + links_bytes(&r.links)
        + steps
}
