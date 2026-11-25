//! Triggers the CI that produced a launch so that it runs a test plan: GitHub Actions
//! `workflow_dispatch`, a GitLab pipeline trigger, or a generic webhook. One request per
//! rerun, no retries (a retry could start a second pipeline); the token is sent only to the
//! origin it is bound to and redirects are not followed.

use fastproof_store::Launch;
use fastproof_store::analysis::plans::TestPlanJson;
use std::collections::BTreeMap;

use reqwest::Method;
use serde_json::json;
use uuid::Uuid;

use axum::http::StatusCode;
use fastproof_store::analysis::settings::branch_glob_match;
use fastproof_store::plan_dispatches::MAX_CI_ENVIRONMENTS;
use fastproof_store::tm::PlanCi;
use franshub_http::ApiError;

use super::egress::MAX_RESPONSE_BYTES;
use super::secrets::{SecretPurpose, origin_of};
use super::settings::{
    MAX_RERUN_VARIABLES, MAX_VARIABLE_VALUE_BYTES, RerunSettings, valid_variable_value,
};
use crate::state::AppState;

/// Tests sent inline to a webhook; a larger plan is fetched from `planUrl`.
pub const MAX_INLINE_TESTS: usize = 2000;

/// What the CI is told about a rerun.
pub struct Job<'a> {
    pub project: &'a str,
    pub launch: &'a Launch,
    pub rerun_id: Uuid,
    pub external_id: &'a str,
    pub plan_id: Uuid,
    pub mode: &'a str,
    pub plan: &'a TestPlanJson,
}

/// What a plan start adds to a [`Job`]: the ref, the variables and the environment name.
pub struct PlanExtras<'a> {
    pub git_ref: &'a str,
    pub variables: &'a BTreeMap<String, String>,
    pub environment: Option<&'a str>,
}

