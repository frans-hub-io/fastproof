import type { LaunchCi, LaunchVcs } from '../api/types';
import { isHttpUrl } from './format';

export function commitUrl(vcs: LaunchVcs | null | undefined, ci: LaunchCi | null | undefined): string | null {
  if (!vcs?.commit) return null;
  if (isHttpUrl(vcs.commitUrl)) return vcs.commitUrl;
  if (!isHttpUrl(vcs.repository)) return null;
  const repo = vcs.repository.replace(/\.git$/, '').replace(/\/$/, '');
  const provider = (ci?.provider ?? '').toLowerCase();
  if (provider.includes('gitlab') || /gitlab/i.test(repo)) return `${repo}/-/commit/${encodeURIComponent(vcs.commit)}`;
  return `${repo}/commit/${encodeURIComponent(vcs.commit)}`;
}

export function repoLabel(repository: string | null | undefined): string {
  if (!repository) return '';
  if (!isHttpUrl(repository)) return repository;
  return new URL(repository).pathname.replace(/^\//, '').replace(/\.git$/, '');
}

export function ciLabel(ci: LaunchCi): string {
  const p = ci.provider || 'CI';
  const pipe = ci.pipelineId ? `pipeline ${ci.pipelineId}` : '';
  const job = ci.jobId ? `job ${ci.jobId}` : '';
  return [p, pipe, job].filter(Boolean).join(' · ');
}
