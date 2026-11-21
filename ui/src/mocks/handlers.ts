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
  resultDetail,
  RUNNING_LAUNCH_ID,
  testCaseExists,
  testCaseHistory,
  TOKEN_OPTIONS,
  treeOf,
  uploadsOf,
  type MockRole,
} from './fixtures';

const API = '/api/v1';
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
    return true;
  });
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

const ACTIVE = /^(text\/html|application\/xhtml\+xml|image\/svg\+xml|application\/xml|text\/xml)/i;

export const handlers = [
  http.all(`${API}/*`, async () => {
    if (latency) await delay(latency);
    return undefined;
  }),

  http.get(`${API}/me`, () => HttpResponse.json(meFor(currentRole()))),
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
    const list = filterLaunches(projectLaunches(key).map(launchView), url);
    return HttpResponse.json(page(list, url, (l) => l.id));
  }),

  http.get(`${API}/projects/:key/launches/:id`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    return HttpResponse.json(launchView(def));
  }),

  http.get(`${API}/projects/:key/launches/:id/uploads`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    return HttpResponse.json({ items: uploadsOf(def) });
  }),

  http.get(`${API}/projects/:key/launches/:id/results`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const url = new URL(request.url);
    return HttpResponse.json(page(filterResults(launchResults(def.launch.id), url), url, (r) => r.id));
  }),

  http.get(`${API}/projects/:key/launches/:id/tree`, ({ params, request }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const g = new URL(request.url).searchParams.get('groupBy') ?? 'suite';
    if (g !== 'suite' && g !== 'feature' && g !== 'package') return error(400, 'invalid_query', 'groupBy must be suite, feature or package', { field: 'groupBy' });
    return HttpResponse.json(treeOf(def.launch.id, g));
  }),

  http.get(`${API}/projects/:key/results/:id`, ({ params }) => {
    const r = resultDetail(String(params.id));
    if (!r) return error(404, 'not_found', 'result not found');
    return HttpResponse.json(r);
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

  http.all(`${API}/*`, () => error(404, 'not_found', 'no such endpoint')),
];
