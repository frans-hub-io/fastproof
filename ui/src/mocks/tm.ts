import { http, HttpResponse } from 'msw';
import type {
  Case,
  CaseBody,
  CaseBulk,
  CaseFieldDef,
  CaseFilter,
  CaseFolder,
  CaseHistoryEntry,
  CaseHtml,
  CasePatch,
  CaseStep,
  CaseStepHtml,
  CaseSummary,
  CaseTreeNode,
  CaseVersion,
  CaseWrite,
  FieldValue,
  ImportFormat,
  ImportOptions,
  ImportPreview,
  ImportReport,
  LaunchManual,
  PlanLaunch,
  RunItem,
  RunItemPatch,
  RunItemSummary,
  RunStatus,
  RunStepWrite,
  StartPlanLaunch,
  StepStatus,
  TestPlan,
  TestPlanItem,
  TestPlanItemsChange,
  TestPlanWrite,
  TmJob,
  VersionSource,
} from '../api/tm';
import type { Launch, ResultSummary } from '../api/types';
import { launchDef, launchResults, launchView, projectLaunches, testCaseHistory, type MockRole } from './fixtures';
import { analyze, isImportant, settingsOf } from './stageC';
import {
  buildTmState,
  CODE_FIELDS,
  FALLBACK_PREVIEW,
  folderPathNames,
  ME,
  PEOPLE,
  runItemFor,
  snapshotOf,
  subtreeIds,
  TM_PROJECT,
  tmId,
  type MockCase,
  type MockPlan,
  type MockRunItem,
  type TmState,
} from './tmFixtures';
import { sharedBriefsFor, sharedPinsOf } from './sharedStepsState';
import { renderMarkdown } from './tmMarkdown';

const API = '/api/v1';
const P = `${API}/projects/:key`;

let st: TmState = buildTmState();
let failSteps = 0;

export function resetTm(): void {
  st = buildTmState();
}

/** Test access to the mock state (vitest). */
export function tmState(): TmState {
  return st;
}

type Listener = (event: string, data: unknown) => void;
const listeners = new Set<Listener>();