impl PlanExtras<'_> {
    /// The variables as sent: the allowlisted ones and `FASTPROOF_ENVIRONMENT`.
    fn sent(&self) -> BTreeMap<String, String> {
        let mut v = self.variables.clone();
        if let Some(e) = self.environment {
            v.insert("FASTPROOF_ENVIRONMENT".into(), e.into());
        }
        v
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn git_ref(settings: &RerunSettings) -> String {
    settings
        .git_ref
        .clone()
        .unwrap_or_else(|| "main".to_string())
}

fn plan_url(st: &AppState, job: &Job<'_>) -> Option<String> {
    super::dispatch::page_url(
        st,
        &format!(
            "api/v1/projects/{}/test-plans/{}/testplan.json",
            job.project, job.plan_id
        ),
    )
}

fn base_url(st: &AppState) -> Option<String> {
    st.cfg
        .public_url
        .as_ref()
        .map(|u| u.as_str().trim_end_matches('/').to_string())
}

fn refused(status: u16) -> String {
    match status {
        401 | 403 => format!("the CI refused the token (HTTP {status})"),
        404 => "the CI does not know the workflow, project or ref (HTTP 404)".to_string(),
        s => format!("the CI answered HTTP {s}"),
    }
}

/// Triggers the CI. Returns a reference the CI gave (a pipeline id, or its URL when that is
/// on the configured origin) or an error text that holds no URL or secret.
pub async fn dispatch(
    st: &AppState,
    project_id: Uuid,
    settings: &RerunSettings,
    job: &Job<'_>,
) -> Result<Option<String>, String> {
    dispatch_with(st, project_id, settings, job, None).await
}

/// [`dispatch`] for a launch started from a plan (`job.mode` is `plan`): the request carries the
/// ref and the variables of `extras`.
pub async fn dispatch_plan(
    st: &AppState,
    project_id: Uuid,
    settings: &RerunSettings,
    job: &Job<'_>,
    extras: &PlanExtras<'_>,
) -> Result<Option<String>, String> {
    dispatch_with(st, project_id, settings, job, Some(extras)).await
}

async fn dispatch_with(
    st: &AppState,
    project_id: Uuid,
    settings: &RerunSettings,
    job: &Job<'_>,
    extras: Option<&PlanExtras<'_>>,
) -> Result<Option<String>, String> {
    let git_ref_of = |settings: &RerunSettings| match extras {
        Some(e) => e.git_ref.to_string(),
        None => git_ref(settings),
    };
    let event = if extras.is_some() { "plan" } else { "rerun" };
    let scope = settings.scope();
    let secret = |name: String| {
        let (url, scope) = (settings.url.clone(), scope.clone());
        async move {
            st.project_secret_scoped(
                project_id,
                &name,
                SecretPurpose::Ci,
                Some(&url),
                Some(&scope),
            )
            .await
        }
    };
    let token = match &settings.token_secret_ref {
        Some(name) => Some(
            secret(name.clone())
                .await
                .map_err(|e| e.to_string())?
                .expose()
                .trim()
                .to_string(),
        ),
        None => None,
    };
    let signing = match (&settings.signing_secret_ref, settings.provider.as_str()) {
        (Some(name), "webhook") => Some(secret(name.clone()).await.map_err(|e| e.to_string())?),
        _ => None,
    };
    let base = settings.url.trim_end_matches('/');
    let url_of_plan = plan_url(st, job);
    let fastproof_url = base_url(st);
    let (method, url, rb_body, content_type): (Method, String, String, &str) =
        match settings.provider.as_str() {
            "github" => {
                let repo = settings.repository.as_deref().unwrap_or_default();
                let wf = settings.workflow.as_deref().unwrap_or_default();
                let mut inputs = json!({
                    "fastproof_plan_id": job.plan_id.to_string(),
                    "fastproof_launch": job.external_id,
                    "fastproof_source_launch": job.launch.id.to_string(),
                });
                if let Some(u) = &fastproof_url {
                    inputs["fastproof_url"] = json!(u);
                }
                if let Some(e) = extras {
                    let sent = e.sent();
                    if !sent.is_empty() {
                        inputs["fastproof_variables"] = json!(json!(sent).to_string());
                    }
                }
                (
                    Method::POST,
                    format!(
                        "{base}/repos/{repo}/actions/workflows/{}/dispatches",
                        enc(wf)
                    ),
                    json!({"ref": git_ref_of(settings), "inputs": inputs}).to_string(),
                    "application/json",
                )
            }
            "gitlab" => {
                let repo = settings.repository.as_deref().unwrap_or_default();
                let mut f = url::form_urlencoded::Serializer::new(String::new());
                f.append_pair("token", token.as_deref().unwrap_or_default());
                f.append_pair("ref", &git_ref_of(settings));
                f.append_pair("variables[FASTPROOF_PLAN_ID]", &job.plan_id.to_string());
                f.append_pair("variables[FASTPROOF_LAUNCH]", job.external_id);
                f.append_pair(
                    "variables[FASTPROOF_SOURCE_LAUNCH]",
                    &job.launch.id.to_string(),
                );
                if let Some(u) = &fastproof_url {
                    f.append_pair("variables[FASTPROOF_URL]", u);
                }
                if let Some(e) = extras {
                    for (k, v) in e.sent() {
                        f.append_pair(&format!("variables[{k}]"), &v);
                    }
                }
                (
                    Method::POST,
                    format!("{base}/api/v4/projects/{}/trigger/pipeline", enc(repo)),
                    f.finish(),
                    "application/x-www-form-urlencoded",
                )
            }
            _ => {
                let inline = job.plan.tests.len() <= MAX_INLINE_TESTS;
                let mut body = json!({
                    "event": event,
                    "project": job.project,
                    "rerunId": job.rerun_id,
                    "mode": job.mode,
                    "launch": {
                        "id": job.launch.id,
                        "number": job.launch.number,
                        "name": job.launch.name,
                        "externalId": job.launch.external_id,
                        "branch": job.launch.vcs.as_ref().and_then(|v| v.branch.clone()),
                        "commit": job.launch.vcs.as_ref().and_then(|v| v.commit.clone()),
                    },
                    "newLaunchExternalId": job.external_id,
                    "planId": job.plan_id,
                    "planUrl": url_of_plan,
                    "testCount": job.plan.tests.len(),
                    "testplan": inline.then_some(job.plan),
                });
                if let Some(e) = extras {
                    body["variables"] = json!(e.sent());
                    body["environment"] = json!(e.environment);
                }
                (
                    Method::POST,
                    settings.url.clone(),
                    body.to_string(),
                    "application/json",
                )
            }
        };
    let mut rb = st
        .egress
        .request(method, &url)
        .map_err(|e| e.to_string())?
        .header(reqwest::header::CONTENT_TYPE, content_type);
    if settings.provider == "webhook" {
        let ts = chrono::Utc::now().timestamp();
        rb = rb
            .header("X-FastProof-Event", event)
            .header("X-FastProof-Delivery", job.rerun_id.to_string())
            .header("X-FastProof-Timestamp", ts.to_string());
        if let Some(k) = &signing {
            rb = rb.header(
                "X-FastProof-Signature",
                super::notify::signature(k.expose().as_bytes(), ts, rb_body.as_bytes()),
            );
        }
    }
    match (settings.provider.as_str(), &token) {
        ("github", Some(t)) => {
            rb = rb
                .bearer_auth(t)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
        }
        ("webhook", Some(t)) => rb = rb.bearer_auth(t),
        _ => {}
    }
    let r = st
        .egress
        .send_no_redirect(rb.body(rb_body), MAX_RESPONSE_BYTES)
        .await
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&r.status) {
        return Err(refused(r.status));
    }
    if settings.provider == "gitlab" {
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
        let web = v
            .get("web_url")
            .and_then(|u| u.as_str())
            .filter(|u| origin_of(u) == origin_of(&settings.url) && u.len() <= 500);
        return Ok(web
            .map(str::to_string)
            .or_else(|| v.get("id").and_then(|i| i.as_u64()).map(|i| i.to_string())));
    }
    Ok(None)
}

