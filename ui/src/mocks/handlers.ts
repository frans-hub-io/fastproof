import { delay, http, HttpResponse } from 'msw';
import type { CreateToken, ErrorBody, Launch, ResultSummary, TokenInfo } from '../api/types';
import {
  attachmentContent,
  initialAudit,
  initialTokens,
  launchDef,
  launchResults,
  launchView,
  meFor,
  projectLaunches,
  projects,
  labelLevels,
  resultDetail,
  resultEnv,
  resultKinds,
  RUNNING_LAUNCH_ID,
  testCaseExists,
  testCaseHistory,
  TOKEN_OPTIONS,
  treeOf,
  treeTestsOf,
  uploadsOf,
  type MockRole,
} from './fixtures';
import { dashboardHandlers } from './handlersDashboards';
import { resetDashboards } from './dashboards';
import { stageCHandlers } from './handlersC';
import { resetTrackers, trackerHandlers } from './trackers';
import { mediaHandlers } from './media';
import { historyImportHandlers, resetHistoryImports } from './historyImports';
import { mergeHandlers, resetMerges } from './merges';
import { analyze, analyzeDetail, isImportant, resetStageC } from './stageC';
import { resetTm, subscribeTmEvents, tmHandlers, tmLaunchList } from './tm';

const API = '/api/v1';
const EXPORT_JOB_ID = '0198aaaa-0000-7000-8000-00000000e001';
let exportPolls = 0;
export const MOCK_ROLE_KEY = 'fastproof-mock-role';

let roleOverride: MockRole | null = null;
let latency = 0;

export function setMockRole(role: MockRole | null): void {
  roleOverride = role;
}

export function setMockLatency(ms: number): void {
  latency = ms;
}

function currentRole(): MockRole {
  if (roleOverride) return roleOverride;
  try {
    if (typeof window !== 'undefined' && window.localStorage.getItem(MOCK_ROLE_KEY) === 'viewer') return 'viewer';
  } catch {
    return 'admin';
  }
  return 'admin';
}

const state = {
  tokens: initialTokens(),
  audit: initialAudit(),
};

export function resetMockState(): void {
  state.tokens = initialTokens();
  state.audit = initialAudit();
  roleOverride = null;
  resetStageC();
  resetDashboards();
  resetTm();
  resetMerges();
  resetHistoryImports();
  resetTrackers();
}

function error(status: number, code: string, message: string, details?: unknown) {
  const body: ErrorBody = { code, message, ...(details === undefined ? {} : { details }) };
  return HttpResponse.json(body, { status });
}

function limitOf(url: URL, def: number, max = 500): number {
  const n = Number(url.searchParams.get('limit') ?? def);
  return Number.isFinite(n) && n > 0 ? Math.min(max, Math.floor(n)) : def;
}

function page<T>(items: T[], url: URL, idOf: (t: T) => string, def = 50) {
  const before = url.searchParams.get('before');
  let start = 0;
  if (before) {
    const i = items.findIndex((x) => idOf(x) === before);
    start = i >= 0 ? i + 1 : items.length;
  }
  const limit = limitOf(url, def);
  const slice = items.slice(start, start + limit);
  const more = start + limit < items.length;
  const last = slice[slice.length - 1];
  return { items: slice, next: more && last ? idOf(last) : null };
}

function pair(v: string | null): [string, string] | null {
  if (!v) return null;
  const i = v.indexOf(':');
  if (i < 0) return [v, ''];
  return [v.slice(0, i), v.slice(i + 1)];
}

function hasProject(key: string): boolean {
  return projects().some((p) => p.key === key);
}

function filterLaunches(list: Launch[], url: URL): Launch[] {
  const branch = url.searchParams.get('branch');
  const env = url.searchParams.get('environment');
  const status = url.searchParams.get('status');
  const attr = pair(url.searchParams.get('attr'));
  const q = url.searchParams.get('q')?.toLowerCase();
  const debug = url.searchParams.get('debug');
  return list.filter((l) => {
    if (branch && l.vcs?.branch !== branch) return false;
    if (env && l.environment?.key !== env) return false;
    if (status && !status.split(',').includes(l.status)) return false;
    if (attr && !l.attributes.some((a) => a.key === attr[0] && (!attr[1] || a.value === attr[1]))) return false;
    if (q && !`${l.name} #${String(l.number)} ${l.vcs?.commit ?? ''}`.toLowerCase().includes(q)) return false;
    if (debug === 'false' && l.debug) return false;
    if (debug === 'true' && !l.debug) return false;
    return true;
  });
}

