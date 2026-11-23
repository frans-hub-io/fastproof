import { http, HttpResponse } from 'msw';
import type { DefectGroup, LogLevel, ProjectSettings, RerunRequest, Rule, TestPlanCreate, TriageRequest } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { launchDef, launchResults, launchView, projects, RUNNING_LAUNCH_ID, type MockRole } from './fixtures';
import {
  addDefectType,
  applyRule,
  clustersOf,
  compare,
  createPlan,
  createRule,
  dashboard,
  defectTypes,
  deleteDefectType,
  deleteRule,
  deliveries,
  envParam,
  filterLogs,
  flakyOf,
  gateOf,
  inEnv,
  jobs,
  liveBatch,
  mockInteractions,
  patchDefectType,
  patchRule,
  planJson,
  putSettings,
  RegexError,
  ruleMatches,
  rules,
  secretsOf,
  setImportant,
  settingsOf,
  rerunsOf,
  createRerun,
  similarTo,
  storedLogs,
  triage,
  writeSecret,
} from './stageC';

const API = '/api/v1';

interface Helpers {
  role: () => MockRole;
  error: (status: number, code: string, message: string, details?: unknown) => Response;
  audit: (action: string, scope: string | undefined, detail: unknown) => void;
}

function hasProject(key: string): boolean {
  return projects().some((p) => p.key === key);
}

const NAME = /^[a-z0-9][a-z0-9._-]{0,62}$/;

/** Open mock live log streams; `window.__fastproofMockLogs(n)` pushes `n` batches into each. */
const livePushers = new Set<(batches: number) => void>();

if (typeof window !== 'undefined') {
  (window as unknown as { __fastproofMockLogs?: (n?: number) => number }).__fastproofMockLogs = (n = 1) => {
    for (const push of livePushers) push(n);
    return livePushers.size;
  };
}
const MAX_REGEX = 1000;

