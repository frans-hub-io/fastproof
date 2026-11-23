//! Triggers the CI that produced a launch so that it runs a test plan: GitHub Actions
//! `workflow_dispatch`, a GitLab pipeline trigger, or a generic webhook. One request per
//! rerun, no retries (a retry could start a second pipeline); the token is sent only to the
//! origin it is bound to and redirects are not followed.

use fastproof_store::Launch;
use fastproof_store::analysis::plans::TestPlanJson;
use reqwest::Method;
use serde_json::json;
use uuid::Uuid;

use super::egress::MAX_RESPONSE_BYTES;
use super::secrets::{SecretPurpose, origin_of};
use super::settings::RerunSettings;
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
                (
                    Method::POST,
                    format!(
                        "{base}/repos/{repo}/actions/workflows/{}/dispatches",
                        enc(wf)
                    ),
                    json!({"ref": git_ref(settings), "inputs": inputs}).to_string(),
                    "application/json",
                )
            }
            "gitlab" => {
                let repo = settings.repository.as_deref().unwrap_or_default();
                let mut f = url::form_urlencoded::Serializer::new(String::new());
                f.append_pair("token", token.as_deref().unwrap_or_default());
                f.append_pair("ref", &git_ref(settings));
                f.append_pair("variables[FASTPROOF_PLAN_ID]", &job.plan_id.to_string());
                f.append_pair("variables[FASTPROOF_LAUNCH]", job.external_id);
                f.append_pair(
                    "variables[FASTPROOF_SOURCE_LAUNCH]",
                    &job.launch.id.to_string(),
                );
                if let Some(u) = &fastproof_url {
                    f.append_pair("variables[FASTPROOF_URL]", u);
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
                let body = json!({
                    "event": "rerun",
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
            .header("X-FastProof-Event", "rerun")
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
    fn path_segments_are_encoded() {
        assert_eq!(enc("group/app"), "group%2Fapp");
        assert_eq!(enc("tests.yml"), "tests.yml");
        assert_eq!(enc("a b?c"), "a%20b%3Fc");
    }
}
