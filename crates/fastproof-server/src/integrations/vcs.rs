//! GitLab merge request notes and GitHub pull request comments (one per project and launch
//! name, found again by a marker and updated in place) and commit statuses.
//!
//! The token is used only for the repository pinned in `vcs.repository`: launch metadata is
//! written by uploaders, so a launch that names another repository is refused. Calls carry the
//! token in `Authorization` and never follow redirects. Text from launches is escaped so a
//! comment cannot carry links, images, mentions or HTML.

use reqwest::Method;
use serde::Deserialize;

use super::egress::{MAX_RESPONSE_BYTES, OutboundError};
use super::notify::{Outcome, Summary};
use super::secrets::SecretPurpose;
use super::settings::VcsSettings;
use crate::state::AppState;

/// Pages of comments searched for the marker.
pub const MAX_COMMENT_PAGES: u32 = 5;

/// `<!-- fastproof:<project>:<launch name> -->` with characters that could end the comment
/// replaced.
pub fn marker(project: &str, launch_name: &str) -> String {
    let clean: String = launch_name
        .chars()
        .map(|c| match c {
            '<' | '>' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect::<String>()
        .replace("--", "-");
    format!("<!-- fastproof:{project}:{} -->", clean.trim())
}

/// Text from a launch as inert Markdown: ASCII punctuation is backslash-escaped, `<`, `>` and
/// `&` become entities, line breaks become spaces, and a zero-width space after `@`, `#`, `!`,
/// `~`, `%`, `$` and in `://` and `www.` keeps mentions, references and bare links from forming.
pub fn md(s: &str) -> String {
    const ZWSP: char = '\u{200B}';
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            c if c.is_control() => out.push(' '),
            '@' | '#' | '!' | '~' | '%' | '$' => {
                out.push('\\');
                out.push(c);
                out.push(ZWSP);
            }
            ':' if chars.get(i + 1) == Some(&'/') => {
                out.push_str("\\:");
                out.push(ZWSP);
            }
            '.' if i >= 3
                && chars[i - 3..i]
                    .iter()
                    .collect::<String>()
                    .eq_ignore_ascii_case("www") =>
            {
                out.push_str("\\.");
                out.push(ZWSP);
            }
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c if c == ZWSP || c.is_whitespace() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A commit status name or description: printable text on one line, at most `max` characters.
fn plain(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect()
}

/// The Markdown body of the comment.
pub fn comment_body(s: &Summary) -> String {
    let c = &s.launch.counts;
    let mut out = vec![marker(&s.project, &s.launch.name)];
    let gate = match s.gate.status.as_str() {
        "failed" => "**Quality gate: failed**",
        "pending" => "Quality gate: pending",
        _ if s.gate.results.is_empty() => "No quality gate for this branch",
        _ => "**Quality gate: passed**",
    };
    out.push(format!(
        "### FastProof: {} #{} ({})",
        md(&s.launch.name),
        s.launch.number,
        md(&s.launch.status)
    ));
    out.push(String::new());
    out.push("| Total | Passed | Failed | Broken | Skipped | Flaky |".into());
    out.push("|---:|---:|---:|---:|---:|---:|".into());
    out.push(format!(
        "| {} | {} | {} | {} | {} | {} |",
        c.total, c.passed, c.failed, c.broken, c.skipped, c.flaky
    ));
    out.push(String::new());
    out.push(gate.into());
    for r in s.gate.results.iter().filter(|r| !r.ok).take(20) {
        out.push(format!(
            "- {}: `{:?} {:?} {}` actual {}",
            md(&r.gate),
            r.rule.metric,
            r.rule.op,
            r.expected,
            r.actual
                .map(|a| a.to_string())
                .unwrap_or_else(|| "n/a".into())
        ));
    }
    if s.new_failures.count > 0 {
        out.push(String::new());
        out.push(format!("New failures: {}", s.new_failures.count));
        for n in &s.new_failures.names {
            out.push(format!("- {}", md(n)));
        }
    }
    if let Some(u) = &s.launch.url {
        out.push(String::new());
        out.push(format!("[Open in FastProof]({u})"));
    }
    out.join("\n")
}

/// The repository path (`group/sub/app` or `owner/repo`) of a repository URL whose host fits
/// the configured API URL; it must be the pinned `vcs.repository`.
pub fn repo_path(settings: &VcsSettings, repository: &str) -> Result<String, String> {
    let api = url::Url::parse(&settings.url).map_err(|_| "vcs.url is not a URL".to_string())?;
    let repo = url::Url::parse(repository.trim())
        .map_err(|_| "the launch repository is not a URL".to_string())?;
    let (ah, rh) = (
        api.host_str().unwrap_or_default().to_ascii_lowercase(),
        repo.host_str().unwrap_or_default().to_ascii_lowercase(),
    );
    if ah != rh && ah != format!("api.{rh}") {
        return Err(format!(
            "the launch repository host {rh} does not belong to vcs.url"
        ));
    }
    let path = repo
        .path()
        .trim_matches('/')
        .trim_end_matches(".git")
        .to_string();
    let parts: Vec<&str> = path.split('/').collect();
    let ok = parts.len() >= 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        });
    if !ok || (settings.provider == "github" && parts.len() != 2) {
        return Err("the launch repository path is not a project path".into());
    }
    if !path.eq_ignore_ascii_case(settings.repository.trim_matches('/')) {
        return Err(format!(
            "the launch repository {} is not vcs.repository; nothing is posted",
            plain(&path, 200)
        ));
    }
    Ok(settings.repository.trim_matches('/').to_string())
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

#[derive(Deserialize)]
struct Note {
    id: u64,
    #[serde(default)]
    body: String,
}

struct Api<'a> {
    st: &'a AppState,
    settings: &'a VcsSettings,
    token: String,
}

impl Api<'_> {
    fn base(&self) -> String {
        let b = self.settings.url.trim_end_matches('/');
        if self.settings.provider == "gitlab" {
            format!("{b}/api/v4")
        } else {
            b.to_string()
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<super::egress::OutResponse, Outcome> {
        let url = format!("{}{path}", self.base());
        let mut rb = self
            .st
            .egress
            .request(method, &url)
            .map_err(Outcome::of_error)?;
        rb = rb.bearer_auth(&self.token);
        if self.settings.provider == "github" {
            rb = rb
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = self
            .st
            .egress
            .send_no_redirect(rb, MAX_RESPONSE_BYTES)
            .await
            .map_err(|e: OutboundError| Outcome::of_error(e))?;
        match Outcome::of_status(r.status) {
            Outcome::Delivered(_) => Ok(r),
            other => Err(other),
        }
    }

    async fn upsert_comment(
        &self,
        repo: &str,
        number: &str,
        body: &str,
        mark: &str,
    ) -> Result<(), Outcome> {
        let (list, item, one) = if self.settings.provider == "gitlab" {
            (
                format!("/projects/{}/merge_requests/{number}/notes", enc(repo)),
                format!("/projects/{}/merge_requests/{number}/notes/", enc(repo)),
                Method::PUT,
            )
        } else {
            (
                format!("/repos/{repo}/issues/{number}/comments"),
                format!("/repos/{repo}/issues/comments/"),
                Method::PATCH,
            )
        };
        let mut found = None;
        for page in 1..=MAX_COMMENT_PAGES {
            let sort = if self.settings.provider == "gitlab" {
                "&sort=asc&order_by=created_at"
            } else {
                ""
            };
            let r = self
                .call(
                    Method::GET,
                    &format!("{list}?per_page=100&page={page}{sort}"),
                    None,
                )
                .await?;
            let notes: Vec<Note> = r.json().map_err(|e| Outcome::Retry(e.to_string(), None))?;
            if let Some(n) = notes.iter().find(|n| n.body.contains(mark)) {
                found = Some(n.id);
                break;
            }
            if notes.len() < 100 {
                break;
            }
        }
        let payload = serde_json::json!({"body": body});
        match found {
            Some(id) => {
                self.call(one, &format!("{item}{id}"), Some(payload))
                    .await?
            }
            None => self.call(Method::POST, &list, Some(payload)).await?,
        };
        Ok(())
    }

    async fn status(&self, repo: &str, sha: &str, s: &Summary) -> Result<(), Outcome> {
        let c = &s.launch.counts;
        let gate = s.gate.status.as_str();
        let description = format!(
            "{} passed, {} failed, {} broken; gate {gate}",
            c.passed, c.failed, c.broken
        );
        let context = plain(&format!("fastproof/{}/{}", s.project, s.launch.name), 200);
        let interrupted = s.launch.status == "interrupted";
        let mut body = if self.settings.provider == "gitlab" {
            let state = match gate {
                "failed" => "failed",
                "pending" => "running",
                _ if interrupted => "canceled",
                _ => "success",
            };
            serde_json::json!({"state": state, "name": context, "description": description})
        } else {
            let state = match gate {
                "failed" => "failure",
                "pending" => "pending",
                _ if interrupted => "error",
                _ => "success",
            };
            serde_json::json!({"state": state, "context": context, "description": description})
        };
        if let Some(u) = &s.launch.url {
            body["target_url"] = serde_json::json!(u);
        }
        let path = if self.settings.provider == "gitlab" {
            format!("/projects/{}/statuses/{sha}", enc(repo))
        } else {
            format!("/repos/{repo}/statuses/{sha}")
        };
        self.call(Method::POST, &path, Some(body)).await?;
        Ok(())
    }
}

/// Whether a launch has anything to post (a merge request number or a commit) and its branch
/// is one of `vcs.sourceBranches`.
pub fn applies(settings: &VcsSettings, s: &Summary) -> bool {
    let mr = settings.comment && mr_number(s).is_some();
    let st = settings.status && commit(s).is_some();
    s.launch.repository.is_some()
        && (mr || st)
        && super::settings::matches_any(&settings.source_branches, s.launch.branch.as_deref())
}

fn mr_number(s: &Summary) -> Option<&str> {
    s.launch
        .merge_request
        .as_deref()
        .filter(|n| !n.is_empty() && n.len() <= 12 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn commit(s: &Summary) -> Option<&str> {
    s.launch
        .commit
        .as_deref()
        .filter(|c| (7..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Posts or updates the comment and the commit status.
pub async fn deliver(
    st: &AppState,
    project_id: uuid::Uuid,
    settings: &VcsSettings,
    s: &Summary,
) -> Outcome {
    let Some(repository) = s.launch.repository.as_deref() else {
        return Outcome::Fail("the launch has no repository".into(), None);
    };
    let repo = match repo_path(settings, repository) {
        Ok(r) => r,
        Err(e) => return Outcome::Fail(e, None),
    };
    if !super::settings::matches_any(&settings.source_branches, s.launch.branch.as_deref()) {
        return Outcome::Fail(
            "the launch branch is not in vcs.sourceBranches".into(),
            None,
        );
    }
    let token = match st
        .project_secret(
            project_id,
            &settings.token_secret_ref,
            SecretPurpose::Vcs,
            Some(&settings.url),
        )
        .await
    {
        Ok(t) => t.expose().trim().to_string(),
        Err(e) => return Outcome::Fail(e.to_string(), None),
    };
    let api = Api {
        st,
        settings,
        token,
    };
    if settings.comment
        && let Some(n) = mr_number(s)
    {
        let body = comment_body(s);
        let mark = marker(&s.project, &s.launch.name);
        if let Err(o) = api.upsert_comment(&repo, n, &body, &mark).await {
            return o;
        }
    }
    if settings.status
        && let Some(sha) = commit(s)
        && let Err(o) = api.status(&repo, sha, s).await
    {
        return o;
    }
    Outcome::Delivered(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gl() -> VcsSettings {
        VcsSettings {
            provider: "gitlab".into(),
            url: "https://gitlab.example.com".into(),
            repository: "group/sub/app".into(),
            source_branches: None,
            token_secret_ref: "t".into(),
            comment: true,
            status: true,
        }
    }

    #[test]
    fn markers_and_paths() {
        assert_eq!(
            marker("shop", "app #7 --> <b>"),
            "<!-- fastproof:shop:app #7 -   b -->"
        );
        assert_eq!(
            repo_path(&gl(), "https://gitlab.example.com/group/sub/app.git").unwrap(),
            "group/sub/app"
        );
        assert_eq!(
            repo_path(&gl(), "https://gitlab.example.com/Group/Sub/App").unwrap(),
            "group/sub/app"
        );
        assert!(repo_path(&gl(), "https://evil.example.com/group/sub/app").is_err());
        assert!(repo_path(&gl(), "https://gitlab.example.com/app").is_err());
        assert!(repo_path(&gl(), "https://gitlab.example.com/a/../b").is_err());
        let other = repo_path(&gl(), "https://gitlab.example.com/victim/secret-app").unwrap_err();
        assert!(other.contains("is not vcs.repository"), "{other}");
        let mut gh = gl();
        gh.provider = "github".into();
        gh.url = "https://api.github.com".into();
        gh.repository = "org/app".into();
        assert_eq!(
            repo_path(&gh, "https://github.com/org/app").unwrap(),
            "org/app"
        );
        assert!(repo_path(&gh, "https://github.com/org/app/extra").is_err());
        assert_eq!(enc("group/app"), "group%2Fapp");
    }

    #[test]
    fn launch_text_is_inert_markdown() {
        let evil = "x [click](https://evil.example.com/a) ![p](https://evil.example.com/t.png) \
                    @all <img src=x onerror=alert(1)> **b** `c` #1 !2 www.evil.example.com &amp;\n| d |";
        let m = md(evil);
        for bad in [
            "[click](", "![", "](", "@all", "<img", "**", "`c`", "https://", "www.evil", "#1",
            "!2", "\n",
        ] {
            assert!(!m.contains(bad), "{bad} survived in {m}");
        }
        assert!(m.contains("&lt;img"), "{m}");
        assert!(
            m.contains("&amp;amp\\;"),
            "an entity in the text stays text: {m}"
        );
        assert!(m.contains("\\|"), "{m}");
        assert_eq!(md("checkout_pays-ok"), "checkout\\_pays\\-ok");
        assert_eq!(plain("a\nb\u{1b}c", 3), "a b");
    }
}
