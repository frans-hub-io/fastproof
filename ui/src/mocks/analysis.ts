import { http, HttpResponse, type HttpHandler } from 'msw';
import type { AnalysisCapabilities, AnalysisLaunchStatus, DecisionVia, StepOutcome, TriageDecision, TriageState, TriageUndoResult } from '../api/analysis';
import type { ResultSummary } from '../api/types';
import type { DefectInfo } from '../api/types-next';
import { launchDef, projects } from './fixtures';
import { restoreTriage } from './stageC';

const API = '/api/v1';

type Source = TriageDecision['source'];
interface Row extends TriageDecision {
  project: string;
}

const journal: Row[] = [];
let seq = 0;
let batches = 0;
/** What the mock capabilities say; tests flip features on. */
export const mockCapabilities: { instanceEnabled: boolean; features: Partial<AnalysisCapabilities['features']> } = { instanceEnabled: false, features: {} };

export function resetAnalysis(): void {
  journal.length = 0;
  seq = 0;
  batches = 0;
  mockCapabilities.instanceEnabled = false;
  mockCapabilities.features = {};
}

function stateOf(r: ResultSummary): TriageState {
  return { defect: r.defect ?? null, muted: r.muted, muteUntil: r.muteUntil ?? null };
}

function same(a: TriageState, b: TriageState): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function batchId(): string {
  batches += 1;
  return `0199aaaa-0000-7000-8000-${batches.toString(16).padStart(12, '0')}`;
}

/** Journals the results whose state changed as one batch; `null` when none did. */
export function journalTriage(project: string, before: ResultSummary[], after: ResultSummary[], by: string, via: DecisionVia, source: Source, reverts: string | null = null): string | null {
  const id = batchId();
  let n = 0;
  for (const b of before) {
    const a = after.find((x) => x.id === b.id);
    if (!a || same(stateOf(b), stateOf(a))) continue;
    seq += 1;
    n += 1;
    journal.push({ project, id: seq, batchId: id, resultId: b.id, launchId: b.launchId, fingerprint: b.fingerprint ?? null, via, source, by, at: new Date().toISOString(), before: stateOf(b), after: stateOf(a), suggestionId: null, revertsBatch: reverts, undoable: true });
  }
  return n ? id : null;
}

function error(status: number, code: string, message: string) {
  return HttpResponse.json({ code, message }, { status });
}

function undoable(project: string, batch: string, rows: Row[]): boolean {
  if (rows.some((r) => r.via === 'rule')) return false;
  return rows.some((r) => {
    const latest = journal.filter((x) => x.project === project && x.resultId === r.resultId).at(-1);
    return latest?.batchId === batch;
  });
}

function view(project: string, r: Row): TriageDecision {
  const d: TriageDecision = { id: r.id, batchId: r.batchId, resultId: r.resultId, launchId: r.launchId, fingerprint: r.fingerprint, via: r.via, source: r.source, by: r.by, at: r.at, before: r.before, after: r.after, suggestionId: r.suggestionId, revertsBatch: r.revertsBatch, undoable: false };
  return { ...d, undoable: undoable(project, r.batchId, journal.filter((x) => x.project === project && x.batchId === r.batchId)) };
}

/** `localStorage['fastproof-mock-analysis'] = 'on'` turns every analysis feature on in the browser mock. */
export const MOCK_ANALYSIS_KEY = 'fastproof-mock-analysis';

function storageOn(): boolean {
  try {
    return typeof window !== 'undefined' && window.localStorage.getItem(MOCK_ANALYSIS_KEY) === 'on';
  } catch {
    return false;
  }
}