const NO_LABEL = '\u0000';

function listItems(v: string | null): string[] {
  return (v ?? '')
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean)
    .map((x) => x.replace(/%(2C|25|00)/gi, (_, h: string) => (h === '00' ? NO_LABEL : h === '25' ? '%' : ',')));
}
const LABEL_NAME = /^[\p{L}\p{N}_\-.:@/]{1,64}$/u;

/** A `groupBy` the server accepts, or null. */
function validGroupBy(g: string): boolean {
  const levels = labelLevels(g);
  if (!levels) return true;
  return levels.length >= 1 && levels.length <= 5 && levels.every((n) => LABEL_NAME.test(n)) && new Set(levels).size === levels.length;
}

function labelValue(r: ResultSummary, name: string): string | null {
  return r.labels.find((l) => l.name === name)?.value || null;
}

function isNewFailure(r: ResultSummary): boolean {
  return (r.status === 'failed' || r.status === 'broken') && !r.muted && (r.transition === 'regressed' || r.transition === 'new');
}

/** The explorer filters shared by the tree and the result list, as the server applies them. */
function explorerFilter(r: ResultSummary, url: URL): boolean {
  const sp = url.searchParams;
  const list = (k: string) => listItems(sp.get(k));
  const message = sp.get('message')?.trim().toLowerCase();
  if (message && !(r.message ?? '').toLowerCase().includes(message)) return false;
  if (sp.get('retried') === 'true' && r.attemptCount <= 1) return false;
  if (sp.get('muted') === 'true' && !r.muted) return false;
  if (sp.get('muted') === 'false' && r.muted) return false;
  if (sp.get('newFailures') === 'true' && !isNewFailure(r)) return false;
  for (const k of ['severity', 'owner', 'tag']) {
    const want = list(k);
    if (want.length && !want.some((v) => (v === NO_LABEL ? !labelValue(r, k) : r.labels.some((l) => l.name === k && l.value === v)))) return false;
  }
  const min = sp.get('minDurationMs');
  const max = sp.get('maxDurationMs');
  if (min && (r.durationMs ?? 0) < Number(min)) return false;
  if (max && (r.durationMs ?? 0) > Number(max)) return false;
  const defects = list('defect');
  if (defects.length && !defects.some((d) => (d === 'none' ? !r.defect : r.defect?.typeId === d))) return false;
  const kinds = list('attachment');
  if (kinds.length && !kinds.some((k) => resultKinds(r).includes(k))) return false;
  const env = sp.get('env');
  if (env && (resultEnv(r) ?? NO_LABEL) !== listItems(env)[0]) return false;
  return true;
}

function countsOf(list: ResultSummary[]) {
  const c = { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };
  for (const r of list) {
    c.total++;
    c[r.status]++;
    if (r.flakyInRun) c.flaky++;
    if (r.muted) c.muted++;
  }
  return c;
}

function valueCounts(values: (string | null | undefined)[]): { value: string; count: number }[] {
  const m = new Map<string, number>();
  for (const v of values) if (v) m.set(v, (m.get(v) ?? 0) + 1);
  return [...m.entries()].map(([value, count]) => ({ value, count })).sort((a, b) => b.count - a.count || a.value.localeCompare(b.value));
}

