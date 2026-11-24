package io.franshub.fastproof;

import java.util.LinkedHashMap;
import java.util.Map;

/** CI metadata; the same rules as the {@code fastproof} CLI and the other reporters, so shards join one launch. */
final class Ci {
    String externalId;
    String name;
    final Map<String, Object> ci = new LinkedHashMap<>();
    final Map<String, Object> vcs = new LinkedHashMap<>();

    private static String v(final Map<String, String> env, final String k) {
        final String s = env.get(k);
        return s == null || s.isBlank() ? null : s;
    }

    private static void put(final Map<String, Object> m, final String k, final Object v) {
        if (v != null) {
            m.put(k, v);
        }
    }

    private static Integer num(final String s) {
        return s != null && s.matches("\\d{1,9}") ? Integer.valueOf(s) : null;
    }

    private static String first(final String a, final String b) {
        return a != null ? a : b;
    }

    static Ci detect(final Map<String, String> env) {
        final Ci c = new Ci();
        if (v(env, "GITLAB_CI") != null) {
            final String pipeline = v(env, "CI_PIPELINE_ID");
            final String path = v(env, "CI_PROJECT_PATH");
            final String iid = v(env, "CI_PIPELINE_IID");
            c.externalId = pipeline == null ? null : "gitlab-" + first(v(env, "CI_PROJECT_ID"), "") + "-" + pipeline;
            c.name = path == null ? null : iid == null ? path : path + " #" + iid;
            put(c.ci, "provider", "gitlab");
            put(c.ci, "pipelineId", pipeline);
            put(c.ci, "jobId", v(env, "CI_JOB_ID"));
            put(c.ci, "url", first(v(env, "CI_PIPELINE_URL"), v(env, "CI_JOB_URL")));
            put(c.ci, "buildOrder", num(iid));
            put(c.vcs, "repository", v(env, "CI_PROJECT_URL"));
            put(c.vcs, "branch", first(v(env, "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME"), v(env, "CI_COMMIT_REF_NAME")));
            put(c.vcs, "commit", v(env, "CI_COMMIT_SHA"));
            return c;
        }
        if (v(env, "GITHUB_ACTIONS") != null) {
            final String run = v(env, "GITHUB_RUN_ID");
            final String repo = v(env, "GITHUB_REPOSITORY");
            final String server = first(v(env, "GITHUB_SERVER_URL"), "https://github.com");
            final String workflow = v(env, "GITHUB_WORKFLOW");
            final String number = v(env, "GITHUB_RUN_NUMBER");
            c.externalId = run == null ? null : "github-" + first(repo, "") + "-" + run + "-" + first(v(env, "GITHUB_RUN_ATTEMPT"), "1");
            c.name = workflow == null ? null : number == null ? workflow : workflow + " #" + number;
            put(c.ci, "provider", "github");
            put(c.ci, "pipelineId", run);
            put(c.ci, "jobId", v(env, "GITHUB_JOB"));
            put(c.ci, "url", repo != null && run != null ? server + "/" + repo + "/actions/runs/" + run : null);
            put(c.ci, "buildOrder", num(number));
            put(c.vcs, "repository", repo == null ? null : server + "/" + repo);
            put(c.vcs, "branch", first(v(env, "GITHUB_HEAD_REF"), v(env, "GITHUB_REF_NAME")));
            put(c.vcs, "commit", v(env, "GITHUB_SHA"));
            return c;
        }
        if (v(env, "JENKINS_URL") != null) {
            final String job = v(env, "JOB_NAME");
            final String build = v(env, "BUILD_NUMBER");
            c.externalId = job != null && build != null ? "jenkins-" + job + "-" + build : null;
            c.name = job == null ? null : build == null ? job : job + " #" + build;
            put(c.ci, "provider", "jenkins");
            put(c.ci, "pipelineId", build);
            put(c.ci, "jobId", job);
            put(c.ci, "url", v(env, "BUILD_URL"));
            put(c.ci, "buildOrder", num(build));
            put(c.vcs, "repository", v(env, "GIT_URL"));
            final String gb = v(env, "GIT_BRANCH");
            put(c.vcs, "branch", first(v(env, "BRANCH_NAME"), gb == null ? null : gb.replaceFirst("^origin/", "")));
            put(c.vcs, "commit", v(env, "GIT_COMMIT"));
            return c;
        }
        return null;
    }
}
