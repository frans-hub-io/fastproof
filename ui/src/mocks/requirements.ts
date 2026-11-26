// Mock of the requirements and coverage API (in-memory): a small tree with links to the mock cases.
import { http, HttpResponse, type HttpHandler } from 'msw';
import type { CaseRequirement, CoverageCounts, CoverageScopeView, Requirement, RequirementCoverage, RequirementLink, RequirementStatus, RequirementSummary } from '../api/requirements';
import { coverageStatusOf } from '../api/requirements';
import type { TmJob } from '../api/tm';
import type { WidgetData, WidgetSettings } from '../api/dashboards';
import { settingsOf } from './stageC';
import { renderMarkdown } from './tmMarkdown';
import { reviewHooks, tmState } from './tm';
import { ME, TM_PROJECT, type MockCase } from './tmFixtures';

const API = '/api/v1/projects/:key';
const MOCK_ROLE_KEY = 'fastproof-mock-role';
const MAX_DEPTH = 5;
const MAX_LINKS = 500;
const DAY = 86_400_000;

interface Req {
  number: number;
  title: string;
  description: string;
  type: string;
  status: RequirementStatus;
  parent: number | null;
  issueKey: string | null;
  url: string | null;
  trackerId: string | null;
  externalState: string | null;
  archivedAt: string | null;
  archivedBy: number | null;
  origin: Requirement['origin'];
  revision: number;
  createdAt: string;
  updatedAt: string;
  contentChangedAt: string;
  updatedBy: string;
}

interface Link {
  requirement: number;
  caseNumber: number;
  origin: RequirementLink['origin'];
  linkStatus: RequirementLink['linkStatus'];
  createdAt: string;
  createdBy: string;
}

interface Mem {
  reqs: Map<number, Req>;
  links: Link[];
  changed: Set<number>;
  imports: Map<string, { parent: number | null; query: string }>;
}

let mem: Mem | null = null;

export function resetRequirements(): void {
  mem = null;
}
reviewHooks.reset.push(resetRequirements);

function iso(offsetDays: number): string {
  return new Date(Date.now() - offsetDays * DAY).toISOString();
}

function seed(): Mem {
  const reqs = new Map<number, Req>();
  const add = (number: number, title: string, parent: number | null, o: Partial<Req> = {}) =>
    reqs.set(number, {
      number,
      title,
      description: `${title}.\n\nAcceptance:\n\n- The behaviour is covered by at least one case\n- It works on desktop and mobile`,
      type: parent === null ? 'epic' : 'story',
      status: 'active',
      parent,
      issueKey: null,
      url: null,
      trackerId: null,
      externalState: null,
      archivedAt: null,
      archivedBy: null,
      origin: 'ui',
      revision: 1,
      createdAt: iso(30),
      updatedAt: iso(3),
      contentChangedAt: iso(3),
      updatedBy: 'maria@example.com',
      ...o,
    });
  add(1, 'Customer can browse the catalog', null);
  add(2, 'Search finds products', 1, { issueKey: 'SHOP-101', url: 'https://issues.example.com/browse/SHOP-101', trackerId: 'jira', externalState: 'Done', origin: 'import' });
  add(3, 'Search tolerates typos', 2);
  add(4, 'Filters narrow the list', 1);
  add(5, 'Product page shows price and stock', 1, { status: 'draft' });
  add(6, 'Customer can check out', null, { issueKey: 'SHOP-120', url: 'https://issues.example.com/browse/SHOP-120', trackerId: 'jira', externalState: 'In Progress', origin: 'import' });
  add(7, 'Pay by card', 6);
  add(8, '3-D Secure challenge', 7, { type: 'task' });
  add(9, 'Delivery options', 6);
  add(10, 'Customer account', null, { status: 'draft' });
  add(11, 'Legacy wish list export', null, { status: 'obsolete', type: 'story' });
  const byName = new Map<string, MockCase>();
  for (const c of tmState().cases.values()) if (!byName.has(c.name)) byName.set(c.name, c);
  const links: Link[] = [];
  const link = (req: number, name: string, origin: Link['origin'] = 'ui', linkStatus: Link['linkStatus'] = 'active') => {
    const c = byName.get(name);
    if (c) links.push({ requirement: req, caseNumber: c.number, origin, linkStatus, createdAt: iso(10), createdBy: origin === 'ai' ? 'fastproof-ai' : 'maria@example.com' });
  };
  link(2, 'Search by product name');
  link(2, 'Search by SKU');
  link(3, 'Search corrects a typo');
  link(3, 'Search handles special characters', 'ai', 'proposed');
  link(4, 'Filter by price range');
  link(4, 'Filter by brand', 'code');
  link(4, 'Reset all filters');
  link(7, 'Pay with a saved card');
  link(7, 'Pay with a new card');
  link(7, 'Declined card can be retried', 'import');
  link(8, '3-D Secure challenge completes');
  link(9, 'Choose express delivery');
  link(9, 'Pick up in a store', 'ai', 'proposed');
  link(10, 'Sign in with a password');
  return { reqs, links, changed: new Set([7]), imports: new Map() };
}