export function subscribeTmEvents(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function emit(event: string, data: unknown): void {
  for (const l of listeners) l(event, data);
}

/** Simulates a code sync (or another user) changing a case: bumps its revision and emits `case.updated`. */
export function touchCase(n: number, source: VersionSource = 'code', by = 'ci'): number | null {
  const c = st.cases.get(n);
  if (!c) return null;
  c.body = { ...c.body, description: `${c.body.description}\n\nUpdated by ${source === 'code' ? 'the code sync' : by}.` };
  newVersion(c, source, ['body.description'], by);
  emit('case.updated', { number: n, revision: c.revision, source });
  return c.revision;
}

if (typeof window !== 'undefined') {
  (window as unknown as { __fastproofMockTm?: unknown }).__fastproofMockTm = {
    touchCase,
    emit,
    reset: resetTm,
    failSteps: (n: number) => {
      failSteps = n;
    },
  };
}

interface Helpers {
  role: () => MockRole;
  error: (status: number, code: string, message: string, details?: unknown) => Response;
  audit: (action: string, scope: string | undefined, detail: unknown) => void;
}

function now(): string {
  return new Date().toISOString();
}

function etag(revision: number): string {
  return `"r${String(revision)}"`;
}

function isAutomated(c: MockCase): boolean {
  return c.identities.some((i) => i.kind === 'code');
}

function codeOwned(c: MockCase): string[] {
  if (!isAutomated(c)) return [];
  const keys = new Set(st.fields.map((f) => f.key));
  return CODE_FIELDS.filter((k) => ['name', 'description', 'links', 'tags'].includes(k) || keys.has(k));
}

function stepHtml(steps: CaseStep[], key: string): CaseStepHtml[] {
  return steps.map((s) => ({ id: s.id, action: renderMarkdown(s.action, key), expected: renderMarkdown(s.expected, key), data: renderMarkdown(s.data, key), steps: stepHtml(s.steps, key) }));
}

function bodyHtml(b: CaseBody, key: string): CaseHtml {
  return { description: renderMarkdown(b.description, key), preconditions: renderMarkdown(b.preconditions, key), steps: stepHtml(b.steps, key) };
}

function folderView(id: string | null): Case['folder'] {
  if (!id) return null;
  const path: { id: string; name: string }[] = [];
  let cur: string | null = id;
  for (let i = 0; cur && i < 20; i++) {
    const f = st.folders.find((x) => x.id === cur);
    if (!f) break;
    path.unshift({ id: f.id, name: f.name });
    cur = f.parentId;
  }
  return { id, path };
}

function caseView(c: MockCase, key: string): Case {
  return {
    number: c.number,
    id: c.id,
    name: c.name,
    status: c.status,
    automated: isAutomated(c),
    origin: c.origin,
    aiUnreviewed: c.aiUnreviewed,
    folder: folderView(c.folderId),
    folderId: c.folderId,
    body: structuredClone(c.body),
    html: bodyHtml(c.body, key),
    fields: structuredClone(c.fields),
    tags: [...c.tags],
    codeOwned: codeOwned(c),
    identities: structuredClone(c.identities.slice(0, 100)),
    identitiesTotal: c.identities.length,
    keys: structuredClone(c.keys),
    revision: c.revision,
    version: c.version,
    createdBy: c.createdBy,
    createdAt: c.createdAt,
    updatedBy: c.updatedBy,
    updatedAt: c.updatedAt,
    archivedAt: c.archivedAt,
    state: null,
    approvedVersion: null,
    reviewState: null,
    sharedSteps: sharedBriefsFor(c.body.steps, key),
  };
}

function lastManual(c: MockCase): CaseSummary['lastResult'] | undefined {
  const done = st.runItems.filter((r) => r.caseNumber === c.number && r.resultId && r.finishedAt).sort((a, b) => (b.finishedAt ?? '').localeCompare(a.finishedAt ?? ''));
  const r = done[0];
  if (!r?.resultId) return undefined;
  return { status: r.status === 'blocked' ? 'broken' : r.status, at: r.finishedAt ?? r.updatedAt, source: 'manual', launchId: r.launchId, resultId: r.resultId };
}

function summaryView(c: MockCase): CaseSummary {
  const manual = lastManual(c);
  const last = manual && (!c.lastResult || manual.at > c.lastResult.at) ? manual : c.lastResult;
  return {
    number: c.number,
    name: c.name,
    status: c.status,
    automated: isAutomated(c),
    folderId: c.folderId,
    fields: structuredClone(c.fields),
    tags: [...c.tags],
    ...(last ? { lastResult: last } : {}),
    updatedAt: c.updatedAt,
    aiUnreviewed: c.aiUnreviewed,
    archived: c.archivedAt !== null,
  };
}

function newVersion(c: MockCase, source: VersionSource, changed: string[], by: string, launchId?: string): void {
  c.revision++;
  c.version++;
  c.updatedAt = now();
  c.updatedBy = by;
  c.versions.push({ version: c.version, source, changed, createdBy: by, createdAt: c.updatedAt, ...(launchId ? { launchId } : {}), snapshot: snapshotOf(c) });
}

function changedKeys(a: ReturnType<typeof snapshotOf>, b: ReturnType<typeof snapshotOf>): string[] {
  const out: string[] = [];
  if (a.name !== b.name) out.push('name');
  if (a.status !== b.status) out.push('status');
  if (a.folderId !== b.folderId) out.push('folderId');
  if (a.body.description !== b.body.description) out.push('body.description');
  if (a.body.preconditions !== b.body.preconditions) out.push('body.preconditions');
  if (JSON.stringify(a.body.steps) !== JSON.stringify(b.body.steps)) out.push('body.steps');
  if (JSON.stringify(a.body.links) !== JSON.stringify(b.body.links)) out.push('body.links');
  if (JSON.stringify([...a.tags].sort()) !== JSON.stringify([...b.tags].sort())) out.push('tags');
  for (const k of new Set([...Object.keys(a.fields), ...Object.keys(b.fields)])) if (JSON.stringify(a.fields[k] ?? null) !== JSON.stringify(b.fields[k] ?? null)) out.push(`fields.${k}`);
  return out;
}

function fieldMatches(c: MockCase, key: string, value: string): boolean {
  const v = c.fields[key];
  if (value === '') return v === undefined || v === null || v === '' || (Array.isArray(v) && v.length === 0);
  return Array.isArray(v) ? v.includes(value) : v !== null && v !== undefined && String(v) === value;
}

function matchesFilter(c: MockCase, f: CaseFilter): boolean {
  if (c.archivedAt) return false;
  if (f.folderIds?.length) {
    const ids = new Set<string>();
    for (const id of f.folderIds) for (const x of f.recursive === false ? [id] : subtreeIds(st, id)) ids.add(x);
    if (!c.folderId || !ids.has(c.folderId)) return false;
  }
  if (f.statuses?.length && !f.statuses.includes(c.status)) return false;
  if (f.automation === 'manual' && isAutomated(c)) return false;
  if (f.automation === 'automated' && !isAutomated(c)) return false;
  for (const [k, vals] of Object.entries(f.fields ?? {})) if (!vals.some((v) => fieldMatches(c, k, v))) return false;
  if (f.tags?.length && !f.tags.every((t) => c.tags.includes(t))) return false;
  if (f.q) {
    const q = f.q.toLowerCase().trim();
    const m = /^(?:tc-?|#)?(\d+)$/.exec(q);
    if (m ? c.number !== Number(m[1]) : !c.name.toLowerCase().includes(q)) return false;
  }
  return true;
}

function filterFromQuery(url: URL): CaseFilter {
  const sp = url.searchParams;
  const fields: Record<string, string[]> = {};
  for (const f of sp.getAll('field')) {
    const i = f.indexOf(':');
    if (i < 0) continue;
    (fields[f.slice(0, i)] ??= []).push(f.slice(i + 1));
  }
  const folder = sp.get('folder');
  const status = sp.get('status');
  const automation = sp.get('automation');
  return {
    ...(folder ? { folderIds: [folder], recursive: sp.get('recursive') !== 'false' } : {}),
    ...(status ? { statuses: status.split(',') as CaseFilter['statuses'] } : {}),
    ...(automation === 'manual' || automation === 'automated' ? { automation } : {}),
    ...(Object.keys(fields).length ? { fields } : {}),
    ...(sp.getAll('tag').length ? { tags: sp.getAll('tag') } : {}),
    ...(sp.get('q') ? { q: sp.get('q') ?? '' } : {}),
  };
}

function listCases(url: URL): MockCase[] {
  const sp = url.searchParams;
  const archived = sp.get('archived') === 'true';
  const f = filterFromQuery(url);
  const stale = Number(sp.get('staleDays'));
  const ai = sp.get('aiUnreviewed') === 'true';
  let list = [...st.cases.values()].filter((c) => (archived ? c.archivedAt !== null : true) && matchesFilter({ ...c, archivedAt: null }, f) && (archived || c.archivedAt === null));
  if (ai) list = list.filter((c) => c.aiUnreviewed);
  if (stale > 0) {
    const limit = Date.now() - stale * 86_400_000;
    list = list.filter((c) => {
      const last = summaryView(c).lastResult;
      return !last || new Date(last.at).getTime() < limit;
    });
  }
  const sort = sp.get('sort') ?? 'number';
  if (sort === 'updated') list.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
  else if (sort === 'name') list.sort((a, b) => a.name.localeCompare(b.name));
  else list.sort((a, b) => a.number - b.number);
  return list;
}

function pageOf<T>(items: T[], url: URL, idOf: (t: T) => string, def = 50, max = 500): { items: T[]; next: string | null } {
  const before = url.searchParams.get('before');
  let start = 0;
  if (before) {
    const i = items.findIndex((x) => idOf(x) === before);
    start = i >= 0 ? i + 1 : items.length;
  }
  const n = Number(url.searchParams.get('limit') ?? def);
  const limit = Number.isFinite(n) && n > 0 ? Math.min(max, Math.floor(n)) : def;
  const slice = items.slice(start, start + limit);
  const last = slice[slice.length - 1];
  return { items: slice, next: start + limit < items.length && last ? idOf(last) : null };
}

function folderCounts(id: string) {
  const ids = subtreeIds(st, id);
  let manual = 0;
  let automated = 0;
  for (const c of st.cases.values()) {
    if (c.archivedAt || !c.folderId || !ids.has(c.folderId)) continue;
    if (isAutomated(c)) automated++;
    else manual++;
  }
  return { total: manual + automated, manual, automated };
}

function folderDepth(id: string): number {
  let d = 0;
  let cur = st.folders.find((f) => f.id === id);
  while (cur?.parentId) {
    d++;
    const pid: string = cur.parentId;
    cur = st.folders.find((f) => f.id === pid);
  }
  return d;
}

function folderList(): CaseFolder[] {
  return st.folders.map((f) => ({ id: f.id, parentId: f.parentId, name: f.name, position: f.position, depth: folderDepth(f.id), counts: folderCounts(f.id), hasChildren: st.folders.some((x) => x.parentId === f.id) }));
}

function treeNodes(groupBy: string, parent: string | null): CaseTreeNode[] | null {
  const live = [...st.cases.values()].filter((c) => !c.archivedAt);
  const counts = (list: MockCase[]) => ({ total: list.length, automated: list.filter(isAutomated).length, manual: list.filter((c) => !isAutomated(c)).length });
  if (groupBy === 'folder') {
    return st.folders
      .filter((f) => f.parentId === parent)
      .sort((a, b) => a.position - b.position)
      .map((f) => ({ id: f.id, name: f.name, counts: folderCounts(f.id), hasChildren: st.folders.some((x) => x.parentId === f.id) }));
  }
  const levels = groupBy === 'behavior' ? ['epic', 'feature', 'story'] : groupBy === 'suite' ? ['suite'] : null;
  if (!levels) return null;
  const path = parent ? parent.split('\u001f') : [];
  if (path.length >= levels.length) return [];
  const within = live.filter((c) => path.every((v, i) => fieldMatches(c, levels[i] ?? '', v)));
  const key = levels[path.length] ?? '';
  const groups = new Map<string, MockCase[]>();
  for (const c of within) {
    const v = c.fields[key];
    const name = typeof v === 'string' && v ? v : '';
    const list = groups.get(name) ?? [];
    list.push(c);
    groups.set(name, list);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (a === '' ? 1 : b === '' ? -1 : a.localeCompare(b)))
    .map(([name, list]) => ({ id: [...path, name].join('\u001f'), name: name || `No ${key}`, counts: counts(list), hasChildren: path.length + 1 < levels.length && name !== '' }));
}

const MAX_NAME = 500;
const MAX_MD = 16384;

function validateWrite(b: CaseWrite, current: MockCase | null): { path: string; message: string }[] {
  const errs: { path: string; message: string }[] = [];
  if (typeof b.name !== 'string' || !b.name.trim()) errs.push({ path: 'name', message: 'Name is required' });
  else if (new TextEncoder().encode(b.name).length > MAX_NAME) errs.push({ path: 'name', message: `At most ${String(MAX_NAME)} bytes` });
  if (b.folderId && !st.folders.some((f) => f.id === b.folderId)) errs.push({ path: 'folderId', message: 'Unknown folder' });
  const md = (path: string, v: string) => {
    if (v.length > MAX_MD) errs.push({ path, message: `At most ${String(MAX_MD)} bytes` });
  };
  md('body.description', b.body.description);
  md('body.preconditions', b.body.preconditions);
  const walk = (steps: CaseStep[], prefix: string, depth: number) => {
    steps.forEach((s, i) => {
      const p = `${prefix}[${String(i)}]`;
      if (depth > 3) errs.push({ path: p, message: 'Steps nest at most 3 levels' });
      md(`${p}.action`, s.action);
      md(`${p}.expected`, s.expected);
      walk(s.steps, `${p}.steps`, depth + 1);
    });
  };
  walk(b.body.steps, 'body.steps', 1);
  const status = b.status ?? current?.status ?? 'draft';
  for (const def of st.fields) {
    const v = b.fields?.[def.key];
    const empty = v === undefined || v === null || v === '' || (Array.isArray(v) && !v.length);
    if (def.required && empty && status !== 'draft') errs.push({ path: `fields.${def.key}`, message: `${def.name} is required` });
    if (!empty && def.type === 'select' && !def.options.some((o) => o.value === v)) errs.push({ path: `fields.${def.key}`, message: `Unknown value ${String(v)}` });
    if (!empty && def.type === 'multiselect' && Array.isArray(v) && v.some((x) => !def.options.some((o) => o.value === x))) errs.push({ path: `fields.${def.key}`, message: 'Unknown value' });
  }
  if ((b.tags ?? []).length > 50) errs.push({ path: 'tags', message: 'At most 50 tags' });
  return errs;
}

function codeViolations(c: MockCase, next: ReturnType<typeof snapshotOf>): string[] {
  const owned = codeOwned(c);
  if (!owned.length) return [];
  const cur = snapshotOf(c);
  const out: string[] = [];
  if (owned.includes('name') && cur.name !== next.name) out.push('name');
  if (owned.includes('description') && cur.body.description !== next.body.description) out.push('description');
  if (owned.includes('links') && JSON.stringify(cur.body.links) !== JSON.stringify(next.body.links)) out.push('links');
  if (owned.includes('tags') && JSON.stringify([...cur.tags].sort()) !== JSON.stringify([...next.tags].sort())) out.push('tags');
  for (const k of owned) if (!['name', 'description', 'links', 'tags'].includes(k) && JSON.stringify(cur.fields[k] ?? null) !== JSON.stringify(next.fields[k] ?? null)) out.push(k);
  return out;
}

// Manual runs

function runItemView(r: MockRunItem, key: string): RunItem {
  return {
    id: r.id,
    caseNumber: r.caseNumber,
    name: r.name,
    folderPath: r.folderPath,
    status: r.status,
    assignee: r.assignee,
    executionNo: r.executionNo,
    updatedAt: r.updatedAt,
    launchId: r.launchId,
    caseVersion: r.caseVersion,
    preconditions: r.preconditions,
    preconditionsHtml: renderMarkdown(r.preconditions, key),
    steps: structuredClone(r.steps),
    stepsHtml: stepHtml(r.steps, key),
    stepResults: structuredClone(r.stepResults),
    comment: r.comment,
    defect: r.defect,
    resultId: r.resultId,
    executedBy: r.executedBy,
    startedAt: r.startedAt,
    finishedAt: r.finishedAt,
    revision: r.revision,
    iterationCount: 0,
    sharedPins: sharedPinsOf(r.steps),
    iterations: [],
    parameterColumns: [],
  };
}

function runSummary(r: MockRunItem): RunItemSummary {
  return { id: r.id, caseNumber: r.caseNumber, name: r.name, folderPath: r.folderPath, status: r.status, assignee: r.assignee, executionNo: r.executionNo, updatedAt: r.updatedAt, iterationCount: 0 };
}

const FINAL: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped'];

function manualCounts(launchId: string): LaunchManual | null {
  const m = st.manual.get(launchId);
  if (!m) return null;
  const items = st.runItems.filter((r) => r.launchId === launchId);
  const n = (s: RunStatus) => items.filter((r) => r.status === s).length;
  return { state: m.state, total: items.length, untested: n('untested'), inProgress: n('in_progress'), passed: n('passed'), failed: n('failed'), blocked: n('blocked'), skipped: n('skipped') };
}

function stepExists(steps: CaseStep[], path: string): CaseStep | null {
  let list = steps;
  let found: CaseStep | null = null;
  for (const part of path.split('.')) {
    if (!/^\d+$/.test(part)) return null;
    found = list[Number(part) - 1] ?? null;
    if (!found) return null;
    list = found.steps;
  }
  return found;
}

const SEV: Record<StepStatus, number> = { untested: 0, passed: 1, skipped: 2, blocked: 3, failed: 4 };

function effective(step: CaseStep, path: string, res: Map<string, StepStatus>): StepStatus {
  const own = res.get(path);
  if (own && own !== 'untested') return own;
  if (!step.steps.length) return own ?? 'untested';
  let worst: StepStatus = 'untested';
  step.steps.forEach((c, i) => {
    const s = effective(c, `${path}.${String(i + 1)}`, res);
    if (SEV[s] > SEV[worst]) worst = s;
  });
  return worst;
}

function suggested(r: MockRunItem): RunStatus {
  if (!r.steps.length) return 'untested';
  const res = new Map(r.stepResults.map((x) => [x.path, x.status] as const));
  const top = r.steps.map((s, i) => effective(s, String(i + 1), res));
  if (top.includes('failed')) return 'failed';
  if (top.includes('blocked')) return 'blocked';
  if (top.includes('untested')) return top.every((s) => s === 'untested') ? 'untested' : 'in_progress';
  if (top.every((s) => s === 'skipped')) return 'skipped';
  return 'passed';
}

function project(r: MockRunItem): void {
  if (FINAL.includes(r.status)) {
    r.resultId ??= tmId(`result:${r.id}:${String(r.executionNo)}`);
    r.finishedAt ??= now();
  } else {
    r.resultId = null;
    r.finishedAt = null;
  }
}

function manualResult(r: MockRunItem, base: ResultSummary | undefined): ResultSummary & { source: 'manual'; caseNumber: number; runItemId: string } {
  const status = r.status === 'blocked' ? 'broken' : r.status === 'passed' || r.status === 'failed' || r.status === 'skipped' ? r.status : 'unknown';
  const failedStep = r.stepResults.find((s) => s.status === 'failed' || s.status === 'blocked');
  return {
    ...(base ?? ({} as ResultSummary)),
    id: r.resultId ?? tmId(`result:${r.id}`),
    launchId: r.launchId,
    name: r.name,
    fullName: `TC-${String(r.caseNumber)} ${r.name}`,
    suitePath: r.folderPath,
    status,
    testCaseId: tmId(`manual:${String(r.caseNumber)}`),
    seriesKey: tmId(`manual-series:${String(r.caseNumber)}`),
    labels: [
      { name: 'manual', value: 'true' },
      { name: 'case', value: `TC-${String(r.caseNumber)}` },
      { name: 'executedBy', value: r.executedBy ?? '' },
      { name: 'manualStatus', value: r.status },
    ],
    message: failedStep?.actual ? failedStep.actual : null,
    attemptCount: r.executionNo,
    flaky: false,
    flakyInRun: false,
    muted: false,
    muteUntil: null,
    durationMs: r.startedAt && r.finishedAt ? new Date(r.finishedAt).getTime() - new Date(r.startedAt).getTime() : null,
    startedAt: r.startedAt,
    defect: null,
    transition: null,
    fingerprint: null,
    clientIds: [],
    source: 'manual',
    caseNumber: r.caseNumber,
    runItemId: r.id,
  };
}

function manualLaunchView(id: string): Launch | null {
  const m = st.manual.get(id);
  if (!m) return null;
  const created = st.createdLaunches.get(id);
  const def = created ? null : launchDef(id);
  const base = created ?? (def ? { ...launchView(def), important: isImportant(id, def.launch.number % 10 === 0) } : null);
  if (!base) return null;
  const counts = { ...base.counts };
  for (const r of st.runItems) {
    if (r.launchId !== id || !FINAL.includes(r.status)) continue;
    counts.total++;
    if (r.status === 'passed') counts.passed++;
    else if (r.status === 'failed') counts.failed++;
    else if (r.status === 'blocked') counts.broken++;
    else counts.skipped++;
  }
  const manual = manualCounts(id);
  return { ...base, counts, ...(m.planId ? { testPlanId: m.planId } : {}), ...(manual ? { manual } : {}) };
}

/** Launches of the project as the list route shows them: created plan launches first, every one with `manual` when it has a run. */
export function tmLaunchList(key: string, base: Launch[]): Launch[] {
  if (key !== TM_PROJECT) return base;
  const created = [...st.createdLaunches.keys()].map((id) => manualLaunchView(id)).filter((l): l is Launch => !!l).sort((a, b) => b.number - a.number);
  return [...created, ...base.map((l) => (st.manual.has(l.id) ? (manualLaunchView(l.id) ?? l) : l))];
}

/** `GET /people`: people bound to the project plus assignees and executors seen in its runs and plans. */
function peopleOf(q: string): { principal: string; display?: string }[] {
  const names = new Set<string>(PEOPLE);
  for (const r of st.runItems) {
    if (r.assignee) names.add(r.assignee);
    if (r.executedBy) names.add(r.executedBy);
  }
  for (const p of st.plans) for (const it of p.items) if (it.assignee) names.add(it.assignee);
  const t = q.trim().toLowerCase();
  return [...names]
    .filter((n) => !t || n.toLowerCase().includes(t))
    .sort()
    .slice(0, 100)
    .map((principal) => (principal === ME ? { principal, display: 'Alex Doe' } : { principal }));
}

function emitRun(r: MockRunItem): void {
  emit('runItem.updated', { launchId: r.launchId, id: r.id, status: r.status, revision: r.revision });
  const manual = manualCounts(r.launchId);
  if (manual) emit('manualRun.updated', { launchId: r.launchId, manual });
}

// Plans

function planItems(p: MockPlan): TestPlanItem[] {
  return [...p.items]
    .sort((a, b) => a.position - b.position)
    .flatMap((it) => {
      const c = st.cases.get(it.caseNumber);
      return c ? [{ caseNumber: c.number, name: c.name, folderId: c.folderId, automated: isAutomated(c), assignee: it.assignee, viaFilter: it.viaFilter, position: it.position }] : [];
    });
}

function planView(p: MockPlan): TestPlan {
  const items = planItems(p);
  const automated = items.filter((i) => i.automated).length;
  const rest: Partial<MockPlan> = { ...p };
  delete rest.items;
  return { ...(rest as TestPlan), counts: { total: items.length, automated, manual: items.length - automated } };
}

function syncDiff(p: MockPlan): { added: number[]; removed: number[] } {
  if (!p.filter) return { added: [], removed: [] };
  const filter = p.filter;
  const matching = new Set([...st.cases.values()].filter((c) => matchesFilter(c, filter)).map((c) => c.number));
  const have = new Set(p.items.map((i) => i.caseNumber));
  return {
    added: [...matching].filter((n) => !have.has(n)).sort((a, b) => a - b),
    removed: p.items.filter((i) => i.viaFilter && !matching.has(i.caseNumber)).map((i) => i.caseNumber),
  };
}

function testplanJson(numbers: number[]) {
  const tests = numbers.flatMap((n) => {
    const c = st.cases.get(n);
    if (!c || !isAutomated(c)) return [];
    const key = c.keys.find((k) => k.kind === 'allure_id');
    return c.identities.filter((i) => i.kind === 'code').map((i) => ({ ...(key ? { id: key.value } : {}), selector: i.fullName }));
  });
  return { version: '1.0', tests };
}

// Import

function parseCsv(text: string, maxRows = 200_000): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text.charAt(i);
    if (quoted) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i++;
        } else quoted = false;
      } else cell += ch;
      continue;
    }
    if (ch === '"') quoted = true;
    else if (ch === ',') {
      row.push(cell);
      cell = '';
    } else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && text[i + 1] === '\n') i++;
      row.push(cell);
      cell = '';
      if (row.some((c) => c !== '')) rows.push(row);
      row = [];
      if (rows.length > maxRows) break;
    } else cell += ch;
  }
  if (cell !== '' || row.length) {
    row.push(cell);
    if (row.some((c) => c !== '')) rows.push(row);
  }
  return rows;
}