/// What was dispatched, for the audit log: the target as it was when the CI was called.
pub fn target(settings: &RerunSettings) -> serde_json::Value {
    json!({
        "provider": settings.provider,
        "repository": settings.repository,
        "workflow": settings.workflow,
        "ref": settings.git_ref.clone().unwrap_or_else(|| "main".into()),
        "origin": origin_of(&settings.url),
        "tokenSecretRef": settings.token_secret_ref,
        "signingSecretRef": settings.signing_secret_ref,
    })
}

/// What a plan start sends to the CI, checked against the project's `rerun` settings.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedPlanCi {
    pub git_ref: String,
    /// One entry per launch: the environment name and the variables of that launch.
    pub environments: Vec<(Option<String>, BTreeMap<String, String>)>,
}

fn not_allowed(name: &str, what: &str) -> ApiError {
    let shown: String = name.chars().take(64).collect();
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "ci_variable_not_allowed",
        format!("CI variable {what}: it is not in the project's rerun.variables"),
    )
    .with_details(json!({"name": shown}))
}

/// Git ref-name rules on top of the allowed characters (letters, digits, `- _ . /`): bounded,
/// no leading `-` or `.`, no empty, dotted or `.lock` segments, no trailing `/` or `.`.
pub fn valid_git_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 200
        && !r.starts_with(['-', '/'])
        && !r.ends_with(['/', '.'])
        && !r.contains("..")
        && !r.contains("//")
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        && r.split('/')
            .all(|seg| !seg.starts_with('.') && !seg.ends_with(".lock"))
}

fn valid_environment(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn checked(
    cfg: &RerunSettings,
    vars: &BTreeMap<String, String>,
    into: &mut BTreeMap<String, String>,
) -> Result<(), ApiError> {
    if vars.len() > MAX_RERUN_VARIABLES {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_RERUN_VARIABLES} CI variables"
        )));
    }
    let allowed = cfg.variables.as_deref().unwrap_or_default();
    for (name, value) in vars {
        let Some(def) = allowed.iter().find(|v| &v.name == name) else {
            return Err(not_allowed(name, "is not allowed"));
        };
        match &def.values {
            Some(list) if !list.contains(value) => {
                return Err(not_allowed(name, "has a value that is not listed"));
            }
            None if !valid_variable_value(value) => {
                return Err(ApiError::bad_request(format!(
                    "the value of {name} has 1 to {MAX_VARIABLE_VALUE_BYTES} bytes without control characters"
                )));
            }
            _ => {}
        }
        into.insert(name.clone(), value.clone());
    }
    Ok(())
}

