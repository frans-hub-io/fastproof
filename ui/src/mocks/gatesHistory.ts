import { http, HttpResponse, type HttpHandler } from 'msw';
import type { GateHistoryBranch, GateHistoryGate, GateHistoryItem, GateSimulate, GateSimulation } from '../api/gatesHistory';
import type { Launch } from '../api/types';
import type { Gate, GateResult, GateRule } from '../api/types-next';
import { projectLaunches, projects } from './fixtures';
import { gateOf, settingsOf } from './stageC';

const API = '/api/v1';

function error(status: number, code: string, message: string) {
  return HttpResponse.json({ code, message }, { status });
}

interface Eval {
  launch: Launch;
  at: string;
  report: GateResult;
}

/** The stored verdicts of the mock project: its finished launches against its current gates. */
function evaluations(project: string): Eval[] {
  return projectLaunches(project)
    .map((d) => d.launch)
    .filter((l) => l.status !== 'running')
    .map((launch) => ({ launch, at: launch.finishedAt ?? launch.startedAt, report: gateOf(project, launch) }))
    .filter((e) => e.report.results.length > 0)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at) || b.launch.number - a.launch.number);
}

function branchOf(l: Launch): string | null {
  return l.vcs?.branch ?? null;
}

function aggregate(evals: Eval[], gateFilter: string | null): { gates: GateHistoryGate[]; branches: GateHistoryBranch[] } {
  interface Acc {
    evaluations: number;
    failed: number;
    flips: number;
    metrics: Map<string, number>;
    last: Map<string, { failed: boolean; since: number | null }>;
    green: number[];
  }
  const gates = new Map<string, Acc>();
  const launches = new Map<string, { branch: string | null; failed: boolean }>();
  for (const e of [...evals].reverse()) {
    const names = [...new Set(e.report.results.filter((r) => !gateFilter || r.gate === gateFilter).map((r) => r.gate))];
    for (const name of names) {
      const g: Acc = gates.get(name) ?? { evaluations: 0, failed: 0, flips: 0, metrics: new Map(), last: new Map(), green: [] };
      gates.set(name, g);
      const bad = e.report.results.filter((r) => r.gate === name && !r.ok);
      const failed = bad.length > 0;
      g.evaluations += 1;
      if (failed) g.failed += 1;
      for (const r of bad) g.metrics.set(r.rule.metric, (g.metrics.get(r.rule.metric) ?? 0) + 1);
      const b = branchOf(e.launch) ?? '';
      const t = Date.parse(e.at);
      const prev = g.last.get(b);
      if (prev) {
        if (prev.failed !== failed) g.flips += 1;
        if (failed && !prev.failed) prev.since = t;
        if (!failed && prev.failed && prev.since !== null) {
          g.green.push((t - prev.since) / 1000);
          prev.since = null;
        }
        prev.failed = failed;
      } else {
        g.last.set(b, { failed, since: failed ? t : null });
      }
      const l = launches.get(e.launch.id) ?? { branch: branchOf(e.launch), failed: false };
      l.failed ||= failed;
      launches.set(e.launch.id, l);
    }
  }
  const out: GateHistoryGate[] = [...gates.entries()]
    .sort((a, b) => (a[0] < b[0] ? -1 : 1))
    .map(([gate, g]) => ({
      gate,
      evaluations: g.evaluations,
      failed: g.failed,
      flips: g.flips,
      meanSecondsToGreen: g.green.length ? g.green.reduce((a, b) => a + b, 0) / g.green.length : null,
      topFailingMetric: [...g.metrics.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))[0]?.[0] as GateHistoryGate['topFailingMetric'],
    }));
  const byBranch = new Map<string, { evaluations: number; failed: number }>();
  for (const l of launches.values()) {
    if (!l.branch) continue;
    const c = byBranch.get(l.branch) ?? { evaluations: 0, failed: 0 };
    c.evaluations += 1;
    if (l.failed) c.failed += 1;
    byBranch.set(l.branch, c);
  }
  const branches = [...byBranch.entries()].map(([branch, c]) => ({ branch, ...c })).sort((a, b) => b.evaluations - a.evaluations || (a.branch < b.branch ? -1 : 1));
  return { gates: out, branches };
}

