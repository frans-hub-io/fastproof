import { http, HttpResponse, type HttpHandler } from 'msw';
import type { CaseStep } from '../api/tm';
import { expandSharedForRun, sharedAll, setSharedAll, sid, uuid, view, type MockShared } from './sharedStepsState';
import { tmState } from './tm';
import type { SharedStepVersionSummary } from '../api/sharedSteps';

export { resetSharedSteps } from './sharedStepsState';

const API = '/api/v1';
const P = `${API}/projects/:key`;

function json(body: unknown, revision?: number, status = 200) {
  return HttpResponse.json(body as Record<string, unknown>, { status, headers: revision === undefined ? {} : { ETag: `"r${String(revision)}"` } });
}

function err(status: number, code: string, message: string, details?: unknown) {
  return HttpResponse.json({ code, message, ...(details === undefined ? {} : { details }) }, { status });
}

function find(n: string): MockShared | undefined {
  return sharedAll().find((s) => String(s.number) === n);
}

function ifMatch(request: Request, s: MockShared): Response | null {
  const h = request.headers.get('If-Match');
  if (!h) return err(428, 'precondition_required', 'send If-Match with the ETag of the resource');
  if (h !== `"r${String(s.revision)}"`) return err(412, 'precondition_failed', 'the resource changed since it was read', { currentRevision: s.revision, changedBy: s.updatedBy, source: 'ui' });
  return null;
}

function count(steps: CaseStep[]): number {
  return steps.reduce((n, s) => n + 1 + count(s.steps), 0);
}

function withIds(steps: CaseStep[]): CaseStep[] {
  return steps.map((s) => ({ ...s, id: s.id || sid(), attachments: [], steps: withIds(s.steps) }));
}

function save(s: MockShared, name: string, description: string, steps: CaseStep[], source: SharedStepVersionSummary['source'] = 'ui'): void {
  const next = withIds(steps);
  const changed = [name !== s.name ? 'name' : '', description !== s.description ? 'description' : '', JSON.stringify(next) !== JSON.stringify(s.steps) ? 'steps' : ''].filter(Boolean);
  s.name = name;
  s.description = description;
  s.steps = next;
  s.revision += 1;
  s.updatedAt = new Date().toISOString();
  s.updatedBy = 'you';
  if (changed.length) {
    s.version += 1;
    s.versions.unshift({ version: s.version, source, changed, createdBy: 'you', createdAt: s.updatedAt, snapshot: { name, description, steps: next } });
  }
}

interface WriteBody {
  name?: string;
  description?: string;
  steps?: CaseStep[];
}

function invalid(b: WriteBody): Response | null {
  if (!b.name?.trim()) return err(400, 'invalid_case', 'name: must have 1 to 200 bytes');
  if (!b.steps?.length) return err(400, 'invalid_case', 'steps: a shared step needs at least one step');
  if (b.steps.some((s) => s.shared)) return err(400, 'invalid_case', 'steps[0].shared: a shared step cannot contain another shared step');
  if (b.steps.some((s) => s.steps.some((c) => c.steps.some((g) => g.steps.length > 0)))) return err(400, 'invalid_case', 'steps: shared steps nest at most 2 levels');
  return null;
}