/// The ref, the environments and the variables of a plan start: variables only by name from
/// `rerun.variables`, with values from its list when it has one; defaults fill the rest, request
/// variables override them and an environment's own override both. The ref is the configured one
/// or one that matches `rerun.refs`.
pub fn resolve_plan_ci(cfg: &RerunSettings, ci: &PlanCi) -> Result<ResolvedPlanCi, ApiError> {
    let default_ref = git_ref(cfg);
    let git_ref = match ci.git_ref.as_deref() {
        None => default_ref,
        Some(r) => {
            let valid = valid_git_ref(r);
            let listed = cfg.refs.iter().flatten().any(|g| branch_glob_match(g, r));
            if !valid || (r != default_ref && !listed) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "ci_ref_not_allowed",
                    "the ref matches none of the project's rerun.refs",
                ));
            }
            r.to_string()
        }
    };
    let mut base: BTreeMap<String, String> = cfg
        .variables
        .iter()
        .flatten()
        .filter_map(|v| v.default.clone().map(|d| (v.name.clone(), d)))
        .collect();
    if let Some(v) = &ci.variables {
        checked(cfg, v, &mut base)?;
    }
    let named = ci.environments.as_deref().unwrap_or_default();
    if named.len() > MAX_CI_ENVIRONMENTS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_CI_ENVIRONMENTS} CI environments"
        )));
    }
    if named.is_empty() {
        return Ok(ResolvedPlanCi {
            git_ref,
            environments: vec![(None, base)],
        });
    }
    let mut seen = std::collections::HashSet::new();
    let mut environments = Vec::with_capacity(named.len());
    for e in named {
        if !valid_environment(&e.name) || !seen.insert(e.name.as_str()) {
            return Err(ApiError::bad_request(
                "environment names are unique, 1 to 64 letters, digits, . _ -, starting with a letter or digit",
            ));
        }
        let mut vars = base.clone();
        if let Some(v) = &e.variables {
            checked(cfg, v, &mut vars)?;
        }
        environments.push((Some(e.name.clone()), vars));
    }
    Ok(ResolvedPlanCi {
        git_ref,
        environments,
    })
}

/// An error text as shown to callers without `settings:write`: no host, origin or secret name.
pub fn generic_error(e: &str) -> String {
    if e.starts_with("the CI answered HTTP")
        || e.starts_with("the CI refused the token")
        || e.starts_with("the CI does not know")
        || e.starts_with("no tests")
        || e.starts_with("the plan would")
        || e.starts_with("the project has too many")
    {
        e.to_string()
    } else {
        "the CI was not reached or the project settings refused the call; a project admin can see why"
            .to_string()
    }
}

/// A CI reference as shown to callers without `settings:write`: the pipeline id only.
pub fn generic_ci_ref(r: &str) -> String {
    r.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_no_url() {
        assert!(refused(404).contains("404"));
        assert!(refused(401).contains("token"));
        assert_eq!(refused(500), "the CI answered HTTP 500");
    }

    #[test]
    fn readers_see_no_destination() {
        assert_eq!(
            generic_error("the CI answered HTTP 422"),
            "the CI answered HTTP 422"
        );
        let g =
            generic_error("egress denied: ci.internal.example is not in FASTPROOF_EGRESS_ALLOW");
        assert!(!g.contains("internal") && !g.contains("EGRESS"), "{g}");
        let g = generic_error("the secret \"ci-token\" may only be sent to https://x.example");
        assert!(!g.contains("ci-token") && !g.contains("x.example"), "{g}");
        assert_eq!(
            generic_ci_ref("https://gitlab.example.com/g/app/-/pipelines/77"),
            "77"
        );
        assert_eq!(generic_ci_ref("77"), "77");
    }

    #[test]
    fn git_ref_rules() {
        for ok in ["main", "release/1.2", "v1.0.0", "feature/a_b-c"] {
            assert!(valid_git_ref(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "/x",
            "x/",
            "x.",
            "a..b",
            "a//b",
            ".hidden",
            "a/.b",
            "x.lock",
            "a.lock/b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "a\nb",
            &"x".repeat(201),
        ] {
            assert!(!valid_git_ref(bad), "{bad:?}");
        }
    }

    #[test]
    fn reserved_variable_names_are_refused() {
        for n in [
            "CI_DEBUG_TRACE",
            "GITLAB_TOKEN",
            "GITHUB_ENV",
            "RUNNER_TEMP",
            "ACTIONS_STEP_DEBUG",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "PATH",
            "BASH_ENV",
            "ENV",
            "CI",
            "NODE_OPTIONS",
        ] {
            assert!(
                !crate::integrations::settings::valid_variable_name(n),
                "{n}"
            );
        }
        for n in ["BROWSER", "SUITE", "TARGET_ENV"] {
            assert!(crate::integrations::settings::valid_variable_name(n), "{n}");
        }
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(enc("group/app"), "group%2Fapp");
        assert_eq!(enc("tests.yml"), "tests.yml");
        assert_eq!(enc("a b?c"), "a%20b%3Fc");
    }
}