function holds(r: GateRule, actual: number): boolean {
  return r.op === 'gt' ? actual > r.value : r.op === 'gte' ? actual >= r.value : r.op === 'lt' ? actual < r.value : actual <= r.value;
}

function simulate(project: string, body: GateSimulate): GateSimulation {
  const gate: Gate = body.gate;
  const launches = projectLaunches(project)
    .map((d) => d.launch)
    .filter((l) => l.status !== 'running' && (!body.branch || branchOf(l) === body.branch))
    .sort((a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt))
    .slice(0, body.launches ?? 50);
  const sim: GateSimulation = { evaluated: 0, failed: 0, skipped: 0, items: [] };
  for (const l of launches) {
    const branch = branchOf(l) ?? '';
    if (gate.branches?.length && !gate.branches.some((p) => new RegExp(`^${p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')}$`).test(branch))) {
      sim.skipped += 1;
      continue;
    }
    const executed = l.counts.total - l.counts.skipped;
    const results = gate.rules.map((rule) => {
      const actual =
        rule.metric === 'passRate'
          ? executed > 0
            ? Math.round((l.counts.passed / executed) * 1000) / 10
            : 0
          : rule.metric === 'total'
            ? executed
            : rule.metric === 'toInvestigate' || rule.metric === 'newFailures' || rule.metric === 'newErrors'
              ? l.counts.failed + l.counts.broken
              : rule.metric === 'duration'
                ? Math.round((l.durationMs ?? 0) / 1000)
                : l.counts[rule.metric];
      return { gate: gate.name, rule, actual, expected: rule.value, ok: holds(rule, actual) };
    });
    const status: GateSimulation['items'][number]['status'] = results.every((r) => r.ok) ? 'passed' : 'failed';
    sim.evaluated += 1;
    if (status === 'failed') sim.failed += 1;
    sim.items.push({ launchId: l.id, launchNumber: l.number, branch: branchOf(l), createdAt: l.startedAt, status, results });
  }
  return sim;
}

/** The gate history and the simulation of a draft gate over the mock launches. */
export const gatesHistoryHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/gates/history`, ({ params, request }) => {
    const key = String(params.key);
    if (!projects().some((p) => p.key === key)) return error(404, 'not_found', 'project not found');
    settingsOf(key);
    const url = new URL(request.url);
    const gate = url.searchParams.get('gate');
    const branch = url.searchParams.get('branch');
    const from = url.searchParams.get('from');
    const before = Number(url.searchParams.get('before') ?? 0);
    const limit = Number(url.searchParams.get('limit') ?? 50);
    const evals = evaluations(key).filter((e) => (!branch || branchOf(e.launch) === branch) && (!from || Date.parse(e.at) >= Date.parse(from)) && (!gate || e.report.results.some((r) => r.gate === gate)));
    const page = evals.slice(before, before + limit);
    const items: GateHistoryItem[] = page.map((e) => {
      const failed = e.report.results.filter((r) => !r.ok && (!gate || r.gate === gate)).map((r) => ({ gate: r.gate, metric: r.rule.metric, actual: r.actual ?? null, expected: r.expected }));
      return { launchId: e.launch.id, launchNumber: e.launch.number, branch: branchOf(e.launch), evaluatedAt: e.at, status: gate ? (failed.length ? 'failed' : 'passed') : e.report.status, failed };
    });
    const next = before + limit < evals.length ? String(before + limit) : null;
    const agg = before === 0 ? aggregate(evals, gate) : { gates: [], branches: [] };
    return HttpResponse.json({ items, next, ...agg });
  }),
  http.post(`${API}/projects/:key/gates/simulate`, async ({ params, request }) => {
    const key = String(params.key);
    if (!projects().some((p) => p.key === key)) return error(404, 'not_found', 'project not found');
    const body = (await request.json()) as GateSimulate;
    if (body.gate.rules.length === 0) return error(400, 'bad_request', 'gates.rules: 1 to 20 rules per gate');
    if (body.launches !== undefined && body.launches !== null && (body.launches < 1 || body.launches > 200)) return error(400, 'bad_request', 'launches must be 1..=200');
    return HttpResponse.json(simulate(key, body));
  }),
];
