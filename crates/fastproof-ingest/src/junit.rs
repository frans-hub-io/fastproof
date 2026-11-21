//! JUnit XML: generic, Surefire (flaky/rerun elements), Playwright, pytest, Vitest, gotestsum.
//!
//! The XML is read with quick-xml, which never processes DTDs: a DOCTYPE is skipped and
//! unknown entity references are kept literally, so nothing expands and nothing external is
//! fetched.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use fastproof_model::normalize::{normalize_path, normalize_text, split_junit_name};
use fastproof_model::{
    AttachmentRef, IdentityInput, KeyValue, Label, Link, Parameter, ParsedResult, SourceFormat,
    SourceRef, Stage, Status, StatusDetails, TestIdentity,
};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use regex::Regex;

use crate::ctx::Ctx;
use crate::error::IngestError;
use crate::text::{
    bound_opt, cap, content_type_for, expected_actual, parse_timestamp_ms, seconds_to_ms,
};

/// Output of one JUnit document.
#[derive(Default)]
pub(crate) struct JunitDoc {
    pub results: Vec<ParsedResult>,
    pub attributes: Vec<KeyValue>,
    pub name: Option<String>,
    pub start: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OutcomeKind {
    Failure,
    Error,
    Skipped,
    FlakyFailure,
    FlakyError,
    RerunFailure,
    RerunError,
}

impl OutcomeKind {
    fn from_name(n: &str) -> Option<OutcomeKind> {
        Some(match n {
            "failure" => OutcomeKind::Failure,
            "error" => OutcomeKind::Error,
            "skipped" => OutcomeKind::Skipped,
            "flakyFailure" => OutcomeKind::FlakyFailure,
            "flakyError" => OutcomeKind::FlakyError,
            "rerunFailure" => OutcomeKind::RerunFailure,
            "rerunError" => OutcomeKind::RerunError,
            _ => return None,
        })
    }

    fn status(self) -> Status {
        match self {
            OutcomeKind::Failure | OutcomeKind::FlakyFailure | OutcomeKind::RerunFailure => {
                Status::Failed
            }
            OutcomeKind::Error | OutcomeKind::FlakyError | OutcomeKind::RerunError => {
                Status::Broken
            }
            OutcomeKind::Skipped => Status::Skipped,
        }
    }
}

#[derive(Default)]
struct Outcome {
    kind: Option<OutcomeKind>,
    message: Option<String>,
    otype: Option<String>,
    text: String,
    stack_trace: String,
    system_out: String,
    system_err: String,
}

#[derive(Default, Clone)]
struct Suite {
    name: Option<String>,
    hostname: Option<String>,
    file: Option<String>,
}

#[derive(Default)]
struct Case {
    name: String,
    classname: String,
    file: Option<String>,
    line: Option<u32>,
    time_ms: Option<i64>,
    timestamp: Option<i64>,
    properties: Vec<(String, String)>,
    outcomes: Vec<Outcome>,
    system_out: String,
    system_err: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sink {
    None,
    CaseOut,
    CaseErr,
    OutcomeText,
    OutcomeTrace,
    OutcomeOut,
    OutcomeErr,
    PropertyText,
}

struct Parser<'c, 'a> {
    ctx: &'c mut Ctx<'a>,
    file: String,
    stack: Vec<String>,
    suites: Vec<Suite>,
    case: Option<Case>,
    outcome: Option<Outcome>,
    property: Option<(String, Option<String>, String)>,
    sink: Vec<Sink>,
    doc: JunitDoc,
    pytest: bool,
    /// Index of results per (suite path, classname, name) for duplicate detection.
    seen: HashMap<(usize, String, String), Vec<usize>>,
    suite_seq: usize,
    suite_ids: Vec<usize>,
    suite_props: Vec<(String, String)>,
    attribute_keys: HashSet<String>,
}

const MAX_SUITE_PROPERTIES: usize = 16;

fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        match after.find(';').filter(|&e| e <= 12) {
            Some(end) => {
                let name = &after[..end];
                match resolve_ref(name) {
                    Some(s) => out.push_str(&s),
                    None => {
                        out.push('&');
                        out.push_str(name);
                        out.push(';');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Predefined and character references only; anything else is not expanded.
fn resolve_ref(name: &str) -> Option<String> {
    Some(match name {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "amp" => "&".into(),
        "apos" => "'".into(),
        "quot" => "\"".into(),
        _ => {
            let num = name.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            let c = if code == 0 {
                '\u{FFFD}'
            } else {
                char::from_u32(code)?
            };
            c.to_string()
        }
    })
}

fn attachment_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[ATTACHMENT\|([^\]\r\n]+)\]\]").expect("regex"))
}

fn playwright_file() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(spec|test)\.[cm]?[jt]sx?$").expect("regex"))
}

impl<'c, 'a> Parser<'c, 'a> {
    fn attrs(&self, e: &BytesStart<'_>) -> Result<Vec<(String, String)>, IngestError> {
        let mut out = Vec::new();
        let mut attrs = e.attributes();
        attrs.with_checks(false);
        for a in attrs {
            let Ok(a) = a else { continue };
            if out.len() as u32 >= self.ctx.limits.max_xml_attributes {
                return Err(IngestError::limit_at(
                    "max_xml_attributes",
                    self.ctx.limits.max_xml_attributes as u64,
                    self.file.clone(),
                ));
            }
            if a.value.len() as u64 > self.ctx.limits.max_string_bytes {
                return Err(IngestError::limit_at(
                    "max_string_bytes",
                    self.ctx.limits.max_string_bytes,
                    self.file.clone(),
                ));
            }
            let key = a.key.local_name().as_ref().to_string();
            out.push((key, unescape(&a.value).replace('\0', "\u{FFFD}")));
        }
        Ok(out)
    }