function m(): Mem {
  mem ??= seed();
  return mem;
}

function role(): 'admin' | 'viewer' {
  try {
    return typeof window !== 'undefined' && window.localStorage.getItem(MOCK_ROLE_KEY) === 'viewer' ? 'viewer' : 'admin';
  } catch {
    return 'admin';
  }
}

function err(status: number, code: string, message: string, details?: unknown) {
  return HttpResponse.json({ code, message, ...(details === undefined ? {} : { details }) }, { status });
}

const etag = (revision: number) => `"r${String(revision)}"`;
const uuid = (n: number) => `0198aa00-0000-7000-8000-${String(n).padStart(12, '0')}`;
const known = (key: string) => key === TM_PROJECT;

function forbid(perm: string) {
  return err(403, 'forbidden', `requires permission ${perm}`, { required: perm });
}

function childrenOf(parent: number | null, archived = false): Req[] {
  return [...m().reqs.values()].filter((r) => r.parent === parent && !!r.archivedAt === archived).sort((a, b) => a.number - b.number);
}

function descendants(n: number, includeSelf = true): Req[] {
  const out: Req[] = [];
  const self = m().reqs.get(n);
  if (self && includeSelf) out.push(self);
  for (const c of [...m().reqs.values()].filter((r) => r.parent === n)) out.push(...descendants(c.number));
  return out;
}

function ancestors(r: Req): Req[] {
  const out: Req[] = [];
  let cur: Req | undefined = r;
  while (cur) {
    out.unshift(cur);
    cur = cur.parent === null ? undefined : m().reqs.get(cur.parent);
  }
  return out;
}

function depthOf(r: Req): number {
  return ancestors(r).length - 1;
}

function subtreeHeight(n: number): number {
  const kids = [...m().reqs.values()].filter((r) => r.parent === n);
  return kids.length === 0 ? 0 : 1 + Math.max(...kids.map((k) => subtreeHeight(k.number)));
}

function activeCases(n: number): number {
  return m().links.filter((l) => l.requirement === n && l.linkStatus === 'active').length;
}

function summary(r: Req): RequirementSummary {
  return {
    number: r.number,
    title: r.title,
    type: r.type,
    status: r.status,
    depth: depthOf(r),
    parentId: r.parent === null ? null : uuid(r.parent),
    hasChildren: childrenOf(r.number).length > 0,
    cases: activeCases(r.number),
    issueKey: r.issueKey,
    externalState: r.externalState,
    archived: !!r.archivedAt,
    updatedAt: r.updatedAt,
  };
}

function view(r: Req): Requirement {
  return {
    id: uuid(r.number),
    number: r.number,
    title: r.title,
    description: r.description,
    descriptionHtml: renderMarkdown(r.description, TM_PROJECT),
    type: r.type,
    status: r.status,
    depth: depthOf(r),
    parentId: r.parent === null ? null : uuid(r.parent),
    path: ancestors(r).map((a) => ({ id: uuid(a.number), number: a.number, title: a.title })),
    children: childrenOf(r.number).length,
    cases: activeCases(r.number),
    tracker: r.issueKey ? { issueKey: r.issueKey, url: r.url, trackerId: r.trackerId, externalState: r.externalState, syncedAt: r.trackerId ? iso(1) : null } : null,
    origin: r.origin,
    archivedAt: r.archivedAt,
    revision: r.revision,
    createdAt: r.createdAt,
    createdBy: 'maria@example.com',
    updatedAt: r.updatedAt,
    updatedBy: r.updatedBy,
    contentChangedAt: r.contentChangedAt,
  };
}

