import { http, HttpResponse, type HttpHandler } from 'msw';
import type { CaseParameters, ParameterGenerate, RunIteration, RunIterationPatch, RunStepWrite } from '../api/parameters';
import type { CaseStep, CaseWrite, RunStatus } from '../api/tm';
import { COLUMN_RE, newRowId, placeholders } from '../lib/tm/parameters';
import { mockRunItemView, paramHooks, tmState } from './tm';
import { ME, TM_PROJECT, type MockRunItem } from './tmFixtures';

const API = '/api/v1';
const P = `${API}/projects/:key`;
const MOCK_ROLE_KEY = 'fastproof-mock-role';
const FINAL: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped'];
const MAX_ROWS = 1000;

interface MockIterations {
  executionNo: number;
  columns: string[];
  iterations: RunIteration[];
}

const mem = new Map<string, MockIterations>();

export function resetParameters(): void {
  mem.clear();
}

/** Test access to the iterations of the mock (vitest). */
export function parametersState(): Map<string, MockIterations> {
  return mem;
}

paramHooks.reset.push(resetParameters);

function role(): 'admin' | 'viewer' {
  try {
    return typeof window !== 'undefined' && window.localStorage.getItem(MOCK_ROLE_KEY) === 'viewer' ? 'viewer' : 'admin';
  } catch {
    return 'admin';
  }
}

function err(status: number, code: string, message: string, details?: unknown): Response {
  return HttpResponse.json({ code, message, ...(details === undefined ? {} : { details }) }, { status });
}

const now = () => new Date().toISOString();

/** The derived status of a parameterized item (the server's rule). */
export function deriveStatus(list: readonly RunStatus[]): RunStatus {
  if (list.includes('failed')) return 'failed';
  if (list.includes('blocked')) return 'blocked';
  if (list.every((s) => s === 'untested')) return 'untested';
  if (list.every((s) => s === 'skipped')) return 'skipped';
  if (list.some((s) => !FINAL.includes(s))) return 'in_progress';
  return 'passed';
}

function enabledIterations(p: CaseParameters): RunIteration[] {
  return p.rows
    .filter((r) => r.enabled)
    .map((r, i) => ({
      rowId: r.id,
      position: i + 1,
      values: Object.fromEntries(p.columns.map((c) => [c, r.values[c] ?? ''] as const)),
      status: 'untested' as const,
      comment: '',
      resultId: null,
      executedBy: null,
      startedAt: null,
      finishedAt: null,
      stepResults: [],
    }));
}

/** The iterations of an item, reset to untested when the item was rerun. */
function stateOf(r: MockRunItem): MockIterations | undefined {
  const m = mem.get(r.id);
  if (m && m.executionNo !== r.executionNo) {
    m.executionNo = r.executionNo;
    m.iterations = m.iterations.map((it) => ({ ...it, status: 'untested', comment: '', resultId: null, executedBy: null, startedAt: null, finishedAt: null, stepResults: [] }));
  }
  return m;
}

function leaves(steps: readonly CaseStep[], prefix = ''): string[] {
  const out: string[] = [];
  steps.forEach((s, i) => {
    const p = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
    if (s.steps.length) out.push(...leaves(s.steps, p));
    else out.push(p);
  });
  return out;
}

function stepExists(steps: readonly CaseStep[], path: string): boolean {
  let list = steps;
  for (const part of path.split('.')) {
    const s = list[Number(part) - 1];
    if (!/^[1-9]\d*$/.test(part) || !s) return false;
    list = s.steps;
  }
  return true;
}

function setStatus(it: RunIteration, status: RunStatus): void {
  if (it.status === status) return;
  it.status = status;
  if (status === 'untested') {
    it.startedAt = null;
    it.executedBy = null;
  } else {
    it.startedAt ??= now();
    it.executedBy = ME;
  }
  it.finishedAt = FINAL.includes(status) ? now() : null;
  it.resultId = FINAL.includes(status) ? `00000000-0000-7000-8000-${String(Math.floor(Math.random() * 1e12)).padStart(12, '0')}` : null;
}

function skipUntested(r: MockRunItem, it: RunIteration): void {
  for (const p of leaves(r.steps)) {
    if (it.stepResults.some((s) => s.path === p && s.status !== 'untested')) continue;
    it.stepResults = it.stepResults.filter((s) => s.path !== p);
    it.stepResults.push({ path: p, status: 'skipped', actual: '', comment: '', attachments: [], updatedBy: ME, updatedAt: now() });
  }
  it.stepResults.sort((a, b) => a.path.localeCompare(b.path, undefined, { numeric: true }));
}

function derive(r: MockRunItem, m: MockIterations, bump = true): void {
  const next = deriveStatus(m.iterations.map((i) => i.status));
  if (next !== r.status) {
    r.status = next;
    if (next !== 'untested') {
      r.startedAt ??= now();
      r.executedBy = ME;
    } else r.startedAt = null;
    r.finishedAt = FINAL.includes(next) ? now() : null;
  }
  if (bump) r.revision++;
  r.updatedAt = now();
}

