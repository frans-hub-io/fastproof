use fastproof_model::{AttachmentRef, ParsedResult};
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
            file: file.to_string(),
        }
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

/// Counts steps of one result across nesting.
pub(crate) struct StepBudget {
    pub count: u64,
    max: u64,
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
}
