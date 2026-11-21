//! Playwright JSON reporter output.

use fastproof_model::normalize::{normalize_path, normalize_text};
use fastproof_model::{
    AttachmentRef, GlobalError, IdentityInput, Label, LaunchMeta, Link, ParsedResult, SourceFormat,
    SourceRef, Stage, Status, StatusDetails, Step, TestIdentity,
};
use serde_json::Value;

use crate::ctx::{Ctx, StepBudget};
use crate::error::IngestError;
use crate::json::{arr, as_string, f64_field, i64_field, str_field, text_field};
use crate::text::{base64_decode, bound_opt, cap, expected_actual, parse_timestamp_ms};

pub(crate) struct PlaywrightDoc {
    pub results: Vec<ParsedResult>,
    pub meta: LaunchMeta,
    pub errors: Vec<GlobalError>,
}

/// Content type of allure-js runtime messages routed through `testInfo.attach`.
const ALLURE_RUNTIME_MESSAGE: &str = "application/vnd.allure.message+json";

pub(crate) fn is_playwright(v: &Value) -> bool {
    v.get("suites").is_some_and(Value::is_array) && v.get("config").is_some_and(Value::is_object)
}

pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

struct Walk<'p> {
    file: &'p str,
    dir: String,
    prefixes: Vec<&'p str>,
}

fn output(v: &Value, key: &str) -> String {
    let mut out = String::new();
    for item in arr(v, key) {
        if let Some(t) = item.get("text").and_then(Value::as_str) {
            out.push_str(t);
        } else if let Some(b) = item.get("buffer").and_then(Value::as_str)
            && let Some(bytes) = base64_decode(b)
        {
            out.push_str(&String::from_utf8_lossy(&bytes));
        } else if let Some(s) = item.as_str() {
            out.push_str(s);
        }
    }
    out
}

fn steps(v: &Value, budget: &mut StepBudget, max: u64) -> Result<Vec<Step>, IngestError> {
    let mut out = Vec::new();
    for s in arr(v, "steps") {
        budget.tick()?;
        let error = s.get("error").filter(|e| !e.is_null());
        let status_details = error
            .map(|e| StatusDetails {
                message: text_field(e, "message").map(|m| cap(m, max)),
                trace: text_field(e, "stack").map(|m| cap(m, max)),
                ..StatusDetails::default()
            })
            .unwrap_or_default();
        out.push(Step {
            name: text_field(s, "title").unwrap_or_default(),
            status: if error.is_some() {
                Status::Failed
            } else {
                Status::Passed
            },
            status_details,
            start: s
                .get("startTime")
                .and_then(Value::as_str)
                .and_then(parse_timestamp_ms),
            stop: None,
            duration_ms: f64_field(s, "duration").map(|d| d.round() as i64),
            steps: steps(s, budget, max)?,
            ..Step::default()
        });
    }
    Ok(out)
}

pub(crate) fn parse(
    v: &Value,
    file: &str,
    ctx: &mut Ctx<'_>,
) -> Result<PlaywrightDoc, IngestError> {
    let null = Value::Null;
    let config = v.get("config").unwrap_or(&null);
    let mut meta = LaunchMeta::default();
    meta.set_attribute("tool", "playwright");
    if let Some(ver) = text_field(config, "version") {
        meta.set_attribute("tool.version", ver);
    }
    if let Some(shard) = config.get("shard").filter(|s| s.is_object())
        && let (Some(c), Some(t)) = (i64_field(shard, "current"), i64_field(shard, "total"))
    {
        meta.set_attribute("shard", format!("{c}/{t}"));
    }
    if let Some(md) = config.get("metadata") {
        if let Some(g) = md.get("gitCommit") {
            meta.vcs.commit = text_field(g, "hash");
            meta.vcs.branch = text_field(g, "branch");
        }
        if let Some(ci) = md.get("ci") {
            meta.ci.url = text_field(ci, "buildHref");
        }
    }
    let stats = v.get("stats").unwrap_or(&null);
    meta.start = stats
        .get("startTime")
        .and_then(Value::as_str)
        .and_then(parse_timestamp_ms);
    meta.stop = meta
        .start
        .zip(f64_field(stats, "duration"))
        .map(|(s, d)| s + d.round() as i64);
    let max = ctx.limits.max_log_bytes;
    let errors = arr(v, "errors")
        .iter()
        .map(|e| GlobalError {
            message: text_field(e, "message").map(|m| cap(m, max)),
            trace: text_field(e, "stack").map(|m| cap(m, max)),
            timestamp: None,
        })
        .collect();

    let mut prefixes = ctx.options.path_prefixes();
    let root = str_field(config, "rootDir");
    if let Some(r) = root.as_deref() {
        prefixes.insert(0, r);
    }
    let walk = Walk {
        file,
        dir: file
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default(),
        prefixes,
    };
    let mut results = Vec::new();
    for s in arr(v, "suites") {
        visit_suite(s, &[], true, &walk, ctx, &mut results)?;
    }
    Ok(PlaywrightDoc {
        results,
        meta,
        errors,
    })
}

