import type { CaseFolder, Person, RunStatus, StepStatus } from '../../api/tm';

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

export interface DefectInfo {
  label: string;
  comment: string;
  issue: string;
  url: string | null;
  by: string;
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

/** Reads the defect of a run item: a triage defect (`name`, `issueKey`, `issueUrl`) or a quick defect (`issue: {url, key}`). */
export function defectInfo(d: unknown): DefectInfo | null {
  if (!d || typeof d !== 'object') return null;
  const o = d as Record<string, unknown>;
  const issue = o.issue && typeof o.issue === 'object' ? (o.issue as Record<string, unknown>) : {};
  const url = str(o.issueUrl) || str(issue.url);
  return {
    label: str(o.name) || str(o.group) || 'Defect',
    comment: str(o.comment),
    issue: str(o.issueKey) || str(issue.key) || url,
    url: /^https?:\/\//i.test(url) ? url : null,
    by: str(o.by),
  };
}

/** Who saved a revision: the principal, or "you (another tab)" when it was the signed-in user. */
export function actorLabel(by: string | null | undefined, me: string | undefined): string {
  if (!by) return 'someone';
  return me && by === me ? 'you (another tab)' : by;
}

/** Whether `name` is one of the people the server knows for the project (exact principal match). */
export function isKnownPerson(people: readonly Person[] | undefined, name: string): boolean {
  const n = name.trim();
  return !n || !!people?.some((p) => p.principal === n);
}

/** Markdown source as plain text for print: emphasis and code marks go, links keep their text and URL, images their alt text. */
export function markdownText(md: string): string {
  return md
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '$1 ($2)')
    .replace(/(\*\*|__)(.+?)\1/g, '$2')
    .replace(/(^|[^*\w])\*(?!\s)([^*]+?)\*(?!\w)/g, '$1$2')
    .replace(/`([^`]+)`/g, '$1');
}
