import { http, HttpResponse, type HttpHandler } from 'msw';
import type { BacktestReport, ClusterSuggestions, LaunchSuggestions, Suggestion, SuggestionBand, WhyItem } from '../api/suggestions';
import type { Cluster } from '../api/types-next';
import { journalTriage } from './analysis';
import { launchDef, projectLaunches } from './fixtures';
import { analyze, BUILT_IN_TYPES, clustersOf, createRule, rules, triage, triageTargets } from './stageC';

const API = '/api/v1';

/** What happened to mock suggestions (by id) and the mock backtests (by project). */
const state = {
  decided: new Map<string, { state: Suggestion['state']; by: string }>(),
  backtests: new Map<string, BacktestReport>(),
};

export function resetSuggestions(): void {
  state.decided.clear();
  state.backtests.clear();
}

function error(status: number, code: string, message: string, details?: unknown) {
  return HttpResponse.json({ code, message, ...(details === undefined ? {} : { details }) }, { status });
}

function hash(s: string): number {
  let h = 0;
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h;
}

function groupOf(message: string): string {
  const m = message.toLowerCase();
  if (/timeout|timed out|locator|selector|element/.test(m)) return 'automationBug';
  if (/connect|refused|503|dns|unavailable|reset/.test(m)) return 'systemIssue';
  return 'productBug';
}

function typeOf(group: string) {
  return BUILT_IN_TYPES.find((t) => t.group === group) ?? BUILT_IN_TYPES[0];
}

function why(band: SuggestionBand, h: number): WhyItem[] {
  if (band === 'same')
    return [
      { signal: 'sameFingerprint', weight: 0.6, detail: 'The same error (fingerprint)' },
      { signal: 'testCase', weight: 0.1, detail: 'The same test was decided this way' },
      { signal: 'confirmed', weight: 0.09, detail: `Decided this way ${String(3 + (h % 5))} times by people, last by alex@example.com` },
      { signal: 'branch', weight: 0.03, detail: 'Decided this way on the branch main' },
    ];
  const s = band === 'high' ? 0.86 : 0.62;
  return [
    { signal: 'message', weight: Math.round(0.45 * s * 1000) / 1000, detail: `The message is ${String(Math.round(s * 100))}% similar` },
    { signal: 'frames', weight: 0.133, detail: '2 of 3 stack frames are shared' },
    { signal: 'environment', weight: 0.05, detail: 'Decided this way on the environment staging' },
    ...(band === 'high' ? [{ signal: 'confirmed' as const, weight: 0.06, detail: 'Decided this way 2 times by people, last by sam@example.com' }] : []),
    { signal: 'age', weight: -0.05, detail: `Last decided ${String(10 + (h % 40))} days ago` },
  ];
}

function precisionOf(project: string, band: SuggestionBand): number | null {
  const b = state.backtests.get(project)?.bands.find((x) => x.band === band);
  return b?.shown ? (b.precision ?? null) : null;
}

function suggestionsOf(project: string, launchId: string, c: Cluster): Suggestion[] {
  const h = hash(c.fingerprint);
  const band: SuggestionBand = (['same', 'high', 'low'] as const)[h % 3] ?? 'low';
  const group = groupOf(c.message ?? '');
  const t = typeOf(group);
  const prev = projectLaunches(project).find((d) => d.launch.id !== launchId);
  const mk = (rank: number, b: SuggestionBand, ty: typeof t, score: number): Suggestion => {
    const id = `sg-${launchId.slice(-6)}-${c.fingerprint}-${String(rank)}`;
    const decided = state.decided.get(id);
    return {
      id,
      rank,
      band: b,
      score,
      precision: precisionOf(project, b),
      defectTypeId: ty?.id ?? 'productBug',
      group: ty?.group ?? 'productBug',
      defectName: ty?.name ?? 'Product bug',
      comment: b === 'same' ? 'Known since the cart rewrite' : null,
      issueUrl: b === 'same' && group === 'productBug' ? 'https://tracker.example.com/browse/PAY-1234' : null,
      issueKey: b === 'same' && group === 'productBug' ? 'PAY-1234' : null,
      signal: b === 'same' ? 'memory' : 'similar',
      why: why(b, h),
      sample: { resultId: c.sampleResultId, launchId: prev?.launch.id ?? launchId, launchNumber: prev?.launch.number ?? 431, decidedBy: 'alex@example.com', decidedAt: new Date(Date.now() - 86_400_000 * (2 + (h % 20))).toISOString(), confirmed: b === 'same' ? 3 + (h % 5) : 2, overridden: 0 },
      ruleId: null,
      state: decided?.state ?? 'open',
      ruleDraftOffered: b === 'same' && !rules().some((r) => r.match.fingerprint === c.fingerprint),
    };
  };
  const out = [mk(1, band, t, band === 'same' ? 0.82 : band === 'high' ? 0.74 : 0.48)];
  if (band === 'high') out.push(mk(2, 'low', typeOf(group === 'productBug' ? 'systemIssue' : 'productBug'), 0.44));
  return out;
}

