import { http, HttpResponse, type HttpHandler } from 'msw';
import type { FlakyHint, FlakyHintBrief, FlakyHintKind, FlakyHints, HintConfidence, HintExample } from '../api/flakyHints';
import { NOW } from './fixtures';

const API = '/api/v1';

/** One run of a mock flaky series, newest first. */
export interface MockRun {
  launchId: string;
  resultId: string;
  at: string;
  status: string;
  flakyInRun: boolean;
}

interface Entry {
  project: string;
  name: string;
  hints: FlakyHint[];
  runs: number;
  failures: number;
  infraFailures: number;
}

/** Series the flaky list has shown, by id (the panel opens from the list). */
const registry = new Map<string, Entry>();

function hash(s: string): number {
  let h = 0x811c9dc5;
  for (const c of s) h = Math.imul(h ^ c.charCodeAt(0), 0x01000193) >>> 0;
  return h;
}

/** A stable series id of a mock series key. */
export function mockSeriesId(seriesKey: string): string {
  const a = hash(`a:${seriesKey}`).toString(16).padStart(8, '0');
  const b = hash(`b:${seriesKey}`).toString(16).padStart(8, '0');
  const c = hash(`c:${seriesKey}`).toString(16).padStart(8, '0');
  const d = hash(`d:${seriesKey}`).toString(16).padStart(8, '0');
  return `${a}-${b.slice(0, 4)}-7${b.slice(5, 8)}-8${c.slice(1, 4)}-${c.slice(4)}${d}`;
}

const KINDS: FlakyHintKind[][] = [['timing'], ['infra', 'retry'], ['environment'], ['order'], ['retry'], [], ['timing', 'retry'], []];

function examples(runs: MockRun[]): HintExample[] {
  return runs
    .filter((r) => r.status !== 'passed' || r.flakyInRun)
    .slice(0, 5)
    .map((r) => ({ launchId: r.launchId, resultId: r.resultId, at: r.at, status: r.status === 'passed' ? 'passedOnRetry' : 'failed' }));
}

function hintOf(kind: FlakyHintKind, confidence: HintConfidence, name: string, runs: MockRun[], failures: number, seed: number): FlakyHint {
  const ex = examples(runs);
  const n = runs.length;
  switch (kind) {
    case 'timing':
      return {
        kind,
        confidence,
        summary: `Runs into a time limit: ${String(Math.max(5, failures - 1))} of ${String(Math.max(5, failures))} failures are timeouts; passing runs reach 86% of it (p95 25.8 s)`,
        evidence: { failures: Math.max(5, failures), timeoutFailures: Math.max(5, failures - 1), ceilingMs: 30_040, atCeiling: Math.max(5, failures - 1), passP95Ms: 25_810, passP95OfCeiling: 0.859, examples: ex },
        sample: Math.max(5, failures),
        actions: ['mute', 'issue', 'rule'],
      };
    case 'environment': {
      const host = `ci-runner-${String((seed % 3) + 2)}`;
      return {
        kind,
        confidence,
        summary: `Fails on host ${host}: 6 of 9 runs there against 1 of ${String(Math.max(10, n - 9))} elsewhere (p = 0.000412)`,
        evidence: { dimension: 'host', value: host, runs: 9, failures: 6, failRate: 0.667, otherRuns: Math.max(10, n - 9), otherFailures: 1, otherFailRate: 0.031, ratio: 21.3, p: 0.000412, examples: ex },
        sample: 9,
        actions: ['mute', 'issue'],
      };
    }
    case 'order':
      return {
        kind,
        confidence,
        summary: `Fails together with ${name.replace(/\w+$/, 'cleanup')}: 5 shared failing launches, Jaccard 0.83`,
        evidence: { via: 'coFailure', name: name.replace(/\w+$/, 'cleanup'), jaccard: 0.833, shared: 5, failures: 5, otherFailures: 6, examples: ex },
        sample: n,
        actions: ['mute', 'issue'],
      };
    case 'retry':
      return {
        kind,
        confidence,
        summary: `Passes on a retry of the same run in 4 of ${String(Math.max(4, failures))} failing runs with 3 different errors`,
        evidence: { retriedRuns: 4, failures: Math.max(4, failures), share: Math.round((4 / Math.max(4, failures)) * 1000) / 1000, distinctErrors: 3, fingerprints: ['5c1d0e2a77f04b19', 'a90e33c1d2b84f6e', '0f4b7c9e1a2d3e58'], examples: ex },
        sample: 4,
        actions: ['mute', 'issue'],
      };
    case 'infra': {
      const all = Math.max(3, failures);
      return {
        kind,
        confidence,
        summary: `3 of ${String(all)} failures coincide with infrastructure trouble (signals grid session_crashed ×2, k8s pod_evicted ×1)`,
        evidence: { matches: 3, failures: all, share: Math.round((3 / all) * 1000) / 1000, bursts: 0, infraErrors: 0, signals: { 'grid:session_crashed': 2, 'k8s:pod_evicted': 1 }, examples: ex },
        sample: 3,
        actions: ['issue', 'rule'],
      };
    }
    case 'time':
      return {
        kind,
        confidence,
        summary: 'Fails more often on hours (UTC) 00–06: 8 of 14 runs against 2 of 60 at other times (p = 0.0000213)',
        evidence: { dimension: 'hour', value: '00–06', runs: 14, failures: 8, failRate: 0.571, otherRuns: 60, otherFailures: 2, otherFailRate: 0.033, ratio: 17.1, p: 0.0000213, examples: ex },
        sample: 74,
        actions: ['mute', 'issue'],
      };
  }
}

