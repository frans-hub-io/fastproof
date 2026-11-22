import type { CaseFilter, CaseQuery, CaseStatus } from '../../api/tm';

export const CASE_STATUSES: readonly CaseStatus[] = ['draft', 'active', 'outdated'];

export const CASE_STATUS_LABEL: Record<CaseStatus, string> = { draft: 'Draft', active: 'Active', outdated: 'Outdated' };

/** Case list state kept in the URL. */
export interface CaseListState {
  folder: string | null;
  recursive: boolean;
  q: string;
  statuses: CaseStatus[];
  automation: 'manual' | 'automated' | null;
  fields: string[];
  tags: string[];
  staleDays: number | null;
  aiDraft: boolean;
  archived: boolean;
}

export function readListState(sp: URLSearchParams): CaseListState {
  const automation = sp.get('automation');
  const stale = Number(sp.get('staleDays'));
  return {
    folder: sp.get('folder'),
    recursive: sp.get('recursive') !== 'false',
    q: sp.get('q') ?? '',
    statuses: (sp.get('status') ?? '').split(',').filter((s): s is CaseStatus => (CASE_STATUSES as readonly string[]).includes(s)),
    automation: automation === 'manual' || automation === 'automated' ? automation : null,
    fields: sp.getAll('field').filter((f) => f.includes(':')),
    tags: sp.getAll('tag').filter(Boolean),
    staleDays: Number.isFinite(stale) && stale > 0 ? Math.floor(stale) : null,
    aiDraft: sp.get('aiUnreviewed') === 'true',
    archived: sp.get('archived') === 'true',
  };
}

export function writeListState(prev: URLSearchParams, s: Partial<CaseListState>): URLSearchParams {
  const next = new URLSearchParams(prev);
  const set = (k: string, v: string | null) => (v ? next.set(k, v) : next.delete(k));
  if ('folder' in s) set('folder', s.folder ?? null);
  if ('recursive' in s) set('recursive', s.recursive === false ? 'false' : null);
  if ('q' in s) set('q', s.q ?? null);
  if ('statuses' in s) set('status', s.statuses?.join(',') ?? null);
  if ('automation' in s) set('automation', s.automation ?? null);
  if ('fields' in s) {
    next.delete('field');
    for (const f of s.fields ?? []) next.append('field', f);
  }
  if ('tags' in s) {
    next.delete('tag');
    for (const t of s.tags ?? []) next.append('tag', t);
  }
  if ('staleDays' in s) set('staleDays', s.staleDays ? String(s.staleDays) : null);
  if ('aiDraft' in s) set('aiUnreviewed', s.aiDraft ? 'true' : null);
  if ('archived' in s) set('archived', s.archived ? 'true' : null);
  return next;
}

export function toCaseQuery(s: CaseListState): CaseQuery {
  return {
    folder: s.folder ?? undefined,
    recursive: s.folder ? s.recursive : undefined,
    q: s.q || undefined,
    status: s.statuses.length ? s.statuses.join(',') : undefined,
    automation: s.automation ?? undefined,
    field: s.fields.length ? s.fields : undefined,
    tag: s.tags.length ? s.tags : undefined,
    staleDays: s.staleDays ?? undefined,
    aiUnreviewed: s.aiDraft || undefined,
    archived: s.archived || undefined,
  };
}

/** The structured filter a plan saves (the same parameters as the case list). */
export function toCaseFilter(s: CaseListState): CaseFilter {
  const fields: Record<string, string[]> = {};
  for (const f of s.fields) {
    const i = f.indexOf(':');
    const k = f.slice(0, i);
    (fields[k] ??= []).push(f.slice(i + 1));
  }
  const out: CaseFilter = {};
  if (s.folder) {
    out.folderIds = [s.folder];
    out.recursive = s.recursive;
  }
  if (s.statuses.length) out.statuses = s.statuses;
  if (s.automation) out.automation = s.automation;
  if (Object.keys(fields).length) out.fields = fields;
  if (s.tags.length) out.tags = s.tags;
  if (s.q) out.q = s.q;
  return out;
}

export function describeFilter(f: CaseFilter | null, folderName: (id: string) => string): string {
  if (!f) return 'No filter';
  const parts: string[] = [];
  if (f.folderIds?.length) parts.push(`in ${f.folderIds.map(folderName).join(', ')}${f.recursive === false ? '' : ' and below'}`);
  if (f.statuses?.length) parts.push(`status ${f.statuses.join(' or ')}`);
  if (f.automation) parts.push(f.automation);
  for (const [k, v] of Object.entries(f.fields ?? {})) parts.push(`${k} = ${v.join(' or ')}`);
  if (f.tags?.length) parts.push(`tags ${f.tags.join(', ')}`);
  if (f.q) parts.push(`matching “${f.q}”`);
  return parts.length ? parts.join('; ') : 'All cases';
}

export function caseLabel(n: number): string {
  return `TC-${String(n)}`;
}

/** `TC-12`, `tc12`, `#12` or `12` → 12. */
export function parseCaseNumber(text: string): number | null {
  const m = /^\s*(?:tc-?|#)?(\d{1,9})\s*$/i.exec(text);
  return m ? Number(m[1]) : null;
}

export type TreeMode = 'folder' | 'behavior' | 'suite';

/** Field filters that select a behavior or suite node (`epic:…`, `feature:…`, `story:…`, `suite:…`). */
export function nodeFields(mode: TreeMode, id: string): string[] {
  const levels = mode === 'behavior' ? ['epic', 'feature', 'story'] : ['suite'];
  return id.split('\u001f').map((v, i) => `${levels[i] ?? 'story'}:${v}`);
}