function clusterView(project: string, launchId: string, c: Cluster): ClusterSuggestions {
  const person = c.defect?.source === 'user' || c.defect?.source === 'ai';
  const rule = c.defect?.source === 'rule' && !!c.defect.ruleId;
  const decided = person ? 'person' : rule ? 'rule' : null;
  const all = c.isNew ? [] : suggestionsOf(project, launchId, c);
  const list = decided ? all.filter((s) => s.state === 'accepted') : all.filter((s) => s.state === 'open' || s.state === 'accepted');
  return {
    fingerprint: c.fingerprint,
    message: c.message ?? null,
    count: c.count,
    sampleResultId: c.sampleResultId,
    defect: c.defect ?? null,
    decided,
    isNew: c.isNew,
    newOnBranch: false,
    errorState: c.isNew ? 'new' : 'active',
    commits: c.isNew ? null : { lastGood: 'a1b2c3d4e5f6', first: 'd4e5f6a1b2c3', compareUrl: 'https://git.example.com/shop/compare/a1b2c3d...d4e5f6a' },
    signals: [],
    suggestions: list,
    noSuggestion: decided ? 'decided' : c.isNew ? 'newError' : list.length ? null : 'belowThreshold',
  };
}

function find(project: string, id: string): { launchId: string; cluster: Cluster; s: Suggestion } | null {
  for (const d of projectLaunches(project)) {
    if (!id.includes(d.launch.id.slice(-6))) continue;
    for (const c of clustersOf(project, d.launch.id)) {
      const s = suggestionsOf(project, d.launch.id, c).find((x) => x.id === id);
      if (s) return { launchId: d.launch.id, cluster: c, s };
    }
  }
  return null;
}

function report(project: string): BacktestReport {
  const now = Date.now();
  return {
    id: `bt-${project}`,
    source: 'project',
    from: new Date(now - 180 * 86_400_000).toISOString(),
    to: new Date(now - 3_600_000).toISOString(),
    decisions: 1240,
    coverage: 0.812,
    noSuggestion: 233,
    bands: [
      { band: 'same', suggested: 640, correct: 631, correctGroup: 636, precision: 0.986, shown: true },
      { band: 'high', suggested: 280, correct: 249, correctGroup: 262, precision: 0.889, shown: true },
      { band: 'low', suggested: 87, correct: 49, correctGroup: 61, precision: 0.563, shown: true },
    ],
    confusions: [
      { expected: 'systemIssue', suggested: 'productBug', count: 14 },
      { expected: 'automationBug', suggested: 'systemIssue', count: 9 },
    ],
    truncated: false,
    createdAt: new Date(now).toISOString(),
    createdBy: 'alex@example.com',
  };
}

/** Suggestions of the mock launches (deterministic per fingerprint), accept, reject, rule
 * drafts, memory and the backtest. */