function filterResults(list: ResultSummary[], url: URL): ResultSummary[] {
  const status = url.searchParams.get('status');
  const q = url.searchParams.get('q')?.toLowerCase();
  const suite = url.searchParams.get('suite');
  const label = pair(url.searchParams.get('label'));
  const flaky = url.searchParams.get('flaky');
  const fp = url.searchParams.get('fingerprint');
  return list.filter((r) => {
    if (status && !status.split(',').includes(r.status)) return false;
    if (q && !`${r.name} ${r.fullName} ${r.message ?? ''}`.toLowerCase().includes(q)) return false;
    if (suite && !r.suitePath.includes(suite)) return false;
    if (label && !r.labels.some((l) => l.name === label[0] && (!label[1] || l.value === label[1]))) return false;
    if (flaky === 'true' && !r.flakyInRun) return false;
    if (flaky === 'false' && r.flakyInRun) return false;
    if (fp && r.fingerprint !== fp) return false;
    return explorerFilter(r, url);
  });
}

/** The tree's search matches names and suites only, like the server's. */
function filterTree(list: ResultSummary[], url: URL): ResultSummary[] {
  const q = url.searchParams.get('q')?.toLowerCase();
  const rest = new URL(url);
  rest.searchParams.delete('q');
  return filterResults(list, rest).filter((r) => !q || `${r.name} ${r.fullName} ${r.suitePath.join(' ')}`.toLowerCase().includes(q));
}

/** Inline budget of tree leaves; e2e tests lower it to exercise paging. */
function treeBudget(): number {
  try {
    const v = Number(globalThis.localStorage.getItem('fastproof.mock.treeBudget'));
    return v > 0 ? v : 5000;
  } catch {
    return 5000;
  }
}

function audit(action: string, scope: string | undefined, detail: unknown): void {
  const me = meFor(currentRole());
  const id = (state.audit[0]?.id ?? 0) + 1;
  state.audit.unshift({ id, at: new Date().toISOString(), principal: me.name, principalKind: me.kind, roles: me.roles.join(','), scope, action, detail });
}

function randomHex(bytes: number): string {
  const a = new Uint8Array(bytes);
  crypto.getRandomValues(a);
  return Array.from(a, (b) => b.toString(16).padStart(2, '0')).join('');
}

function withImportant(def: Parameters<typeof launchView>[0]): Launch {
  const l = launchView(def);
  return { ...l, important: isImportant(l.id, l.number % 10 === 0) };
}

const ACTIVE = /^(text\/html|application\/xhtml\+xml|image\/svg\+xml|application\/xml|text\/xml)/i;