    fn push_text(&mut self, text: &str) {
        let max = self.ctx.limits.max_log_bytes.saturating_mul(2) as usize;
        let target: Option<&mut String> = match self.sink.last().copied().unwrap_or(Sink::None) {
            Sink::None => None,
            Sink::CaseOut => self.case.as_mut().map(|c| &mut c.system_out),
            Sink::CaseErr => self.case.as_mut().map(|c| &mut c.system_err),
            Sink::OutcomeText => self.outcome.as_mut().map(|o| &mut o.text),
            Sink::OutcomeTrace => self.outcome.as_mut().map(|o| &mut o.stack_trace),
            Sink::OutcomeOut => self.outcome.as_mut().map(|o| &mut o.system_out),
            Sink::OutcomeErr => self.outcome.as_mut().map(|o| &mut o.system_err),
            Sink::PropertyText => self.property.as_mut().map(|p| &mut p.2),
        };
        if let Some(t) = target {
            // Keep head and the most recent tail; the final bound happens on conversion.
            if t.len() + text.len() > max {
                let keep = max / 2;
                if t.len() > keep {
                    let mut cut = t.len() - keep / 2;
                    while !t.is_char_boundary(cut) {
                        cut += 1;
                    }
                    let mut head_end = keep / 2;
                    while !t.is_char_boundary(head_end) {
                        head_end -= 1;
                    }
                    let tail = t[cut..].to_string();
                    t.truncate(head_end);
                    t.push_str("\n... [truncated] ...\n");
                    t.push_str(&tail);
                }
            }
            t.push_str(&text.replace('\0', "\u{FFFD}"));
        }
    }

