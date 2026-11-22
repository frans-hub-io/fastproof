//! allure-results (Allure 2 and Allure 3 writers).

use std::collections::{HashMap, HashSet};

use fastproof_model::{
    AttachmentRef, Category, Container, Description, Executor, GlobalError, Globals, IdentityInput,
    KeyValue, Label, Link, Parameter, ParameterMode, ParsedResult, SourceFormat, SourceRef, Stage,
    Status, StatusDetails, Step, StepKind, TestIdentity,
};
use serde_json::Value;

use crate::ctx::{Ctx, StepBudget};
use crate::error::IngestError;
use crate::json::{arr, bool_field, clean, i64_field, str_field, text_field};
use crate::text::{cap, sanitize_html};

fn parent_dir(file: &str) -> &str {
    file.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn status_of(v: &Value) -> Status {
    str_field(v, "status")
        .map(|s| Status::parse_lenient(&s))
        .unwrap_or(Status::Unknown)
}

fn status_details(v: &Value, ctx: &Ctx<'_>) -> StatusDetails {
    let Some(d) = v.get("statusDetails").filter(|d| d.is_object()) else {
        return StatusDetails::default();
    };
    let max = ctx.limits.max_log_bytes;
    StatusDetails {
        message: text_field(d, "message").map(|s| cap(s, max)),
        trace: text_field(d, "trace").map(|s| cap(s, max)),
        actual: text_field(d, "actual").map(|s| cap(s, max)),
        expected: text_field(d, "expected").map(|s| cap(s, max)),
        known: bool_field(d, "known"),
        muted: bool_field(d, "muted"),
        flaky: bool_field(d, "flaky"),
    }
}

fn parameters(v: &Value) -> Vec<Parameter> {
    arr(v, "parameters")
        .iter()
        .filter(|p| p.is_object())
        .map(|p| Parameter {
            name: p
                .get("name")
                .and_then(Value::as_str)
                .map(clean)
                .unwrap_or_default(),
            // Allure 3 reads only string values; anything else is a missing value.
            value: p.get("value").and_then(Value::as_str).map(clean),
            excluded: bool_field(p, "excluded"),
            mode: match str_field(p, "mode").as_deref() {
                Some("masked") => ParameterMode::Masked,
                Some("hidden") => ParameterMode::Hidden,
                _ => ParameterMode::Default,
            },
        })
        .collect()
}

fn attachments(v: &Value, budget: &mut StepBudget) -> Result<Vec<AttachmentRef>, IngestError> {
    let mut out = Vec::new();
    for a in arr(v, "attachments") {
        let Some(source) = text_field(a, "source") else {
            continue;
        };
        budget.attachment()?;
        let name = text_field(a, "name").unwrap_or_else(|| source.clone());
        let mut r = AttachmentRef::new(name, source, text_field(a, "type"));
        r.size = i64_field(a, "size").and_then(|s| u64::try_from(s).ok());
        out.push(r);
    }
    Ok(out)
}

fn steps(
    v: &Value,
    kind: StepKind,
    budget: &mut StepBudget,
    ctx: &Ctx<'_>,
) -> Result<Vec<Step>, IngestError> {
    let mut out = Vec::new();
    for s in arr(v, "steps") {
        if !s.is_object() {
            continue;
        }
        out.push(step(s, kind, budget, ctx)?);
    }
    Ok(out)
}

fn step(
    s: &Value,
    kind: StepKind,
    budget: &mut StepBudget,
    ctx: &Ctx<'_>,
) -> Result<Step, IngestError> {
    budget.tick()?;
    Ok(Step {
        name: text_field(s, "name").unwrap_or_default(),
        status: status_of(s),
        status_details: status_details(s, ctx),
        start: i64_field(s, "start"),
        stop: i64_field(s, "stop"),
        duration_ms: None,
        parameters: parameters(s),
        attachments: attachments(s, budget)?,
        steps: steps(s, StepKind::Step, budget, ctx)?,
        kind,
    })
}

fn description(v: &Value) -> Description {
    Description {
        text: text_field(v, "description"),
        html: text_field(v, "descriptionHtml").map(|h| sanitize_html(&h)),
    }
}

/// Parses one `*-result.json`. `Ok(None)` when the document is not a usable result.
pub(crate) fn parse_result(
    v: &Value,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Option<ParsedResult>, IngestError> {
    if !v.is_object() {
        ctx.warn(Some(file), "result is not a JSON object");
        return Ok(None);
    }
    let mut budget = ctx.steps(file);
    let mut labels: Vec<Label> = arr(v, "labels")
        .iter()
        .filter_map(|l| {
            Some(Label::new(
                text_field(l, "name")?,
                str_field(l, "value").unwrap_or_default(),
            ))
        })
        .collect();
    ctx.check_labels(labels.len(), file)?;
    ctx.clamp_labels(&mut labels, file);
    let links: Vec<Link> = arr(v, "links")
        .iter()
        .filter_map(|l| {
            Some(Link {
                name: text_field(l, "name"),
                url: text_field(l, "url")?,
                link_type: text_field(l, "type"),
            })
        })
        .collect();
    let params = parameters(v);
    let full_name = ctx.name(
        str_field(v, "fullName").unwrap_or_default(),
        "fullName",
        file,
    );
    let name = match text_field(v, "name") {
        Some(n) => ctx.name(n, "name", file),
        None if !full_name.is_empty() => full_name.clone(),
        None => "Unknown test".to_string(),
    };
    let allure_id = labels
        .iter()
        .find(|l| {
            (l.name.eq_ignore_ascii_case("ALLURE_ID") || l.name.eq_ignore_ascii_case("AS_ID"))
                && !l.value.trim().is_empty()
        })
        .map(|l| l.value.trim().to_string());
    let test_case_id = str_field(v, "testCaseId");
    let history_id = str_field(v, "historyId");
    let status = status_of(v);
    let stage = str_field(v, "stage").and_then(|s| Stage::parse_lenient(&s));
    let start = i64_field(v, "start");
    let stop = i64_field(v, "stop");
    let interrupted = status != Status::Skipped
        && (matches!(
            stage,
            Some(Stage::Scheduled | Stage::Running | Stage::Interrupted)
        ) || stop.is_none());
    let trace_label = labels
        .iter()
        .find(|l| {
            let n = l.name.to_ascii_lowercase();
            n == "traceparent"
        })
        .map(|l| l.value.clone());
    let trace_id = labels
        .iter()
        .find(|l| matches!(l.name.to_ascii_lowercase().as_str(), "trace_id" | "traceid"))
        .map(|l| l.value.clone())
        .or_else(|| trace_label.as_deref().and_then(trace_id_from_traceparent));
    let code_ref = {
        let class = labels
            .iter()
            .find(|l| l.name == "testClass")
            .map(|l| l.value.as_str());
        let method = labels
            .iter()
            .find(|l| l.name == "testMethod")
            .map(|l| l.value.as_str());
        match (class, method) {
            (Some(c), Some(m)) => Some(format!("{c}.{m}")),
            _ => None,
        }
    };
    let identity = TestIdentity::build(IdentityInput {
        format: Some(SourceFormat::AllureResults),
        allure_id: allure_id.as_deref(),
        allure_test_case_id: test_case_id.as_deref(),
        ctrf_test_id: None,
        full_name: &full_name,
        display_name: &name,
        parameters: &params,
        env_key: ctx.env_key(None).as_deref(),
        source: SourceRef {
            file: None,
            line: None,
            code_ref,
        },
        history_id: history_id.as_deref(),
    });
    let mut result = ParsedResult {
        source_uuid: text_field(v, "uuid"),
        identity,
        name,
        full_name,
        status,
        raw_status: str_field(v, "status"),
        status_details: status_details(v, ctx),
        stage: stage.unwrap_or(Stage::Finished),
        interrupted,
        start,
        stop,
        duration_ms: match (start, stop) {
            (Some(a), Some(b)) if b >= a => Some(b - a),
            _ => None,
        },
        attempt: None,
        description: description(v),
        parameters: params,
        labels,
        links,
        steps: steps(v, StepKind::Step, &mut budget, ctx)?,
        attachments: attachments(v, &mut budget)?,
        fixtures: Default::default(),
        stdout: None,
        stderr: None,
        trace_id,
        traceparent: trace_label,
        format: SourceFormat::AllureResults,
        source_file: Some(file.to_string()),
    };
    ctx.add_refs(budget.attachments(), file)?;
    let dir = parent_dir(file).to_string();
    result.visit_attachments_mut(&mut |a| a.resolved_path = Some(join(&dir, &a.source)));
    ctx.finish(&mut result);
    Ok(Some(result))
}

pub(crate) fn trace_id_from_traceparent(tp: &str) -> Option<String> {
    let parts: Vec<&str> = tp.trim().split('-').collect();
    if parts.len() >= 4 && parts[1].len() == 32 && parts[1].bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(parts[1].to_ascii_lowercase())
    } else {
        None
    }
}

/// Upload-relative candidate path of an Allure `source` (resolution checks it exists).
pub(crate) fn join(dir: &str, source: &str) -> String {
    if dir.is_empty() {
        source.to_string()
    } else {
        format!("{dir}/{source}")
    }
}

pub(crate) fn parse_container(
    v: &Value,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Option<Container>, IngestError> {
    let Some(uuid) = text_field(v, "uuid") else {
        ctx.warn(Some(file), "container without uuid");
        return Ok(None);
    };
    let mut seen = HashSet::new();
    let children: Vec<String> = arr(v, "children")
        .iter()
        .filter_map(|c| c.as_str().map(str::to_string))
        .filter(|c| seen.insert(c.clone()))
        .collect();
    let mut budget = ctx.steps(file);
    let fixtures = |key: &str, budget: &mut StepBudget| -> Result<Vec<Step>, IngestError> {
        let mut out = Vec::new();
        for f in arr(v, key) {
            if f.is_object() {
                out.push(step(f, StepKind::Fixture, budget, ctx)?);
            }
        }
        Ok(out)
    };
    let befores = fixtures("befores", &mut budget)?;
    let afters = fixtures("afters", &mut budget)?;
    ctx.add_refs(budget.attachments(), file)?;
    let dir = parent_dir(file).to_string();
    let mut c = Container {
        uuid,
        name: text_field(v, "name"),
        children,
        befores,
        afters,
        start: i64_field(v, "start"),
        stop: i64_field(v, "stop"),
        source_file: Some(file.to_string()),
    };
    for s in c.befores.iter_mut().chain(c.afters.iter_mut()) {
        s.visit_attachments_mut(&mut |a| a.resolved_path = Some(join(&dir, &a.source)));
        if ctx.options.redact_secret_parameters {
            s.visit_parameters_mut(&mut |p| p.redact());
        }
    }
    Ok(Some(c))
}

pub(crate) fn parse_globals(
    v: &Value,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Globals, IngestError> {
    let max = ctx.limits.max_log_bytes;
    let errors = arr(v, "errors")
        .iter()
        .map(|e| GlobalError {
            message: text_field(e, "message").map(|s| cap(s, max)),
            trace: text_field(e, "trace").map(|s| cap(s, max)),
            timestamp: i64_field(e, "timestamp"),
        })
        .collect();
    let dir = parent_dir(file).to_string();
    let mut budget = ctx.steps(file);
    let mut attachments = attachments(v, &mut budget)?;
    ctx.add_refs(budget.attachments(), file)?;
    for a in &mut attachments {
        a.resolved_path = Some(join(&dir, &a.source));
    }
    Ok(Globals {
        errors,
        attachments,
    })
}

pub(crate) fn parse_executor(v: &Value) -> Executor {
    Executor {
        name: text_field(v, "name"),
        executor_type: text_field(v, "type"),
        url: text_field(v, "url"),
        build_order: i64_field(v, "buildOrder"),
        build_name: text_field(v, "buildName"),
        build_url: text_field(v, "buildUrl"),
        report_name: text_field(v, "reportName"),
        report_url: text_field(v, "reportUrl"),
    }
}

pub(crate) fn parse_categories(v: &Value, file: &str, ctx: &mut Ctx<'_>) -> Vec<Category> {
    let Some(items) = v.as_array() else {
        ctx.warn(Some(file), "categories.json is not an array");
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in items {
        let Some(name) = text_field(c, "name") else {
            continue;
        };
        let mut check = |key: &str| -> Option<String> {
            let re = text_field(c, key)?;
            match regex::RegexBuilder::new(&re).size_limit(1 << 20).build() {
                Ok(_) => Some(re),
                Err(e) => {
                    ctx.warn(Some(file), format!("category {name:?}: invalid {key}: {e}"));
                    None
                }
            }
        };
        let message_regex = check("messageRegex");
        let trace_regex = check("traceRegex");
        out.push(Category {
            description: text_field(c, "description"),
            description_html: text_field(c, "descriptionHtml").map(|h| sanitize_html(&h)),
            message_regex,
            trace_regex,
            matched_statuses: arr(c, "matchedStatuses")
                .iter()
                .filter_map(Value::as_str)
                .map(Status::parse_lenient)
                .collect(),
            flaky: c.get("flaky").and_then(Value::as_bool),
            name,
        });
    }
    out
}

/// Parses Java `.properties` content (environment.properties).
pub(crate) fn parse_properties(text: &str) -> Vec<KeyValue> {
    let mut out = Vec::new();
    let mut logical = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if logical.is_empty()
            && (trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!'))
        {
            continue;
        }
        let trailing = trimmed.chars().rev().take_while(|c| *c == '\\').count();
        if trailing % 2 == 1 {
            logical.push_str(&trimmed[..trimmed.len() - 1]);
            continue;
        }
        logical.push_str(trimmed);
        let entry = std::mem::take(&mut logical);
        if let Some(kv) = split_property(&entry) {
            out.push(kv);
        }
    }
    if !logical.is_empty()
        && let Some(kv) = split_property(&logical)
    {
        out.push(kv);
    }
    out
}

fn split_property(line: &str) -> Option<KeyValue> {
    let mut key = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    key.push(unescape_char(n, &mut chars));
                }
            }
            '=' | ':' => break,
            c if c.is_whitespace() => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if chars.peek().is_some_and(|c| *c == '=' || *c == ':') {
                    chars.next();
                }
                break;
            }
            c => key.push(c),
        }
    }
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    let mut value = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                value.push(unescape_char(n, &mut chars));
            }
        } else {
            value.push(c);
        }
    }
    if key.is_empty() {
        None
    } else {
        Some(KeyValue::new(key, value))
    }
}