export const sharedStepsHandlers: HttpHandler[] = [
  http.get(`${P}/shared-steps`, ({ request }) => {
    const url = new URL(request.url);
    const q = (url.searchParams.get('q') ?? '').trim().toLowerCase();
    const archived = url.searchParams.get('archived') === 'true';
    const after = Number(url.searchParams.get('before') ?? 0);
    const limit = Math.min(500, Number(url.searchParams.get('limit') ?? 50));
    const num = /^(?:ss-)?(\d+)$/.exec(q)?.[1];
    const list = sharedAll().filter((s) => !!s.archivedAt === archived && s.number > after && (!q || s.name.toLowerCase().includes(q) || (num !== undefined && String(s.number) === num)));
    const items = list.slice(0, limit).map((s) => ({
      number: s.number,
      name: s.name,
      version: s.version,
      approvedVersion: null,
      reviewState: null,
      stepCount: count(s.steps),
      cases: s.usedBy.length,
      aiUnreviewed: s.aiUnreviewed,
      archived: !!s.archivedAt,
      updatedAt: s.updatedAt,
    }));
    return json({ items, next: list.length > limit ? String(items[items.length - 1]?.number) : null });
  }),
  http.post(`${P}/shared-steps`, async ({ request, params }) => {
    const b = (await request.json()) as WriteBody;
    const bad = invalid(b);
    if (bad) return bad;
    const at = new Date().toISOString();
    const n = Math.max(0, ...sharedAll().map((s) => s.number)) + 1;
    const steps = withIds(b.steps ?? []);
    const s: MockShared = {
      id: uuid(n),
      number: n,
      name: (b.name ?? '').trim(),
      description: b.description ?? '',
      steps,
      version: 1,
      revision: 1,
      archivedAt: null,
      aiUnreviewed: false,
      versions: [{ version: 1, source: 'ui', changed: ['name', 'description', 'steps'], createdBy: 'you', createdAt: at, snapshot: { name: (b.name ?? '').trim(), description: b.description ?? '', steps } }],
      usedBy: [],
      openRuns: 0,
      createdBy: 'you',
      createdAt: at,
      updatedBy: 'you',
      updatedAt: at,
    };
    sharedAll().push(s);
    return json(view(s, String(params.key)), 1, 201);
  }),
  http.get(`${P}/shared-steps/:n`, ({ params }) => {
    const s = find(String(params.n));
    return s ? json(view(s, String(params.key)), s.revision) : err(404, 'not_found', 'shared step not found');
  }),
  http.put(`${P}/shared-steps/:n`, async ({ request, params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    const bad = ifMatch(request, s);
    if (bad) return bad;
    if (s.archivedAt) return err(409, 'shared_step_archived', `SS-${String(s.number)} is archived; restore it first`);
    const b = (await request.json()) as WriteBody;
    const inv = invalid(b);
    if (inv) return inv;
    save(s, (b.name ?? '').trim(), b.description ?? '', b.steps ?? []);
    return json(view(s, String(params.key)), s.revision);
  }),
  http.delete(`${P}/shared-steps/:n`, ({ request, params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    if (new URL(request.url).searchParams.get('purge') === 'true') {
      if (s.usedBy.length || s.openRuns) return err(409, 'shared_step_in_use', `SS-${String(s.number)} is used by cases or open runs`, { cases: s.usedBy.length, openRuns: s.openRuns });
      setSharedAll(sharedAll().filter((x) => x !== s));
    } else {
      s.archivedAt = new Date().toISOString();
      s.revision += 1;
    }
    return new HttpResponse(null, { status: 204 });
  }),
  http.post(`${P}/shared-steps/:n/restore`, ({ params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    s.archivedAt = null;
    s.revision += 1;
    return json(view(s, String(params.key)), s.revision);
  }),
  http.get(`${P}/shared-steps/:n/versions`, ({ params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    return json({ items: s.versions.map((v) => ({ version: v.version, source: v.source, changed: v.changed, createdBy: v.createdBy, createdAt: v.createdAt })), next: null });
  }),
  http.get(`${P}/shared-steps/:n/versions/:v`, ({ params }) => {
    const v = find(String(params.n))?.versions.find((x) => String(x.version) === String(params.v));
    return v ? json(v) : err(404, 'not_found', 'version not found');
  }),
  http.post(`${P}/shared-steps/:n/versions/:v/restore`, ({ request, params }) => {
    const s = find(String(params.n));
    const v = s?.versions.find((x) => String(x.version) === String(params.v));
    if (!s || !v) return err(404, 'not_found', 'version not found');
    const bad = ifMatch(request, s);
    if (bad) return bad;
    save(s, v.snapshot.name, v.snapshot.description, structuredClone(v.snapshot.steps), 'restore');
    return json(view(s, String(params.key)), s.revision);
  }),
  http.get(`${P}/shared-steps/:n/usages`, ({ params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    const cases = tmState().cases;
    const items = s.usedBy.map((n) => {
      const c = cases.get(n);
      return { caseNumber: n, name: c?.name ?? `Case ${String(n)}`, status: c?.status ?? 'active', archived: !!c?.archivedAt };
    });
    return json({ items, next: null, openRuns: s.openRuns });
  }),
  http.post(`${P}/shared-steps/:n/inline`, ({ params }) => {
    const s = find(String(params.n));
    if (!s) return err(404, 'not_found', 'shared step not found');
    const updated = s.usedBy.length;
    s.usedBy = [];
    return json({ updated, skipped: 0 });
  }),
  http.get(`${P}/cases/:n/shared-step-changes`, ({ params }) => {
    const n = Number(params.n);
    const items = sharedAll()
      .filter((s) => s.usedBy.includes(n))
      .flatMap((s) => s.versions.filter((v) => v.version > 1).map((v) => ({ sharedStepNumber: s.number, name: s.name, version: v.version, source: v.source, changed: v.changed, createdBy: v.createdBy, createdAt: v.createdAt })))
      .sort((a, b) => b.createdAt.localeCompare(a.createdAt));
    return json({ items, next: null });
  }),
  http.post(`${P}/cases/:n/extract-shared`, async ({ request, params }) => {
    const c = tmState().cases.get(Number(params.n));
    if (!c) return err(404, 'not_found', 'case not found');
    const h = request.headers.get('If-Match');
    if (!h) return err(428, 'precondition_required', 'send If-Match with the ETag of the resource');
    if (h !== `"r${String(c.revision)}"`) return err(412, 'precondition_failed', 'the resource changed since it was read', { currentRevision: c.revision, changedBy: c.updatedBy, source: 'ui' });
    const b = (await request.json()) as { stepIds: string[]; name: string; description?: string };
    const idx = c.body.steps.map((s, i) => (b.stepIds.includes(s.id) ? i : -1)).filter((i) => i >= 0);
    const first = idx[0] ?? 0;
    const last = idx[idx.length - 1] ?? 0;
    if (!idx.length || idx.length !== b.stepIds.length || last - first + 1 !== idx.length) return err(400, 'invalid_case', 'stepIds: the steps must be adjacent top-level steps');
    const picked = c.body.steps.slice(first, last + 1);
    const at = new Date().toISOString();
    const n = Math.max(0, ...sharedAll().map((s) => s.number)) + 1;
    const created: MockShared = {
      id: uuid(n),
      number: n,
      name: b.name.trim(),
      description: b.description ?? '',
      steps: picked,
      version: 1,
      revision: 1,
      archivedAt: null,
      aiUnreviewed: false,
      versions: [{ version: 1, source: 'ui', changed: ['name', 'description', 'steps'], createdBy: 'you', createdAt: at, snapshot: { name: b.name.trim(), description: b.description ?? '', steps: picked } }],
      usedBy: [c.number],
      openRuns: 0,
      createdBy: 'you',
      createdAt: at,
      updatedBy: 'you',
      updatedAt: at,
    };
    sharedAll().push(created);
    c.body.steps.splice(first, picked.length, { id: sid(), action: '', expected: '', data: '', attachments: [], steps: [], shared: { id: created.id, number: created.number } });
    c.revision += 1;
    c.version += 1;
    const r = await fetch(`${API}/projects/${String(params.key)}/cases/${String(params.n)}`);
    const caseView = (await r.json()) as Record<string, unknown>;
    return json({ sharedStep: view(created, String(params.key)), case: caseView }, undefined, 201);
  }),
  http.post(`${P}/run-items/:id/refresh`, async ({ params }) => {
    const item = tmState().runItems.find((r) => r.id === String(params.id));
    if (!item) return err(404, 'not_found', 'run item not found');
    if (item.status !== 'untested' || item.stepResults.length > 0) return err(409, 'run_item_started', 'the item has results; start a new execution instead');
    const c = tmState().cases.get(item.caseNumber);
    if (c) item.steps = expandSharedForRun(structuredClone(c.body.steps));
    item.revision += 1;
    const r = await fetch(`${API}/projects/${String(params.key)}/run-items/${String(params.id)}`);
    const body = (await r.json()) as { revision: number };
    return json(body, body.revision);
  }),
];
