import type { CaseSnapshot, CaseStatus } from '../../api/tm';
import type { CaseWorkflowSettings, ReviewKind } from '../../api/reviews';

export const EMPTY_SNAPSHOT: CaseSnapshot = { name: '', status: 'draft', folderId: null, body: { description: '', preconditions: '', steps: [], links: [] }, fields: {}, tags: [] };

export function bytesOf(text: string): number {
  return new TextEncoder().encode(text).length;
}

export function itemKey(i: { kind: ReviewKind; number: number }): string {
  return `${i.kind === 'case' ? 'TC' : 'SS'}-${String(i.number)}`;
}

export function itemLink(project: string, i: { kind: ReviewKind; number: number }): string {
  return i.kind === 'case' ? `/p/${project}/cases/${String(i.number)}?tab=review` : `/p/${project}/shared-steps/${String(i.number)}`;
}

export const CATEGORIES_LIST: { value: CaseStatus; label: string }[] = [
  { value: 'draft', label: 'Draft' },
  { value: 'active', label: 'Active' },
  { value: 'outdated', label: 'Outdated' },
];

const KEY_RE = /^[a-z][a-z0-9_]{0,31}$/;
const COLOR_RE = /^#[0-9a-fA-F]{6}$/;
/** The checks the server makes on `caseWorkflow`, with messages that name the field. */
export function validateWorkflow(w: CaseWorkflowSettings): string[] {
  const errors: string[] = [];
  if (w.statuses.length < 1 || w.statuses.length > 20) errors.push('A workflow has 1 to 20 statuses.');
  const keys = new Set<string>();
  for (const [i, s] of w.statuses.entries()) {
    const at = `Status ${String(i + 1)}`;
    if (!KEY_RE.test(s.key)) errors.push(`${at}: the key must start with a lowercase letter and use only a-z, 0-9 and _ (at most 32 characters).`);
    else if (keys.has(s.key)) errors.push(`${at}: the key "${s.key}" is used twice.`);
    keys.add(s.key);
    if (bytesOf(s.name) < 1 || bytesOf(s.name) > 100) errors.push(`${at}: the name is 1 to 100 bytes.`);
    if (s.color && !COLOR_RE.test(s.color)) errors.push(`${at}: the colour must look like #1c7ed6.`);
  }
  for (const c of CATEGORIES_LIST) if (!w.statuses.some((s) => s.category === c.value)) errors.push(`At least one status must be in the ${c.label.toLowerCase()} category.`);
  for (const [name, flow] of [['Manual', w.manual], ['Automated', w.automated]] as const) {
    if (!keys.has(flow.initial)) errors.push(`${name} cases: the initial state must be one of the statuses.`);
    if (flow.transitions.length > 100) errors.push(`${name} cases: at most 100 transitions.`);
    for (const [i, t] of flow.transitions.entries()) {
      const at = `${name} cases, transition ${String(i + 1)}`;
      if (t.from.length === 0) errors.push(`${at}: choose where it starts.`);
      if (t.from.some((f) => f !== '*' && !keys.has(f))) errors.push(`${at}: a "from" state does not exist.`);
      if (!t.to || (t.to !== '*' && !keys.has(t.to))) errors.push(`${at}: choose where it leads.`);
      if ((t.requires?.length ?? 0) > 3) errors.push(`${at}: at most 3 conditions.`);
    }
  }
  return errors;
}