/** Header → target, as the server's generic CSV suggests (and its tool parsers read). */
const MAPPING_GUESS: [RegExp, string][] = [
  [/^(title|name|case name|summary)$/i, 'name'],
  [/^(section|suite|folder|section hierarchy)$/i, 'folder'],
  [/^(description|objective)$/i, 'description'],
  [/^(preconditions?)$/i, 'preconditions'],
  [/^(steps?|steps \(step\)|step action|action)$/i, 'steps'],
  [/^(expected|expected result|steps \(expected result\)|step result)$/i, 'expected'],
  [/^(data|test data|steps \(additional info\))$/i, 'data'],
  [/^(tags?|labels?)$/i, 'tags'],
  [/^(id|case id|key)$/i, 'key'],
  [/^status$/i, 'status'],
];

/** The target of a header in a tool format: its system columns as the parser reads them, the rest `field:<header>`. */
function guessTarget(h: string): string {
  return MAPPING_GUESS.find(([re]) => re.test(h.trim()))?.[1] ?? `field:${h.trim()}`;
}

function detectFormat(headers: string[]): ImportFormat {
  const h = headers.map((x) => x.toLowerCase());
  if (h.includes('section') && h.includes('title')) return 'testrail-csv';
  if (h.includes('suite') && h.includes('title') && h.includes('steps_actions')) return 'qase-csv';
  if (h.includes('allure_id') || h.includes('allure id')) return 'testops-csv';
  return 'generic-csv';
}

async function readFile(request: Request): Promise<{ file: File | null; options: ImportOptions | null }> {
  const form = await request.formData();
  const f = form.get('file');
  const o = form.get('options');
  let options: ImportOptions | null = null;
  if (o) {
    try {
      options = JSON.parse(typeof o === 'string' ? o : await o.text()) as ImportOptions;
    } catch {
      options = null;
    }
  }
  return { file: f instanceof File ? f : null, options };
}

function previewOf(rows: string[][]): ImportPreview | null {
  const [headers, ...rest] = rows;
  if (!headers || headers.length < 2) return null;
  const format = detectFormat(headers);
  const mapping: Record<string, string> = {};
  for (const h of headers) {
    const t = guessTarget(h);
    if (format === 'generic-csv' || t.startsWith('field:')) mapping[h] = t;
  }
  const keyCol = headers.findIndex((h) => guessTarget(h) === 'key');
  const nameCol = headers.findIndex((h) => guessTarget(h) === 'name');
  const continued = rest.some((r, i) => i > 0 && ((keyCol >= 0 && r[keyCol] === '' && (nameCol < 0 || r[nameCol] === '')) || (keyCol >= 0 && r[keyCol] !== '' && r[keyCol] === rest[i - 1]?.[keyCol])));
  return { format, headers, rows: rest.slice(0, 20), suggestedMapping: mapping, stepsLayout: continued ? 'rows' : 'cells' };
}

interface ImportRow {
  row: number;
  name: string;
  folder: string;
  key: string;
  description: string;
  preconditions: string;
  steps: CaseStep[];
  fields: Record<string, FieldValue>;
  tags: string[];
}

function importRows(headers: string[], rows: string[][], o: ImportOptions): ImportRow[] {
  const given = o.mapping ?? {};
  const mapping: Record<string, string> = {};
  for (const h of headers) mapping[h] = o.format === 'generic-csv' ? (given[h] ?? (o.mapping ? 'ignore' : guessTarget(h))) : (given[h] ?? guessTarget(h));
  const col = (target: string) => headers.findIndex((h) => mapping[h] === target);
  const ci = { name: col('name'), folder: col('folder'), key: col('key'), d: col('description'), p: col('preconditions'), a: col('steps'), e: col('expected'), data: col('data'), tags: col('tags') };
  const fieldKey = (name: string) => st.fields.find((f) => f.key === name.toLowerCase() || f.name.toLowerCase() === name.toLowerCase())?.key ?? name;
  const fieldCols = headers.flatMap((h, i) => (mapping[h]?.startsWith('field:') ? [{ i, key: fieldKey(mapping[h].slice(6)) }] : []));
  const out: ImportRow[] = [];
  rows.forEach((r, idx) => {
    const name = ci.name >= 0 ? (r[ci.name] ?? '').trim() : '';
    const action = ci.a >= 0 ? (r[ci.a] ?? '') : '';
    const expected = ci.e >= 0 ? (r[ci.e] ?? '') : '';
    const data = ci.data >= 0 ? (r[ci.data] ?? '') : '';
    const prev = out[out.length - 1];
    const keyValue = ci.key >= 0 ? (r[ci.key] ?? '').trim() : '';
    if (!name && !keyValue && o.stepsLayout === 'rows' && prev && (action || expected)) {
      prev.steps.push({ id: `s_${tmId(`imp:${String(idx)}`).replace(/-/g, '').slice(0, 20)}`, action, expected, data, attachments: [], steps: [] });
      return;
    }
    const fields: Record<string, FieldValue> = {};
    for (const fc of fieldCols) if (r[fc.i]) fields[fc.key] = r[fc.i] ?? null;
    const steps: CaseStep[] = [];
    if (action || expected) {
      const actions = o.stepsLayout === 'cells' ? action.split(/\n(?=\d+[.)]\s)/) : [action];
      const exps = o.stepsLayout === 'cells' ? expected.split(/\n(?=\d+[.)]\s)/) : [expected];
      actions.forEach((a, i) => steps.push({ id: `s_${tmId(`imp:${String(idx)}:${String(i)}`).replace(/-/g, '').slice(0, 20)}`, action: a.replace(/^\d+[.)]\s*/, ''), expected: (exps[i] ?? '').replace(/^\d+[.)]\s*/, ''), data: i === 0 ? data : '', attachments: [], steps: [] }));
    }
    out.push({
      row: idx + 2,
      name,
      folder: ci.folder >= 0 ? (r[ci.folder] ?? '') : '',
      key: ci.key >= 0 ? (r[ci.key] ?? '') : '',
      description: ci.d >= 0 ? (r[ci.d] ?? '') : '',
      preconditions: ci.p >= 0 ? (r[ci.p] ?? '') : '',
      steps,
      fields,
      tags: ci.tags >= 0 ? (r[ci.tags] ?? '').split(/[,;]/).map((t) => t.trim()).filter(Boolean) : [],
    });
  });
  return out;
}

function keyKind(format: ImportFormat): 'testrail' | 'qase' | 'testops' | 'external' {
  return format === 'testrail-csv' ? 'testrail' : format.startsWith('qase') ? 'qase' : format === 'testops-csv' ? 'testops' : 'external';
}

function importReport(rows: ImportRow[], o: ImportOptions): ImportReport {
  const report: ImportReport = { create: 0, update: 0, skip: 0, folders: 0, unknownFields: [], unknownValues: {}, errors: [], createdFields: [], warnings: [] };
  const folders = new Set<string>();
  const kind = keyKind(o.format);
  for (const r of rows) {
    if (!r.name) {
      if (report.errors.length < 1000) report.errors.push({ row: r.row, code: 'invalid_case', message: 'the case has no name' });
      continue;
    }
    const existing = r.key ? [...st.cases.values()].find((c) => c.keys.some((k) => k.kind === kind && k.value === r.key)) : undefined;
    if (existing) {
      if (o.mode === 'upsert') report.update++;
      else report.skip++;
    } else report.create++;
    if (r.folder && !st.folders.some((f) => folderPathNames(st, f.id).join('/') === r.folder.replace(/\s*>\s*/g, '/'))) folders.add(r.folder);
    for (const [k, v] of Object.entries(r.fields)) {
      const def = st.fields.find((f) => f.key === k);
      if (!def) {
        if (!report.unknownFields.includes(k)) report.unknownFields.push(k);
        continue;
      }
      if (def.type === 'select' && typeof v === 'string' && !def.options.some((op) => op.value.toLowerCase() === v.toLowerCase())) {
        const list = (report.unknownValues[k] ??= []);
        if (!list.includes(v)) list.push(v);
      }
    }
  }
  report.folders = folders.size;
  if (o.unknownFieldValues === 'create') {
    report.createdFields = report.unknownFields;
    report.unknownFields = [];
  } else report.createdFields = [];
  const required = st.fields.filter((f) => f.required && !f.archived);
  report.warnings = required.length && o.status !== 'draft' ? rows.filter((r) => r.name && required.some((f) => r.fields[f.key] === undefined)).slice(0, 1000).map((r) => ({ row: r.row, code: 'required_fields_missing', message: `imported as draft: no value for ${required.filter((f) => r.fields[f.key] === undefined).map((f) => f.key).join(', ')}` })) : [];
  return report;
}

function ensureFolderPath(path: string, root: string | null): string | null {
  let parent = root;
  for (const name of path.split(/\s*[>/]\s*/).filter(Boolean)) {
    let f = st.folders.find((x) => x.parentId === parent && x.name.toLowerCase() === name.toLowerCase());
    if (!f) {
      f = { id: tmId(`folder:import:${String(parent)}:${name}`), parentId: parent, name, position: st.folders.filter((x) => x.parentId === parent).length };
      st.folders.push(f);
    }
    parent = f.id;
  }
  return parent;
}