paramHooks.added = (r, c) => {
  const p = c.body.parameters;
  if (!p || !p.columns.length || !p.rows.some((x) => x.enabled)) return;
  mem.set(r.id, { executionNo: r.executionNo, columns: [...p.columns], iterations: enabledIterations(p) });
};

paramHooks.view = (r, v) => {
  const m = stateOf(r);
  return m ? { ...v, iterationCount: m.iterations.length, iterations: structuredClone(m.iterations), parameterColumns: [...m.columns] } : v;
};

paramHooks.summary = (r, s) => {
  const m = mem.get(r.id);
  return m ? { ...s, iterationCount: m.iterations.length } : s;
};

paramHooks.stepGuard = (r) => (mem.has(r.id) ? err(409, 'parameterized_item', 'this run item has iterations: write the steps of an iteration') : null);

paramHooks.patch = (r, b) => {
  const m = stateOf(r);
  if (!m) return null;
  if ((b.status || b.untestedStepsTo) && !b.allIterations) return err(409, 'parameterized_item', 'set allIterations to change every iteration');
  if (!b.status && !b.untestedStepsTo) return 'handled';
  for (const it of m.iterations) {
    if (b.untestedStepsTo === 'skipped') skipUntested(r, it);
    if (b.status) setStatus(it, b.status);
  }
  derive(r, m, false);
  return 'handled';
};

paramHooks.validate = (b: CaseWrite) => {
  const p = b.body.parameters;
  if (!p) return [];
  const errs: { path: string; message: string }[] = [];
  p.columns.forEach((c, i) => {
    if (!COLUMN_RE.test(c)) errs.push({ path: `body.parameters.columns[${String(i)}]`, message: 'a column is a letter or _ and up to 63 letters, digits, _, . or -' });
  });
  if (p.rows.length > MAX_ROWS) errs.push({ path: 'body.parameters.rows', message: `at most ${String(MAX_ROWS)} rows` });
  const known = new Set(p.columns);
  const check = (path: string, text: string) => {
    for (const x of placeholders(text)) if (!known.has(x.name)) errs.push({ path, message: `unknown parameter \${${x.name}}` });
  };
  check('body.preconditions', b.body.preconditions);
  const walk = (list: readonly CaseStep[], prefix: string) =>
    list.forEach((s, i) => {
      const at = `${prefix}[${String(i)}]`;
      check(`${at}.action`, s.action);
      check(`${at}.expected`, s.expected);
      check(`${at}.data`, s.data);
      walk(s.steps, `${at}.steps`);
    });
  walk(b.body.steps, 'body.steps');
  return errs;
};

function find(key: string, id: string): { r: MockRunItem; m: MockIterations } | null {
  if (key !== TM_PROJECT) return null;
  const r = tmState().runItems.find((x) => x.id === id);
  const m = r ? stateOf(r) : undefined;
  return r && m ? { r, m } : null;
}

/** Greedy pairwise rows: each row starts from an uncovered pair and takes the values that cover most new pairs. */
function pairwise(names: string[], lists: string[][]): Record<string, string>[] {
  const n = names.length;
  const size = (i: number) => lists[i]?.length ?? 0;
  if (n === 1) return (lists[0] ?? []).map((v) => ({ [names[0] ?? '']: v }));
  const key = (i: number, a: number, j: number, b: number) => (i < j ? `${String(i)}:${String(a)}|${String(j)}:${String(b)}` : `${String(j)}:${String(b)}|${String(i)}:${String(a)}`);
  const uncovered = new Set<string>();
  for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) for (let a = 0; a < size(i); a++) for (let b = 0; b < size(j); b++) uncovered.add(key(i, a, j, b));
  const out: Record<string, string>[] = [];
  while (uncovered.size && out.length < MAX_ROWS) {
    const first = uncovered.values().next().value ?? '';
    const [p, q] = first.split('|').map((x) => x.split(':').map(Number));
    const row: (number | undefined)[] = new Array<number | undefined>(n).fill(undefined);
    row[p?.[0] ?? 0] = p?.[1];
    row[q?.[0] ?? 0] = q?.[1];
    for (let k = 0; k < n; k++) {
      if (row[k] !== undefined) continue;
      let best = 0;
      let gain = -1;
      for (let v = 0; v < size(k); v++) {
        let g = 0;
        for (let j = 0; j < n; j++) if (j !== k && row[j] !== undefined && uncovered.has(key(k, v, j, row[j] ?? 0))) g++;
        if (g > gain) {
          gain = g;
          best = v;
        }
      }
      row[k] = best;
    }
    for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) uncovered.delete(key(i, row[i] ?? 0, j, row[j] ?? 0));
    out.push(Object.fromEntries(names.map((name, i) => [name, lists[i]?.[row[i] ?? 0] ?? ''] as const)));
  }
  return out;
}