fn visit_suite(
    s: &Value,
    titles: &[String],
    top: bool,
    walk: &Walk<'_>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<ParsedResult>,
) -> Result<(), IngestError> {
    // Every suite copies the titles of its ancestors.
    ctx.charge_copy(titles.iter().map(|t| t.len() as u64 + 24).sum(), walk.file)?;
    let mut path = titles.to_vec();
    if !top && let Some(t) = text_field(s, "title") {
        path.push(ctx.name(t, "suite title", walk.file));
    }
    for spec in arr(s, "specs") {
        visit_spec(spec, &path, walk, ctx, out)?;
    }
    for child in arr(s, "suites") {
        visit_suite(child, &path, false, walk, ctx, out)?;
    }
    Ok(())
}

fn visit_spec(
    spec: &Value,
    titles: &[String],
    walk: &Walk<'_>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<ParsedResult>,
) -> Result<(), IngestError> {
    let max = ctx.limits.max_log_bytes;
    let title = ctx.name(
        text_field(spec, "title").unwrap_or_default(),
        "title",
        walk.file,
    );
    let spec_file = text_field(spec, "file")
        .map(|f| normalize_path(&f, &walk.prefixes))
        .unwrap_or_default();
    let mut segs: Vec<String> = Vec::new();
    if !spec_file.is_empty() {
        segs.push(spec_file.clone());
    }
    segs.extend(titles.iter().map(|t| normalize_text(t)));
    segs.push(normalize_text(&title));
    // Names built from the titles of all ancestors are copies too.
    ctx.charge_copy(
        2 * segs.iter().map(|t| t.len() as u64 + 3).sum::<u64>(),
        walk.file,
    )?;
    let full_name = ctx.name(segs.join(" \u{203a} "), "full name", walk.file);
    let display = ctx.name(
        titles
            .iter()
            .cloned()
            .chain(std::iter::once(title.clone()))
            .collect::<Vec<_>>()
            .join(" \u{203a} "),
        "display name",
        walk.file,
    );
    let spec_tags: Vec<String> = arr(spec, "tags").iter().filter_map(as_string).collect();
    ctx.check_labels(spec_tags.len(), walk.file)?;
    let tag_bytes: u64 = spec_tags.iter().map(|t| t.len() as u64 + 48).sum();
    for (test_index, test) in arr(spec, "tests").iter().enumerate() {
        // Every test of the spec copies the spec tags.
        if test_index > 0 {
            ctx.charge_copy(tag_bytes, walk.file)?;
        }
        let project = text_field(test, "projectName").or_else(|| text_field(test, "projectId"));
        let expected_status = text_field(test, "expectedStatus").unwrap_or_else(|| "passed".into());
        let mut labels: Vec<Label> = spec_tags
            .iter()
            .map(|t| Label::new("tag", t.trim_start_matches('@').to_string()))
            .collect();
        let mut links: Vec<Link> = Vec::new();
        let mut allure_id = None;
        for a in arr(test, "annotations") {
            let Some(ty) = text_field(a, "type") else {
                continue;
            };
            let desc = str_field(a, "description").unwrap_or_default();
            if ty.eq_ignore_ascii_case("allure_id") || ty.eq_ignore_ascii_case("as_id") {
                allure_id = Some(desc.clone());
            }
            if (desc.starts_with("http://") || desc.starts_with("https://"))
                && matches!(ty.as_str(), "issue" | "tms" | "link" | "fail" | "fixme")
            {
                links.push(Link {
                    name: None,
                    url: desc.clone(),
                    link_type: Some(if ty == "tms" || ty == "link" {
                        ty.clone()
                    } else {
                        "issue".into()
                    }),
                });
            }
            labels.push(Label::new(ty, desc));
            ctx.check_labels(labels.len(), walk.file)?;
        }
        if let Some(p) = &project {
            labels.push(Label::new("project", p.clone()));
        }
        // The worker label is added per result.
        ctx.check_labels(labels.len() + 1, walk.file)?;
        let identity = TestIdentity::build(IdentityInput {
            format: Some(SourceFormat::PlaywrightJson),
            allure_id: allure_id.as_deref(),
            full_name: &full_name,
            display_name: &display,
            env_key: ctx.env_key(project.as_deref()).as_deref(),
            source: SourceRef {
                file: (!spec_file.is_empty()).then(|| spec_file.clone()),
                line: i64_field(spec, "line").and_then(|l| u32::try_from(l).ok()),
                code_ref: text_field(spec, "id"),
            },
            ..Default::default()
        });
        let results_v = arr(test, "results");
        let count = results_v.len();
        ctx.check_attempts(count, walk.file)?;
        // Every result of the test copies its identity, names, labels and links.
        let copy_bytes = 512
            + crate::ctx::identity_bytes(&identity)
            + crate::ctx::labels_bytes(&labels)
            + crate::ctx::links_bytes(&links)
            + (title.len() + full_name.len() + spec_file.len()) as u64;
        for (i, rv) in results_v.iter().enumerate() {
            if i > 0 {
                ctx.charge_copy(copy_bytes, walk.file)?;
            }
            if out.len() as u64 >= ctx.limits.max_results {
                return Err(IngestError::limit_at(
                    "max_results",
                    ctx.limits.max_results,
                    walk.file.to_string(),
                ));
            }
            let raw = text_field(rv, "status").unwrap_or_default();
            let status = match raw.as_str() {
                "passed" if expected_status == "failed" => Status::Failed,
                "failed" | "timedOut" if expected_status == "failed" => Status::Passed,
                "passed" => Status::Passed,
                "failed" | "timedOut" => Status::Failed,
                "skipped" => Status::Skipped,
                "interrupted" => Status::Broken,
                _ => Status::Unknown,
            };
            let null = Value::Null;
            let err = arr(rv, "errors")
                .first()
                .or_else(|| rv.get("error"))
                .unwrap_or(&null);
            let message = text_field(err, "message");
            let ea = message
                .as_deref()
                .map(strip_ansi)
                .and_then(|m| expected_actual(&m));
            let mut budget = ctx.steps(walk.file);
            let start = rv
                .get("startTime")
                .and_then(Value::as_str)
                .and_then(parse_timestamp_ms);
            let duration = f64_field(rv, "duration").map(|d| d.round() as i64);
            let retry = i64_field(rv, "retry")
                .and_then(|r| u32::try_from(r).ok())
                .unwrap_or(i as u32);
            let mut attachments: Vec<AttachmentRef> = Vec::new();
            for a in arr(rv, "attachments") {
                let ct = text_field(a, "contentType");
                if ct.as_deref() == Some(ALLURE_RUNTIME_MESSAGE) {
                    continue;
                }
                let name = text_field(a, "name").unwrap_or_else(|| "attachment".into());
                if let Some(p) = text_field(a, "path") {
                    budget.attachment()?;
                    ctx.count_ref(walk.file)?;
                    let mut r = AttachmentRef::new(name, p.clone(), ct);
                    if !p.starts_with('/') && !p.contains(":\\") {
                        r.resolved_path = Some(crate::allure::join(&walk.dir, &p));
                    }
                    attachments.push(r);
                } else if let Some(b) = text_field(a, "body") {
                    match base64_decode(&b) {
                        Some(bytes) => {
                            budget.attachment()?;
                            attachments.push(ctx.add_inline(&name, ct, bytes, walk.file)?)
                        }
                        None => ctx.warn(
                            Some(walk.file),
                            format!("attachment {name:?}: invalid base64 body"),
                        ),
                    }
                }
            }
            let mut r = ParsedResult {
                source_uuid: Some(format!(
                    "{}:{}:{}",
                    text_field(spec, "id").unwrap_or_default(),
                    text_field(test, "projectId").unwrap_or_default(),
                    retry
                )),
                identity: identity.clone(),
                name: title.clone(),
                full_name: full_name.clone(),
                status,
                raw_status: Some(raw.clone()),
                status_details: StatusDetails {
                    message: message.map(|m| cap(m, max)),
                    trace: text_field(err, "stack").map(|m| cap(m, max)),
                    expected: ea.as_ref().map(|e| e.0.clone()),
                    actual: ea.map(|e| e.1),
                    flaky: i + 1 == count
                        && count > 1
                        && text_field(test, "status").as_deref() == Some("flaky"),
                    ..StatusDetails::default()
                },
                stage: if raw == "interrupted" {
                    Stage::Interrupted
                } else {
                    Stage::Finished
                },
                interrupted: raw == "interrupted",
                start,
                stop: start.zip(duration).map(|(s, d)| s + d),
                duration_ms: duration,
                attempt: Some(retry),
                labels: labels.clone(),
                links: links.clone(),
                steps: steps(rv, &mut budget, max)?,
                attachments,
                stdout: bound_opt(&output(rv, "stdout"), max),
                stderr: bound_opt(&output(rv, "stderr"), max),
                format: SourceFormat::PlaywrightJson,
                source_file: Some(walk.file.to_string()),
                ..ParsedResult::default()
            };
            if let Some(w) = i64_field(rv, "workerIndex") {
                r.labels.push(Label::new("thread", format!("worker-{w}")));
            }
            ctx.finish(&mut r);
            out.push(r);
        }
    }
    Ok(())
}