fn unescape_char(c: char, rest: &mut std::iter::Peekable<std::str::Chars<'_>>) -> char {
    match c {
        't' => '\t',
        'n' => '\n',
        'r' => '\r',
        'f' => '\u{c}',
        'u' => {
            let hex: String = (0..4).filter_map(|_| rest.next()).collect();
            u32::from_str_radix(&hex, 16)
                .ok()
                .and_then(char::from_u32)
                .unwrap_or('\u{FFFD}')
        }
        other => other,
    }
}

/// Attaches container fixtures to the results they list as children (Allure 2 semantics:
/// befores/afters of all containers of a result, sorted by start).
///
/// Every copy is charged twice: to the step limit of the receiving result and, by its
/// estimated size, to `limits.max_total_uncompressed_bytes`, so a container that lists a
/// child many times, or many containers that list many children, cannot multiply data
/// beyond the limits. Returns the number of bytes charged.
pub fn attach_fixtures(
    results: &mut [ParsedResult],
    containers: &[Container],
    limits: &crate::Limits,
) -> Result<u64, IngestError> {
    let mut used = 0u64;
    let max = limits.max_total_uncompressed_bytes;
    attach_fixtures_with(
        results,
        containers,
        limits.max_steps_per_result,
        &mut |n, ctx| {
            used = used.saturating_add(n);
            if used > max {
                Err(IngestError::limit_at(
                    "max_total_uncompressed_bytes",
                    max,
                    ctx.to_string(),
                ))
            } else {
                Ok(())
            }
        },
    )?;
    Ok(used)
}