export const parametersHandlers: HttpHandler[] = [
  http.post(`${P}/cases/:n/parameters/generate`, async ({ params, request }) => {
    if (String(params.key) !== TM_PROJECT || !tmState().cases.has(Number(params.n))) return err(404, 'not_found', 'case not found');
    if (role() !== 'admin') return err(403, 'forbidden', 'requires permission cases:write', { required: 'cases:write' });
    const b = (await request.json()) as ParameterGenerate;
    const names = Object.keys(b.values);
    if (!names.length || names.some((n) => !COLUMN_RE.test(n) || !(b.values[n] ?? []).length)) return err(400, 'invalid_case', 'name at least one parameter with values');
    const lists = names.map((n) => b.values[n] ?? []);
    let rows: Record<string, string>[];
    if (b.mode === 'all') {
      const total = lists.reduce((a, l) => a * l.length, 1);
      if (total > MAX_ROWS) return err(409, 'limit_reached', 'maxParameterRows (1000) reached', { limit: 'maxParameterRows', max: MAX_ROWS });
      rows = [{}];
      names.forEach((n, i) => {
        rows = rows.flatMap((r) => (lists[i] ?? []).map((v) => ({ ...r, [n]: v })));
      });
    } else rows = pairwise(names, lists);
    return HttpResponse.json({ rows: rows.map((values) => ({ id: newRowId(), values, enabled: true })) });
  }),

  http.put(`${P}/run-items/:id/iterations/:row/steps/:path`, async ({ params, request }) => {
    const f = find(String(params.key), String(params.id));
    if (!f) return err(404, 'not_found', 'run item not found');
    if (role() !== 'admin') return err(403, 'forbidden', 'requires permission runs:execute', { required: 'runs:execute' });
    const { r, m } = f;
    if (tmState().manual.get(r.launchId)?.state === 'closed') return err(409, 'manual_run_closed', 'reopen the manual run first');
    const it = m.iterations.find((x) => x.rowId === params.row);
    if (!it) return err(404, 'not_found', 'iteration not found');
    const path = String(params.path);
    if (!stepExists(r.steps, path)) return err(404, 'not_found', 'step not found');
    const b = (await request.json()) as RunStepWrite;
    const prev = it.stepResults.find((s) => s.path === path);
    it.stepResults = it.stepResults.filter((s) => s.path !== path);
    it.stepResults.push({ path, status: b.status, actual: b.actual ?? prev?.actual ?? '', comment: b.comment ?? prev?.comment ?? '', attachments: prev?.attachments ?? [], updatedBy: ME, updatedAt: now() });
    it.stepResults.sort((a, b2) => a.path.localeCompare(b2.path, undefined, { numeric: true }));
    if (it.status === 'untested') setStatus(it, 'in_progress');
    derive(r, m);
    const results = new Map(it.stepResults.map((s) => [s.path, s.status] as const));
    const done = leaves(r.steps).every((p) => (results.get(p) ?? 'untested') !== 'untested');
    const any = (s: string) => [...results.values()].includes(s as never);
    const suggested: RunStatus = any('failed') ? 'failed' : any('blocked') ? 'blocked' : !done ? 'in_progress' : any('passed') ? 'passed' : 'skipped';
    return HttpResponse.json({ iterationStatus: it.status, itemStatus: r.status, suggestedStatus: suggested, revision: r.revision });
  }),

  http.patch(`${P}/run-items/:id/iterations/:row`, async ({ params, request }) => {
    const f = find(String(params.key), String(params.id));
    if (!f) return err(404, 'not_found', 'run item not found');
    if (role() !== 'admin') return err(403, 'forbidden', 'requires permission runs:execute', { required: 'runs:execute' });
    const { r, m } = f;
    if (tmState().manual.get(r.launchId)?.state === 'closed') return err(409, 'manual_run_closed', 'reopen the manual run first');
    const it = m.iterations.find((x) => x.rowId === params.row);
    if (!it) return err(404, 'not_found', 'iteration not found');
    const h = request.headers.get('If-Match');
    if (!h) return err(428, 'precondition_required', 'send If-Match with the ETag you read');
    if (h !== `"r${String(r.revision)}"`) return err(412, 'precondition_failed', 'the resource changed since you read it', { currentRevision: r.revision, changedBy: r.executedBy, source: 'ui' });
    const b = (await request.json()) as RunIterationPatch;
    if (!b.status && b.comment === undefined && !b.untestedStepsTo) return err(400, 'bad_request', 'nothing to change');
    if (b.untestedStepsTo === 'skipped') skipUntested(r, it);
    if (b.status) setStatus(it, b.status);
    if (b.comment !== undefined) it.comment = b.comment ?? '';
    derive(r, m);
    return HttpResponse.json(mockRunItemView(r, String(params.key)), { headers: { ETag: `"r${String(r.revision)}"` } });
  }),
];
