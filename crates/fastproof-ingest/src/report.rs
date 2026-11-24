//! The native FastProof report JSON ([`fastproof_model::ReportResult`]) into the model, with
//! the same limits, sanitization and identity rules as the file formats.

use fastproof_model::{
    AttachmentId, AttachmentRef, Description, IdentityInput, ParsedResult, ReportAttachment,
    ReportError, ReportResult, ReportStep, SourceFormat, SourceRef, Stage, StatusDetails, Step,
    StepKind, TestIdentity,
};

use crate::Limits;
use crate::allure::trace_id_from_traceparent;
use crate::text::{bound, sanitize_html};

/// Longest client id of a result.
pub const MAX_CLIENT_ID_BYTES: usize = 256;
/// Attachment references per result, steps included.
pub const MAX_ATTACHMENTS_PER_RESULT: usize = 1000;

/// Options of [`report_to_parsed`].
#[derive(Debug, Clone)]
pub struct ReportOptions<'a> {
    pub limits: &'a Limits,
    /// Environment key of the launch (part of every series key).
    pub environment_key: Option<&'a str>,
}

struct Walk<'a> {
    limits: &'a Limits,
    steps: u64,
    attachments: usize,
}

impl Walk<'_> {
    fn text(&self, what: &str, s: &str) -> Result<(), String> {
        if s.len() as u64 > self.limits.max_string_bytes {
            return Err(format!(
                "{what} is longer than max_string_bytes ({})",
                self.limits.max_string_bytes
            ));
        }
        Ok(())
    }

    fn details(&self, e: Option<ReportError>) -> Result<StatusDetails, String> {
        let Some(e) = e else {
            return Ok(StatusDetails::default());
        };
        let max = self.limits.max_log_bytes;
        let b = |s: Option<String>| s.map(|t| bound(&t, max).text);
        Ok(StatusDetails {
            message: b(e.message),
            trace: b(e.trace),
            actual: b(e.actual),
            expected: b(e.expected),
            ..StatusDetails::default()
        })
    }

    fn attachment(&mut self, a: ReportAttachment) -> Result<AttachmentRef, String> {
        self.attachments += 1;
        if self.attachments > MAX_ATTACHMENTS_PER_RESULT {
            return Err(format!(
                "more than {MAX_ATTACHMENTS_PER_RESULT} attachment references in one result"
            ));
        }
        let id = a.id.trim().to_ascii_lowercase();
        if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(format!("attachment id {:?} is not an attachment id", a.id));
        }
        let name = a.name.unwrap_or_default();
        self.text("attachment name", &name)?;
        let mut r = AttachmentRef::new(name, format!("attachment:{id}"), a.content_type);
        r.id = Some(AttachmentId(id));
        Ok(r)
    }

    fn step(&mut self, s: ReportStep, depth: u32) -> Result<Step, String> {
        self.steps += 1;
        if self.steps > self.limits.max_steps_per_result {
            return Err(format!(
                "more than max_steps_per_result ({}) steps",
                self.limits.max_steps_per_result
            ));
        }
        if depth > self.limits.max_json_depth {
            return Err("steps are nested too deeply".into());
        }
        self.text("step name", &s.name)?;
        let mut attachments = Vec::with_capacity(s.attachments.len());
        for a in s.attachments {
            attachments.push(self.attachment(a)?);
        }
        let mut steps = Vec::with_capacity(s.steps.len());
        for c in s.steps {
            steps.push(self.step(c, depth + 1)?);
        }
        Ok(Step {
            name: s.name,
            status: s.status,
            status_details: self.details(s.error)?,
            start: s.start,
            stop: s.stop,
            duration_ms: s.duration_ms,
            parameters: s.parameters,
            attachments,
            steps,
            kind: StepKind::Step,
        })
    }
}

/// Warnings for a report result whose names [`report_to_parsed`] cuts to `max_name_bytes`.
pub fn name_warnings(r: &ReportResult, limits: &Limits) -> Vec<String> {
    let max = limits.max_name_bytes;
    let full = r.full_name.as_deref().filter(|f| !f.trim().is_empty());
    [("name", Some(r.name.as_str())), ("fullName", full)]
        .into_iter()
        .filter_map(|(what, v)| {
            let n = v?.len();
            (n as u64 > max)
                .then(|| format!("{what} of {n} bytes truncated to max_name_bytes ({max})"))
        })
        .collect()
}