pub(crate) fn step_bytes(s: &Step) -> u64 {
    let d = &s.status_details;
    let text = [&d.message, &d.trace, &d.actual, &d.expected]
        .iter()
        .map(|t| t.as_ref().map_or(0, |t| t.len()))
        .sum::<usize>();
    let params: usize = s
        .parameters
        .iter()
        .map(|p| p.name.len() + p.value.as_ref().map_or(0, |v| v.len()) + 32)
        .sum();
    let atts: usize = s
        .attachments
        .iter()
        .map(|a| a.name.len() + a.source.len() + 96)
        .sum();
    (160 + s.name.len() + text + params + atts) as u64 + s.steps.iter().map(step_bytes).sum::<u64>()
}

pub(crate) fn attach_fixtures_with(
    results: &mut [ParsedResult],
    containers: &[Container],
    max_steps_per_result: u64,
    charge: &mut dyn FnMut(u64, &str) -> Result<(), IngestError>,
) -> Result<(), IngestError> {
    let mut by_uuid: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in results.iter().enumerate() {
        if let Some(u) = r.source_uuid.as_deref() {
            by_uuid.entry(u.to_string()).or_default().push(i);
        }
    }
    let mut touched = HashSet::new();
    // Step counts kept up to date instead of recounting a result for every container.
    let mut step_counts: Vec<u64> = results.iter().map(|r| r.step_count() as u64).collect();
    for c in containers {
        if c.befores.is_empty() && c.afters.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        let steps: u64 = c
            .befores
            .iter()
            .chain(c.afters.iter())
            .map(|s| s.count() as u64)
            .sum();
        let bytes: u64 = c
            .befores
            .iter()
            .chain(c.afters.iter())
            .map(step_bytes)
            .sum();
        for child in &c.children {
            if !seen.insert(child.as_str()) {
                continue;
            }
            let Some(idx) = by_uuid.get(child.as_str()) else {
                continue;
            };
            for &i in idx {
                let r = &mut results[i];
                if step_counts[i] + steps > max_steps_per_result {
                    return Err(IngestError::limit_at(
                        "max_steps_per_result",
                        max_steps_per_result,
                        format!("fixtures of container {}", c.uuid),
                    ));
                }
                charge(bytes, &format!("fixtures of container {}", c.uuid))?;
                r.fixtures.befores.extend(c.befores.iter().cloned());
                r.fixtures.afters.extend(c.afters.iter().cloned());
                step_counts[i] += steps;
                touched.insert(i);
            }
        }
    }
    for i in touched {
        let r = &mut results[i];
        r.fixtures
            .befores
            .sort_by_key(|s| s.start.unwrap_or(i64::MAX));
        r.fixtures
            .afters
            .sort_by_key(|s| s.start.unwrap_or(i64::MAX));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties() {
        let kv = parse_properties(
            "# comment\nos=linux\nbrowser : chrome\nurl=https\\://www.example.com\nmulti=a\\\n  b\nempty=\nkey\\ space=v\nuni=\\u00e9\n",
        );
        let m: Vec<(String, String)> = kv.into_iter().map(|k| (k.key, k.value)).collect();
        assert_eq!(
            m,
            vec![
                ("os".into(), "linux".into()),
                ("browser".into(), "chrome".into()),
                ("url".into(), "https://www.example.com".into()),
                ("multi".into(), "ab".into()),
                ("empty".into(), "".into()),
                ("key space".into(), "v".into()),
                ("uni".into(), "é".into()),
            ]
        );
    }

    #[test]
    fn traceparent() {
        assert_eq!(
            trace_id_from_traceparent("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01")
                .as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_eq!(trace_id_from_traceparent("garbage"), None);
    }
}
