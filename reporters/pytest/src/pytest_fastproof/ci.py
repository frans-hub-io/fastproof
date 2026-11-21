"""CI metadata: the same rules as the ``fastproof`` CLI, so shards and the CLI join one launch."""

from __future__ import annotations

import os
from typing import Any, Dict, Mapping, Optional


def _clean(d: Dict[str, Any]) -> Dict[str, Any]:
    return {k: v for k, v in d.items() if v is not None}


def detect_ci(env: Optional[Mapping[str, str]] = None) -> Optional[Dict[str, Any]]:
    env = os.environ if env is None else env

    def v(k: str) -> Optional[str]:
        s = env.get(k)
        return s if s and s.strip() else None

    def num(s: Optional[str]) -> Optional[int]:
        return int(s) if s and s.isdigit() else None

    if v("GITLAB_CI"):
        pipeline, path, iid = v("CI_PIPELINE_ID"), v("CI_PROJECT_PATH"), v("CI_PIPELINE_IID")
        return {
            "externalId": f"gitlab-{v('CI_PROJECT_ID') or ''}-{pipeline}" if pipeline else None,
            "name": (f"{path} #{iid}" if iid else path) if path else None,
            "ci": _clean(
                {
                    "provider": "gitlab",
                    "pipelineId": pipeline,
                    "jobId": v("CI_JOB_ID"),
                    "url": v("CI_PIPELINE_URL") or v("CI_JOB_URL"),
                    "buildOrder": num(iid),
                }
            ),
            "vcs": _clean(
                {
                    "repository": v("CI_PROJECT_URL"),
                    "branch": v("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME") or v("CI_COMMIT_REF_NAME"),
                    "commit": v("CI_COMMIT_SHA"),
                }
            ),
        }
    if v("GITHUB_ACTIONS"):
        run, repo = v("GITHUB_RUN_ID"), v("GITHUB_REPOSITORY")
        server = v("GITHUB_SERVER_URL") or "https://github.com"
        workflow, number = v("GITHUB_WORKFLOW"), v("GITHUB_RUN_NUMBER")
        return {
            "externalId": f"github-{repo or ''}-{run}-{v('GITHUB_RUN_ATTEMPT') or '1'}" if run else None,
            "name": (f"{workflow} #{number}" if number else workflow) if workflow else None,
            "ci": _clean(
                {
                    "provider": "github",
                    "pipelineId": run,
                    "jobId": v("GITHUB_JOB"),
                    "url": f"{server}/{repo}/actions/runs/{run}" if repo and run else None,
                    "buildOrder": num(number),
                }
            ),
            "vcs": _clean(
                {
                    "repository": f"{server}/{repo}" if repo else None,
                    "branch": v("GITHUB_HEAD_REF") or v("GITHUB_REF_NAME"),
                    "commit": v("GITHUB_SHA"),
                }
            ),
        }
    if v("JENKINS_URL"):
        job, build = v("JOB_NAME"), v("BUILD_NUMBER")
        branch = v("BRANCH_NAME") or v("GIT_BRANCH")
        if branch and branch.startswith("origin/") and not v("BRANCH_NAME"):
            branch = branch[len("origin/") :]
        return {
            "externalId": f"jenkins-{job}-{build}" if job and build else None,
            "name": (f"{job} #{build}" if build else job) if job else None,
            "ci": _clean(
                {"provider": "jenkins", "pipelineId": build, "jobId": job, "url": v("BUILD_URL"), "buildOrder": num(build)}
            ),
            "vcs": _clean({"repository": v("GIT_URL"), "branch": branch, "commit": v("GIT_COMMIT")}),
        }
    return None