export const handlers = [
  http.all(`${API}/*`, async () => {
    if (latency) await delay(latency);
    return undefined;
  }),

  ...tmHandlers({ role: currentRole, error, audit }),

  http.get(`${API}/me`, () => HttpResponse.json(meFor(currentRole()))),
  http.get(`${API}/version`, () => HttpResponse.json({ version: '0.2.0-dev1' })),
  http.post('/auth/logout', () => HttpResponse.json({ loggedOut: true })),

  http.get(`${API}/projects`, () => {
    const role = currentRole();
    const list = projects().filter((p) => role === 'admin' || p.key === 'web-shop');
    return HttpResponse.json({ items: list });
  }),

  http.get(`${API}/projects/:key`, ({ params }) => {
    const p = projects().find((x) => x.key === params.key);
    return p ? HttpResponse.json(p) : error(404, 'not_found', 'project not found');
  }),

  http.get(`${API}/projects/:key/launches`, ({ params, request }) => {
    const key = String(params.key);
    if (!hasProject(key)) return error(404, 'not_found', 'project not found');
    const url = new URL(request.url);
    const list = filterLaunches(tmLaunchList(key, projectLaunches(key).map(withImportant)), url);
    return HttpResponse.json(page(list, url, (l) => l.id));
  }),

  http.get(`${API}/projects/:key/launches/:id`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    return HttpResponse.json(withImportant(def));
  }),

  http.get(`${API}/projects/:key/launches/:id/export`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    if (def.launch.counts.total > 10_000) return error(409, 'export_too_large', 'the launch has more than 10000 results');
    const format = new URL(request.url).searchParams.get('format');
    if (format === 'xlsx') return new HttpResponse(new Uint8Array([0x50, 0x4b, 3, 4]), { headers: { 'Content-Type': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' } });
    if (format !== 'html') return error(400, 'bad_request', 'format must be html or xlsx');
    return new HttpResponse(`<!doctype html><title>#${String(def.launch.number)}</title><p>${request.url}</p>`, { headers: { 'Content-Type': 'text/html; charset=utf-8' } });
  }),

  http.post(`${API}/projects/:key/launches/:id/exports`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    exportPolls = 0;
    return HttpResponse.json({ id: EXPORT_JOB_ID, launchId: def.launch.id, format: 'html', status: 'queued', total: def.launch.counts.total, done: 0, createdBy: 'me', createdAt: new Date().toISOString() }, { status: 202 });
  }),

  http.get(`${API}/projects/:key/launches/:id/exports/:job`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    exportPolls += 1;
    const done = exportPolls >= 2;
    return HttpResponse.json({ id: EXPORT_JOB_ID, launchId: def.launch.id, format: 'html', status: done ? 'done' : 'running', total: def.launch.counts.total, done: done ? def.launch.counts.total : 100, createdBy: 'me', createdAt: new Date().toISOString() });
  }),

  http.get(`${API}/projects/:key/launches/:id/exports/:job/download`, () => new HttpResponse('<!doctype html><title>job</title>', { headers: { 'Content-Type': 'text/html; charset=utf-8' } })),

  http.get(`${API}/projects/:key/launches/:id/uploads`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    return HttpResponse.json({ items: uploadsOf(def) });
  }),

  http.get(`${API}/projects/:key/launches/:id/results`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    const key = def.project;
    return HttpResponse.json(page(filterResults(launchResults(def.launch.id).map((r) => analyze(key, r)), url), url, (r) => r.id));
  }),

  http.get(`${API}/projects/:key/launches/:id/tree`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    const g = url.searchParams.get('groupBy') ?? 'suite';
    if (!validGroupBy(g)) return error(400, 'invalid_query', 'groupBy must be suite, feature, package or up to five label names', { field: 'groupBy' });
    const results = filterTree(launchResults(def.launch.id).map((r) => analyze(def.project, r)), url);
    return HttpResponse.json(treeOf(def.launch.id, g, { results, includeTests: url.searchParams.get('includeTests') === 'true', reveal: url.searchParams.get('reveal'), budget: treeBudget() }));
  }),

  http.get(`${API}/projects/:key/launches/:id/labels`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    const env = url.searchParams.get('env');
    const results = launchResults(def.launch.id).filter((r) => !env || (resultEnv(r) ?? NO_LABEL) === listItems(env)[0]);
    const by = new Map<string, { count: number; values: Set<string> }>();
    for (const r of results) {
      const seen = new Set<string>();
      for (const l of r.labels) {
        const e = by.get(l.name) ?? { count: 0, values: new Set<string>() };
        if (!seen.has(l.name)) e.count++;
        seen.add(l.name);
        e.values.add(l.value);
        by.set(l.name, e);
      }
    }
    const items = [...by.entries()].map(([name, e]) => ({ name, count: e.count, values: e.values.size })).sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
    return HttpResponse.json({ items, partial: false });
  }),

  http.get(`${API}/projects/:key/launches/:id/facets`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    const env = url.searchParams.get('env');
    const results = launchResults(def.launch.id)
      .map((r) => analyze(def.project, r))
      .filter((r) => !env || (resultEnv(r) ?? NO_LABEL) === listItems(env)[0]);
    const labels = ['severity', 'owner', 'tag'].map((name) => ({
      name,
      values: valueCounts(results.flatMap((r) => [...new Set(r.labels.filter((l) => l.name === name).map((l) => l.value))])),
      none: results.filter((r) => !labelValue(r, name)).length,
      more: false,
    }));
    const noDefect = results.filter((r) => !r.defect).length;
    const defects = valueCounts(results.map((r) => r.defect?.typeId));
    if (noDefect) defects.push({ value: 'none', count: noDefect });
    const durations = results.map((r) => r.durationMs ?? 0);
    return HttpResponse.json({
      counts: countsOf(results),
      retried: results.filter((r) => r.attemptCount > 1).length,
      newFailures: results.filter(isNewFailure).length,
      baselineLaunchId: null,
      manual: 0,
      automated: results.length,
      labels,
      defects,
      attachments: ['trace', 'video', 'screenshot'].map((value) => ({ value, count: results.filter((r) => resultKinds(r).includes(value)).length })),
      maxDurationMs: durations.length ? Math.max(...durations) : null,
      partial: false,
    });
  }),

  http.get(`${API}/projects/:key/launches/:id/environments`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const groups = new Map<string | null, ResultSummary[]>();
    for (const r of launchResults(def.launch.id)) {
      const k = resultEnv(r);
      groups.set(k, [...(groups.get(k) ?? []), r]);
    }
    const items = [...groups.entries()].map(([key, list]) => ({ key, counts: countsOf(list) })).sort((a, b) => b.counts.total - a.counts.total);
    return HttpResponse.json({ items });
  }),

  http.get(`${API}/projects/:key/launches/:id/tree/tests`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    const g = url.searchParams.get('groupBy') ?? 'suite';
    if (!validGroupBy(g)) return error(400, 'invalid_query', 'groupBy must be suite, feature, package or up to five label names', { field: 'groupBy' });
    let path: unknown;
    try {
      path = JSON.parse(url.searchParams.get('path') ?? '');
    } catch {
      path = null;
    }
    if (!Array.isArray(path) || !path.every((p) => typeof p === 'string')) return error(400, 'invalid_query', 'path must be a JSON array of strings', { field: 'path' });
    const results = filterTree(launchResults(def.launch.id).map((r) => analyze(def.project, r)), url);
    const items = treeTestsOf(def.launch.id, g, results, path);
    return HttpResponse.json(page(items, url, (t) => t.id));
  }),

  http.get(`${API}/projects/:key/results/:id`, ({ params }) => {
    const r = resultDetail(String(params.id));
    if (!r) return error(404, 'not_found', 'result not found');
    return HttpResponse.json(analyzeDetail(String(params.key), r));
  }),

  http.get(`${API}/projects/:key/test-cases/:id/history`, ({ params, request }) => {
    const key = String(params.key);
    if (!hasProject(key) || !testCaseExists(String(params.id))) return error(404, 'not_found', 'test case not found');
    const url = new URL(request.url);
    const items = testCaseHistory(key, String(params.id), url.searchParams.get('branch') ?? undefined, url.searchParams.get('environment') ?? undefined, limitOf(url, 100));
    return HttpResponse.json({ items });
  }),

  http.get(`${API}/projects/:key/attachments/:id/content`, ({ params, request }) => {
    const a = attachmentContent(String(params.id));
    if (!a) return error(404, 'not_found', 'attachment not found');
    const download = new URL(request.url).searchParams.get('download') === '1';
    const body = a.body();
    const headers: Record<string, string> = {
      'Content-Type': a.meta.contentType,
      'X-Content-Type-Options': 'nosniff',
      'Content-Security-Policy': "sandbox; default-src 'none'",
      'Cache-Control': 'private, max-age=31536000, immutable',
    };
    if (download || ACTIVE.test(a.meta.contentType)) headers['Content-Disposition'] = `attachment; filename="${a.meta.name.replace(/[^A-Za-z0-9._-]/g, '_')}"`;
    return new HttpResponse(typeof body === 'string' ? body : body.slice().buffer, { headers });
  }),

  http.get(`${API}/projects/:key/events`, ({ params, request }) => {
    if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
    const encoder = new TextEncoder();
    let timer: ReturnType<typeof setInterval> | undefined;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('retry: 5000\n\n'));
        if (params.key !== 'web-shop') return;
        const offTm = subscribeTmEvents((event, data) => {
          try {
            controller.enqueue(encoder.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
          } catch {
            offTm();
          }
        });
        request.signal.addEventListener('abort', offTm);
        let tick = 0;
        timer = setInterval(() => {
          tick++;
          try {
            controller.enqueue(encoder.encode(`event: launch.updated\ndata: ${JSON.stringify({ id: RUNNING_LAUNCH_ID })}\n\n`));
            if (tick % 3 === 0) controller.enqueue(encoder.encode(`event: upload.updated\ndata: ${JSON.stringify({ launchId: RUNNING_LAUNCH_ID, id: 'upload' })}\n\n`));
          } catch {
            clearInterval(timer);
          }
        }, 3000);
        request.signal.addEventListener('abort', () => clearInterval(timer));
      },
      cancel() {
        clearInterval(timer);
      },
    });
    return new HttpResponse(stream, { headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' } });
  }),

  http.get(`${API}/tokens/options`, () => HttpResponse.json(TOKEN_OPTIONS)),

  http.get(`${API}/tokens`, ({ request }) => {
    const status = new URL(request.url).searchParams.get('status');
    const list = state.tokens.filter((t) => !status || t.status === status);
    return HttpResponse.json({ tokens: list });
  }),

  http.post(`${API}/tokens`, async ({ request }) => {
    const body = (await request.json()) as CreateToken;
    if (!body.name || !/^[A-Za-z0-9._@-]{1,128}$/.test(body.name)) return error(400, 'invalid_body', 'name must be 1-128 characters of A-Z a-z 0-9 . _ @ -', { field: 'name' });
    if (state.tokens.some((t) => t.name === body.name && t.status === 'active')) return error(409, 'conflict', `an active token named ${body.name} already exists`);
    const role = TOKEN_OPTIONS.roles.find((r) => r.name === body.role);
    if (!role) return error(400, 'invalid_body', `unknown role ${body.role}`, { field: 'role' });
    if (!body.global && !(body.scopes && body.scopes.length > 0)) return error(400, 'invalid_body', 'set scopes or global', { field: 'scopes' });
    const days = body.expiresInDays ?? TOKEN_OPTIONS.defaultDays;
    if (days < 1 || days > TOKEN_OPTIONS.maxDays) return error(400, 'invalid_body', `expiresInDays must be 1..${String(TOKEN_OPTIONS.maxDays)}`, { field: 'expiresInDays' });
    const now = Date.now();
    const info: TokenInfo = {
      id: crypto.randomUUID(),
      name: body.name,
      role: body.role,
      scopes: body.global ? [] : (body.scopes ?? []),
      global: !!body.global,
      ...(body.description ? { description: body.description } : {}),
      createdBy: meFor(currentRole()).name,
      createdAt: new Date(now).toISOString(),
      expiresAt: new Date(now + days * 86_400_000).toISOString(),
      lastUsedAt: null,
      revokedAt: null,
      status: 'active',
    };
    state.tokens.unshift(info);
    audit('token.create', info.scopes.length === 1 ? info.scopes[0] : undefined, { name: info.name, role: info.role });
    return HttpResponse.json({ ...info, token: `fp_${randomHex(32)}` }, { status: 201 });
  }),

  http.delete(`${API}/tokens/:id`, ({ params }) => {
    const t = state.tokens.find((x) => x.id === params.id);
    if (!t) return error(404, 'not_found', 'token not found');
    t.status = 'revoked';
    t.revokedAt = new Date().toISOString();
    audit('token.revoke', undefined, { name: t.name });
    return new HttpResponse(null, { status: 204 });
  }),

  http.get(`${API}/audit`, ({ request }) => {
    if (currentRole() !== 'admin') return error(403, 'forbidden', 'requires permission audit:read', { required: 'audit:read' });
    const url = new URL(request.url);
    const action = url.searchParams.get('action');
    const principal = url.searchParams.get('principal');
    const scope = url.searchParams.get('scope');
    const list = state.audit.filter(
      (e) => (!action || (action.endsWith('*') ? e.action.startsWith(action.slice(0, -1)) : e.action === action)) && (!principal || e.principal === principal) && (!scope || e.scope === scope),
    );
    return HttpResponse.json(page(list, url, (e) => String(e.id), 100));
  }),

  ...trackerHandlers({ role: currentRole, error, audit }),
  ...stageCHandlers({ role: currentRole, error, audit }),
  ...dashboardHandlers({ role: currentRole, error, audit }),
  ...mediaHandlers(error),
  ...mergeHandlers(error),
  ...historyImportHandlers(error),

  http.all(`${API}/*`, () => error(404, 'not_found', 'no such endpoint')),
];
