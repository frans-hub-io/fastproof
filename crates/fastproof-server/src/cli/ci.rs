//! CI metadata from the environment of GitLab CI, GitHub Actions and Jenkins.

use fastproof_store::{LaunchCi, LaunchVcs};

/// What the CI tells about the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CiMeta {
    /// Shared by every job of one pipeline, so parallel shards join one launch.
    pub external_id: Option<String>,
    pub name: Option<String>,
    pub ci: LaunchCi,
    pub vcs: LaunchVcs,
}

/// Detects the CI; `None` outside of the supported ones.
pub fn detect(get: &dyn Fn(&str) -> Option<String>) -> Option<CiMeta> {
    let v = |k: &str| get(k).filter(|s| !s.trim().is_empty());
    if v("GITLAB_CI").is_some() {
        let pipeline = v("CI_PIPELINE_ID");
        return Some(CiMeta {
            external_id: pipeline
                .as_ref()
                .map(|p| format!("gitlab-{}-{p}", v("CI_PROJECT_ID").unwrap_or_default())),
            name: v("CI_PROJECT_PATH").map(|p| match v("CI_PIPELINE_IID") {
                Some(n) => format!("{p} #{n}"),
                None => p,
            }),
            ci: LaunchCi {
                provider: Some("gitlab".into()),
                pipeline_id: pipeline,
                job_id: v("CI_JOB_ID"),
                url: v("CI_PIPELINE_URL").or(v("CI_JOB_URL")),
                build_order: v("CI_PIPELINE_IID").and_then(|n| n.parse().ok()),
                merge_request: v("CI_MERGE_REQUEST_IID"),
            },
            vcs: LaunchVcs {
                repository: v("CI_PROJECT_URL"),
                branch: v("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME").or(v("CI_COMMIT_REF_NAME")),
                commit: v("CI_COMMIT_SHA"),
                commit_url: None,
            },
        });
    }
    if v("GITHUB_ACTIONS").is_some() {
        let run = v("GITHUB_RUN_ID");
        let repo = v("GITHUB_REPOSITORY");
        let server = v("GITHUB_SERVER_URL").unwrap_or_else(|| "https://github.com".into());
        return Some(CiMeta {
            external_id: run.as_ref().map(|r| {
                format!(
                    "github-{}-{r}-{}",
                    repo.clone().unwrap_or_default(),
                    v("GITHUB_RUN_ATTEMPT").unwrap_or_else(|| "1".into())
                )
            }),
            name: v("GITHUB_WORKFLOW").map(|w| match v("GITHUB_RUN_NUMBER") {
                Some(n) => format!("{w} #{n}"),
                None => w,
            }),
            ci: LaunchCi {
                provider: Some("github".into()),
                pipeline_id: run.clone(),
                job_id: v("GITHUB_JOB"),
                url: match (&repo, &run) {
                    (Some(r), Some(id)) => Some(format!("{server}/{r}/actions/runs/{id}")),
                    _ => None,
                },
                build_order: v("GITHUB_RUN_NUMBER").and_then(|n| n.parse().ok()),
                merge_request: v("GITHUB_REF").and_then(|r| github_pr(&r)),
            },
            vcs: LaunchVcs {
                repository: repo.map(|r| format!("{server}/{r}")),
                branch: v("GITHUB_HEAD_REF").or(v("GITHUB_REF_NAME")),
                commit: v("GITHUB_SHA"),
                commit_url: None,
            },
        });
    }
    if v("JENKINS_URL").is_some() {
        let job = v("JOB_NAME");
        let build = v("BUILD_NUMBER");
        return Some(CiMeta {
            external_id: match (&job, &build) {
                (Some(j), Some(b)) => Some(format!("jenkins-{j}-{b}")),
                _ => None,
            },
            name: match (&job, &build) {
                (Some(j), Some(b)) => Some(format!("{j} #{b}")),
                (Some(j), None) => Some(j.clone()),
                _ => None,
            },
            ci: LaunchCi {
                provider: Some("jenkins".into()),
                pipeline_id: build.clone(),
                job_id: job,
                url: v("BUILD_URL"),
                build_order: build.and_then(|n| n.parse().ok()),
                merge_request: v("CHANGE_ID"),
            },
            vcs: LaunchVcs {
                repository: v("GIT_URL"),
                branch: v("BRANCH_NAME")
                    .or(v("GIT_BRANCH").map(|b| b.trim_start_matches("origin/").to_string())),
                commit: v("GIT_COMMIT"),
                commit_url: None,
            },
        });
    }
    None
}

/// The pull request number of `refs/pull/<n>/merge` (or `/head`).
fn github_pr(r: &str) -> Option<String> {
    let n = r.strip_prefix("refs/pull/")?.split('/').next()?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(vars: &[(&str, &str)]) -> HashMap<String, String> {
        vars.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn gitlab_github_jenkins() {
        let m = env(&[
            ("GITLAB_CI", "true"),
            ("CI_PROJECT_ID", "42"),
            ("CI_PIPELINE_ID", "1001"),
            ("CI_PIPELINE_IID", "7"),
            ("CI_JOB_ID", "555"),
            ("CI_PROJECT_PATH", "group/app"),
            (
                "CI_PIPELINE_URL",
                "https://gitlab.example.com/group/app/-/pipelines/1001",
            ),
            ("CI_COMMIT_REF_NAME", "main"),
            ("CI_COMMIT_SHA", "abc"),
        ]);
        let c = detect(&|k| m.get(k).cloned()).unwrap();
        assert_eq!(c.external_id.as_deref(), Some("gitlab-42-1001"));
        assert_eq!(c.name.as_deref(), Some("group/app #7"));
        assert_eq!(c.ci.build_order, Some(7));
        assert_eq!(c.vcs.branch.as_deref(), Some("main"));

        let m = env(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_RUN_ID", "99"),
            ("GITHUB_RUN_ATTEMPT", "2"),
            ("GITHUB_REPOSITORY", "org/app"),
            ("GITHUB_WORKFLOW", "ci"),
            ("GITHUB_RUN_NUMBER", "12"),
            ("GITHUB_REF_NAME", "feature-x"),
            ("GITHUB_SHA", "def"),
        ]);
        let c = detect(&|k| m.get(k).cloned()).unwrap();
        assert_eq!(c.external_id.as_deref(), Some("github-org/app-99-2"));
        assert_eq!(
            c.ci.url.as_deref(),
            Some("https://github.com/org/app/actions/runs/99")
        );
        assert_eq!(c.vcs.branch.as_deref(), Some("feature-x"));
        assert_eq!(c.ci.merge_request, None);
        let m = env(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REF", "refs/pull/17/merge"),
        ]);
        assert_eq!(
            detect(&|k| m.get(k).cloned())
                .unwrap()
                .ci
                .merge_request
                .as_deref(),
            Some("17")
        );
        let m = env(&[("GITLAB_CI", "true"), ("CI_MERGE_REQUEST_IID", "5")]);
        assert_eq!(
            detect(&|k| m.get(k).cloned())
                .unwrap()
                .ci
                .merge_request
                .as_deref(),
            Some("5")
        );

        let m = env(&[
            ("JENKINS_URL", "https://ci.example.com/"),
            ("JOB_NAME", "app/main"),
            ("BUILD_NUMBER", "31"),
            ("GIT_BRANCH", "origin/main"),
        ]);
        let c = detect(&|k| m.get(k).cloned()).unwrap();
        assert_eq!(c.external_id.as_deref(), Some("jenkins-app/main-31"));
        assert_eq!(c.vcs.branch.as_deref(), Some("main"));
        assert!(detect(&|_| None).is_none());
    }
}