function withEtag(r: Req, status = 200) {
  return HttpResponse.json(view(r), { status, headers: { ETag: etag(r.revision) } });
}

function ifMatch(request: Request, r: Req): Response | null {
  const h = request.headers.get('If-Match');
  if (!h) return err(428, 'precondition_required', 'send If-Match with the ETag of the resource');
  if (h !== etag(r.revision)) return err(412, 'precondition_failed', 'the resource changed since it was read', { currentRevision: r.revision, changedBy: r.updatedBy, source: 'ui' });
  return null;
}

function caseOf(n: number): MockCase | undefined {
  return tmState().cases.get(n);
}

function isAutomated(c: MockCase): boolean {
  return c.identities.some((i) => i.kind === 'code');
}

function lastOf(c: MockCase): RequirementLink['lastResult'] {
  if (c.lastResult) return c.lastResult;
  const status = ['', 'passed', 'passed', 'failed'][c.number % 4];
  if (!status) return null;
  return { status, at: iso(2), source: 'manual', launchId: uuid(900 + (c.number % 7)), resultId: uuid(5000 + c.number) };
}

function linkView(l: Link): RequirementLink | null {
  const c = caseOf(l.caseNumber);
  if (!c || c.archivedAt) return null;
  return { caseNumber: c.number, name: c.name, status: c.status, automated: isAutomated(c), origin: l.origin, linkStatus: l.linkStatus, createdAt: l.createdAt, createdBy: l.createdBy, lastResult: lastOf(c) };
}

function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 16777619);
  return h >>> 0;
}

type ResultKind = 'passed' | 'failed' | 'skipped' | 'none';

function resultIn(c: MockCase, scope: CoverageScopeView): ResultKind {
  if (scope.kind === 'default') {
    const s = lastOf(c)?.status;
    return !s ? 'none' : s === 'passed' ? 'passed' : s === 'skipped' ? 'skipped' : 'failed';
  }
  const seedKey = `${scope.kind}|${scope.planId ?? ''}|${(scope.launchIds ?? []).join(',')}|${String(scope.days ?? '')}|${scope.branch ?? ''}|${scope.environment ?? ''}`;
  return (['none', 'failed', 'skipped', 'passed', 'passed', 'passed'] as const)[hash(`${seedKey}#${String(c.number)}`) % 6] ?? 'passed';
}

function countsFor(n: number, scope: CoverageScopeView): CoverageCounts {
  const reqs = new Set(descendants(n).filter((r) => !r.archivedAt).map((r) => r.number));
  const cases = new Set(m().links.filter((l) => reqs.has(l.requirement) && l.linkStatus === 'active').map((l) => l.caseNumber));
  const counts: CoverageCounts = { total: 0, passed: 0, failed: 0, notRun: 0, skipped: 0 };
  for (const cn of cases) {
    const c = caseOf(cn);
    if (!c || c.archivedAt) continue;
    counts.total++;
    const r = resultIn(c, scope);
    if (r === 'passed') counts.passed++;
    else if (r === 'failed') counts.failed++;
    else if (r === 'skipped') counts.skipped++;
    else counts.notRun++;
  }
  return counts;
}

function coverageItem(r: Req, scope: CoverageScopeView): RequirementCoverage {
  const cases = countsFor(r.number, scope);
  const changed = descendants(r.number).some((d) => m().changed.has(d.number));
  return { number: r.number, title: r.title, type: r.type, status: r.status, depth: depthOf(r), hasChildren: childrenOf(r.number).length > 0, cases, coverage: coverageStatusOf(cases), changedAfterRun: changed && cases.total > 0 };
}

function scopeView(p: URLSearchParams): CoverageScopeView | Response {
  const planId = p.get('planId');
  const launchIdsText = p.get('launchIds');
  const days = p.get('days');
  const branch = p.get('branch');
  const environment = p.get('environment');
  if (planId) return { kind: 'plan', planId, launchIds: p.get('launchId') ? [String(p.get('launchId'))] : null, launches: 1 };
  if (launchIdsText) {
    const ids = launchIdsText.split(',').filter(Boolean);
    if (ids.length > 50) return err(400, 'invalid_body', 'launchIds: at most 50 launches');
    return { kind: 'launches', launchIds: ids, launches: ids.length };
  }
  if (days || branch || environment) {
    const d = days === null ? 30 : Number(days);
    if (!Number.isInteger(d) || d < 1 || d > 365) return err(400, 'invalid_body', 'days: must be 1 to 365');
    return { kind: 'window', days: d, branch, environment, launches: Math.max(1, Math.round(d / 6)) };
  }
  return { kind: 'default', launches: 3 };
}

