//! CTRF 0.2.x (lenient with 0.0.x).

use fastproof_model::normalize::{normalize_path, normalize_text};
use fastproof_model::{
    AttachmentRef, IdentityInput, KeyValue, Label, LaunchMeta, Parameter, ParsedResult,
    SourceFormat, SourceRef, Stage, Status, StatusDetails, Step, TestIdentity,
};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::error::IngestError;
use crate::json::{arr, as_string, bool_field, i64_field, str_field, text_field};
use crate::text::{bound_opt, cap, expected_actual};

pub(crate) struct CtrfDoc {
    pub results: Vec<ParsedResult>,
    pub meta: LaunchMeta,
}

pub(crate) fn is_ctrf(v: &Value) -> bool {
    v.get("reportFormat")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("ctrf"))
        || (v.get("results").and_then(|r| r.get("tool")).is_some()
            && v.get("results")
                .and_then(|r| r.get("tests"))
                .is_some_and(Value::is_array))
}

fn ctrf_status(s: &str) -> Status {
    match s.trim().to_ascii_lowercase().as_str() {
        "passed" => Status::Passed,
        "failed" => Status::Failed,
        "skipped" | "pending" => Status::Skipped,
        _ => Status::Unknown,
    }
}

fn suite_path(v: &Value) -> Vec<String> {
    match v.get("suite") {
        Some(Value::Array(a)) => a.iter().filter_map(as_string).collect(),
        Some(Value::String(s)) => s
            .split(" > ")
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn joined_lines(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(as_string)
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn attachments(
    v: &Value,
    dir: &str,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<AttachmentRef>, IngestError> {
    let mut budget = ctx.steps(file);
    let mut out: Vec<AttachmentRef> = Vec::new();
    for a in arr(v, "attachments") {
        let Some(path) = text_field(a, "path") else {
            continue;
        };
        budget.attachment()?;
        let name = text_field(a, "name")
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).to_string());
        let mut r = AttachmentRef::new(name, path.clone(), text_field(a, "contentType"));
        r.resolved_path = Some(crate::allure::join(dir, &path));
        out.push(r);
    }
    if let Some(s) = text_field(v, "screenshot")
        && !s.starts_with("data:")
        && s.len() < 4096
        && !out.iter().any(|a| a.source == s)
    {
        budget.attachment()?;
        let mut r = AttachmentRef::new("screenshot", s.clone(), Some("image/png".into()));
        r.resolved_path = Some(crate::allure::join(dir, &s));
        out.push(r);
    }
    ctx.add_refs(budget.attachments(), file)?;
    Ok(out)
}

pub(crate) fn parse(v: &Value, file: &str, ctx: &mut Ctx<'_>) -> Result<CtrfDoc, IngestError> {
    let null = Value::Null;
    let results_v = v.get("results").unwrap_or(&null);
    let tool = results_v.get("tool").unwrap_or(&null);
    let tool_name = text_field(tool, "name").unwrap_or_default();
    let env = results_v.get("environment").unwrap_or(&null);
    let mut meta = LaunchMeta::default();
    if let Value::Object(m) = env {
        for (k, val) in m {
            if let Some(s) = as_string(val) {
                meta.environment.push(KeyValue::new(k.clone(), s));
            }
        }
        meta.vcs.repository =
            text_field(env, "repositoryUrl").or_else(|| text_field(env, "repositoryName"));
        meta.vcs.branch = text_field(env, "branchName");
        meta.vcs.commit = text_field(env, "commit");
        meta.ci.pipeline = text_field(env, "buildName");
        meta.ci.url = text_field(env, "buildUrl");
        meta.ci.build_order = i64_field(env, "buildNumber");
        meta.environment_key = text_field(env, "testEnvironment");
        meta.name = text_field(env, "appName");
    }
    if !tool_name.is_empty() {
        meta.set_attribute("tool", tool_name.clone());
    }
    if let Some(ver) = text_field(tool, "version") {
        meta.set_attribute("tool.version", ver);
    }
    for key in ["specVersion", "reportId", "runId", "generatedBy"] {
        if let Some(s) = text_field(v, key) {
            meta.set_attribute(&format!("ctrf.{key}"), s);
        }
    }
    let summary = results_v.get("summary").unwrap_or(&null);
    meta.start = i64_field(summary, "start").filter(|s| *s > 0);
    meta.stop = i64_field(summary, "stop").filter(|s| *s > 0);

    let dir = file
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let prefixes = ctx.options.path_prefixes();
    let max = ctx.limits.max_log_bytes;
    let mut results = Vec::new();
    for t in arr(results_v, "tests") {
        if !t.is_object() {
            continue;
        }
        let name = match text_field(t, "name") {
            Some(n) => ctx.name(n, "name", file),
            None => "Unknown test".into(),
        };
        let file_path = text_field(t, "filePath").map(|p| normalize_path(&p, &prefixes));
        let mut suite = suite_path(t);
        // Playwright's CTRF reporter puts the project first; it is an environment dimension.
        let project = if tool_name.eq_ignore_ascii_case("playwright") && suite.len() >= 2 {
            Some(suite.remove(0))
        } else {
            text_field(t, "browser")
        };
        if let Some(f) = &file_path {
            let base = f.rsplit('/').next().unwrap_or(f);
            suite.retain(|s| s != base && s != f);
        }
        let mut segments: Vec<String> = Vec::new();
        if let Some(f) = &file_path {
            segments.push(f.clone());
        }
        segments.extend(suite.iter().map(|s| normalize_text(s)));
        segments.push(normalize_text(&name));
        let full_name = ctx.name(segments.join(" \u{203a} "), "full name", file);
        let mut parameters: Vec<Parameter> = Vec::new();
        if let Some(Value::Object(m)) = t.get("parameters") {
            for (k, val) in m {
                parameters.push(Parameter::new(
                    k.clone(),
                    as_string(val).unwrap_or_else(|| val.to_string()),
                ));
            }
        }
        let mut labels: Vec<Label> = Vec::new();
        for tag in arr(t, "tags").iter().filter_map(as_string) {
            labels.push(Label::new("tag", tag));
        }
        if let Some(Value::Object(m)) = t.get("labels") {
            for (k, val) in m {
                labels.push(Label::new(
                    k.clone(),
                    as_string(val).unwrap_or_else(|| val.to_string()),
                ));
            }
        }
        if let Some(ty) = text_field(t, "type") {
            labels.push(Label::new("type", ty));
        }
        for s in &suite {
            labels.push(Label::new("suite", s.clone()));
        }
        ctx.check_labels(labels.len(), file)?;
        let retry_attempts = arr(t, "retryAttempts");
        ctx.check_attempts(retry_attempts.len() + 1, file)?;
        let allure_id = labels
            .iter()
            .find(|l| {
                l.name.eq_ignore_ascii_case("ALLURE_ID") || l.name.eq_ignore_ascii_case("AS_ID")
            })
            .map(|l| l.value.clone());
        let test_id = text_field(t, "testId");
        let identity = TestIdentity::build(IdentityInput {
            format: Some(SourceFormat::Ctrf),
            allure_id: allure_id.as_deref(),
            ctrf_test_id: test_id.as_deref(),
            full_name: &full_name,
            display_name: &name,
            parameters: &parameters,
            env_key: ctx.env_key(project.as_deref()).as_deref(),
            source: SourceRef {
                file: file_path.clone(),
                line: i64_field(t, "line").and_then(|l| u32::try_from(l).ok()),
                code_ref: None,
            },
            ..Default::default()
        });
        let steps: Vec<Step> = arr(t, "steps")
            .iter()
            .filter_map(|s| {
                Some(Step {
                    name: text_field(s, "name")?,
                    status: text_field(s, "status")
                        .map(|x| ctrf_status(&x))
                        .unwrap_or_default(),
                    ..Step::default()
                })
            })
            .collect();
        if steps.len() as u64 > ctx.limits.max_steps_per_result {
            return Err(IngestError::limit_at(
                "max_steps_per_result",
                ctx.limits.max_steps_per_result,
                file.to_string(),
            ));
        }
        // Steps belong to the final attempt; they stay out of the copies made for retries.
        let base = ParsedResult {
            identity,
            name: name.clone(),
            full_name: full_name.clone(),
            stage: Stage::Finished,
            parameters,
            labels,
            format: SourceFormat::Ctrf,
            source_file: Some(file.to_string()),
            ..ParsedResult::default()
        };
        let details = |x: &Value| {
            let message = text_field(x, "message").map(|m| cap(m, max));
            let trace = text_field(x, "trace").map(|m| cap(m, max));
            let ea = message.as_deref().and_then(expected_actual);
            StatusDetails {
                expected: ea.as_ref().map(|e| e.0.clone()),
                actual: ea.map(|e| e.1),
                message,
                trace,
                ..StatusDetails::default()
            }
        };
        let attempts_total = retry_attempts.len() + 1;
        let copy_bytes = crate::ctx::result_bytes(&base);
        for (i, ra) in retry_attempts.iter().enumerate() {
            ctx.charge_copy(copy_bytes, file)?;
            let mut r = base.clone();
            r.status = text_field(ra, "status")
                .map(|s| ctrf_status(&s))
                .unwrap_or(Status::Failed);
            r.raw_status = text_field(ra, "status");
            r.status_details = details(ra);
            r.start = i64_field(ra, "start").filter(|s| *s > 0);
            r.stop = i64_field(ra, "stop").filter(|s| *s > 0);
            r.duration_ms = i64_field(ra, "duration");
            // CTRF numbers attempts from 1; the model counts retries from 0.
            r.attempt = Some(i as u32);
            r.source_uuid = text_field(ra, "attemptId");
            r.attachments = attachments(ra, &dir, file, ctx)?;
            r.stdout = bound_opt(&joined_lines(ra, "stdout"), max);
            r.stderr = bound_opt(&joined_lines(ra, "stderr"), max);
            ctx.finish(&mut r);
            results.push(r);
        }
        let mut r = base;
        r.steps = steps;
        r.status = text_field(t, "status")
            .map(|s| ctrf_status(&s))
            .unwrap_or_default();
        r.raw_status = text_field(t, "rawStatus").or_else(|| text_field(t, "status"));
        r.status_details = details(t);
        r.status_details.flaky = bool_field(t, "flaky");
        r.start = i64_field(t, "start").filter(|s| *s > 0);
        r.stop = i64_field(t, "stop").filter(|s| *s > 0);
        r.duration_ms = i64_field(t, "duration");
        r.attempt = (attempts_total > 1).then_some(retry_attempts.len() as u32);
        r.source_uuid = text_field(t, "attemptId")
            .or_else(|| text_field(t, "id"))
            .or_else(|| text_field(t, "executionId"));
        r.attachments = attachments(t, &dir, file, ctx)?;
        r.stdout = bound_opt(&joined_lines(t, "stdout"), max);
        r.stderr = bound_opt(&joined_lines(t, "stderr"), max);
        r.trace_id = text_field(t, "traceId")
            .or_else(|| t.get("extra").and_then(|e| text_field(e, "traceId")));
        ctx.finish(&mut r);
        results.push(r);
        if results.len() as u64 > ctx.limits.max_results {
            return Err(IngestError::limit_at(
                "max_results",
                ctx.limits.max_results,
                file.to_string(),
            ));
        }
    }
    if let Some(s) = str_field(v, "specVersion")
        && !s.starts_with("0.2")
        && !s.starts_with("0.0")
        && !s.starts_with("0.1")
    {
        ctx.warn(
            Some(file),
            format!("CTRF specVersion {s} is newer than 0.2; parsed leniently"),
        );
    }
    Ok(CtrfDoc { results, meta })
}