export function stageCHandlers({ role, error, audit }: Helpers) {
  const forbid = (perm: string) => error(403, 'forbidden', `requires permission ${perm}`, { required: perm });
  const writer = () => role() === 'admin';

  return [
    http.get(`${API}/projects/:key/defect-types`, ({ params }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      return HttpResponse.json({ items: defectTypes() });
    }),

    http.post(`${API}/projects/:key/defect-types`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const b = (await request.json()) as { group?: string; name?: string; shortName?: string; color?: string };
      if (!b.name || b.name.length > 80) return error(400, 'invalid_body', 'name must be 1-80 characters', { field: 'name' });
      if (!b.group || !(DEFECT_GROUPS as readonly string[]).includes(b.group)) return error(400, 'invalid_body', 'unknown group', { field: 'group' });
      if (b.color && !/^#[0-9a-fA-F]{6}$/.test(b.color)) return error(400, 'invalid_body', 'color must be #rrggbb', { field: 'color' });
      const t = addDefectType({ group: b.group as DefectGroup, name: b.name, shortName: (b.shortName ?? b.name.slice(0, 3)).toUpperCase(), color: b.color ?? '#868e96' });
      audit('defectType.create', String(params.key), { id: t.id, name: t.name });
      return HttpResponse.json(t, { status: 201 });
    }),

    http.patch(`${API}/projects/:key/defect-types/:id`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const t = patchDefectType(String(params.id), (await request.json()) as Record<string, string>);
      if (!t) return error(404, 'not_found', 'defect type not found');
      audit('defectType.update', String(params.key), { id: t.id });
      return HttpResponse.json(t);
    }),

    http.delete(`${API}/projects/:key/defect-types/:id`, ({ params }) => {
      if (!writer()) return forbid('settings:write');
      const r = deleteDefectType(String(params.id));
      if (r === 'missing') return error(404, 'not_found', 'defect type not found');
      if (r === 'builtIn') return error(409, 'conflict', 'built-in defect types cannot be deleted');
      audit('defectType.delete', String(params.key), { id: params.id });
      return new HttpResponse(null, { status: 204 });
    }),

    http.post(`${API}/projects/:key/triage`, async ({ params, request }) => {
      if (!writer()) return forbid('results:triage');
      const body = (await request.json()) as TriageRequest;
      if (!body.resultIds?.length && !(body.launchId && body.fingerprint)) return error(400, 'invalid_body', 'give resultIds, or launchId and fingerprint', { field: 'resultIds' });
      if ((body.resultIds?.length ?? 0) > 1000) return error(400, 'invalid_body', 'at most 1000 results per request', { field: 'resultIds' });
      if (body.comment && body.comment.length > 4000) return error(400, 'invalid_body', 'comment is longer than 4000 characters', { field: 'comment' });
      if (body.issue && !/^https?:\/\//.test(body.issue.url ?? '')) return error(400, 'invalid_body', 'issue url must be http(s)', { field: 'issue.url' });
      if (body.defectTypeId && !defectTypes().some((t) => t.id === body.defectTypeId)) return error(400, 'invalid_body', 'unknown defect type', { field: 'defectTypeId' });
      if (body.mute && !body.mute.reason.trim()) return error(400, 'invalid_body', 'a mute needs a reason', { field: 'mute.reason' });
      const updated = triage(String(params.key), body);
      audit('result.triage', String(params.key), { updated, fingerprint: body.fingerprint, defectTypeId: body.defectTypeId, mute: !!body.mute, unmute: !!body.unmute });
      return HttpResponse.json({ updated });
    }),

    http.get(`${API}/projects/:key/launches/:id/clusters`, ({ params, request }) => {
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      const env = envParam(new URL(request.url).searchParams.get('env'));
      return HttpResponse.json({ items: clustersOf(def.project, def.launch.id, env) });
    }),

    http.get(`${API}/projects/:key/results/:id/similar`, ({ params, request }) => {
      const limit = Math.min(50, Math.max(1, Number(new URL(request.url).searchParams.get('limit') ?? 10) || 10));
      const items = similarTo(String(params.key), String(params.id), limit);
      if (!items) return error(404, 'not_found', 'result not found');
      return HttpResponse.json({ items });
    }),

    http.get(`${API}/projects/:key/results/:id/mock-interactions`, ({ params, request }) => {
      const attempt = Number(new URL(request.url).searchParams.get('attempt') ?? 1) || 1;
      const r = mockInteractions(String(params.key), String(params.id), attempt);
      if (!r) return error(404, 'not_found', 'result not found');
      if (r === 'notConfigured') return error(409, 'not_configured', 'FastMock is not configured for this project');
      return HttpResponse.json(r);
    }),

    http.get(`${API}/projects/:key/rules`, ({ params }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      return HttpResponse.json({ items: String(params.key) === 'web-shop' ? rules() : [] });
    }),

    http.post(`${API}/projects/:key/rules/test`, async ({ request }) => {
      if (!writer()) return forbid('rules:write');
      const body = (await request.json()) as { rule?: Partial<Rule>; launchId?: string };
      if (!body.launchId || !launchDef(body.launchId)) return error(400, 'invalid_body', 'unknown launch', { field: 'launchId' });
      const match = body.rule?.match ?? {};
      if ((match.messageRegex?.length ?? 0) > MAX_REGEX || (match.traceRegex?.length ?? 0) > MAX_REGEX) return error(400, 'invalid_body', 'regex is too long', { field: 'match.messageRegex' });
      try {
        return HttpResponse.json({ matches: ruleMatches(body.launchId, match) });
      } catch (e) {
        if (e instanceof RegexError) return error(400, 'invalid_regex', e.message, { field: 'match.messageRegex' });
        throw e;
      }
    }),

    http.post(`${API}/projects/:key/rules`, async ({ params, request }) => {
      if (!writer()) return forbid('rules:write');
      const body = (await request.json()) as Partial<Rule>;
      if (!body.name?.trim()) return error(400, 'invalid_body', 'name is required', { field: 'name' });
      try {
        const r = createRule(body);
        audit('rule.create', String(params.key), { id: r.id, name: r.name });
        return HttpResponse.json(r, { status: 201 });
      } catch (e) {
        if (e instanceof RegexError) return error(400, 'invalid_regex', e.message, { field: 'match.messageRegex' });
        throw e;
      }
    }),

    http.post(`${API}/projects/:key/rules/:id/apply`, ({ params, request }) => {
      if (!writer()) return forbid('rules:write');
      const launchId = new URL(request.url).searchParams.get('launchId') ?? '';
      if (!launchDef(launchId)) return error(400, 'invalid_query', 'unknown launch', { field: 'launchId' });
      const updated = applyRule(String(params.id), launchId);
      if (updated === null) return error(404, 'not_found', 'rule not found');
      audit('rule.apply', String(params.key), { id: params.id, launchId, updated });
      return HttpResponse.json({ updated });
    }),

    http.patch(`${API}/projects/:key/rules/:id`, async ({ params, request }) => {
      if (!writer()) return forbid('rules:write');
      try {
        const r = patchRule(String(params.id), (await request.json()) as Partial<Rule>);
        if (!r) return error(404, 'not_found', 'rule not found');
        audit('rule.update', String(params.key), { id: r.id });
        return HttpResponse.json(r);
      } catch (e) {
        if (e instanceof RegexError) return error(400, 'invalid_regex', e.message, { field: 'match.messageRegex' });
        throw e;
      }
    }),

    http.delete(`${API}/projects/:key/rules/:id`, ({ params }) => {
      if (!writer()) return forbid('rules:write');
      if (!deleteRule(String(params.id))) return error(404, 'not_found', 'rule not found');
      audit('rule.delete', String(params.key), { id: params.id });
      return new HttpResponse(null, { status: 204 });
    }),

    http.get(`${API}/projects/:key/settings`, ({ params }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      return HttpResponse.json(settingsOf(String(params.key)));
    }),

    http.put(`${API}/projects/:key/settings`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const body = (await request.json()) as Partial<ProjectSettings>;
      if (body.flaky && (body.flaky.historyDepth < 2 || body.flaky.historyDepth > 100)) return error(400, 'invalid_body', 'flaky.historyDepth must be 2..100', { field: 'flaky.historyDepth' });
      if (body.retention && (body.retention.rawDays < 1 || body.retention.rawDays > 3650)) return error(400, 'invalid_body', 'retention.rawDays must be 1..3650', { field: 'retention.rawDays' });
      const secrets = new Set(secretsOf(String(params.key)).map((s) => s.name));
      for (const ref of [body.vcs?.tokenSecretRef, body.fastmock?.tokenSecretRef, ...(body.notifications ?? []).map((n) => n.secretRef)]) {
        if (ref && !secrets.has(ref)) return error(400, 'invalid_body', `unknown secret ${ref}`, { field: 'secretRef' });
      }
      for (const n of body.notifications ?? []) {
        if (n.kind !== 'teams') continue;
        if (n.target) return error(400, 'invalid_body', 'teams notifications: the workflow URL is a credential; store it in a secret with purpose teams and refer to it with targetSecretRef (target is not accepted)', { field: 'notifications.target' });
        const ref = secretsOf(String(params.key)).find((x) => x.name === n.targetSecretRef);
        if (!n.targetSecretRef) return error(400, 'invalid_body', 'teams notifications need targetSecretRef (a secret with purpose teams holding the workflow URL)', { field: 'notifications.targetSecretRef' });
        if (ref && ref.purpose !== 'teams') return error(400, 'invalid_body', `the secret "${ref.name}" is for ${ref.purpose}, not teams`, { field: 'notifications.targetSecretRef' });
      }
      for (const t of body.issueTrackers ?? []) {
        if (!t.urlTemplate.includes('{key}')) return error(400, 'invalid_body', 'urlTemplate must contain {key}', { field: 'issueTrackers.urlTemplate' });
      }
      audit('settings.update', String(params.key), { sections: Object.keys(body) });
      return HttpResponse.json(putSettings(String(params.key), body));
    }),

    http.get(`${API}/projects/:key/secrets`, ({ params }) => {
      if (!writer()) return forbid('settings:write');
      return HttpResponse.json({ items: secretsOf(String(params.key)) });
    }),

    http.put(`${API}/projects/:key/secrets/:name`, async ({ params, request }) => {
      if (!writer()) return forbid('settings:write');
      const name = String(params.name);
      if (!NAME.test(name)) return error(400, 'invalid_body', 'name must match [a-z0-9][a-z0-9._-]{0,62}', { field: 'name' });
      const body = (await request.json()) as { value?: string; purpose?: string; origin?: string };
      if (!body.value || body.value.length > 8192) return error(400, 'invalid_body', 'value must be 1-8192 characters', { field: 'value' });
      if (!body.purpose || !['vcs', 'fastmock', 'slack', 'telegram', 'webhook', 'smtp', 'teams'].includes(body.purpose)) return error(400, 'invalid_body', 'purpose is required', { field: 'purpose' });
      if (body.purpose === 'teams') {
        if (!/^https:\/\/[^\s/]+\/\S*$/.test(body.value)) return error(400, 'invalid_body', 'a teams secret is the workflow URL: https://… with its sig parameter', { field: 'value' });
        body.origin = new URL(body.value).origin;
      }
      if ((body.purpose === 'vcs' || body.purpose === 'fastmock') && !body.origin) return error(400, 'invalid_body', `a ${body.purpose} secret needs origin`, { field: 'origin' });
      writeSecret(String(params.key), name, body.purpose, body.origin ?? null);
      audit('secret.write', String(params.key), { name, purpose: body.purpose });
      return new HttpResponse(null, { status: 204 });
    }),

    http.get(`${API}/projects/:key/flaky`, ({ params, request }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      const u = new URL(request.url).searchParams;
      return HttpResponse.json({ items: flakyOf(String(params.key), u.get('branch') ?? undefined, u.get('environment') ?? undefined, Math.min(200, Number(u.get('limit') ?? 50) || 50)) });
    }),

    http.get(`${API}/projects/:key/launches/:a/compare/:b`, ({ params }) => {
      const r = compare(String(params.key), String(params.a), String(params.b));
      if (!r) return error(404, 'not_found', 'launch not found');
      return HttpResponse.json(r);
    }),

    http.get(`${API}/projects/:key/dashboard`, ({ params, request }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      const u = new URL(request.url).searchParams;
      const bucket = u.get('bucket') ?? 'day';
      if (bucket !== 'day' && bucket !== 'week') return error(400, 'invalid_query', 'bucket must be day or week', { field: 'bucket' });
      return HttpResponse.json(dashboard(String(params.key), { branch: u.get('branch') ?? undefined, environment: u.get('environment') ?? undefined, from: u.get('from') ?? undefined, to: u.get('to') ?? undefined, bucket }));
    }),

    http.get(`${API}/projects/:key/launches/:id/gate`, ({ params }) => {
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      return HttpResponse.json(gateOf(def.project, launchView(def)));
    }),

    http.post(`${API}/projects/:key/notifications/:id/test`, ({ params }) => {
      if (!writer()) return forbid('settings:write');
      const n = settingsOf(String(params.key)).notifications.find((x) => x.id === params.id);
      if (!n) return error(404, 'not_found', 'notification not found');
      audit('notification.test', String(params.key), { id: n.id, kind: n.kind });
      const status = n.kind === 'teams' ? 202 : n.kind === 'email' ? 250 : n.kind === 'webhook' ? 204 : 200;
      return HttpResponse.json({ delivered: true, responseStatus: status, error: null });
    }),

    http.get(`${API}/projects/:key/notifications/deliveries`, ({ params }) => {
      if (!hasProject(String(params.key))) return error(404, 'not_found', 'project not found');
      return HttpResponse.json({ items: deliveries(String(params.key)), next: null });
    }),

    http.patch(`${API}/projects/:key/launches/:id`, async ({ params, request }) => {
      if (!writer()) return forbid('launches:write');
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      const body = (await request.json()) as { important?: boolean };
      if (typeof body.important === 'boolean') setImportant(def.launch.id, body.important);
      audit('launch.update', def.project, { id: def.launch.id, important: body.important });
      return HttpResponse.json({ ...launchView(def), important: body.important });
    }),

    http.post(`${API}/projects/:key/test-plans`, async ({ params, request }) => {
      if (!writer()) return forbid('launches:write');
      const body = (await request.json()) as TestPlanCreate;
      if (!body.name.trim() || body.name.length > 200) return error(400, 'invalid_body', 'name must be 1-200 characters', { field: 'name' });
      if (body.fromLaunchId && !launchDef(body.fromLaunchId)) return error(400, 'invalid_body', 'unknown launch', { field: 'fromLaunchId' });
      const id = createPlan(String(params.key), body);
      audit('testPlan.create', String(params.key), { id, name: body.name });
      return HttpResponse.json({ id }, { status: 201 });
    }),

    http.get(`${API}/projects/:key/test-plans/:id/testplan.json`, ({ params }) => {
      const plan = planJson(String(params.id));
      if (!plan) return error(404, 'not_found', 'test plan not found');
      return HttpResponse.json(plan, { headers: { 'Content-Disposition': 'attachment; filename="testplan.json"' } });
    }),

    http.get(`${API}/projects/:key/launches/:id/reruns`, ({ params }) => {
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      return HttpResponse.json(rerunsOf(def.launch.id));
    }),

    http.post(`${API}/projects/:key/launches/:id/rerun`, async ({ params, request }) => {
      if (!writer()) return forbid('launches:write');
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      const body = (await request.json()) as RerunRequest;
      if (!settingsOf(def.project).rerun) return error(409, 'conflict', 'the project has no rerun settings (integrations: rerun)');
      const r = createRerun(def.launch.id, body);
      if (r.testCount === 0) return error(409, 'conflict', 'no tests to run: the launch has no such results');
      audit('launch.rerun', def.project, { rerunId: r.id, tests: r.testCount });
      return HttpResponse.json(r, { status: 201 });
    }),

    http.get(`${API}/admin/jobs`, () => {
      if (role() !== 'admin') return forbid('settings:write');
      return HttpResponse.json({ items: jobs() });
    }),

    http.get(`${API}/projects/:key/launches/:id/logs/stream`, ({ params, request }) => {
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      const u = new URL(request.url).searchParams;
      const resultId = u.get('resultId');
      const level = u.get('level');
      const q = u.get('q');
      const encoder = new TextEncoder();
      let timer: ReturnType<typeof setInterval> | undefined;
      let pusher: ((batches: number) => void) | undefined;
      const stop = () => {
        clearInterval(timer);
        if (pusher) livePushers.delete(pusher);
      };
      const running = def.launch.id === RUNNING_LAUNCH_ID;
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(encoder.encode('retry: 3000\n\n'));
          if (!running) {
            controller.enqueue(encoder.encode('event: end\ndata: {}\n\n'));
            return;
          }
          let tick = 0;
          const emit = (gapEvery: boolean) => {
            tick++;
            try {
              const lines = filterLogs(liveBatch(def.launch.id, resultId, tick), level, q);
              if (lines.length) controller.enqueue(encoder.encode(`event: log\ndata: ${JSON.stringify({ lines })}\n\n`));
              if (gapEvery && tick % 12 === 6) controller.enqueue(encoder.encode(`event: gap\ndata: ${JSON.stringify({ skipped: 120 + tick })}\n\n`));
            } catch {
              stop();
            }
          };
          pusher = (batches: number) => {
            for (let i = 0; i < batches; i++) emit(false);
          };
          livePushers.add(pusher);
          timer = setInterval(() => emit(true), 250);
          request.signal.addEventListener('abort', stop);
        },
        cancel() {
          stop();
        },
      });
      return new HttpResponse(stream, { headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' } });
    }),

    http.get(`${API}/projects/:key/launches/:id/logs`, ({ params, request }) => {
      const def = launchDef(String(params.id));
      if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
      const u = new URL(request.url).searchParams;
      const level = u.get('level');
      if (level && !(['trace', 'debug', 'info', 'warn', 'error'] as LogLevel[]).includes(level as LogLevel)) return error(400, 'invalid_query', 'unknown level', { field: 'level' });
      const attempt = u.get('attempt') ? Number(u.get('attempt')) : null;
      const env = envParam(u.get('env'));
      const keys = env === undefined ? null : new Set(launchResults(def.launch.id).filter((r) => inEnv(r, env)).flatMap((r) => [r.id, ...(r.clientIds ?? [])]));
      const all = filterLogs(storedLogs(def.launch.id, u.get('resultId'), attempt), level, u.get('q')).filter((l) => !keys || keys.has(l.resultId));
      const limit = Math.min(10_000, Math.max(1, Number(u.get('limit') ?? 1000) || 1000));
      const before = u.get('before');
      const end = before ? Math.max(0, Math.min(all.length, Number(before))) : all.length;
      const start = Math.max(0, end - limit);
      return HttpResponse.json({ items: all.slice(start, end), next: start > 0 ? String(start) : null });
    }),
  ];
}

