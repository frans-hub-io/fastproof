import { http, HttpResponse } from 'msw';
import type { DashboardWidget, DashboardWrite } from '../api/dashboards';
import { projects, type MockRole } from './fixtures';
import { cloneDashboard, createDashboard, dashboardData, deleteDashboard, getDashboard, listDashboards, lockDashboard, saveDashboard, validateWrite } from './dashboards';

const API = '/api/v1';

interface Helpers {
  role: () => MockRole;
  error: (status: number, code: string, message: string, details?: unknown) => Response;
  audit: (action: string, scope: string | undefined, detail: unknown) => void;
}

const ME = 'alex@example.com';

export function dashboardHandlers({ role, error, audit }: Helpers) {
  const forbid = (perm: string) => error(403, 'forbidden', `requires permission ${perm}`, { required: perm });
  const known = (key: string) => projects().some((p) => p.key === key);
  const withEtag = (body: { revision: number }, status = 200) => HttpResponse.json(body, { status, headers: { ETag: `"r${String(body.revision)}"` } });
  const revisionOf = (h: string | null): number => /^(?:W\/)?"r(\d+)"$/.exec(h ?? '')?.[1] ? Number(/(\d+)/.exec(h ?? '')?.[1]) : -1;

  return [
    http.get(`${API}/projects/:key/dashboards`, ({ params }) => {
      if (!known(String(params.key))) return error(404, 'not_found', 'project not found');
      return HttpResponse.json({ items: listDashboards(String(params.key)) });
    }),

    http.post(`${API}/projects/:key/dashboards`, async ({ params, request }) => {
      const key = String(params.key);
      if (!known(key)) return error(404, 'not_found', 'project not found');
      if (role() !== 'admin') return forbid('dashboards:write');
      const b = (await request.json()) as Partial<DashboardWrite>;
      const bad = validateWrite(b);
      if (bad) return error(400, 'invalid_body', bad);
      const d = createDashboard(key, ME, b as DashboardWrite);
      if (d === 'limit') return error(409, 'conflict', 'a project has at most 50 dashboards');
      audit('dashboard.create', key, { id: d.id, name: d.name });
      return withEtag(d, 201);
    }),

    http.post(`${API}/projects/:key/dashboard-preview`, async ({ params, request }) => {
      if (!known(String(params.key))) return error(404, 'not_found', 'project not found');
      if (role() !== 'admin') return forbid('dashboards:write');
      const b = (await request.json()) as { widgets?: DashboardWidget[] };
      if (!Array.isArray(b.widgets) || b.widgets.length > 24) return error(400, 'invalid_body', 'at most 24 widgets');
      return HttpResponse.json(dashboardData(String(params.key), b.widgets, role() === 'admin'));
    }),

    http.get(`${API}/projects/:key/dashboards/:id`, ({ params }) => {
      const d = getDashboard(String(params.key), String(params.id));
      return d ? withEtag(d) : error(404, 'not_found', 'dashboard not found');
    }),

    http.get(`${API}/projects/:key/dashboards/:id/data`, ({ params }) => {
      const d = getDashboard(String(params.key), String(params.id));
      return d ? HttpResponse.json(dashboardData(String(params.key), d.widgets, role() === 'admin')) : error(404, 'not_found', 'dashboard not found');
    }),

    http.put(`${API}/projects/:key/dashboards/:id`, async ({ params, request }) => {
      const key = String(params.key);
      if (role() !== 'admin') return forbid('dashboards:write');
      const b = (await request.json()) as Partial<DashboardWrite>;
      const bad = validateWrite(b);
      if (bad) return error(400, 'invalid_body', bad);
      const r = saveDashboard(key, String(params.id), revisionOf(request.headers.get('If-Match')), b as DashboardWrite);
      if (r === 'missing') return error(404, 'not_found', 'dashboard not found');
      if (r === 'stale') return error(412, 'precondition_failed', 'the dashboard changed since it was read', { currentRevision: getDashboard(key, String(params.id))?.revision, changedBy: 'sam@example.com', source: 'ui' });
      audit('dashboard.update', key, { id: r.id, widgets: r.widgets.length });
      return withEtag(r);
    }),

    http.put(`${API}/projects/:key/dashboards/:id/lock`, async ({ params, request }) => {
      if (role() !== 'admin') return forbid('dashboards:write');
      const b = (await request.json()) as { locked?: boolean };
      if (typeof b.locked !== 'boolean') return error(400, 'invalid_body', 'locked must be true or false');
      const rev = revisionOf(request.headers.get('If-Match'));
      if (request.headers.get('If-Match') === null) return error(428, 'precondition_required', 'send If-Match with the ETag of the resource');
      const d = lockDashboard(String(params.key), String(params.id), rev, b.locked);
      if (d === 'missing') return error(404, 'not_found', 'dashboard not found');
      if (d === 'stale') return error(412, 'precondition_failed', 'the dashboard changed since it was read', { currentRevision: getDashboard(String(params.key), String(params.id))?.revision, changedBy: null, source: 'ui' });
      audit(b.locked ? 'dashboard.lock' : 'dashboard.unlock', String(params.key), { id: d.id });
      return withEtag(d);
    }),

    http.post(`${API}/projects/:key/dashboards/:id/clone`, async ({ params, request }) => {
      const key = String(params.key);
      if (role() !== 'admin') return forbid('dashboards:write');
      const b = (await request.json()) as { name?: string; targetProject?: string };
      const target = b.targetProject || key;
      if (!known(target)) return error(404, 'not_found', 'project not found');
      const d = cloneDashboard(key, String(params.id), ME, b.name, target);
      if (d === 'missing') return error(404, 'not_found', 'dashboard not found');
      if (d === 'limit') return error(409, 'conflict', 'a project has at most 50 dashboards');
      audit('dashboard.clone', key, { from: params.id, id: d.id, targetProject: target });
      return HttpResponse.json({ ...d, project: target }, { status: 201, headers: { ETag: `"r${String(d.revision)}"` } });
    }),

    http.delete(`${API}/projects/:key/dashboards/:id`, ({ params, request }) => {
      if (role() !== 'admin') return forbid('dashboards:write');
      if (request.headers.get('If-Match') === null) return error(428, 'precondition_required', 'send If-Match with the ETag of the resource');
      const r = deleteDashboard(String(params.key), String(params.id), revisionOf(request.headers.get('If-Match')));
      if (r === 'missing') return error(404, 'not_found', 'dashboard not found');
      if (r === 'stale') return error(412, 'precondition_failed', 'the dashboard changed since it was read');
      audit('dashboard.delete', String(params.key), { id: params.id });
      return new HttpResponse(null, { status: 204 });
    }),
  ];
}