function parseQuery(text: string): ((r: Req) => boolean) | { position: number; length: number; message: string } {
  const preds: ((r: Req) => boolean)[] = [];
  const re = /\s+and\s+/gi;
  let start = 0;
  const parts: { text: string; at: number }[] = [];
  for (let x = re.exec(text); x; x = re.exec(text)) {
    parts.push({ text: text.slice(start, x.index), at: start });
    start = x.index + x[0].length;
  }
  parts.push({ text: text.slice(start), at: start });
  for (const part of parts) {
    const c = /^\s*(\w+)\s*(=|!=|~)\s*"?([^"]*?)"?\s*$/.exec(part.text);
    if (!c) return { position: part.at, length: Math.max(1, part.text.length), message: 'expected: attribute = value' };
    const [, attr = '', op = '=', raw = ''] = c;
    const value = raw.toLowerCase();
    const get = (r: Req): string => {
      switch (attr) {
        case 'number':
          return String(r.number);
        case 'title':
          return r.title.toLowerCase();
        case 'type':
          return r.type.toLowerCase();
        case 'status':
          return r.status;
        case 'parent':
          return String(r.parent ?? '');
        case 'issue':
          return (r.issueKey ?? '').toLowerCase();
        case 'externalState':
          return (r.externalState ?? '').toLowerCase();
        default:
          return coverageStatusOf(countsFor(r.number, { kind: 'default', launches: 3 })).toLowerCase();
      }
    };
    if (!['number', 'title', 'type', 'status', 'parent', 'issue', 'externalState', 'coverage'].includes(attr)) return { position: part.at + part.text.indexOf(attr), length: attr.length, message: `unknown attribute "${attr}"` };
    preds.push((r) => (op === '~' ? get(r).includes(value) : op === '!=' ? get(r) !== value : get(r) === value));
  }
  return (r) => preds.every((p) => p(r));
}

function page<T extends { number: number }>(items: T[], p: URLSearchParams): { items: T[]; next: string | null } {
  const after = Number(p.get('before') ?? 0);
  const limit = Math.min(500, Number(p.get('limit') ?? 100));
  const rest = items.filter((i) => i.number > after);
  const out = rest.slice(0, limit);
  return { items: out, next: rest.length > limit ? String(out[out.length - 1]?.number) : null };
}

interface WriteBody {
  title?: string;
  description?: string | null;
  type?: string | null;
  status?: RequirementStatus | null;
  parentNumber?: number | null;
  issueKey?: string | null;
  url?: string | null;
  trackerId?: string | null;
}

const STATUSES = ['draft', 'active', 'done', 'obsolete'];

function validate(b: WriteBody, partial: boolean): Response | null {
  if (!partial && !b.title?.trim()) return err(400, 'invalid_body', 'title: must have 1 to 300 characters');
  if (partial && b.title !== undefined && !b.title.trim()) return err(400, 'invalid_body', 'title: must have 1 to 300 characters');
  if (b.status && !STATUSES.includes(b.status)) return err(400, 'invalid_body', 'status: unknown status');
  return null;
}

function checkParent(parent: number | null | undefined, selfNumber: number | null): Response | null {
  if (parent === null || parent === undefined) return null;
  const p = m().reqs.get(parent);
  if (!p || p.archivedAt) return err(404, 'not_found', 'parent requirement not found');
  if (selfNumber !== null && descendants(selfNumber).some((d) => d.number === parent)) return err(409, 'requirement_cycle', 'a requirement cannot move under its own subtree');
  const height = selfNumber === null ? 0 : subtreeHeight(selfNumber);
  if (depthOf(p) + 1 + height >= MAX_DEPTH) return err(409, 'limit_reached', 'requirements nest at most 5 levels', { limit: 'max_depth', max: MAX_DEPTH });
  return null;
}

