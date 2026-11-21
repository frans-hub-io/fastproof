import type { LaunchStart } from './types';

/** CI metadata; the same rules as the `fastproof` CLI, so shards and the CLI join one launch. */
export interface CiMeta {
  externalId?: string;
  name?: string;
  ci: NonNullable<LaunchStart['ci']>;
  vcs: NonNullable<LaunchStart['vcs']>;
}

type Env = Record<string, string | undefined>;

function clean<T extends object>(o: T): T {
  for (const k of Object.keys(o) as (keyof T)[]) if (o[k] === undefined) delete o[k];
  return o;
}

export function detectCi(env: Env = process.env): CiMeta | undefined {
  const v = (k: string) => {
    const s = env[k];
    return s && s.trim() ? s : undefined;
  };
  const num = (s: string | undefined) => (s && /^\d+$/.test(s) ? Number(s) : undefined);
  if (v('GITLAB_CI')) {
    const pipeline = v('CI_PIPELINE_ID');
    const path = v('CI_PROJECT_PATH');
    const iid = v('CI_PIPELINE_IID');
    return {
      externalId: pipeline ? `gitlab-${v('CI_PROJECT_ID') ?? ''}-${pipeline}` : undefined,
      name: path ? (iid ? `${path} #${iid}` : path) : undefined,
      ci: clean({
        provider: 'gitlab',
        pipelineId: pipeline,
        jobId: v('CI_JOB_ID'),
        url: v('CI_PIPELINE_URL') ?? v('CI_JOB_URL'),
        buildOrder: num(iid),
      }),
      vcs: clean({
        repository: v('CI_PROJECT_URL'),
        branch: v('CI_MERGE_REQUEST_SOURCE_BRANCH_NAME') ?? v('CI_COMMIT_REF_NAME'),
        commit: v('CI_COMMIT_SHA'),
      }),
    };
  }
  if (v('GITHUB_ACTIONS')) {
    const run = v('GITHUB_RUN_ID');
    const repo = v('GITHUB_REPOSITORY');
    const server = v('GITHUB_SERVER_URL') ?? 'https://github.com';
    const workflow = v('GITHUB_WORKFLOW');
    const number = v('GITHUB_RUN_NUMBER');
    return {
      externalId: run ? `github-${repo ?? ''}-${run}-${v('GITHUB_RUN_ATTEMPT') ?? '1'}` : undefined,
      name: workflow ? (number ? `${workflow} #${number}` : workflow) : undefined,
      ci: clean({
        provider: 'github',
        pipelineId: run,
        jobId: v('GITHUB_JOB'),
        url: repo && run ? `${server}/${repo}/actions/runs/${run}` : undefined,
        buildOrder: num(number),
      }),
      vcs: clean({
        repository: repo ? `${server}/${repo}` : undefined,
        branch: v('GITHUB_HEAD_REF') ?? v('GITHUB_REF_NAME'),
        commit: v('GITHUB_SHA'),
      }),
    };
  }
  if (v('JENKINS_URL')) {
    const job = v('JOB_NAME');
    const build = v('BUILD_NUMBER');
    return {
      externalId: job && build ? `jenkins-${job}-${build}` : undefined,
      name: job ? (build ? `${job} #${build}` : job) : undefined,
      ci: clean({ provider: 'jenkins', pipelineId: build, jobId: job, url: v('BUILD_URL'), buildOrder: num(build) }),
      vcs: clean({
        repository: v('GIT_URL'),
        branch: v('BRANCH_NAME') ?? v('GIT_BRANCH')?.replace(/^origin\//, ''),
        commit: v('GIT_COMMIT'),
      }),
    };
  }
  return undefined;
}
