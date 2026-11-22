import type { CaseFolder, RunStatus, StepStatus } from '../../api/tm';

const UUID = /^[0-9a-zA-Z-]{1,64}$/;

export function runStatusVar(s: RunStatus | StepStatus): string {
  switch (s) {
    case 'passed':
      return 'var(--fp-tm-passed)';
    case 'failed':
      return 'var(--fp-tm-failed)';
    case 'blocked':
      return 'var(--fp-tm-blocked)';
    case 'skipped':
      return 'var(--fp-tm-skipped)';
    case 'in_progress':
      return 'var(--fp-tm-progress)';
    default:
      return 'var(--fp-tm-untested)';
  }
}

export function caseLink(project: string, n: number): string {
  return `/p/${project}/cases/${String(n)}`;
}

export function folderPathLabel(folders: readonly CaseFolder[] | undefined, id: string | null): string {
  if (!id || !folders) return 'No folder';
  const byId = new Map(folders.map((f) => [f.id, f] as const));
  const parts: string[] = [];
  let cur = byId.get(id);
  for (let i = 0; cur && i < 20; i++) {
    parts.unshift(cur.name);
    cur = cur.parentId ? byId.get(cur.parentId) : undefined;
  }
  return parts.join(' / ') || 'No folder';
}

/** The attachment id of an image source the server rendered: `attachment:<id>` or this project's content URL. */
export function attachmentIdOf(src: string | null, project: string): string | null {
  if (!src) return null;
  if (src.startsWith('attachment:')) {
    const id = src.slice('attachment:'.length);
    return UUID.test(id) ? id : null;
  }
  const prefix = `/api/v1/projects/${encodeURIComponent(project)}/attachments/`;
  if (src.startsWith(prefix) && src.endsWith('/content')) {
    const id = src.slice(prefix.length, -'/content'.length);
    return UUID.test(id) ? id : null;
  }
  return null;
}