function keyTaken(issueKey: string | null | undefined, self: number | null): boolean {
  return !!issueKey && [...m().reqs.values()].some((r) => r.issueKey === issueKey && r.number !== self && !r.archivedAt);
}

function stamp(r: Req): void {
  r.revision += 1;
  r.updatedAt = new Date().toISOString();
  r.updatedBy = ME;
}

const jobs = () => tmState().jobs;

/** Simulates another user or the tracker sync changing a requirement: bumps its revision and emits `requirement.updated`. */
export function touchRequirement(n: number, title?: string): number | null {
  const r = m().reqs.get(n);
  if (!r) return null;
  if (title !== undefined) r.title = title;
  r.contentChangedAt = new Date().toISOString();
  r.revision += 1;
  r.updatedAt = r.contentChangedAt;
  r.updatedBy = 'sync@example.com';
  const hooks = typeof window === 'undefined' ? undefined : (window as unknown as { __fastproofMockTm?: { emit?: (event: string, data: unknown) => void } }).__fastproofMockTm;
  hooks?.emit?.('requirement.updated', { number: n, revision: r.revision });
  return r.revision;
}

if (typeof window !== 'undefined') {
  (window as unknown as { __fastproofMockRequirements?: unknown }).__fastproofMockRequirements = { touchRequirement };
}

function newJob(kind: TmJob['kind'], status: TmJob['status'], extra: Partial<TmJob> = {}): TmJob & { tick: number } {
  const id = uuid(Math.floor(Math.random() * 1e9) + 100_000);
  const job = { id, kind, status, progress: { done: 0, total: null }, report: null, createdBy: ME, createdAt: new Date().toISOString(), finishedAt: null, downloadUrl: null, tick: 0, ...extra } as TmJob & { tick: number };
  jobs().set(id, job);
  return job;
}

function advance(job: TmJob, steps: { after: number; to: Partial<TmJob>; then?: () => void }[]): void {
  for (const s of steps) {
    setTimeout(() => {
      Object.assign(job, s.to);
      s.then?.();
    }, s.after);
  }
}

const REPORT = { create: 3, update: 1, skip: 2, errors: [], folders: 0, unknownFields: [], unknownValues: {} };

function importedRequirements(parent: number | null): void {
  const mem_ = m();
  const next = Math.max(0, ...mem_.reqs.keys()) + 1;
  const items = [
    ['Guest checkout', 'SHOP-301'],
    ['Saved addresses', 'SHOP-302'],
    ['Order history', 'SHOP-303'],
  ] as const;
  items.forEach(([title, issueKey], i) => {
    if (keyTaken(issueKey, null)) return;
    const at = new Date().toISOString();
    mem_.reqs.set(next + i, { number: next + i, title, description: '', type: 'story', status: 'draft', parent, issueKey, url: `https://issues.example.com/browse/${issueKey}`, trackerId: 'jira', externalState: 'To Do', archivedAt: null, archivedBy: null, origin: 'import', revision: 1, createdAt: at, updatedAt: at, contentChangedAt: at, updatedBy: ME });
  });
}