function applyRows(rows: ImportRow[], o: ImportOptions): void {
  const kind = keyKind(o.format);
  for (const r of rows) {
    if (!r.name) continue;
    const existing = r.key ? [...st.cases.values()].find((c) => c.keys.some((k) => k.kind === kind && k.value === r.key)) : undefined;
    const fields: Record<string, FieldValue> = {};
    for (const [k, v] of Object.entries(r.fields)) {
      const def = st.fields.find((f) => f.key === k);
      if (!def) continue;
      if (def.type === 'select' && typeof v === 'string') {
        const opt = def.options.find((op) => op.value.toLowerCase() === v.toLowerCase());
        if (opt) fields[k] = opt.value;
        else if (o.unknownFieldValues === 'create') {
          def.options.push({ value: v, archived: false });
          fields[k] = v;
        }
      } else fields[k] = v;
    }
    const folderId = r.folder ? ensureFolderPath(r.folder, o.targetFolderId ?? null) : (o.targetFolderId ?? null);
    const body: CaseBody = { description: r.description, preconditions: r.preconditions, steps: r.steps, links: [] };
    if (existing) {
      if (o.mode !== 'upsert') continue;
      existing.body = body;
      existing.fields = { ...existing.fields, ...fields };
      newVersion(existing, 'import', ['body'], ME);
      continue;
    }
    st.seq++;
    const c: MockCase = {
      id: tmId(`case:${String(st.seq)}`),
      number: st.seq,
      name: r.name,
      status: o.status ?? 'active',
      origin: 'import',
      aiUnreviewed: false,
      folderId,
      body,
      fields,
      tags: r.tags,
      identities: [],
      keys: r.key ? [{ kind, value: r.key }] : [],
      revision: 1,
      version: 1,
      createdBy: ME,
      createdAt: now(),
      updatedBy: ME,
      updatedAt: now(),
      archivedAt: null,
      versions: [],
      attachments: [],
    };
    c.versions = [{ version: 1, source: 'import', changed: ['created'], createdBy: ME, createdAt: c.createdAt, snapshot: snapshotOf(c) }];
    st.cases.set(c.number, c);
  }
}

function jobView(j: TmState['jobs'] extends Map<string, infer V> ? V : never): TmJob {
  const view: Partial<typeof j> = { ...j };
  delete view.rows;
  delete view.headers;
  delete view.options;
  delete view.tick;
  return view as TmJob;
}

