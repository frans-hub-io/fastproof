// Signals about test attempts: pushed by grids, CI and Kubernetes, or written by the analysis
// from the FastMock journal. The DTOs are the generated `schema.gen.ts` types.
import { useQuery } from '@tanstack/react-query';
import { projectPath, request } from './client';
import type { components } from './schema.gen';

type S = components['schemas'];

export type AttemptSignal = S['AttemptSignal'];
export type AttemptSignals = S['AttemptSignals'];
export type SignalSource = S['SignalSource'];
export type SignalConfidence = S['SignalConfidence'];
export type SignalPush = S['SignalPush'];
export type SignalsAccepted = S['SignalsAccepted'];

export const signalKeys = {
  result: (key: string, id: string) => ['analysis', key, 'signals', id] as const,
};

/** The signals of a result's attempts, by attempt and time. */
export function useResultSignals(key: string, resultId: string, enabled = true) {
  return useQuery({
    queryKey: signalKeys.result(key, resultId),
    queryFn: () => request<AttemptSignals>(`${projectPath(key)}/results/${encodeURIComponent(resultId)}/signals`),
    enabled: enabled && !!resultId,
  });
}

const SOURCES: Record<SignalSource, string> = {
  grid: 'Grid',
  ci: 'CI',
  k8s: 'Kubernetes',
  mock: 'FastMock',
};

const SOURCE_COLORS: Record<SignalSource, string> = {
  grid: 'orange',
  ci: 'blue',
  k8s: 'cyan',
  mock: 'grape',
};

const KINDS: Record<string, string> = {
  session_crashed: 'Browser session crashed',
  browser_oom: 'Browser ran out of memory',
  startup_timeout: 'Browser did not start in time',
  node_lost: 'Grid node lost',
  evicted: 'Session evicted',
  runner_oom: 'Runner ran out of memory',
  pod_evicted: 'Pod evicted',
  job_cancelled: 'Job cancelled',
  oom_killed: 'Container killed for memory',
  node_not_ready: 'Node not ready',
  unmatched: 'Request matched no stub (near miss)',
  stub_error: 'Stub answered with an error',
  slow: 'Slow stub',
  unexpected_calls: 'Call to a route no stub serves',
  injected_fault: 'Fault injected by the mock',
};

export function signalSourceLabel(s: SignalSource): string {
  return SOURCES[s];
}

export function signalSourceColor(s: SignalSource): string {
  return SOURCE_COLORS[s];
}

/** A readable name of a signal kind; unknown kinds are shown as sent. */
export function signalKindLabel(kind: string): string {
  return KINDS[kind] ?? kind;
}

export function confidenceLabel(c: SignalConfidence): string {
  return c === 'exact' ? 'this attempt' : 'same time window';
}

/** `detail` as `key: value` lines, keys sorted; values are text, never markup. */
export function detailLines(d: AttemptSignal['detail']): string[] {
  const v = d as Record<string, unknown>;
  return Object.keys(v)
    .sort()
    .map((k) => {
      const x = v[k];
      return `${k}: ${typeof x === 'string' ? x : JSON.stringify(x)}`;
    });
}

/** Signals grouped by attempt number (unknown attempt last), each group in time order. */
export function byAttempt(items: AttemptSignal[]): { attempt: number | null; items: AttemptSignal[] }[] {
  const groups = new Map<number | null, AttemptSignal[]>();
  for (const s of items) {
    const a = s.attempt ?? null;
    const g = groups.get(a);
    if (g) g.push(s);
    else groups.set(a, [s]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => (a ?? Number.MAX_SAFE_INTEGER) - (b ?? Number.MAX_SAFE_INTEGER))
    .map(([attempt, xs]) => ({ attempt, items: [...xs].sort((x, y) => Date.parse(x.at) - Date.parse(y.at) || x.id - y.id) }));
}