export const requirementsHandlers: HttpHandler[] = [
  http.get(`${API}/requirements/tree`, ({ params, request }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    const p = new URL(request.url).searchParams;
    const parent = p.get('parent');
    if (parent !== null && !m().reqs.has(Number(parent))) return err(404, 'not_found', 'requirement not found');
    return HttpResponse.json(page(childrenOf(parent === null ? null : Number(parent)).map(summary), p));
  }),

  http.get(`${API}/requirements`, ({ params, request }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    const p = new URL(request.url).searchParams;
    let list = [...m().reqs.values()].filter((r) => !!r.archivedAt === (p.get('archived') === 'true'));
    const q = (p.get('q') ?? '').trim().toLowerCase();
    if (q) {
      const num = /^(?:req-)?(\d+)$/.exec(q)?.[1];
      const words = q.split(/\s+/);
      list = list.filter((r) => (num !== undefined && String(r.number) === num) || words.every((w) => `${r.title} ${r.description} ${r.issueKey ?? ''}`.toLowerCase().includes(w)));
    }
    if (p.get('parent')) {
      const under = new Set(descendants(Number(p.get('parent')), false).map((r) => r.number));
      list = list.filter((r) => under.has(r.number));
    }
    if (p.get('status')) list = list.filter((r) => r.status === p.get('status'));
    if (p.get('type')) list = list.filter((r) => r.type === p.get('type'));
    if (p.get('query')) {
      const pred = parseQuery(p.get('query') ?? '');
      if (typeof pred !== 'function') return err(400, 'invalid_query', pred.message, pred);
      list = list.filter(pred);
    }
    return HttpResponse.json(page(list.sort((a, b) => a.number - b.number).map(summary), p));
  }),

  http.post(`${API}/requirements/import`, async ({ params, request }) => {
    const key = String(params.key);
    if (!known(key)) return err(404, 'not_found', 'project not found');
    if (role() !== 'admin') return forbid('cases:import');
    const b = (await request.json()) as { trackerId?: string; query?: string; parentNumber?: number | null };
    const tracker = settingsOf(key).issueTrackers.find((t) => t.id === b.trackerId && t.api);
    if (!tracker) return err(400, 'invalid_body', 'trackerId: no issue tracker with an API connection has this id');
    if (!b.query?.trim()) return err(400, 'invalid_body', 'query: must not be empty');
    if (b.parentNumber != null && !m().reqs.has(b.parentNumber)) return err(404, 'not_found', 'parent requirement not found');
    const job = newJob('requirements', 'queued');
    m().imports.set(job.id, { parent: b.parentNumber ?? null, query: b.query });
    const fail = /fail/i.test(b.query);
    advance(job, [
      { after: 120, to: { status: 'running' } },
      fail
        ? { after: 300, to: { status: 'failed', finishedAt: new Date().toISOString(), report: { ...REPORT, create: 0, update: 0, skip: 0, errors: [{ row: 0, code: 'tracker_error', message: 'The tracker rejected the query: unknown field "fail".' }] } } }
        : { after: 300, to: { status: 'ready', report: REPORT } },
    ]);
    return HttpResponse.json(job, { status: 202 });
  }),

  http.post(`${API}/requirements/import/:jobId/apply`, ({ params }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    if (role() !== 'admin') return forbid('cases:import');
    const prev = jobs().get(String(params.jobId));
    const info = m().imports.get(String(params.jobId));
    if (!prev || !info) return err(404, 'not_found', 'job not found');
    if (prev.status !== 'ready') return err(409, 'conflict', 'the dry run is not ready or was applied already');
    prev.status = 'cancelled';
    const job = newJob('requirements', 'running', { report: REPORT });
    advance(job, [{ after: 250, to: { status: 'done', finishedAt: new Date().toISOString(), progress: { done: 6, total: 6 } }, then: () => importedRequirements(info.parent) }]);
    return HttpResponse.json(job, { status: 202 });
  }),

  http.post(`${API}/requirements`, async ({ params, request }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    if (role() !== 'admin') return forbid('cases:write');
    const b = (await request.json()) as WriteBody;
    const bad = validate(b, false) ?? checkParent(b.parentNumber, null);
    if (bad) return bad;
    if (keyTaken(b.issueKey, null)) return err(409, 'issue_key_taken', 'another requirement uses this issue key');
    const number = Math.max(0, ...m().reqs.keys()) + 1;
    const at = new Date().toISOString();
    const r: Req = {
      number,
      title: (b.title ?? '').trim(),
      description: b.description ?? '',
      type: b.type?.trim() || 'story',
      status: b.status ?? 'draft',
      parent: b.parentNumber ?? null,
      issueKey: b.issueKey?.trim() || null,
      url: b.url ?? null,
      trackerId: b.trackerId ?? null,
      externalState: null,
      archivedAt: null,
      archivedBy: null,
      origin: 'ui',
      revision: 1,
      createdAt: at,
      updatedAt: at,
      contentChangedAt: at,
      updatedBy: ME,
    };
    m().reqs.set(number, r);
    return withEtag(r, 201);
  }),

  http.get(`${API}/requirements/:n`, ({ params }) => {
    const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
    return r ? withEtag(r) : err(404, 'not_found', 'requirement not found');
  }),

  ...(['put', 'patch'] as const).map((method) =>
    http[method](`${API}/requirements/:n`, async ({ params, request }) => {
      if (role() !== 'admin') return forbid('cases:write');
      const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
      if (!r) return err(404, 'not_found', 'requirement not found');
      const stale = ifMatch(request, r);
      if (stale) return stale;
      if (r.archivedAt) return err(409, 'conflict', 'the requirement is archived; restore it first');
      const b = (await request.json()) as WriteBody;
      const bad = validate(b, method === 'patch') ?? (b.parentNumber !== undefined && b.parentNumber !== r.parent ? checkParent(b.parentNumber, r.number) : null);
      if (bad) return bad;
      if (method === 'put' && keyTaken(b.issueKey, r.number)) return err(409, 'issue_key_taken', 'another requirement uses this issue key');
      if (b.title !== undefined) r.title = b.title.trim();
      if (b.description !== undefined) r.description = b.description ?? '';
      if (b.type) r.type = b.type.trim();
      if (b.status) r.status = b.status;
      if (b.parentNumber !== undefined) r.parent = b.parentNumber;
      if (method === 'put') {
        r.issueKey = b.issueKey ?? null;
        r.url = b.url ?? null;
      }
      r.contentChangedAt = new Date().toISOString();
      m().changed.add(r.number);
      stamp(r);
      return withEtag(r);
    }),
  ),

  http.delete(`${API}/requirements/:n`, ({ params }) => {
    if (role() !== 'admin') return forbid('cases:write');
    const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
    if (!r || r.archivedAt) return err(404, 'not_found', 'requirement not found');
    const at = new Date().toISOString();
    for (const d of descendants(r.number)) {
      if (d.archivedAt) continue;
      d.archivedAt = at;
      d.archivedBy = r.number;
    }
    return new HttpResponse(null, { status: 204 });
  }),

  http.post(`${API}/requirements/:n/restore`, ({ params }) => {
    if (role() !== 'admin') return forbid('cases:write');
    const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
    if (!r || !r.archivedAt) return err(404, 'not_found', 'archived requirement not found');
    const parent = r.parent === null ? undefined : m().reqs.get(r.parent);
    if (parent?.archivedAt) return err(409, 'conflict', `the parent requirement REQ-${String(parent.number)} is archived; restore it first`);
    for (const d of descendants(r.number)) {
      if (d.archivedBy === r.number || d.number === r.number) {
        d.archivedAt = null;
        d.archivedBy = null;
      }
    }
    stamp(r);
    return withEtag(r);
  }),

  http.post(`${API}/requirements/:n/links`, async ({ params, request }) => {
    if (role() !== 'admin') return forbid('cases:write');
    const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
    if (!r) return err(404, 'not_found', 'requirement not found');
    if (r.archivedAt) return err(409, 'conflict', 'the requirement is archived; restore it first');
    const b = (await request.json()) as { add?: number[] | null; remove?: number[] | null; confirm?: number[] | null };
    for (const n of [...(b.add ?? []), ...(b.confirm ?? [])]) if (!caseOf(n)) return err(404, 'not_found', `case TC-${String(n)} not found`);
    const mine = () => m().links.filter((l) => l.requirement === r.number);
    const fresh = (b.add ?? []).filter((n) => !mine().some((l) => l.caseNumber === n));
    if (mine().length + fresh.length > MAX_LINKS) return err(409, 'limit_reached', 'a requirement links at most 500 cases', { limit: 'max_links', max: MAX_LINKS });
    for (const n of b.add ?? []) {
      const had = mine().find((l) => l.caseNumber === n);
      if (had) had.linkStatus = 'active';
      else m().links.push({ requirement: r.number, caseNumber: n, origin: 'ui', linkStatus: 'active', createdAt: new Date().toISOString(), createdBy: ME });
    }
    for (const n of b.confirm ?? []) {
      const had = mine().find((l) => l.caseNumber === n);
      if (had) had.linkStatus = 'active';
    }
    const gone = new Set(b.remove ?? []);
    m().links = m().links.filter((l) => !(l.requirement === r.number && gone.has(l.caseNumber)));
    stamp(r);
    return withEtag(r);
  }),

  http.get(`${API}/requirements/:n/cases`, ({ params, request }) => {
    const r = known(String(params.key)) ? m().reqs.get(Number(params.n)) : undefined;
    if (!r) return err(404, 'not_found', 'requirement not found');
    const p = new URL(request.url).searchParams;
    const status = p.get('linkStatus');
    const items = m()
      .links.filter((l) => l.requirement === r.number && (!status || l.linkStatus === status))
      .map(linkView)
      .filter((l): l is RequirementLink => !!l)
      .sort((a, b) => a.caseNumber - b.caseNumber)
      .map((l) => ({ link: l, number: l.caseNumber }));
    const out = page(items, p);
    return HttpResponse.json({ items: out.items.map((i) => i.link), next: out.next });
  }),

  http.get(`${API}/cases/:n/requirements`, ({ params }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    const n = Number(params.n);
    if (!caseOf(n)) return err(404, 'not_found', 'case not found');
    const out: CaseRequirement[] = m()
      .links.filter((l) => l.caseNumber === n)
      .map((l) => ({ l, r: m().reqs.get(l.requirement) }))
      .filter((x): x is { l: Link; r: Req } => !!x.r && !x.r.archivedAt)
      .sort((a, b) => a.r.number - b.r.number)
      .map(({ l, r }) => ({ number: r.number, title: r.title, type: r.type, status: r.status, origin: l.origin, linkStatus: l.linkStatus }));
    return HttpResponse.json(out);
  }),

  http.get(`${API}/coverage/exports/:id/download`, ({ params, request }) => {
    const job = jobs().get(String(params.id));
    if (!job || job.kind !== 'coverage_export') return err(404, 'not_found', 'export not found');
    if (job.status !== 'done') return err(409, 'conflict', 'the export is not ready');
    if (new URL(request.url).searchParams.get('token') !== 'mock') return err(403, 'forbidden', 'the download link is not valid');
    const rows = [...m().reqs.values()].filter((r) => !r.archivedAt).map((r) => `REQ-${String(r.number)},"${r.title}",${coverageStatusOf(countsFor(r.number, { kind: 'default', launches: 3 }))}`);
    return new HttpResponse(['requirement,title,coverage', ...rows].join('\n'), { headers: { 'Content-Type': 'text/csv', 'Content-Disposition': 'attachment; filename="coverage.csv"' } });
  }),

  http.post(`${API}/coverage/exports`, async ({ params, request }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    const b = (await request.json()) as { format?: string };
    if (b.format !== 'csv' && b.format !== 'xlsx') return err(400, 'invalid_body', 'format: csv or xlsx');
    const job = newJob('coverage_export', 'queued');
    job.downloadUrl = `/api/v1/projects/${String(params.key)}/coverage/exports/${job.id}/download?token=mock`;
    advance(job, [
      { after: 100, to: { status: 'running' } },
      { after: 300, to: { status: 'done', finishedAt: new Date().toISOString(), progress: { done: 1, total: 1 } } },
    ]);
    return HttpResponse.json({ ...job, tick: undefined }, { status: 202 });
  }),

  http.get(`${API}/coverage`, ({ params, request }) => {
    if (!known(String(params.key))) return err(404, 'not_found', 'project not found');
    const p = new URL(request.url).searchParams;
    const scope = scopeView(p);
    if (scope instanceof Response) return scope;
    const parent = p.get('parent');
    if (parent !== null && !m().reqs.has(Number(parent))) return err(404, 'not_found', 'requirement not found');
    const items = childrenOf(parent === null ? null : Number(parent)).map((r) => coverageItem(r, scope));
    return HttpResponse.json({ scope, ...page(items, p) });
  }),
];

/** Data of the `requirement-coverage` dashboard widget. */
export function requirementCoverageData(project: string, s: WidgetSettings): Extract<WidgetData, { kind: 'requirementCoverage' }> {
  const windowed = s.days !== undefined || !!s.branch || !!s.environment;
  const scope: CoverageScopeView = windowed ? { kind: 'window', days: s.days ?? 30, branch: s.branch ?? null, environment: s.environment ?? null, launches: 5 } : { kind: 'default', launches: 3 };
  const all = project === TM_PROJECT ? childrenOf(null).map((r) => coverageItem(r, scope)) : [];
  const counts = { uncovered: 0, failed: 0, notRun: 0, partial: 0, passed: 0 };
  for (const i of all) counts[i.coverage]++;
  return { kind: 'requirementCoverage', scope, total: all.length, counts, items: all.slice(0, s.limit ?? 10) };
}