function csvCell(v: string): string {
  const safe = /^[=+\-@\t\r]/.test(v) ? `'${v}` : v;
  return /[",\n\r]/.test(safe) ? `"${safe.replace(/"/g, '""')}"` : safe;
}

export function tmHandlers({ role, error, audit }: Helpers) {
  const forbid = (perm: string) => error(403, 'forbidden', `requires permission ${perm}`, { required: perm });
  const writer = () => role() === 'admin';
  const tmProject = (key: string) => key === TM_PROJECT;
  const notFound = (what: string) => error(404, 'not_found', `${what} not found`);

  const caseOf = (key: string, n: string): MockCase | null => {
    if (!tmProject(key) || !/^\d+$/.test(n)) return null;
    return st.cases.get(Number(n)) ?? null;
  };

  const precondition = (request: Request, revision: number, changedBy: string | null, source: string): Response | null => {
    const m = request.headers.get('If-Match');
    if (!m) return error(428, 'precondition_required', 'send If-Match with the ETag you read');
    if (m !== etag(revision)) return error(412, 'precondition_failed', 'the resource changed since you read it', { currentRevision: revision, changedBy, source });
    return null;
  };

  const lastSource = (c: MockCase) => c.versions[c.versions.length - 1]?.source ?? 'ui';

  const withEtag = (body: unknown, revision: number, status = 200) => HttpResponse.json(body as Record<string, unknown>, { status, headers: { ETag: etag(revision) } });

  return [
    // Cases

    http.get(`${P}/cases/tree`, ({ params, request }) => {
      if (!tmProject(String(params.key))) return HttpResponse.json({ nodes: [] });
      const url = new URL(request.url);
      const nodes = treeNodes(url.searchParams.get('groupBy') ?? 'folder', url.searchParams.get('parent'));
      if (!nodes) return error(400, 'invalid_query', 'groupBy must be folder, behavior or suite', { field: 'groupBy' });
      return HttpResponse.json({ nodes });
    }),

    http.get(`${P}/cases/export`, ({ params, request }) => {
      if (!tmProject(String(params.key))) return notFound('project');
      const url = new URL(request.url);
      const list = listCases(url).slice(0, 50_000);
      if (url.searchParams.get('format') === 'ndjson') {
        const text = list.map((c) => JSON.stringify(caseView(c, String(params.key)))).join('\n');
        return new HttpResponse(`${text}\n`, { headers: { 'Content-Type': 'application/x-ndjson', 'Content-Disposition': 'attachment; filename="cases.ndjson"' } });
      }
      const lines = ['Number,Name,Status,Automated,Folder,Tags', ...list.map((c) => [`TC-${String(c.number)}`, c.name, c.status, String(isAutomated(c)), folderPathNames(st, c.folderId).join(' / '), c.tags.join(';')].map(csvCell).join(','))];
      return new HttpResponse(`${lines.join('\r\n')}\r\n`, { headers: { 'Content-Type': 'text/csv; charset=utf-8', 'Content-Disposition': 'attachment; filename="cases.csv"' } });
    }),

    http.post(`${P}/cases/exports`, async ({ params, request }) => {
      if (!tmProject(String(params.key))) return notFound('project');
      await request.json();
      const id = tmId(`job:${String(Date.now())}:${String(Math.random())}`);
      const job = { id, kind: 'export' as const, status: 'done' as const, progress: { done: 1, total: 1 }, report: null, createdBy: ME, createdAt: now(), finishedAt: now(), downloadUrl: `${API}/projects/${String(params.key)}/cases/export?format=ndjson`, tick: 0 };
      st.jobs.set(id, job);
      audit('export.create', String(params.key), { id });
      return HttpResponse.json(jobView(job), { status: 202 });
    }),

    http.get(`${P}/tm-jobs/:id`, ({ params }) => {
      const j = st.jobs.get(String(params.id));
      return j ? HttpResponse.json(jobView(j)) : notFound('job');
    }),

    http.post(`${P}/cases/bulk`, async ({ params, request }) => {
      const key = String(params.key);
      if (!tmProject(key)) return notFound('project');
      if (!writer()) return forbid('cases:write');
      const b = (await request.json()) as CaseBulk;
      let targets: MockCase[] = [];
      if (b.numbers) {
        if (b.numbers.length > 10_000) return error(409, 'limit_reached', 'too many cases', { limit: 'max_bulk', max: 10_000 });
        targets = b.numbers.map((n) => st.cases.get(n)).filter((c): c is MockCase => !!c);
      } else if (b.filter) {
        const f = b.filter;
        targets = [...st.cases.values()].filter((c) => matchesFilter(c, f));
      }
      const skipped: { number: number; code: string }[] = [];
      let updated = 0;
      for (const c of targets) {
        const before = snapshotOf(c);
        if (b.action === 'archive') {
          if (!c.archivedAt) {
            c.archivedAt = now();
            c.revision++;
            updated++;
          }
          continue;
        }
        if (b.action === 'restore') {
          if (c.archivedAt) {
            c.archivedAt = null;
            c.revision++;
            updated++;
          }
          continue;
        }
        if (b.action === 'move') {
          const folderId = (b.value ?? null) as string | null;
          if (folderId && !st.folders.some((f) => f.id === folderId)) return notFound('folder');
          c.folderId = folderId;
        } else if (b.action === 'setStatus') c.status = b.value as MockCase['status'];
        else if (b.action === 'setField') {
          const v = b.value as { key: string; value: FieldValue };
          if (codeOwned(c).includes(v.key)) {
            skipped.push({ number: c.number, code: 'field_from_code' });
            continue;
          }
          c.fields = { ...c.fields, [v.key]: v.value };
        } else {
          if (codeOwned(c).includes('tags')) {
            skipped.push({ number: c.number, code: 'field_from_code' });
            continue;
          }
          const tags = (b.value ?? []) as string[];
          c.tags = b.action === 'addTags' ? [...new Set([...c.tags, ...tags])] : c.tags.filter((t) => !tags.includes(t));
        }
        const changed = changedKeys(before, snapshotOf(c));
        if (!changed.length) {
          skipped.push({ number: c.number, code: 'unchanged' });
          continue;
        }
        newVersion(c, 'bulk', changed, ME);
        updated++;
      }
      audit('case.bulk', key, { action: b.action, updated });
      return HttpResponse.json({ updated, skipped });
    }),

    http.get(`${P}/cases`, ({ params, request }) => {
      const key = String(params.key);
      if (!tmProject(key)) return HttpResponse.json({ items: [], next: null });
      const url = new URL(request.url);
      const page = pageOf(listCases(url), url, (c) => String(c.number), 50);
      return HttpResponse.json({ items: page.items.map(summaryView), next: page.next });
    }),

    http.post(`${P}/cases`, async ({ params, request }) => {
      const key = String(params.key);
      if (!tmProject(key)) return notFound('project');
      if (!writer()) return forbid('cases:write');
      const b = (await request.json()) as CaseWrite;
      const errs = validateWrite(b, null);
      if (errs.length) return error(400, 'invalid_case', errs[0]?.message ?? 'invalid case', errs);
      st.seq++;
      const c: MockCase = {
        id: tmId(`case:${String(st.seq)}`),
        number: st.seq,
        name: b.name.trim(),
        status: b.status ?? 'draft',
        origin: 'ui',
        aiUnreviewed: false,
        folderId: b.folderId ?? null,
        body: b.body,
        fields: b.fields ?? {},
        tags: b.tags ?? [],
        identities: [],
        keys: [],
        revision: 1,
        version: 1,
        createdBy: ME,
        createdAt: now(),
        updatedBy: ME,
        updatedAt: now(),
        archivedAt: null,
        versions: [],
        attachments: [],
      };
      c.versions = [{ version: 1, source: 'ui', changed: ['created'], createdBy: ME, createdAt: c.createdAt, snapshot: snapshotOf(c) }];
      st.cases.set(c.number, c);
      audit('case.create', key, { number: c.number });
      return withEtag(caseView(c, key), c.revision, 201);
    }),

    http.get(`${P}/cases/:n`, ({ params }) => {
      const c = caseOf(String(params.key), String(params.n));
      return c ? withEtag(caseView(c, String(params.key)), c.revision) : notFound('case');
    }),

    http.put(`${P}/cases/:n`, async ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const pre = precondition(request, c.revision, c.updatedBy, lastSource(c));
      if (pre) return pre;
      const b = (await request.json()) as CaseWrite;
      const errs = validateWrite(b, c);
      if (errs.length) return error(400, 'invalid_case', errs[0]?.message ?? 'invalid case', errs);
      const next = snapshotOf({ name: b.name.trim(), status: b.status ?? c.status, folderId: b.folderId === undefined ? c.folderId : b.folderId, body: b.body, fields: b.fields ?? c.fields, tags: b.tags ?? c.tags });
      const code = codeViolations(c, next);
      if (code.length) return error(409, 'field_from_code', 'these fields come from code', { fields: code });
      const changed = changedKeys(snapshotOf(c), next);
      Object.assign(c, { name: next.name, status: next.status, folderId: next.folderId, body: next.body, fields: next.fields, tags: next.tags });
      if (changed.length) newVersion(c, 'ui', changed, ME);
      audit('case.update', key, { number: c.number, changed });
      return withEtag(caseView(c, key), c.revision);
    }),

    http.patch(`${P}/cases/:n`, async ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const pre = precondition(request, c.revision, c.updatedBy, lastSource(c));
      if (pre) return pre;
      const b = (await request.json()) as CasePatch;
      const next = snapshotOf({ ...c, folderId: b.folderId === undefined ? c.folderId : b.folderId, status: b.status ?? c.status, fields: b.fields ? { ...c.fields, ...b.fields } : c.fields, tags: b.tags ?? c.tags });
      const code = codeViolations(c, next);
      if (code.length) return error(409, 'field_from_code', 'these fields come from code', { fields: code });
      const changed = changedKeys(snapshotOf(c), next);
      Object.assign(c, { folderId: next.folderId, status: next.status, fields: next.fields, tags: next.tags });
      if (changed.length) newVersion(c, 'ui', changed, ME);
      return withEtag(caseView(c, key), c.revision);
    }),

    http.post(`${P}/cases/:n/review`, ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const pre = precondition(request, c.revision, c.updatedBy, lastSource(c));
      if (pre) return pre;
      if (c.aiUnreviewed) {
        c.aiUnreviewed = false;
        c.revision++;
        c.updatedAt = now();
        c.updatedBy = ME;
        audit('case.review', key, { number: c.number });
      }
      return withEtag(caseView(c, key), c.revision);
    }),

    http.delete(`${P}/cases/:n`, ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      if (new URL(request.url).searchParams.get('purge') === 'true') {
        st.cases.delete(c.number);
        audit('case.purge', key, { number: c.number });
        return new HttpResponse(null, { status: 204 });
      }
      c.archivedAt = now();
      c.revision++;
      audit('case.archive', key, { number: c.number });
      emit('case.archived', { number: c.number });
      return new HttpResponse(null, { status: 204 });
    }),

    http.post(`${P}/cases/:n/restore`, ({ params }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      c.archivedAt = null;
      c.revision++;
      audit('case.restore', key, { number: c.number });
      return withEtag(caseView(c, key), c.revision);
    }),

    http.get(`${P}/cases/:n/versions`, ({ params, request }) => {
      const c = caseOf(String(params.key), String(params.n));
      if (!c) return notFound('case');
      const url = new URL(request.url);
      const list = [...c.versions].reverse().map((v) => ({ version: v.version, source: v.source, changed: v.changed, createdBy: v.createdBy, createdAt: v.createdAt, ...(v.launchId ? { launchId: v.launchId } : {}) }));
      return HttpResponse.json(pageOf(list, url, (v) => String(v.version), 50));
    }),

    http.get(`${P}/cases/:n/versions/:v`, ({ params }) => {
      const c = caseOf(String(params.key), String(params.n));
      const v = c?.versions.find((x) => String(x.version) === params.v);
      return v ? HttpResponse.json(v) : notFound('version');
    }),

    http.post(`${P}/cases/:n/versions/:v/restore`, ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      const v: CaseVersion | undefined = c?.versions.find((x) => String(x.version) === params.v);
      if (!c || !v) return notFound('version');
      if (!writer()) return forbid('cases:write');
      const pre = precondition(request, c.revision, c.updatedBy, lastSource(c));
      if (pre) return pre;
      const snap = structuredClone(v.snapshot);
      const changed = changedKeys(snapshotOf(c), snap);
      Object.assign(c, { name: snap.name, status: snap.status, folderId: snap.folderId, body: snap.body, fields: snap.fields, tags: snap.tags });
      newVersion(c, 'restore', changed.length ? changed : ['restore'], ME);
      audit('case.update', key, { number: c.number, restoredFrom: v.version });
      return withEtag(caseView(c, key), c.revision);
    }),

    http.get(`${P}/cases/:n/history`, ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      const url = new URL(request.url);
      const branch = url.searchParams.get('branch') ?? undefined;
      const environment = url.searchParams.get('environment') ?? undefined;
      const source = url.searchParams.get('source');
      const items: CaseHistoryEntry[] = [];
      if (source !== 'manual') {
        for (const id of c.identities) {
          for (const h of testCaseHistory(key, id.testCaseId, branch, environment, 100)) {
            const def = launchDef(h.launchId);
            items.push({ launchId: h.launchId, launchNumber: h.launchNumber, launchName: def?.launch.name ?? '', resultId: h.resultId, status: h.status, source: 'automated', at: h.startedAt ?? '', branch: h.branch ?? null, environment: h.environment ?? null });
          }
        }
      }
      if (source !== 'automated') {
        for (const r of st.runItems) {
          if (r.caseNumber !== c.number || !r.resultId) continue;
          const l = manualLaunchView(r.launchId);
          if (branch && l?.vcs?.branch !== branch) continue;
          if (environment && l?.environment?.key !== environment) continue;
          items.push({ launchId: r.launchId, launchNumber: l?.number ?? 0, launchName: l?.name ?? '', resultId: r.resultId, status: r.status === 'blocked' ? 'broken' : r.status, source: 'manual', at: r.finishedAt ?? r.updatedAt, branch: l?.vcs?.branch ?? null, environment: l?.environment?.key ?? null, ...(r.executedBy ? { executedBy: r.executedBy } : {}) });
        }
      }
      items.sort((a, b) => b.at.localeCompare(a.at));
      return HttpResponse.json({ items: items.slice(0, 100) });
    }),

    http.post(`${P}/cases/:n/identities`, async ({ params, request }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const b = (await request.json()) as { testCaseId?: string };
      const ident = st.identities.find((i) => i.testCaseId === b.testCaseId);
      if (!ident) return notFound('identity');
      if (ident.caseNumber !== null && ident.caseNumber !== c.number) return error(409, 'conflict', `the test is linked to TC-${String(ident.caseNumber)}`);
      ident.caseNumber = c.number;
      if (!c.identities.some((i) => i.testCaseId === ident.testCaseId)) c.identities.push({ testCaseId: ident.testCaseId, fullName: ident.fullName, kind: ident.kind, lastSeenAt: ident.lastSeenAt, branches: ident.branches });
      c.revision++;
      audit('case.linkIdentity', key, { number: c.number, testCaseId: ident.testCaseId });
      return withEtag(caseView(c, key), c.revision);
    }),

    http.delete(`${P}/cases/:n/identities/:tid`, ({ params }) => {
      const key = String(params.key);
      const c = caseOf(key, String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const idx = c.identities.findIndex((i) => i.testCaseId === params.tid);
      if (idx < 0) return notFound('identity');
      const [gone] = c.identities.splice(idx, 1);
      c.revision++;
      if (gone) {
        const known = st.identities.find((i) => i.testCaseId === gone.testCaseId);
        if (known) known.caseNumber = null;
        else st.identities.push({ testCaseId: gone.testCaseId, fullName: gone.fullName, displayName: gone.fullName.split(/[ .>]+/).pop() ?? gone.fullName, kind: gone.kind, caseNumber: null, firstSeenAt: gone.lastSeenAt, lastSeenAt: gone.lastSeenAt, branches: gone.branches });
      }
      audit('case.unlinkIdentity', key, { number: c.number, testCaseId: params.tid });
      return new HttpResponse(null, { status: 204 });
    }),

    http.get(`${P}/test-identities`, ({ params, request }) => {
      if (!tmProject(String(params.key))) return HttpResponse.json({ items: [], next: null });
      const url = new URL(request.url);
      const q = url.searchParams.get('q')?.toLowerCase();
      const branch = url.searchParams.get('branch');
      const unlinked = url.searchParams.get('unlinked') === 'true';
      const list = st.identities.filter((i) => (!unlinked || i.caseNumber === null) && (!q || i.fullName.toLowerCase().includes(q)) && (!branch || i.branches.includes(branch)));
      return HttpResponse.json(pageOf(list, url, (i) => i.testCaseId, 50));
    }),

    http.post(`${P}/cases/:n/attachments`, async ({ params, request }) => {
      const c = caseOf(String(params.key), String(params.n));
      if (!c) return notFound('case');
      if (!writer()) return forbid('cases:write');
      const buf = await request.arrayBuffer();
      if (buf.byteLength > 52_428_800) return error(413, 'payload_too_large', 'attachment over 50 MiB');
      return HttpResponse.json(await storeAttachment(request, buf, (id) => c.attachments.push(id)), { status: 201 });
    }),

    http.get(`${P}/attachments/:id/content`, ({ params }) => {
      const a = st.attachments.get(String(params.id));
      if (!a) return undefined;
      return new HttpResponse(a.body, { headers: { 'Content-Type': a.contentType, 'X-Content-Type-Options': 'nosniff', 'Content-Security-Policy': "sandbox; default-src 'none'" } });
    }),

    http.post(`${P}/markdown/preview`, async ({ params, request }) => {
      const b = (await request.json()) as { markdown?: unknown };
      if (typeof b.markdown !== 'string') return error(400, 'invalid_markdown', 'markdown must be a string');
      if (b.markdown.length > 65_536) return error(400, 'invalid_markdown', 'over 64 KiB');
      return HttpResponse.json({ html: renderMarkdown(b.markdown, String(params.key)) });
    }),

    // Folders and fields

    http.get(`${P}/case-folders`, ({ params, request }) => {
      const q = new URL(request.url).searchParams;
      const parent = q.get('parent');
      const all = tmProject(String(params.key)) ? folderList() : [];
      const order = (a: CaseFolder, b: CaseFolder) => a.position - b.position || a.name.localeCompare(b.name);
      if (q.get('all') === 'true') {
        if (parent) return error(400, 'bad_request', 'all=true lists the whole tree: leave out parent');
        return HttpResponse.json({ items: all.sort((a, b) => a.depth - b.depth || order(a, b)), truncated: false });
      }
      return HttpResponse.json({ items: all.filter((f) => f.parentId === parent).sort(order) });
    }),

    http.post(`${P}/case-folders`, async ({ params, request }) => {
      const key = String(params.key);
      if (!tmProject(key)) return notFound('project');
      if (!writer()) return forbid('cases:write');
      const b = (await request.json()) as { name?: string; parentId?: string | null; position?: number };
      const name = (b.name ?? '').trim();
      if (!name || name.length > 500) return error(400, 'invalid_case', 'name must be 1-500 bytes', [{ path: 'name', message: 'Required' }]);
      const parentId = b.parentId ?? null;
      if (parentId && !st.folders.some((f) => f.id === parentId)) return notFound('folder');
      if (parentId && folderDepth(parentId) >= 9) return error(409, 'limit_reached', 'folders nest at most 10 levels', { limit: 'max_folder_depth', max: 10 });
      if (st.folders.some((f) => f.parentId === parentId && f.name.toLowerCase() === name.toLowerCase())) return error(409, 'folder_name_taken', `a folder named ${name} already exists here`);
      const f = { id: tmId(`folder:new:${String(Date.now())}:${String(Math.random())}`), parentId, name, position: b.position ?? st.folders.filter((x) => x.parentId === parentId).length };
      st.folders.push(f);
      audit('caseFolder.create', key, { id: f.id, name });
      return HttpResponse.json(folderList().find((x) => x.id === f.id), { status: 201 });
    }),

    http.patch(`${P}/case-folders/:id`, async ({ params, request }) => {
      const key = String(params.key);
      const f = tmProject(key) ? st.folders.find((x) => x.id === params.id) : undefined;
      if (!f) return notFound('folder');
      if (!writer()) return forbid('cases:write');
      const b = (await request.json()) as { name?: string; parentId?: string | null; position?: number };
      if (b.parentId !== undefined) {
        if (b.parentId && subtreeIds(st, f.id).has(b.parentId)) return error(400, 'invalid_case', 'a folder cannot move into itself', [{ path: 'parentId', message: 'Cycle' }]);
        f.parentId = b.parentId;
      }
      if (b.name !== undefined) {
        const name = b.name.trim();
        if (st.folders.some((x) => x.id !== f.id && x.parentId === f.parentId && x.name.toLowerCase() === name.toLowerCase())) return error(409, 'folder_name_taken', `a folder named ${name} already exists here`);
        f.name = name;
      }
      if (b.position !== undefined) f.position = b.position;
      audit('caseFolder.update', key, { id: f.id });
      return HttpResponse.json(folderList().find((x) => x.id === f.id));
    }),

    http.delete(`${P}/case-folders/:id`, ({ params, request }) => {
      const key = String(params.key);
      const f = tmProject(key) ? st.folders.find((x) => x.id === params.id) : undefined;
      if (!f) return notFound('folder');
      if (!writer()) return forbid('cases:write');
      const mode = new URL(request.url).searchParams.get('mode');
      const ids = subtreeIds(st, f.id);
      const cases = [...st.cases.values()].filter((c) => c.folderId && ids.has(c.folderId) && !c.archivedAt);
      const children = st.folders.filter((x) => x.parentId === f.id);
      if (!mode && (cases.length || children.length)) return error(409, 'folder_not_empty', 'the folder holds cases or folders; choose what happens to them', { cases: cases.length, folders: children.length });
      if (mode === 'moveUp') {
        for (const c of st.cases.values()) if (c.folderId === f.id) c.folderId = f.parentId;
        for (const ch of children) ch.parentId = f.parentId;
        st.folders = st.folders.filter((x) => x.id !== f.id);
      } else {
        for (const c of cases) c.archivedAt = now();
        st.folders = st.folders.filter((x) => !ids.has(x.id));
        for (const c of st.cases.values()) if (c.folderId && ids.has(c.folderId)) c.folderId = null;
      }
      audit('caseFolder.delete', key, { id: f.id, mode });
      return new HttpResponse(null, { status: 204 });
    }),

    http.get(`${P}/case-fields`, ({ params, request }) => {
      const archived = new URL(request.url).searchParams.get('archived') === 'true';
      const list = tmProject(String(params.key)) ? st.fields.filter((f) => archived || !f.archived) : [];
      return HttpResponse.json({ items: [...list].sort((a, b) => a.position - b.position || a.key.localeCompare(b.key)) });
    }),

    http.post(`${P}/case-fields`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const b = (await request.json()) as Partial<CaseFieldDef>;
      if (!b.key || !/^[a-z][a-zA-Z0-9_]{0,62}$/.test(b.key) || !b.name || !b.type) return error(400, 'invalid_case', 'key, name and type are required', [{ path: 'key', message: 'Required' }]);
      if (st.fields.some((f) => f.key === b.key)) return error(409, 'conflict', 'a field with this key exists');
      const f: CaseFieldDef = { id: tmId(`field:${b.key}`), key: b.key, name: b.name, type: b.type, options: b.options ?? [], required: !!b.required, defaultValue: b.defaultValue ?? null, sourceLabel: b.sourceLabel ?? null, builtIn: false, position: st.fields.length, archived: false };
      st.fields.push(f);
      audit('caseField.create', String(params.key), { key: f.key });
      return HttpResponse.json(f, { status: 201 });
    }),

    http.patch(`${P}/case-fields/:id`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const f = st.fields.find((x) => x.id === params.id);
      if (!f) return notFound('field');
      const b = (await request.json()) as Partial<CaseFieldDef>;
      if (b.type && b.type !== f.type) return error(400, 'invalid_case', 'the type of a field cannot change', [{ path: 'type', message: 'Immutable' }]);
      Object.assign(f, { ...b, id: f.id, key: f.key, type: f.type, builtIn: f.builtIn });
      return HttpResponse.json(f);
    }),

    http.delete(`${P}/case-fields/:id`, ({ params }) => {
      if (!writer()) return forbid('settings:write');
      const f = st.fields.find((x) => x.id === params.id);
      if (!f) return notFound('field');
      f.archived = true;
      return new HttpResponse(null, { status: 204 });
    }),

    // Suggestions

    http.get(`${P}/cases/:n/suggestions`, ({ params }) => {
      const c = caseOf(String(params.key), String(params.n));
      if (!c) return notFound('case');
      return HttpResponse.json({ items: st.suggestions.filter((s) => s.caseNumber === c.number) });
    }),

    http.post(`${P}/case-suggestions/:id/:decision`, ({ params, request }) => {
      const key = String(params.key);
      const s = tmProject(key) ? st.suggestions.find((x) => x.id === params.id) : undefined;
      const c = s ? st.cases.get(s.caseNumber) : undefined;
      if (!s || !c) return notFound('suggestion');
      if (!writer()) return forbid('cases:write');
      if (s.status !== 'pending') return error(409, 'suggestion_stale', `the suggestion is already ${s.status}`);
      if (params.decision === 'reject') {
        Object.assign(s, { status: 'rejected', decidedBy: ME, decidedAt: now() });
        audit('caseSuggestion.reject', key, { id: s.id });
        return HttpResponse.json(s);
      }
      if (params.decision !== 'accept') return notFound('route');
      const pre = precondition(request, c.revision, c.updatedBy, lastSource(c));
      if (pre) return pre;
      if (c.revision !== s.baseRevision) {
        s.status = 'stale';
        return error(409, 'suggestion_stale', 'the case changed after the suggestion was made');
      }
      const before = snapshotOf(c);
      const p = s.proposal;
      if (p.name) c.name = p.name;
      if (p.body) c.body = structuredClone(p.body);
      if (p.tags) c.tags = [...p.tags];
      if (p.fields) c.fields = { ...c.fields, ...p.fields };
      if (p.status) c.status = p.status;
      newVersion(c, 'ai', changedKeys(before, snapshotOf(c)), ME);
      Object.assign(s, { status: 'accepted', decidedBy: ME, decidedAt: now() });
      audit('caseSuggestion.accept', key, { id: s.id, source: 'ai', agent: s.agent });
      emit('case.updated', { number: c.number, revision: c.revision, source: 'ai' });
      return HttpResponse.json(s);
    }),

    // Plans

    http.get(`${P}/test-plans`, ({ params, request }) => {
      const url = new URL(request.url);
      if (url.searchParams.get('kind') !== 'managed') return undefined;
      if (!tmProject(String(params.key))) return HttpResponse.json({ items: [] });
      const q = url.searchParams.get('q')?.toLowerCase();
      const archived = url.searchParams.get('archived') === 'true';
      return HttpResponse.json({ items: st.plans.filter((p) => p.archived === archived && (!q || p.name.toLowerCase().includes(q))).map(planView) });
    }),

    http.post(`${P}/test-plans`, async ({ params, request }) => {
      const b = (await request.clone().json()) as TestPlanWrite & { kind?: string };
      if (b.kind !== 'managed') return undefined;
      const key = String(params.key);
      if (!tmProject(key)) return notFound('project');
      if (!writer()) return forbid('plans:write');
      if (!b.name.trim() || b.name.length > 200) return error(400, 'invalid_body', 'name must be 1-200 characters', { field: 'name' });
      const numbers = new Set(b.caseNumbers ?? []);
      const items = [...numbers].filter((n) => st.cases.has(n)).map((n, i) => ({ caseNumber: n, assignee: null, viaFilter: false, position: i }));
      const filter = b.filter ?? null;
      if (filter) for (const c of st.cases.values()) if (matchesFilter(c, filter) && !numbers.has(c.number)) items.push({ caseNumber: c.number, assignee: null, viaFilter: true, position: items.length });
      const plan: MockPlan = { id: tmId(`plan:${String(Date.now())}:${String(Math.random())}`), kind: 'managed', name: b.name.trim(), description: b.description ?? '', filter, counts: { total: 0, manual: 0, automated: 0 }, revision: 1, createdBy: ME, createdAt: now(), updatedAt: null, archived: false, items };
      st.plans.unshift(plan);
      audit('testPlan.create', key, { id: plan.id, name: plan.name });
      return withEtag(planView(plan), plan.revision, 201);
    }),

    http.get(`${P}/test-plans/:id/testplan.json`, ({ params }) => {
      const p = st.plans.find((x) => x.id === params.id);
      if (!p) return undefined;
      const json = testplanJson(p.items.map((i) => i.caseNumber));
      if (!json.tests.length) return error(409, 'plan_has_no_automated_cases', 'the plan has no automated cases');
      return HttpResponse.json(json, { headers: { 'Content-Disposition': 'attachment; filename="testplan.json"' } });
    }),

    http.get(`${P}/test-plans/:id/items`, ({ params, request }) => {
      const p = st.plans.find((x) => x.id === params.id);
      if (!p || !tmProject(String(params.key))) return notFound('test plan');
      const url = new URL(request.url);
      const folder = url.searchParams.get('folder');
      const assignee = url.searchParams.get('assignee');
      const list = planItems(p).filter((i) => (!folder || i.folderId === folder) && (!assignee || i.assignee === (assignee === 'me' ? ME : assignee)));
      return HttpResponse.json(pageOf(list, url, (i) => String(i.caseNumber), 50));
    }),

    http.post(`${P}/test-plans/:id/items`, async ({ params, request }) => {
      const key = String(params.key);
      const p = st.plans.find((x) => x.id === params.id);
      if (!p || !tmProject(key)) return notFound('test plan');
      if (!writer()) return forbid('plans:write');
      const b = (await request.json()) as TestPlanItemsChange;
      if ((b.add?.length ?? 0) + (b.remove?.length ?? 0) + (b.assign?.length ?? 0) > 10_000) return error(409, 'limit_reached', 'at most 10000 changes per request', { limit: 'max_bulk', max: 10_000 });
      const remove = new Set(b.remove ?? []);
      p.items = p.items.filter((i) => !remove.has(i.caseNumber));
      for (const n of b.add ?? []) {
        if (!st.cases.has(n)) return notFound('case');
        if (!p.items.some((i) => i.caseNumber === n)) p.items.push({ caseNumber: n, assignee: null, viaFilter: false, position: p.items.length });
      }
      if (p.items.length > 50_000) return error(409, 'limit_reached', 'too many cases in the plan', { limit: 'max_plan_cases', max: 50_000 });
      for (const a of b.assign ?? []) {
        const it = p.items.find((i) => i.caseNumber === a.number);
        if (it) it.assignee = a.assignee ?? null;
      }
      p.revision++;
      p.updatedAt = now();
      audit('testPlan.items', key, { id: p.id, add: b.add?.length ?? 0, remove: remove.size, assign: b.assign?.length ?? 0 });
      return HttpResponse.json(planView(p));
    }),

    http.post(`${P}/test-plans/:id/sync`, ({ params, request }) => {
      const key = String(params.key);
      const p = st.plans.find((x) => x.id === params.id);
      if (!p || !tmProject(key)) return notFound('test plan');
      if (!writer()) return forbid('plans:write');
      const diff = syncDiff(p);
      if (new URL(request.url).searchParams.get('dryRun') !== 'false') return HttpResponse.json(diff);
      const removed = new Set(diff.removed);
      p.items = p.items.filter((i) => !removed.has(i.caseNumber));
      for (const n of diff.added) p.items.push({ caseNumber: n, assignee: null, viaFilter: true, position: p.items.length });
      p.revision++;
      p.updatedAt = now();
      audit('testPlan.sync', key, { id: p.id, added: diff.added.length, removed: diff.removed.length });
      return HttpResponse.json(diff);
    }),

    http.post(`${P}/test-plans/:id/launches`, async ({ params, request }) => {
      const key = String(params.key);
      const p = st.plans.find((x) => x.id === params.id);
      if (!p || !tmProject(key)) return notFound('test plan');
      if (!writer()) return forbid('runs:execute');
      const b = (await request.json()) as StartPlanLaunch;
      if (!b.name.trim()) return error(400, 'invalid_body', 'name is required', { field: 'name' });
      const defs = projectLaunches(key);
      const template = defs[defs.length - 1];
      if (!template) return notFound('project');
      const number = Math.max(...defs.map((d) => d.launch.number), ...[...st.createdLaunches.values()].map((l) => l.number)) + 1;
      const id = tmId(`launch:created:${String(number)}`);
      const base = launchView(template);
      const empty = { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };
      const launch = {
        ...base,
        id,
        number,
        name: b.name.trim(),
        status: 'running' as const,
        startedAt: now(),
        finishedAt: null,
        durationMs: null,
        counts: { ...base.counts, ...empty },
        environment: b.environment ? { key: Object.values(b.environment)[0] ?? 'manual', values: b.environment } : base.environment,
        attributes: [...(b.attributes ?? []), ...(b.externalId ? [{ key: 'externalId', value: b.externalId }] : [])],
        uploads: { queued: 0, processing: 0, failed: 0 },
        important: false,
        debug: false,
      };
      st.createdLaunches.set(id, launch);
      st.manual.set(id, { launchId: id, planId: p.id, state: 'open' });
      let i = 0;
      for (const it of p.items) {
        const c = st.cases.get(it.caseNumber);
        if (!c || isAutomated(c)) continue;
        st.runItems.push(runItemFor(st, id, c, b.assign === 'plan' ? it.assignee : b.assign === 'me' ? ME : null, i++));
      }
      audit('run.start', key, { launchId: id, planId: p.id });
      const externalId = b.externalId ?? `fastproof-${String(number)}`;
      const body: PlanLaunch = {
        launch: manualLaunchView(id) ?? launch,
        testplanUrl: `${API}/projects/${key}/launches/${id}/testplan.json`,
        ciHint: `fastproof testplan --launch ${externalId} > testplan.json\nexport ALLURE_TESTPLAN_PATH="$PWD/testplan.json"`,
        launches: [manualLaunchView(id) ?? launch],
        dispatches: [],
        skips: [],
      };
      return HttpResponse.json(body, { status: 201 });
    }),

    http.get(`${P}/test-plans/:id/launches`, ({ params }) => {
      const p = tmProject(String(params.key)) ? st.plans.find((x) => x.id === params.id) : undefined;
      if (!p) return notFound('test plan');
      const items = [...st.manual.values()]
        .filter((m) => m.planId === p.id)
        .map((m) => manualLaunchView(m.launchId))
        .filter((l): l is Launch => !!l)
        .sort((a, b) => b.number - a.number);
      return HttpResponse.json({ items, next: null });
    }),

    http.get(`${P}/people`, ({ params, request }) => {
      if (!tmProject(String(params.key))) return HttpResponse.json({ items: [] });
      return HttpResponse.json({ items: peopleOf(new URL(request.url).searchParams.get('q') ?? '') });
    }),

    http.get(`${P}/test-plans/:id`, ({ params }) => {
      const p = st.plans.find((x) => x.id === params.id);
      if (!p) return undefined;
      return withEtag(planView(p), p.revision);
    }),

    http.patch(`${P}/test-plans/:id`, async ({ params, request }) => {
      const key = String(params.key);
      const p = st.plans.find((x) => x.id === params.id);
      if (!p) return undefined;
      if (!writer()) return forbid('plans:write');
      const pre = precondition(request, p.revision, p.createdBy ?? null, 'ui');
      if (pre) return pre;
      const b = (await request.json()) as Partial<TestPlanWrite>;
      if (b.name) p.name = b.name.trim();
      if (b.description !== undefined) p.description = b.description ?? '';
      if (b.filter !== undefined) p.filter = b.filter;
      p.revision++;
      p.updatedAt = now();
      audit('testPlan.update', key, { id: p.id });
      return withEtag(planView(p), p.revision);
    }),

    http.delete(`${P}/test-plans/:id`, ({ params }) => {
      const p = st.plans.find((x) => x.id === params.id);
      if (!p) return undefined;
      if (!writer()) return forbid('plans:write');
      p.archived = true;
      p.revision++;
      audit('testPlan.archive', String(params.key), { id: p.id });
      return new HttpResponse(null, { status: 204 });
    }),

    // Launch additions

    http.get(`${P}/launches/:id/testplan.json`, ({ params }) => {
      const m = st.manual.get(String(params.id));
      const p = m?.planId ? st.plans.find((x) => x.id === m.planId) : undefined;
      if (!p) return undefined;
      return HttpResponse.json(testplanJson(p.items.map((i) => i.caseNumber)));
    }),

    http.get(`${P}/launches/:id`, ({ params }) => {
      const view = String(params.key) === TM_PROJECT ? manualLaunchView(String(params.id)) : null;
      return view ? HttpResponse.json(view) : undefined;
    }),

    http.get(`${P}/launches/:id/results`, ({ params, request }) => {
      const key = String(params.key);
      const id = String(params.id);
      if (!st.manual.has(id)) return undefined;
      const url = new URL(request.url);
      const base = launchResults(id).map((r) => ({ ...analyze(key, r), source: 'automated' as const }));
      const template = base[0] ?? launchResults(projectLaunches(key)[1]?.launch.id ?? '')[0];
      const manual = st.runItems.filter((r) => r.launchId === id && r.resultId).map((r) => manualResult(r, template));
      const status = url.searchParams.get('status');
      const q = url.searchParams.get('q')?.toLowerCase();
      const source = url.searchParams.get('source');
      const label = url.searchParams.get('label');
      const all = [...manual, ...base].filter(
        (r) =>
          (!status || status.split(',').includes(r.status)) &&
          (!q || `${r.name} ${r.fullName}`.toLowerCase().includes(q)) &&
          (!source || r.source === source) &&
          (!label || r.labels.some((l) => `${l.name}:${l.value}` === label || l.name === label)),
      );
      return HttpResponse.json(pageOf(all, url, (r) => r.id, 50));
    }),

    // Manual runs

    http.post(`${P}/launches/:id/run-items`, async ({ params, request }) => {
      const key = String(params.key);
      const id = String(params.id);
      if (!tmProject(key) || (!launchDef(id) && !st.createdLaunches.has(id))) return notFound('launch');
      if (!writer()) return forbid('runs:execute');
      const b = (await request.json()) as { caseNumbers?: number[]; planId?: string; assignee?: string | null };
      const numbers = [...(b.caseNumbers ?? [])];
      if (b.planId) {
        const p = st.plans.find((x) => x.id === b.planId);
        if (!p) return notFound('test plan');
        numbers.push(...p.items.map((i) => i.caseNumber));
      }
      const m = st.manual.get(id);
      if (m?.state === 'closed') return error(409, 'manual_run_closed', 'reopen the manual run first');
      st.manual.set(id, m ?? { launchId: id, planId: b.planId ?? null, state: 'open' });
      let added = 0;
      let skipped = 0;
      for (const n of new Set(numbers)) {
        const c = st.cases.get(n);
        if (!c || isAutomated(c) || st.runItems.some((r) => r.launchId === id && r.caseNumber === n)) {
          skipped++;
          continue;
        }
        st.runItems.push(runItemFor(st, id, c, b.assignee ?? null, added));
        added++;
      }
      return HttpResponse.json({ added, skipped });
    }),

    http.get(`${P}/launches/:id/run-items`, ({ params, request }) => {
      const id = String(params.id);
      const url = new URL(request.url);
      const status = url.searchParams.get('status');
      const assignee = url.searchParams.get('assignee');
      const folder = url.searchParams.get('folder');
      const list = st.runItems.filter(
        (r) =>
          r.launchId === id &&
          (!status || status.split(',').includes(r.status)) &&
          (!assignee || r.assignee === (assignee === 'me' ? ME : assignee === 'none' ? null : assignee)) &&
          (!folder || (st.cases.get(r.caseNumber)?.folderId ?? '') === folder),
      );
      return HttpResponse.json({ ...pageOf(list.map(runSummary), url, (r) => r.id, 50) });
    }),

    http.get(`${P}/launches/:id/manual-report`, ({ params, request }) => {
      const id = String(params.id);
      if (!st.manual.has(id)) return notFound('manual run');
      const items = st.runItems.filter((r) => r.launchId === id);
      if (new URL(request.url).searchParams.get('format') === 'json') {
        const launch = manualLaunchView(id);
        const steps = (r: MockRunItem) => {
          const res = new Map(r.stepResults.map((x) => [x.path, x] as const));
          const flat: { path: string; step: CaseStep }[] = [];
          const walk = (list: CaseStep[], prefix: string) =>
            list.forEach((s2, i) => {
              const path = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
              flat.push({ path, step: s2 });
              walk(s2.steps, path);
            });
          walk(r.steps, '');
          const status = new Map(r.stepResults.map((x) => [x.path, x.status] as const));
          return flat.map(({ path, step }) => ({ path, action: step.action, expected: step.expected, status: effective(step, path, status), actual: res.get(path)?.actual ?? '', comment: res.get(path)?.comment ?? '' }));
        };
        return HttpResponse.json({
          launchId: id,
          launchNumber: launch?.number ?? 0,
          launchName: launch?.name ?? '',
          items: items.map((r) => ({ caseNumber: r.caseNumber, name: r.name, caseVersion: r.caseVersion, status: r.status, assignee: r.assignee, executedBy: r.executedBy, executionNo: r.executionNo, startedAt: r.startedAt, finishedAt: r.finishedAt, comment: r.comment, defect: r.defect, steps: steps(r) })),
        });
      }
      const lines = ['Case,Name,Version,Status,Assignee,Executed by,Finished,Failed steps', ...items.map((r) => [`TC-${String(r.caseNumber)}`, r.name, String(r.caseVersion), r.status, r.assignee ?? '', r.executedBy ?? '', r.finishedAt ?? '', r.stepResults.filter((s) => s.status === 'failed').map((s) => s.path).join(' ')].map(csvCell).join(','))];
      return new HttpResponse(`${lines.join('\r\n')}\r\n`, { headers: { 'Content-Type': 'text/csv; charset=utf-8', 'Content-Disposition': 'attachment; filename="manual-report.csv"' } });
    }),

    http.post(`${P}/launches/:id/manual/:action`, async ({ params, request }) => {
      const key = String(params.key);
      const id = String(params.id);
      const m = st.manual.get(id);
      if (!m) return notFound('manual run');
      if (!writer()) return forbid(params.action === 'reopen' ? 'plans:write' : 'runs:execute');
      if (params.action === 'close') {
        const b = (await request.json().catch(() => ({}))) as { untestedTo?: string };
        if (b.untestedTo === 'skipped') {
          for (const r of st.runItems) {
            if (r.launchId !== id || FINAL.includes(r.status)) continue;
            r.status = 'skipped';
            r.revision++;
            r.updatedAt = now();
            project(r);
          }
        }
        m.state = 'closed';
        const created = st.createdLaunches.get(id);
        if (created) st.createdLaunches.set(id, { ...created, status: 'finished', finishedAt: now() });
        audit('manualRun.close', key, { launchId: id });
      } else if (params.action === 'reopen') {
        m.state = 'open';
        audit('manualRun.reopen', key, { launchId: id });
      } else return notFound('route');
      const manual = manualCounts(id);
      emit('manualRun.updated', { launchId: id, manual });
      return HttpResponse.json(manual);
    }),

    http.get(`${P}/run-items`, ({ params, request }) => {
      if (!tmProject(String(params.key))) return HttpResponse.json({ items: [], next: null });
      const url = new URL(request.url);
      const status = url.searchParams.get('status');
      const assignee = url.searchParams.get('assignee');
      const list = st.runItems.filter((r) => (!status || status.split(',').includes(r.status)) && (!assignee || r.assignee === (assignee === 'me' ? ME : assignee)) && st.manual.get(r.launchId)?.state !== 'closed');
      return HttpResponse.json(
        pageOf(
          list.map((r) => {
            const l = manualLaunchView(r.launchId);
            return { ...runSummary(r), launchId: r.launchId, launchNumber: l?.number ?? 0, launchName: l?.name ?? '' };
          }),
          url,
          (r) => r.id,
          50,
        ),
      );
    }),

    http.get(`${P}/run-items/:id`, ({ params }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r || !tmProject(String(params.key))) return notFound('run item');
      return withEtag(runItemView(r, String(params.key)), r.revision);
    }),

    http.put(`${P}/run-items/:id/steps/:path`, async ({ params, request }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r || !tmProject(String(params.key))) return notFound('run item');
      if (!writer()) return forbid('runs:execute');
      if (st.manual.get(r.launchId)?.state === 'closed') return error(409, 'manual_run_closed', 'reopen the manual run first');
      if (failSteps > 0) {
        failSteps--;
        return error(503, 'unavailable', 'the server is restarting');
      }
      const path = String(params.path);
      if (!stepExists(r.steps, path)) return notFound('step');
      const b = (await request.json()) as RunStepWrite;
      if (!['untested', 'passed', 'failed', 'blocked', 'skipped'].includes(b.status)) return error(400, 'invalid_case', 'unknown status', [{ path: 'status', message: 'Unknown' }]);
      if ((b.actual ?? '').length > 16384 || (b.comment ?? '').length > 16384) return error(400, 'invalid_markdown', 'over the size limit');
      const prev = r.stepResults.find((s) => s.path === path);
      const attachments = (b.attachmentIds ?? prev?.attachments.map((a) => a.id) ?? []).map((aid) => {
        const a = st.attachments.get(aid);
        return { id: aid, sha256: '', size: a?.body.byteLength ?? 0, contentType: a?.contentType ?? 'application/octet-stream', name: a?.name ?? aid };
      });
      if (attachments.length > 20) return error(409, 'limit_reached', 'at most 20 attachments per step', { limit: 'max_step_attachments', max: 20 });
      r.stepResults = r.stepResults.filter((s) => s.path !== path);
      if (b.status !== 'untested' || b.actual || b.comment || attachments.length) {
        r.stepResults.push({ path, status: b.status, actual: b.actual ?? prev?.actual ?? '', comment: b.comment ?? prev?.comment ?? '', attachments, updatedBy: ME, updatedAt: now() });
      }
      r.stepResults.sort((a, b2) => a.path.localeCompare(b2.path, undefined, { numeric: true }));
      if (!FINAL.includes(r.status)) r.status = r.stepResults.some((s) => s.status !== 'untested') ? 'in_progress' : 'untested';
      r.startedAt ??= now();
      r.executedBy = ME;
      r.revision++;
      r.updatedAt = now();
      emitRun(r);
      return HttpResponse.json({ itemStatus: r.status, suggestedStatus: suggested(r), revision: r.revision });
    }),

    http.patch(`${P}/run-items/:id`, async ({ params, request }) => {
      const key = String(params.key);
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r || !tmProject(key)) return notFound('run item');
      if (!writer()) return forbid('runs:execute');
      if (st.manual.get(r.launchId)?.state === 'closed') return error(409, 'manual_run_closed', 'reopen the manual run first');
      const pre = precondition(request, r.revision, r.executedBy, 'ui');
      if (pre) return pre;
      const b = (await request.json()) as RunItemPatch;
      if (b.untestedStepsTo === 'skipped') {
        const walk = (steps: CaseStep[], prefix: string) =>
          steps.forEach((s, i) => {
            const p = prefix ? `${prefix}.${String(i + 1)}` : String(i + 1);
            if (s.steps.length) walk(s.steps, p);
            else if (!r.stepResults.some((x) => x.path === p && x.status !== 'untested')) {
              r.stepResults = r.stepResults.filter((x) => x.path !== p);
              r.stepResults.push({ path: p, status: 'skipped', actual: '', comment: '', attachments: [], updatedBy: ME, updatedAt: now() });
            }
          });
        walk(r.steps, '');
      }
      if (b.status) {
        r.status = b.status;
        r.executedBy = ME;
        r.startedAt ??= now();
      }
      if (b.comment !== undefined) r.comment = b.comment ?? '';
      if (b.assignee !== undefined) r.assignee = b.assignee;
      project(r);
      r.revision++;
      r.updatedAt = now();
      if (b.status) audit('runItem.update', key, { id: r.id, status: r.status });
      emitRun(r);
      return withEtag(runItemView(r, key), r.revision);
    }),

    http.post(`${P}/run-items/:id/attachments`, async ({ params, request }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r) return notFound('run item');
      if (!writer()) return forbid('runs:execute');
      const buf = await request.arrayBuffer();
      if (buf.byteLength > 52_428_800) return error(413, 'payload_too_large', 'attachment over 50 MiB');
      return HttpResponse.json(await storeAttachment(request, buf), { status: 201 });
    }),

    http.post(`${P}/run-items/:id/defect`, async ({ params, request }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r) return notFound('run item');
      if (!writer()) return forbid('runs:execute');
      if (!r.resultId) return error(409, 'run_item_not_final', 'finish the case first: a defect is set on its result');
      const b = (await request.json()) as Record<string, unknown>;
      r.defect = { ...b, at: now(), by: ME };
      r.revision++;
      audit('runItem.update', String(params.key), { id: r.id, defect: true });
      return HttpResponse.json({ updated: r.resultId ? 1 : 0 });
    }),

    http.get(`${P}/run-items/:id/new-issue-url`, ({ params, request }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r) return notFound('run item');
      const tracker = new URL(request.url).searchParams.get('tracker');
      const failed = r.stepResults.find((s) => s.status === 'failed' || s.status === 'blocked');
      const step = failed ? stepExists(r.steps, failed.path) : null;
      const title = `TC-${String(r.caseNumber)} ${r.name}: ${failed ? `step ${failed.path} ${failed.status}` : 'failed'}`;
      const description = [`Step: ${step?.action ?? '-'}`, `Expected: ${step?.expected ?? '-'}`, `Actual: ${failed?.actual ?? '-'}`, `Run item: ${location.origin}/p/${String(params.key)}/launches/${r.launchId}/run/${r.id}`].join('\n\n');
      if (tracker === 'jira') {
        const jira = settingsOf(String(params.key)).issueTrackers.find((t) => t.type === 'jira') as { urlTemplate: string; projectId?: string | null; issueTypeId?: string | null } | undefined;
        const base = new URL(jira?.urlTemplate ?? 'https://jira.example.com/browse/{key}').origin;
        const prefill = { title, description };
        if (jira?.projectId && jira.issueTypeId) return HttpResponse.json({ url: `${base}/secure/CreateIssueDetails!init.jspa?${new URLSearchParams({ pid: jira.projectId, issuetype: jira.issueTypeId, summary: title, description }).toString()}`, prefilled: true, prefill });
        return HttpResponse.json({ url: `${base}/secure/CreateIssue!default.jspa`, prefilled: false, prefill });
      }
      const url = `https://gitlab.example.com/shop/web-shop/-/issues/new?${new URLSearchParams({ 'issue[title]': title, 'issue[description]': description }).toString()}`;
      return HttpResponse.json({ url, prefilled: true });
    }),

    http.post(`${P}/run-items/:id/rerun`, ({ params, request }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r) return notFound('run item');
      if (!writer()) return forbid('runs:execute');
      const pre = precondition(request, r.revision, r.executedBy, 'ui');
      if (pre) return pre;
      st.executions.push({ itemId: r.id, executionNo: r.executionNo, status: r.status, caseVersion: r.caseVersion, comment: r.comment, executedBy: r.executedBy, startedAt: r.startedAt, finishedAt: r.finishedAt, defect: r.defect, stepResults: structuredClone(r.stepResults), rerunBy: ME, rerunAt: now() });
      r.executionNo++;
      r.status = 'untested';
      r.stepResults = [];
      r.resultId = null;
      r.startedAt = null;
      r.finishedAt = null;
      r.revision++;
      emitRun(r);
      return withEtag(runItemView(r, String(params.key)), r.revision);
    }),

    http.get(`${P}/run-items/:id/executions`, ({ params }) => {
      const r = st.runItems.find((x) => x.id === params.id);
      if (!r || !tmProject(String(params.key))) return notFound('run item');
      const items = st.executions.filter((x) => x.itemId === r.id).sort((a, b) => b.executionNo - a.executionNo).map((x) => ({ executionNo: x.executionNo, status: x.status, caseVersion: x.caseVersion, comment: x.comment, executedBy: x.executedBy, startedAt: x.startedAt, finishedAt: x.finishedAt, defect: x.defect, stepResults: x.stepResults, rerunBy: x.rerunBy, rerunAt: x.rerunAt }));
      return HttpResponse.json({ items, next: null });
    }),

    // Import

    http.post(`${P}/imports/preview`, async ({ params, request }) => {
      if (!tmProject(String(params.key))) return notFound('project');
      if (!writer()) return forbid('cases:import');
      const { file } = await readFile(request);
      if (!file) return error(400, 'invalid_body', 'send the file as multipart field "file"');
      const text = await file.slice(0, 5 * 1024 * 1024).text();
      const preview = file.name.endsWith('.json') ? FALLBACK_PREVIEW : previewOf(parseCsv(text, 21));
      return HttpResponse.json(preview ?? FALLBACK_PREVIEW);
    }),

    http.post(`${P}/imports`, async ({ params, request }) => {
      const key = String(params.key);
      if (!tmProject(key)) return notFound('project');
      if (!writer()) return forbid('cases:import');
      if ([...st.jobs.values()].some((j) => j.kind === 'import' && (j.status === 'running' || j.status === 'dry_run'))) return error(409, 'import_busy', 'another import of this project is running');
      const { file, options } = await readFile(request);
      if (!file || !options) return error(400, 'invalid_body', 'send "file" and "options"');
      const rows = parseCsv(await file.text());
      const [headers = [], ...body] = file.name.endsWith('.json') ? [FALLBACK_PREVIEW.headers, ...FALLBACK_PREVIEW.rows] : rows;
      const id = tmId(`job:${String(Date.now())}:${String(Math.random())}`);
      const job = { id, kind: 'import' as const, status: 'dry_run' as TmJob['status'], progress: { done: 0, total: body.length }, report: null as ImportReport | null, createdBy: ME, createdAt: now(), finishedAt: null as string | null, rows: body, headers, options, tick: 0 };
      st.jobs.set(id, job);
      audit('import.start', key, { id, format: options.format });
      return HttpResponse.json(jobView(job), { status: 202 });
    }),

    http.get(`${P}/imports/:id`, ({ params }) => {
      const j = st.jobs.get(String(params.id));
      if (!j || j.kind !== 'import') return notFound('import');
      j.tick++;
      const rows = importRows(j.headers ?? [], j.rows ?? [], j.options ?? { format: 'generic-csv', mode: 'createOnly', unknownFieldValues: 'skip' });
      if (j.status === 'dry_run') {
        j.progress = { done: Math.min(j.progress.total ?? 0, (j.progress.total ?? 0) * (j.tick / 2)), total: j.progress.total };
        if (j.tick >= 2) {
          j.status = 'ready';
          j.report = importReport(rows, j.options ?? { format: 'generic-csv', mode: 'createOnly', unknownFieldValues: 'skip' });
          j.progress = { done: j.progress.total ?? 0, total: j.progress.total };
          emit('tmJob.updated', { id: j.id, kind: j.kind, status: j.status });
        }
      } else if (j.status === 'running') {
        const total = rows.length;
        const done = Math.min(total, j.progress.done + Math.max(1, Math.ceil(total / 3)));
        j.progress = { done, total };
        if (done >= total) {
          applyRows(rows, j.options ?? { format: 'generic-csv', mode: 'createOnly', unknownFieldValues: 'skip' });
          j.status = 'done';
          j.finishedAt = now();
          emit('tmJob.updated', { id: j.id, kind: j.kind, status: j.status });
        }
      }
      return HttpResponse.json(jobView(j));
    }),

    http.post(`${P}/imports/:id/apply`, ({ params }) => {
      const j = st.jobs.get(String(params.id));
      if (!j || j.kind !== 'import') return notFound('import');
      if (!writer()) return forbid('cases:import');
      if (j.status !== 'ready') return error(409, 'job_not_ready', `the import is ${j.status}; only a ready import can be applied`);
      j.status = 'running';
      j.progress = { done: 0, total: j.rows?.length ?? 0 };
      audit('import.apply', String(params.key), { id: j.id });
      return HttpResponse.json(jobView(j), { status: 202 });
    }),

    http.delete(`${P}/imports/:id`, ({ params }) => {
      const j = st.jobs.get(String(params.id));
      if (!j || j.kind !== 'import') return notFound('import');
      if (j.status === 'done' || j.status === 'failed' || j.status === 'cancelled') return error(409, 'job_finished', `the job is ${j.status}`);
      j.status = 'cancelled';
      j.finishedAt = now();
      audit('import.cancel', String(params.key), { id: j.id });
      return HttpResponse.json(jobView(j));
    }),
  ];
}

function safeDecode(v: string): string {
  try {
    return decodeURIComponent(v);
  } catch {
    return 'attachment';
  }
}

async function sha256(buf: ArrayBuffer): Promise<string> {
  const d = await crypto.subtle.digest('SHA-256', buf);
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, '0')).join('');
}

async function storeAttachment(request: Request, buf: ArrayBuffer, onStored?: (id: string) => void) {
  const contentType = request.headers.get('Content-Type') ?? 'application/octet-stream';
  const name = safeDecode(request.headers.get('X-FastProof-Attachment-Name') ?? 'attachment');
  const hash = await sha256(buf);
  const id = tmId(`attachment:${hash}`);
  st.attachments.set(id, { name, contentType, body: buf.slice(0) });
  onStored?.(id);
  return { id, sha256: hash, size: buf.byteLength, contentType, name };
}