    fn start(&mut self, e: &BytesStart<'_>, empty: bool) -> Result<(), IngestError> {
        let name = e.local_name().as_ref().to_string();
        let parent = self.stack.last().map(String::as_str).unwrap_or("");
        let attrs = self.attrs(e)?;
        let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let mut sink = Sink::None;
        match name.as_str() {
            "testsuites" if self.stack.is_empty() => {
                self.doc.name = get("name").filter(|s| !s.is_empty());
                self.pytest = self.doc.name.as_deref() == Some("pytest tests");
                self.doc.start = get("timestamp").and_then(|t| parse_timestamp_ms(&t));
            }
            "testsuite" => {
                let ts = get("timestamp").and_then(|t| parse_timestamp_ms(&t));
                if self.doc.start.is_none() || ts.is_some_and(|t| Some(t) < self.doc.start) {
                    self.doc.start = ts.or(self.doc.start);
                }
                self.suites.push(Suite {
                    name: get("name").filter(|s| !s.is_empty()),
                    hostname: get("hostname").filter(|s| !s.is_empty()),
                    file: get("file").filter(|s| !s.is_empty()),
                });
                self.suite_seq += 1;
                self.suite_ids.push(self.suite_seq);
                if empty {
                    self.suites.pop();
                    self.suite_ids.pop();
                }
            }
            "testcase" if self.case.is_none() => {
                self.case = Some(Case {
                    name: get("name").unwrap_or_default(),
                    classname: get("classname").unwrap_or_default(),
                    file: get("file").filter(|s| !s.is_empty()),
                    line: get("line").and_then(|l| l.trim().parse().ok()),
                    time_ms: get("time").and_then(|t| seconds_to_ms(&t)),
                    timestamp: get("timestamp").and_then(|t| parse_timestamp_ms(&t)),
                    ..Case::default()
                });
                if empty {
                    self.finish_case()?;
                }
            }
            "property" => {
                let pname = get("name").unwrap_or_default();
                let value = get("value");
                if empty {
                    self.add_property(pname, value.unwrap_or_default())?;
                } else {
                    self.property = Some((pname, value, String::new()));
                    sink = Sink::PropertyText;
                }
            }
            n if self.case.is_some()
                && self.outcome.is_none()
                && OutcomeKind::from_name(n).is_some() =>
            {
                let o = Outcome {
                    kind: OutcomeKind::from_name(n),
                    message: get("message").filter(|s| !s.is_empty()),
                    otype: get("type").filter(|s| !s.is_empty()),
                    ..Outcome::default()
                };
                if empty {
                    self.push_outcome(o)?;
                } else {
                    self.outcome = Some(o);
                    sink = Sink::OutcomeText;
                }
            }
            "system-out" | "system-err" if !empty => {
                let out = name == "system-out";
                if self.outcome.is_some() {
                    sink = if out {
                        Sink::OutcomeOut
                    } else {
                        Sink::OutcomeErr
                    };
                } else if self.case.is_some() && parent == "testcase" {
                    sink = if out { Sink::CaseOut } else { Sink::CaseErr };
                }
            }
            "stackTrace" if self.outcome.is_some() && !empty => sink = Sink::OutcomeTrace,
            _ => {}
        }
        if !empty {
            if self.stack.len() as u32 >= self.ctx.limits.max_xml_depth {
                return Err(IngestError::limit_at(
                    "max_xml_depth",
                    self.ctx.limits.max_xml_depth as u64,
                    self.file.clone(),
                ));
            }
            self.stack.push(name);
            self.sink.push(sink);
        }
        Ok(())
    }

    fn add_property(&mut self, name: String, value: String) -> Result<(), IngestError> {
        if name.is_empty() {
            return Ok(());
        }
        if let Some(c) = self.case.as_mut() {
            c.properties.push((name, value));
            self.ctx.check_labels(c.properties.len(), &self.file)?;
        } else if self.suite_props.len() <= MAX_SUITE_PROPERTIES {
            self.suite_props.push((name, value));
        }
        Ok(())
    }