/// Validates a report result and converts it, computing its identity. Masked and hidden
/// parameter values are dropped after hashing. The error is one sentence for the client.
pub fn report_to_parsed(
    mut r: ReportResult,
    o: &ReportOptions<'_>,
) -> Result<ParsedResult, String> {
    let id = r.id.trim().to_string();
    if id.is_empty() || id.len() > MAX_CLIENT_ID_BYTES {
        return Err(format!("id must have 1 to {MAX_CLIENT_ID_BYTES} bytes"));
    }
    if r.name.trim().is_empty() {
        return Err("name is required".into());
    }
    let mut w = Walk {
        limits: o.limits,
        steps: 0,
        attachments: 0,
    };
    w.text("name", &r.name)?;
    let mut full_name = r
        .full_name
        .clone()
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| r.name.clone());
    w.text("fullName", &full_name)?;
    // Names are kept up to `max_name_bytes`, as in uploaded reports (see [`name_warnings`]).
    let max_name = o.limits.max_name_bytes as usize;
    crate::ctx::truncate_with_ellipsis(&mut r.name, max_name);
    crate::ctx::truncate_with_ellipsis(&mut full_name, max_name);
    for p in &r.parameters {
        w.text("parameter", &p.name)?;
        if let Some(v) = &p.value {
            w.text("parameter value", v)?;
        }
    }
    let source = r.source.clone().unwrap_or_default();
    let env_key = o.environment_key.map(str::trim).filter(|e| !e.is_empty());
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::FastproofJson),
        allure_test_case_id: r.test_case_id.as_deref(),
        full_name: &full_name,
        display_name: &r.name,
        parameters: &r.parameters,
        env_key,
        source: SourceRef {
            file: source.file.clone(),
            line: source.line,
            code_ref: None,
        },
        ..Default::default()
    });
    let mut attachments = Vec::with_capacity(r.attachments.len());
    for a in r.attachments {
        attachments.push(w.attachment(a)?);
    }
    let mut steps = Vec::with_capacity(r.steps.len());
    for s in r.steps {
        steps.push(w.step(s, 1)?);
    }
    let mut labels = r.labels;
    crate::ctx::clamp_labels(&mut labels);
    let max_log = o.limits.max_log_bytes;
    let running = r.status.is_running();
    let interrupted = !running && r.stop.is_none() && r.start.is_some() && r.duration_ms.is_none();
    let mut out = ParsedResult {
        source_uuid: Some(id.to_string()),
        identity,
        name: r.name,
        full_name,
        status: r.status.status(),
        raw_status: None,
        status_details: w.details(r.error)?,
        stage: if running {
            Stage::Running
        } else {
            Stage::Finished
        },
        interrupted,
        start: r.start,
        stop: r.stop,
        duration_ms: r.duration_ms,
        attempt: r.retry,
        description: Description {
            text: r
                .description
                .map(|d| bound(&d, o.limits.max_string_bytes).text),
            html: r.description_html.map(|h| sanitize_html(&h)),
        },
        parameters: r.parameters,
        labels,
        links: r.links,
        steps,
        attachments,
        stdout: r.stdout.map(|t| bound(&t, max_log)),
        stderr: r.stderr.map(|t| bound(&t, max_log)),
        trace_id: r.traceparent.as_deref().and_then(trace_id_from_traceparent),
        traceparent: r.traceparent,
        format: SourceFormat::FastproofJson,
        source_file: None,
        ..ParsedResult::default()
    };
    out.redact_secret_parameters();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastproof_model::{Parameter, ParameterMode, Status};

    fn base() -> ReportResult {
        ReportResult {
            id: "r-1".into(),
            name: "pays".into(),
            full_name: Some("shop.Checkout#pays".into()),
            status: Status::Passed.into(),
            ..Default::default()
        }
    }

    #[test]
    fn long_names_are_cut_with_a_warning() {
        let mut r = base();
        r.name = "п".repeat(3000);
        r.full_name = Some("x".repeat(4096));
        let limits = Limits::default();
        let w = name_warnings(&r, &limits);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("name of 6000 bytes"), "{w:?}");
        let p = report_to_parsed(
            r,
            &ReportOptions {
                limits: &limits,
                environment_key: None,
            },
        )
        .unwrap();
        assert!(p.name.len() <= 4096 && p.name.ends_with('\u{2026}'));
        assert_eq!(p.full_name.len(), 4096, "a name at the limit is kept");
    }

    #[test]
    fn long_labels_are_cut() {
        let mut r = base();
        r.labels = vec![fastproof_model::Label::new("tag", "t".repeat(5000))];
        let limits = Limits::default();
        let p = report_to_parsed(
            r,
            &ReportOptions {
                limits: &limits,
                environment_key: None,
            },
        )
        .unwrap();
        assert_eq!(p.labels[0].value.len(), crate::ctx::MAX_LABEL_VALUE_BYTES);
    }

    #[test]
    fn identity_and_redaction() {
        let mut r = base();
        r.parameters = vec![
            Parameter::new("browser", "firefox"),
            Parameter {
                mode: ParameterMode::Masked,
                ..Parameter::new("password", "hunter2")
            },
        ];
        r.description_html = Some("<p>ok</p><script>alert(1)</script>".into());
        let limits = Limits::default();
        let p = report_to_parsed(
            r.clone(),
            &ReportOptions {
                limits: &limits,
                environment_key: Some("staging"),
            },
        )
        .unwrap();
        assert_eq!(p.identity.full_name, "shop.Checkout#pays");
        assert!(p.identity.series_key.split('.').count() == 3);
        assert!(p.parameters[1].value.is_none());
        assert!(!p.description.html.unwrap().contains("script"));
        let q = report_to_parsed(
            r,
            &ReportOptions {
                limits: &limits,
                environment_key: Some("staging"),
            },
        )
        .unwrap();
        assert_eq!(p.identity.series_key, q.identity.series_key);
    }

    #[test]
    fn limits_and_validation() {
        let limits = Limits {
            max_steps_per_result: 2,
            ..Limits::default()
        };
        let o = ReportOptions {
            limits: &limits,
            environment_key: None,
        };
        let mut r = base();
        r.steps = vec![
            ReportStep {
                name: "a".into(),
                ..Default::default()
            };
            3
        ];
        assert!(
            report_to_parsed(r, &o)
                .unwrap_err()
                .contains("max_steps_per_result")
        );
        let mut r = base();
        r.id = String::new();
        assert!(report_to_parsed(r, &o).is_err());
        let mut r = base();
        r.attachments = vec![ReportAttachment {
            id: "../../etc".into(),
            ..Default::default()
        }];
        assert!(report_to_parsed(r, &o).is_err());
    }
}