export const suggestionsHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/launches/:id/suggestions`, ({ params }) => {
    const key = String(params.key);
    const def = launchDef(String(params.id));
    if (!def || def.project !== key) return error(404, 'not_found', 'launch not found');
    const running = def.launch.status === 'running';
    const out: LaunchSuggestions = {
      launchId: def.launch.id,
      state: running ? 'pending' : 'ready',
      computedAt: running ? null : (def.launch.finishedAt ?? null),
      clusters: running ? [] : clustersOf(key, def.launch.id).map((c) => clusterView(key, def.launch.id, c)),
      truncated: false,
    };
    return HttpResponse.json(out);
  }),
  http.post(`${API}/projects/:key/launches/:id/suggestions/recompute`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    return HttpResponse.json({ state: 'queued', queuedAt: new Date().toISOString(), finishedAt: def.launch.finishedAt ?? null, steps: {} }, { status: 202 });
  }),
  http.post(`${API}/projects/:key/suggestions/:id/accept`, ({ params }) => {
    const key = String(params.key);
    const f = find(key, String(params.id));
    if (!f) return error(404, 'not_found', 'suggestion not found');
    if (f.s.state !== 'open') return error(409, 'suggestion_closed', `the suggestion is ${f.s.state}`, { state: f.s.state });
    const body = { launchId: f.launchId, fingerprint: f.cluster.fingerprint, defectTypeId: f.s.defectTypeId, source: 'user' as const };
    const targets = triageTargets(body).filter((r) => {
      const d = analyze(key, r).defect;
      return !d || (d.source !== 'user' && d.source !== 'ai');
    });
    const before = targets.map((r) => analyze(key, r));
    const updated = triage(key, { ...body, launchId: undefined, fingerprint: undefined, resultIds: targets.map((r) => r.id) });
    const batchId = journalTriage(key, before, targets.map((r) => analyze(key, r)), 'admin', 'suggestion', 'user');
    state.decided.set(f.s.id, { state: 'accepted', by: 'admin' });
    return HttpResponse.json({ suggestionId: f.s.id, updated, skipped: f.cluster.count - updated, batchId });
  }),
  http.post(`${API}/projects/:key/suggestions/:id/reject`, ({ params }) => {
    const key = String(params.key);
    const f = find(key, String(params.id));
    if (!f) return error(404, 'not_found', 'suggestion not found');
    if (f.s.state !== 'open') return error(409, 'suggestion_closed', `the suggestion is ${f.s.state}`, { state: f.s.state });
    state.decided.set(f.s.id, { state: 'rejected', by: 'admin' });
    return HttpResponse.json({ ...f.s, state: 'rejected' });
  }),
  http.post(`${API}/projects/:key/suggestions/:id/rule`, ({ params }) => {
    const key = String(params.key);
    const f = find(key, String(params.id));
    if (!f) return error(404, 'not_found', 'suggestion not found');
    const r = createRule({ name: `Same as ${f.cluster.fingerprint}`, enabled: false, match: { fingerprint: f.cluster.fingerprint }, action: { defectTypeId: f.s.defectTypeId } });
    return HttpResponse.json(r, { status: 201 });
  }),
  http.get(`${API}/projects/:key/analysis/memory`, ({ params, request }) => {
    const key = String(params.key);
    const fp = new URL(request.url).searchParams.get('fingerprint') ?? '';
    for (const d of projectLaunches(key)) {
      const c = clustersOf(key, d.launch.id).find((x) => x.fingerprint === fp);
      if (!c || c.isNew) continue;
      const s = suggestionsOf(key, d.launch.id, c)[0];
      if (!s) continue;
      return HttpResponse.json({
        items: [
          { fingerprint: fp, defectTypeId: s.defectTypeId, group: s.group, defectName: s.defectName, messageNorm: c.message ?? '', frames: [], confirmed: s.sample?.confirmed ?? 1, overridden: 0, comment: s.comment, issueUrl: s.issueUrl, issueKey: s.issueKey, testCases: c.count, branches: ['main'], environments: ['staging'], firstDecidedAt: s.sample?.decidedAt ?? new Date().toISOString(), lastDecidedAt: s.sample?.decidedAt ?? new Date().toISOString(), lastDecidedBy: 'alex@example.com', sample: s.sample },
        ],
      });
    }
    return HttpResponse.json({ items: [] });
  }),
  http.get(`${API}/projects/:key/analysis/backtest/latest`, ({ params }) => {
    const r = state.backtests.get(String(params.key));
    return r ? HttpResponse.json(r) : error(404, 'not_found', 'backtest not found');
  }),
  http.post(`${API}/projects/:key/analysis/backtest`, ({ params }) => {
    const key = String(params.key);
    state.backtests.set(key, report(key));
    return HttpResponse.json({ id: `task-bt-${key}`, kind: 'backtest', status: 'queued', progress: {}, error: null, createdAt: new Date().toISOString(), finishedAt: null }, { status: 202 });
  }),
];