    /// Keeps an outcome element of the current case. A case has at most one attempt per
    /// failure/error/flaky/rerun element plus a skipped element, so more than
    /// `max_attempts_per_result + 1` elements are over the attempt limit.
    fn push_outcome(&mut self, o: Outcome) -> Result<(), IngestError> {
        if let Some(c) = self.case.as_mut() {
            c.outcomes.push(o);
            if c.outcomes.len() as u64 > self.ctx.limits.max_attempts_per_result.saturating_add(1) {
                return Err(IngestError::limit_at(
                    "max_attempts_per_result",
                    self.ctx.limits.max_attempts_per_result,
                    self.file.clone(),
                ));
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), IngestError> {
        let Some(name) = self.stack.pop() else {
            return Ok(());
        };
        self.sink.pop();
        match name.as_str() {
            "testsuite" => {
                self.suites.pop();
                self.suite_ids.pop();
            }
            "testcase" => self.finish_case()?,
            "properties" if self.case.is_none() => {
                // Suite properties become launch attributes. Surefire dumps every JVM system
                // property here; such large blocks are dropped.
                let props = std::mem::take(&mut self.suite_props);
                if props.len() <= MAX_SUITE_PROPERTIES {
                    for (k, v) in props {
                        if self.attribute_keys.insert(k.clone()) {
                            self.doc.attributes.push(KeyValue::new(k, v));
                        }
                    }
                }
            }
            "property" => {
                if let Some((n, v, text)) = self.property.take() {
                    self.add_property(n, v.unwrap_or_else(|| text.trim().to_string()))?;
                }
            }
            n if OutcomeKind::from_name(n).is_some()
                && self
                    .outcome
                    .as_ref()
                    .is_some_and(|o| o.kind == OutcomeKind::from_name(n)) =>
            {
                if let Some(o) = self.outcome.take() {
                    self.push_outcome(o)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_case(&mut self) -> Result<(), IngestError> {
        let Some(mut case) = self.case.take() else {
            return Ok(());
        };
        case.name = self.ctx.name(case.name, "testcase name", &self.file);
        case.classname = self
            .ctx
            .name(case.classname, "testcase classname", &self.file);
        self.outcome = None;
        let suite = self.suites.last().cloned().unwrap_or_default();
        let suite_id = self.suite_ids.last().copied().unwrap_or(0);
        let playwright = !case.classname.is_empty()
            && playwright_file().is_match(&case.classname)
            && case.name.contains(" \u{203a} ");
        let max = self.ctx.limits.max_log_bytes;

        let (base, split) = if playwright {
            (case.name.trim().to_string(), Vec::new())
        } else {
            split_junit_name(&case.name)
        };
        let classname = normalize_text(&case.classname);
        let full_name = if playwright {
            format!(
                "{} \u{203a} {}",
                normalize_path(&classname, &self.ctx.options.path_prefixes()),
                normalize_text(&base)
            )
        } else if classname.is_empty() {
            normalize_text(&base)
        } else {
            format!("{}#{}", classname, normalize_text(&base))
        };
        let full_name = self.ctx.name(full_name, "full name", &self.file);
        let parameters: Vec<Parameter> = split.iter().map(|(n, v)| Parameter::new(n, v)).collect();
        let mut labels: Vec<Label> = Vec::new();
        let mut links: Vec<Link> = Vec::new();
        let mut allure_id = None;
        for (n, v) in &case.properties {
            let lower = n.to_ascii_lowercase();
            if (lower == "allure_id" || lower == "as_id") && !v.trim().is_empty() {
                allure_id = Some(v.trim().to_string());
            }
            if matches!(lower.as_str(), "issue" | "tms" | "link")
                && (v.starts_with("http://") || v.starts_with("https://"))
            {
                links.push(Link {
                    name: None,
                    url: v.clone(),
                    link_type: Some(lower.clone()),
                });
            }
            labels.push(Label::new(n.clone(), v.clone()));
        }
        if let Some(s) = &suite.name {
            labels.push(Label::new("suite", s.clone()));
        }
        if !playwright && let Some(h) = &suite.hostname {
            labels.push(Label::new("host", h.clone()));
        }
        self.ctx.check_labels(labels.len(), &self.file)?;
        let file = case.file.clone().or(suite.file.clone()).or_else(|| {
            (playwright || looks_like_file(&case.classname)).then(|| case.classname.clone())
        });
        let source = SourceRef {
            file: file.map(|f| normalize_path(&f, &self.ctx.options.path_prefixes())),
            line: case.line,
            code_ref: (!classname.is_empty() && !playwright)
                .then(|| format!("{classname}.{}", base.trim())),
        };
        let env = if playwright {
            suite.hostname.clone()
        } else {
            None
        };
        let identity = TestIdentity::build(IdentityInput {
            format: Some(SourceFormat::JunitXml),
            allure_id: allure_id.as_deref(),
            full_name: &full_name,
            display_name: case.name.trim(),
            parameters: &parameters,
            env_key: self.ctx.env_key(env.as_deref()).as_deref(),
            source,
            ..Default::default()
        });

        // Attempt list: flaky* (earlier failed attempts of a passing test), the main
        // failure/error elements (Vitest writes one per failed attempt), then rerun*.
        let flaky: Vec<&Outcome> = case
            .outcomes
            .iter()
            .filter(|o| {
                matches!(
                    o.kind,
                    Some(OutcomeKind::FlakyFailure | OutcomeKind::FlakyError)
                )
            })
            .collect();
        let main: Vec<&Outcome> = case
            .outcomes
            .iter()
            .filter(|o| matches!(o.kind, Some(OutcomeKind::Failure | OutcomeKind::Error)))
            .collect();
        let reruns: Vec<&Outcome> = case
            .outcomes
            .iter()
            .filter(|o| {
                matches!(
                    o.kind,
                    Some(OutcomeKind::RerunFailure | OutcomeKind::RerunError)
                )
            })
            .collect();
        let skipped = case
            .outcomes
            .iter()
            .find(|o| o.kind == Some(OutcomeKind::Skipped));

        let mut attempts: Vec<(Status, Option<&Outcome>, bool)> = Vec::new();
        for o in &flaky {
            attempts.push((
                o.kind.map(OutcomeKind::status).unwrap_or(Status::Failed),
                Some(o),
                false,
            ));
        }
        for o in &main {
            attempts.push((
                o.kind.map(OutcomeKind::status).unwrap_or(Status::Failed),
                Some(o),
                false,
            ));
        }
        for o in &reruns {
            attempts.push((
                o.kind.map(OutcomeKind::status).unwrap_or(Status::Failed),
                Some(o),
                false,
            ));
        }
        if main.is_empty() && reruns.is_empty() {
            match skipped {
                Some(s) => attempts.push((Status::Skipped, Some(s), true)),
                None => attempts.push((Status::Passed, None, true)),
            }
        } else if let Some(last) = attempts.last_mut() {
            last.2 = true;
        }
        let count = attempts.len();
        self.ctx.check_attempts(count, &self.file)?;
        let base_result = ParsedResult {
            identity,
            name: case.name.trim().to_string(),
            full_name: full_name.clone(),
            status: Status::Unknown,
            stage: Stage::Finished,
            parameters,
            labels,
            links,
            format: SourceFormat::JunitXml,
            source_file: Some(self.file.clone()),
            ..ParsedResult::default()
        };
        let dir = self
            .file
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default();
        // Every attempt after the first is a copy of the shared parts.
        let copy_bytes = crate::ctx::result_bytes(&base_result);
        for (i, (status, outcome, is_final)) in attempts.into_iter().enumerate() {
            if self.doc.results.len() as u64 >= self.ctx.limits.max_results {
                return Err(IngestError::limit_at(
                    "max_results",
                    self.ctx.limits.max_results,
                    self.file.clone(),
                ));
            }
            if i > 0 {
                self.ctx.charge_copy(copy_bytes, &self.file)?;
            }
            let mut r = base_result.clone();
            r.status = status;
            r.attempt = (count > 1).then_some(i as u32);
            if let Some(o) = outcome {
                let trace = if o.stack_trace.trim().is_empty() {
                    o.text.trim()
                } else {
                    o.stack_trace.trim()
                };
                let message = o
                    .message
                    .clone()
                    .filter(|m| !is_generic_message(m) || trace.is_empty())
                    .or_else(|| first_meaningful_line(trace))
                    .filter(|m| !m.is_empty());
                let ea = message
                    .as_deref()
                    .and_then(expected_actual)
                    .or_else(|| expected_actual(trace));
                r.status_details = StatusDetails {
                    message: message.map(|m| cap(m, max)),
                    trace: (!trace.is_empty()).then(|| cap(trace.to_string(), max)),
                    expected: ea.as_ref().map(|e| cap(e.0.clone(), max)),
                    actual: ea.map(|e| cap(e.1, max)),
                    ..StatusDetails::default()
                };
                r.raw_status = o.otype.clone().map(|t| match t.as_str() {
                    "pytest.xfail" => "xfail".to_string(),
                    "pytest.skip" => "skip".to_string(),
                    _ => t,
                });
                if o.kind == Some(OutcomeKind::Skipped) && r.raw_status.is_none() {
                    r.raw_status = Some("skipped".into());
                }
                if !o.system_out.is_empty() {
                    r.stdout = bound_opt(&o.system_out, max);
                }
                if !o.system_err.is_empty() {
                    r.stderr = bound_opt(&o.system_err, max);
                }
            }
            if is_final {
                r.duration_ms = case.time_ms;
                r.start = case.timestamp;
                r.stop = case.timestamp.zip(case.time_ms).map(|(s, d)| s + d);
                if r.stdout.is_none() {
                    r.stdout = bound_opt(&case.system_out, max);
                }
                if r.stderr.is_none() {
                    r.stderr = bound_opt(&case.system_err, max);
                }
            }
            let mut atts: Vec<AttachmentRef> = Vec::new();
            let mut sources: HashSet<String> = HashSet::new();
            for text in [&case.system_out, &case.system_err]
                .into_iter()
                .filter(|_| is_final)
                .chain(
                    outcome
                        .into_iter()
                        .flat_map(|o| [&o.system_out, &o.system_err, &o.text]),
                )
            {
                for m in attachment_marker().captures_iter(text) {
                    let path = m[1].trim().to_string();
                    if !sources.insert(path.clone()) {
                        continue;
                    }
                    if atts.len() as u64 >= self.ctx.limits.max_attachments_per_result {
                        return Err(IngestError::limit_at(
                            "max_attachments_per_result",
                            self.ctx.limits.max_attachments_per_result,
                            self.file.clone(),
                        ));
                    }
                    self.ctx.count_ref(&self.file)?;
                    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                    let mut a = AttachmentRef::new(
                        name.clone(),
                        path.clone(),
                        content_type_for(&name).map(str::to_string),
                    );
                    a.resolved_path = Some(crate::allure::join(&dir, &path));
                    atts.push(a);
                }
            }
            r.attachments = atts;
            let idx = self.doc.results.len();
            self.ctx.finish(&mut r);
            self.doc.results.push(r);
            if is_final {
                self.seen
                    .entry((suite_id, case.classname.clone(), case.name.clone()))
                    .or_default()
                    .push(idx);
            }
        }
        Ok(())
    }

    /// pytest-rerunfailures writes failed attempts as plain passing testcases; when the same
    /// test appears again later in the suite, the earlier "passed" entries were failures.
    fn fix_pytest_reruns(&mut self) {
        if !self.pytest {
            return;
        }
        for idx in self.seen.values() {
            if idx.len() < 2 {
                continue;
            }
            for &i in &idx[..idx.len() - 1] {
                let r = &mut self.doc.results[i];
                if r.status == Status::Passed {
                    r.status = Status::Failed;
                    r.raw_status = Some("rerun".into());
                    r.status_details.message =
                        Some("failed attempt (the JUnit writer recorded no details)".into());
                }
            }
        }
    }
}

/// gotestsum writes `message="Failed"` and puts the real output in the body.
fn is_generic_message(m: &str) -> bool {
    matches!(
        m.trim(),
        "Failed" | "failed" | "Failure" | "failure" | "Error" | "error"
    )
}

fn first_meaningful_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("=== RUN")
                && !l.starts_with("=== PAUSE")
                && !l.starts_with("=== CONT")
        })
        .map(str::to_string)
}

fn looks_like_file(s: &str) -> bool {
    s.contains('/') && s.rsplit('/').next().is_some_and(|b| b.contains('.'))
}

/// Parses a JUnit XML document.
pub(crate) fn parse(
    text: &str,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Result<JunitDoc, String>, IngestError> {
    let mut reader = Reader::from_str(text);
    {
        let cfg = reader.config_mut();
        cfg.check_end_names = false;
        cfg.allow_unmatched_ends = true;
        cfg.allow_dangling_amp = true;
        cfg.check_comments = false;
    }
    let mut p = Parser {
        ctx,
        file: file.to_string(),
        stack: Vec::new(),
        suites: Vec::new(),
        case: None,
        outcome: None,
        property: None,
        sink: Vec::new(),
        doc: JunitDoc::default(),
        pytest: false,
        seen: HashMap::new(),
        suite_seq: 0,
        suite_ids: Vec::new(),
        suite_props: Vec::new(),
        attribute_keys: HashSet::new(),
    };
    loop {
        let ev = match reader.read_event() {
            Ok(ev) => ev,
            Err(e) => {
                if p.doc.results.is_empty() {
                    return Ok(Err(format!(
                        "XML error at byte {}: {e}",
                        reader.buffer_position()
                    )));
                }
                p.ctx.warn(
                    Some(file),
                    format!(
                        "XML error at byte {}: {e}; kept results parsed so far",
                        reader.buffer_position()
                    ),
                );
                break;
            }
        };
        match ev {
            Event::Start(e) => p.start(&e, false)?,
            Event::Empty(e) => p.start(&e, true)?,
            Event::End(_) => p.end()?,
            Event::Text(t) => {
                let s = t.xml10_content();
                p.push_text(&s);
            }
            Event::CData(t) => {
                let s = t.xml10_content();
                p.push_text(&s);
            }
            Event::GeneralRef(r) => {
                let s = match resolve_ref(&r) {
                    Some(s) => s,
                    None => format!("&{};", &*r),
                };
                p.push_text(&s);
            }
            Event::DocType(_) | Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if p.case.is_some() {
        p.finish_case()?;
    }
    p.fix_pytest_reruns();
    Ok(Ok(p.doc))
}

/// True when the document root is `<testsuites>` or `<testsuite>`.
pub(crate) fn is_junit(head: &str) -> bool {
    let mut reader = Reader::from_str(head);
    reader.config_mut().allow_dangling_amp = true;
    for _ in 0..64 {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = e.local_name();
                return n.as_ref() == "testsuites" || n.as_ref() == "testsuite";
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_only_predefined_and_char_refs() {
        assert_eq!(
            unescape("a &lt;b&gt; &amp; &#65;&#x42; &xxe; & tail"),
            "a <b> & AB &xxe; & tail"
        );
    }

    #[test]
    fn detects_root() {
        assert!(is_junit("<?xml version=\"1.0\"?>\n<!-- c --><testsuites>"));
        assert!(is_junit("<testsuite name=\"x\"/>"));
        assert!(!is_junit("<html></html>"));
    }
}