function capabilities(key: string): AnalysisCapabilities {
  const all = storageOn();
  const on = mockCapabilities.instanceEnabled || all;
  const f: Partial<AnalysisCapabilities['features']> = all ? { suggestions: true, errors: true, logPatterns: true, gateAnalytics: true, flakyHints: true, signals: true, analytics: true } : mockCapabilities.features;
  const feature = (k: keyof AnalysisCapabilities['features']) => on && !!f[k];
  return {
    enabled: on,
    instanceEnabled: on,
    projectEnabled: true,
    clickhouse: false,
    trigram: true,
    gateOpen: true,
    features: {
      suggestions: feature('suggestions'),
      errors: feature('errors'),
      logPatterns: feature('logPatterns'),
      gateAnalytics: feature('gateAnalytics'),
      flakyHints: feature('flakyHints'),
      signals: feature('signals'),
      analytics: feature('analytics'),
      portfolio: false,
      crossProjectSimilar: false,
    },
    windows: { rawDays: 90, memoryDays: 365, analyticsDays: key.length ? 30 : 30 },
    withClickHouse: ['portfolio', 'crossProjectSimilar'],
  };
}

/** Capabilities, launch status, the triage journal and undo over the mock triage. */
export const analysisHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/analysis/capabilities`, ({ params }) => {
    const key = String(params.key);
    if (!projects().some((p) => p.key === key)) return error(404, 'not_found', 'project not found');
    return HttpResponse.json(capabilities(key));
  }),
  http.get(`${API}/projects/:key/launches/:id/analysis`, ({ params }) => {
    const def = launchDef(String(params.id));
    if (!def || def.project !== params.key) return error(404, 'not_found', 'launch not found');
    const done = (mockCapabilities.instanceEnabled || storageOn()) && def.launch.status !== 'running';
    const skipped = { status: 'skipped' as const, millis: 0, detail: { reason: 'not available' } as unknown as StepOutcome['detail'] };
    const status: AnalysisLaunchStatus = done
      ? { state: 'done', queuedAt: def.launch.finishedAt ?? null, finishedAt: def.launch.finishedAt ?? null, steps: { errors: skipped, signals: skipped, logRules: skipped, suggestions: skipped, flakyHints: skipped } }
      : { state: 'none', queuedAt: null, finishedAt: null, steps: {} };
    return HttpResponse.json(status);
  }),
  http.get(`${API}/projects/:key/triage/history`, ({ params, request }) => {
    const key = String(params.key);
    const url = new URL(request.url);
    const resultId = url.searchParams.get('resultId');
    const batch = url.searchParams.get('batchId');
    if (!resultId === !batch) return error(400, 'bad_request', 'give resultId or batchId');
    const rows = journal.filter((r) => r.project === key && (resultId ? r.resultId === resultId : r.batchId === batch)).reverse();
    return HttpResponse.json({ items: rows.map((r) => view(key, r)), next: null });
  }),
  http.post(`${API}/projects/:key/triage/undo`, async ({ params, request }) => {
    const key = String(params.key);
    const body = (await request.json()) as { batchId?: string };
    const rows = journal.filter((r) => r.project === key && r.batchId === body.batchId);
    if (!rows.length || !body.batchId) return error(404, 'not_found', 'batch not found');
    if (rows.some((r) => r.via === 'rule')) return error(409, 'batch_not_undoable', 'decisions of a rule at ingest cannot be undone; edit the rule');
    const id = batchId();
    const skipped: TriageUndoResult['skipped'] = [];
    let restored = 0;
    for (const r of rows) {
      const latest = journal.filter((x) => x.project === key && x.resultId === r.resultId).at(-1);
      if (latest?.batchId !== r.batchId) {
        skipped.push({ resultId: r.resultId, code: 'changed_since' });
        continue;
      }
      restoreTriage(r.resultId, { defect: (r.before.defect ?? null) as DefectInfo | null, muted: r.before.muted, muteUntil: r.before.muteUntil ?? null });
      seq += 1;
      restored += 1;
      journal.push({ ...r, id: seq, batchId: id, via: 'undo', at: new Date().toISOString(), before: r.after, after: r.before, revertsBatch: r.batchId });
    }
    const out: TriageUndoResult = { batchId: restored ? id : null, reverts: body.batchId, restored, skipped };
    return HttpResponse.json(out);
  }),
];
