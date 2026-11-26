import type { Case, CaseLink, CaseSnapshot, CaseStep, CaseWrite, FieldValue } from '../../api/tm';
import { flattenSteps } from './steps';

export interface FieldChange {
  key: string;
  label: string;
  before: string;
  after: string;
}

export type StepChangeKind = 'added' | 'removed' | 'changed' | 'moved' | 'same';

export interface StepPartChange {
  part: 'action' | 'expected' | 'data' | 'attachments';
  before: string;
  after: string;
}

export interface StepChange {
  id: string;
  kind: StepChangeKind;
  beforePath: string | null;
  afterPath: string | null;
  /** The step as it reads after the change (before it, for removed steps). */
  action: string;
  parts: StepPartChange[];
}

export interface VersionDiff {
  fields: FieldChange[];
  steps: StepChange[];
  /** True when nothing differs. */
  same: boolean;
}

export function fieldText(v: FieldValue | undefined): string {
  if (v === null || v === undefined) return '';
  if (Array.isArray(v)) return v.join(', ');
  return String(v);
}

function linksText(links: readonly CaseLink[]): string {
  return links.map((l) => `${l.type}: ${l.name} <${l.url}>`).join('\n');
}

/**
 * The parameters of a body as text, one line per row, so a change of columns, values, row order or `enabled` (an AI
 * suggestion of parameters among them) shows in every diff. JSON keeps values with `,` or `=` unambiguous.
 */
export function parametersText(p: CaseSnapshot['body']['parameters']): string {
  if (!p) return '';
  const lines = [`columns: ${JSON.stringify(p.columns)}`];
  for (const r of p.rows) lines.push(`${r.enabled ? 'row' : 'row (disabled)'} ${r.id}: ${JSON.stringify(p.columns.map((c) => r.values[c] ?? ''))}`);
  if (p.generator) lines.push(`generator: ${p.generator.mode} ${JSON.stringify(p.generator.values)}`);
  return lines.join('\n');
}

const PARTS: StepPartChange['part'][] = ['action', 'expected', 'data', 'attachments'];

function partText(s: CaseStep, part: StepPartChange['part']): string {
  return part === 'attachments' ? s.attachments.join(', ') : s[part];
}

/**
 * Differences between two snapshots: top-level fields by name, custom fields by key, steps matched by their stable
 * `id` (a step whose id stays is changed or moved, never removed and added).
 */
export function diffSnapshots(a: CaseSnapshot, b: CaseSnapshot, fieldLabels: Record<string, string> = {}, folderName: (id: string | null) => string = (id) => id ?? '—'): VersionDiff {
  const fields: FieldChange[] = [];
  const push = (key: string, label: string, before: string, after: string) => {
    if (before !== after) fields.push({ key, label, before, after });
  };
  push('name', 'Name', a.name, b.name);
  push('status', 'Status', a.status, b.status);
  push('folderId', 'Folder', folderName(a.folderId ?? null), folderName(b.folderId ?? null));
  push('description', 'Description', a.body.description, b.body.description);
  push('preconditions', 'Preconditions', a.body.preconditions, b.body.preconditions);
  push('tags', 'Tags', [...a.tags].sort().join(', '), [...b.tags].sort().join(', '));
  push('links', 'Links', linksText(a.body.links), linksText(b.body.links));
  push('parameters', 'Parameters', parametersText(a.body.parameters), parametersText(b.body.parameters));
  const keys = [...new Set([...Object.keys(a.fields), ...Object.keys(b.fields)])].sort();
  for (const k of keys) push(`fields.${k}`, fieldLabels[k] ?? k, fieldText(a.fields[k]), fieldText(b.fields[k]));

  const before = flattenSteps(a.body.steps);
  const after = flattenSteps(b.body.steps);
  const beforeById = new Map(before.map((f) => [f.step.id, f] as const));
  const afterIds = new Set(after.map((f) => f.step.id));
  const steps: StepChange[] = [];
  const removedAt = new Map<number, StepChange[]>();
  before.forEach((f, i) => {
    if (afterIds.has(f.step.id)) return;
    const change: StepChange = { id: f.step.id, kind: 'removed', beforePath: f.path, afterPath: null, action: f.step.action, parts: PARTS.map((p) => ({ part: p, before: partText(f.step, p), after: '' })).filter((p) => p.before) };
    const list = removedAt.get(i) ?? [];
    list.push(change);
    removedAt.set(i, list);
  });
  let cursor = 0;
  const flushRemoved = (upTo: number) => {
    for (; cursor < upTo; cursor++) steps.push(...(removedAt.get(cursor) ?? []));
  };
  for (const f of after) {
    const old = beforeById.get(f.step.id);
    if (!old) {
      steps.push({ id: f.step.id, kind: 'added', beforePath: null, afterPath: f.path, action: f.step.action, parts: PARTS.map((p) => ({ part: p, before: '', after: partText(f.step, p) })).filter((p) => p.after) });
      continue;
    }
    flushRemoved(before.indexOf(old));
    const parts = PARTS.map((p) => ({ part: p, before: partText(old.step, p), after: partText(f.step, p) })).filter((p) => p.before !== p.after);
    const moved = old.path !== f.path;
    steps.push({ id: f.step.id, kind: parts.length ? 'changed' : moved ? 'moved' : 'same', beforePath: old.path, afterPath: f.path, action: f.step.action, parts });
  }
  flushRemoved(before.length);
  return { fields, steps, same: fields.length === 0 && steps.every((s) => s.kind === 'same') };
}

/** The folder of a case (`folderId`; `folder.id` from servers before it). */
export function folderIdOf(c: Case): string | null {
  return c.folderId !== undefined ? c.folderId : (c.folder?.id ?? null);
}

export function currentSnapshot(c: Case): CaseSnapshot {
  return { name: c.name, status: c.status, folderId: folderIdOf(c), body: c.body, fields: c.fields, tags: c.tags };
}

export function writeSnapshot(w: CaseWrite, c: Case): CaseSnapshot {
  return { name: w.name, status: w.status ?? c.status, folderId: w.folderId === undefined ? folderIdOf(c) : w.folderId, body: w.body, fields: w.fields ?? c.fields, tags: w.tags ?? c.tags };
}

export function toWrite(c: Case): CaseWrite {
  return structuredClone({ name: c.name, folderId: folderIdOf(c), status: c.status, body: c.body, fields: c.fields, tags: c.tags });
}
