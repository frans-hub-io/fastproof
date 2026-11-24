using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;

namespace FransHub.FastProof;

internal sealed class CiInfo
{
    public string? ExternalId;
    public string? Name;
    public JsonObject? Ci;
    public JsonObject? Vcs;
}

internal static class CiDetector
{
    public static CiInfo? Detect(IReadOnlyDictionary<string, string> env)
    {
        string? V(string k) => env.TryGetValue(k, out var s) && !string.IsNullOrWhiteSpace(s) ? s : null;
        long? Num(string? s) => s != null && s.Length > 0 && s.All(char.IsAsciiDigit) && long.TryParse(s, out var n) ? n : null;

        if (V("GITLAB_CI") != null)
        {
            var pipeline = V("CI_PIPELINE_ID");
            var path = V("CI_PROJECT_PATH");
            var iid = V("CI_PIPELINE_IID");
            return new CiInfo
            {
                ExternalId = pipeline != null ? $"gitlab-{V("CI_PROJECT_ID") ?? ""}-{pipeline}" : null,
                Name = path == null ? null : iid != null ? $"{path} #{iid}" : path,
                Ci = Clean(("provider", "gitlab"), ("pipelineId", pipeline), ("jobId", V("CI_JOB_ID")),
                    ("url", V("CI_PIPELINE_URL") ?? V("CI_JOB_URL")), ("buildOrder", Num(iid))),
                Vcs = Clean(("repository", V("CI_PROJECT_URL")),
                    ("branch", V("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME") ?? V("CI_COMMIT_REF_NAME")),
                    ("commit", V("CI_COMMIT_SHA"))),
            };
        }
        if (V("GITHUB_ACTIONS") != null)
        {
            var run = V("GITHUB_RUN_ID");
            var repo = V("GITHUB_REPOSITORY");
            var server = V("GITHUB_SERVER_URL") ?? "https://github.com";
            var workflow = V("GITHUB_WORKFLOW");
            var number = V("GITHUB_RUN_NUMBER");
            return new CiInfo
            {
                ExternalId = run != null ? $"github-{repo ?? ""}-{run}-{V("GITHUB_RUN_ATTEMPT") ?? "1"}" : null,
                Name = workflow == null ? null : number != null ? $"{workflow} #{number}" : workflow,
                Ci = Clean(("provider", "github"), ("pipelineId", run), ("jobId", V("GITHUB_JOB")),
                    ("url", repo != null && run != null ? $"{server}/{repo}/actions/runs/{run}" : null),
                    ("buildOrder", Num(number))),
                Vcs = Clean(("repository", repo != null ? $"{server}/{repo}" : null),
                    ("branch", V("GITHUB_HEAD_REF") ?? V("GITHUB_REF_NAME")), ("commit", V("GITHUB_SHA"))),
            };
        }
        if (V("JENKINS_URL") != null)
        {
            var job = V("JOB_NAME");
            var build = V("BUILD_NUMBER");
            var branch = V("BRANCH_NAME") ?? V("GIT_BRANCH");
            if (branch != null && V("BRANCH_NAME") == null && branch.StartsWith("origin/", StringComparison.Ordinal))
            {
                branch = branch.Substring("origin/".Length);
            }
            return new CiInfo
            {
                ExternalId = job != null && build != null ? $"jenkins-{job}-{build}" : null,
                Name = job == null ? null : build != null ? $"{job} #{build}" : job,
                Ci = Clean(("provider", "jenkins"), ("pipelineId", build), ("jobId", job), ("url", V("BUILD_URL")),
                    ("buildOrder", Num(build))),
                Vcs = Clean(("repository", V("GIT_URL")), ("branch", branch), ("commit", V("GIT_COMMIT"))),
            };
        }
        return null;
    }

    private static JsonObject Clean(params (string Key, object? Value)[] pairs)
    {
        var o = new JsonObject();
        foreach (var (k, v) in pairs)
        {
            switch (v)
            {
                case null: break;
                case string s: o[k] = s; break;
                case long l: o[k] = l; break;
            }
        }
        return o;
    }
}