/**
 * The likely causes the mock server gives a flaky series (deterministic by its key), registered
 * so that `GET .../flaky/{seriesId}/hints` can answer them.
 */
export function flakyHintsFor(project: string, seriesKey: string, name: string, runs: MockRun[]): { seriesId: string; hints: FlakyHintBrief[]; infraShare: number | null } {
  const seriesId = mockSeriesId(seriesKey);
  const seed = hash(seriesKey);
  const kinds = KINDS[seed % KINDS.length] ?? [];
  const failures = runs.filter((r) => r.status === 'failed' || r.status === 'broken' || r.flakyInRun).length;
  const confidences: HintConfidence[] = ['high', 'medium', 'low'];
  const hints = kinds.map((k, i) => hintOf(k, confidences[(seed + i) % 2 === 0 ? i : i + 1] ?? 'low', name, runs, failures, seed));
  const infraFailures = kinds.includes('infra') ? Math.min(failures, 3) : 0;
  registry.set(seriesId, { project, name, hints, runs: runs.length, failures, infraFailures });
  return {
    seriesId,
    hints: hints.map((h) => ({ kind: h.kind, confidence: h.confidence })),
    infraShare: failures > 0 && hints.length > 0 ? Math.round((infraFailures / failures) * 1000) / 1000 : null,
  };
}

export const flakyHintsHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/flaky/:seriesId/hints`, ({ params }) => {
    const e = registry.get(String(params.seriesId));
    if (!e || e.project !== String(params.key)) {
      return HttpResponse.json({ code: 'not_found', message: 'series not found' }, { status: 404 });
    }
    const body: FlakyHints = {
      seriesId: String(params.seriesId),
      computedAt: new Date(NOW - 3 * 3_600_000).toISOString(),
      windowRuns: e.runs,
      failures: e.failures,
      infraFailures: e.infraFailures,
      infraShare: e.failures > 0 && e.hints.length > 0 ? Math.round((e.infraFailures / e.failures) * 1000) / 1000 : null,
      backend: 'postgres',
      hints: e.hints,
    };
    return HttpResponse.json(body);
  }),
];
