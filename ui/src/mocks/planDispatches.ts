import { http, HttpResponse, type HttpHandler } from 'msw';
import type { PlanCheck, PlanCi, PlanDispatch } from '../api/planDispatches';
import { settingsOf } from './stageC';
import { tmState } from './tm';

const API = '/api/v1';
const validValue = (v: string): boolean => v.length >= 1 && v.length <= 256 && Array.from({ length: v.length }, (_, i) => v.charCodeAt(i)).every((c) => c > 31 && c !== 127);
const ENV_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;

interface Store {
  dispatches: PlanDispatch[];
  checks: Map<string, PlanCheck>;
}

const store: Store = { dispatches: [], checks: new Map() };

export function resetPlanDispatches(): void {
  store.dispatches = [];
  store.checks = new Map();
}

export interface CiRefusal {
  status: number;
  code: string;
  message: string;
  details?: unknown;
}

export interface CiStart {
  gitRef: string;
  environments: { name: string | null; variables: Record<string, string> }[];
}

function glob(pattern: string, ref: string): boolean {
  const re = pattern
    .split('**')
    .map((part) => part.split('*').map((x) => x.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('[^/]*'))
    .join('.*');
  return new RegExp(`^${re}$`).test(ref);
}

/** Checks a plan start's `ci` against the project's rerun settings like the server does. */
export function planCiResolve(project: string, ci: PlanCi): CiStart | CiRefusal {
  const rerun = settingsOf(project).rerun;
  if (!rerun) return { status: 409, code: 'conflict', message: 'the project has no rerun settings (integrations: rerun)' };
  const allowed = rerun.variables ?? [];
  const fallback = rerun.ref ?? 'main';
  const gitRef = ci.ref ?? fallback;
  if (ci.ref !== undefined && ci.ref !== fallback && !(rerun.refs ?? []).some((g) => glob(g, ci.ref ?? ''))) {
    return { status: 400, code: 'ci_ref_not_allowed', message: "the ref matches none of the project's rerun.refs" };
  }
  const base: Record<string, string> = {};
  for (const v of allowed) if (v.default) base[v.name] = v.default;
  const take = (vars: Record<string, string> | null | undefined, into: Record<string, string>): CiRefusal | null => {
    for (const [name, value] of Object.entries(vars ?? {})) {
      const def = allowed.find((v) => v.name === name);
      if (!def || (def.values && def.values.length > 0 && !def.values.includes(value))) {
        return { status: 400, code: 'ci_variable_not_allowed', message: `CI variable ${name} is not allowed`, details: { name } };
      }
      if (!validValue(value)) return { status: 400, code: 'bad_request', message: `the value of ${name} has 1 to 256 bytes without control characters` };
      into[name] = value;
    }
    return null;
  };
  const refused = take(ci.variables, base);
  if (refused) return refused;
  const named = ci.environments ?? [];
  if (named.length > 10 || new Set(named.map((e) => e.name)).size !== named.length || named.some((e) => !ENV_NAME.test(e.name))) {
    return { status: 400, code: 'bad_request', message: 'environment names are unique, 1 to 64 letters, digits, . _ -' };
  }
  if (named.length === 0) return { gitRef, environments: [{ name: null, variables: base }] };
  const environments: CiStart['environments'] = [];
  for (const e of named) {
    const vars = { ...base };
    const r = take(e.variables, vars);
    if (r) return r;
    environments.push({ name: e.name, variables: vars });
  }
  return { gitRef, environments };
}

/** Records the dispatches of a start: an environment named `broken` is refused by the CI, the first launch gets a plan check with a warning. */
export function planCiDispatch(project: string, planId: string, automated: number, start: CiStart, launches: { id: string; number: number }[], by: string): PlanDispatch[] {
  const provider = settingsOf(project).rerun?.provider ?? 'github';
  const out = start.environments.map((e, i): PlanDispatch => {
    const l = launches[i];
    const failed = e.name === 'broken';
    const now = new Date().toISOString();
    return {
      id: `dispatch-${String(store.dispatches.length + i + 1)}`,
      planId,
      launchId: l?.id ?? '',
      launchNumber: l?.number ?? null,
      environment: e.name,
      provider,
      status: failed ? 'failed' : 'dispatched',
      error: failed ? 'the CI answered HTTP 422' : null,
      ciRef: failed || provider === 'webhook' ? null : `https://ci.example.com/acme/shop/-/pipelines/${String(100 + store.dispatches.length + i)}`,
      gitRef: start.gitRef,
      variables: e.variables,
      requestedBy: by,
      createdAt: now,
      dispatchedAt: failed ? null : now,
    };
  });
  store.dispatches = [...out.slice().reverse(), ...store.dispatches];
  const first = launches[0];
  if (first && automated > 0) {
    store.checks.set(first.id, { planId, inPlan: automated, ran: Math.max(automated - 1, 0), extra: 1, missing: Math.min(automated, 1), extraNames: ['checkout.legacy_flow'], warning: true });
  }
  return out;
}

export const planDispatchesHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/test-plans/:id/dispatches`, ({ params }) => {
    const items = store.dispatches.filter((d) => d.planId === params.id);
    return HttpResponse.json({ items, next: null });
  }),
  http.get(`${API}/projects/:key/launches/:id/plan-check`, ({ params }) => {
    const id = String(params.id);
    const planId = tmState().manual.get(id)?.planId;
    const c = store.checks.get(id) ?? (planId ? { planId, inPlan: 0, ran: 0, extra: 0, missing: 0, extraNames: [], warning: false } : undefined);
    if (!c) return HttpResponse.json({ code: 'not_found', message: 'plan check not found' }, { status: 404 });
    return HttpResponse.json(c);
  }),
];
