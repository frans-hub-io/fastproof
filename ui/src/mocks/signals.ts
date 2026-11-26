import { http, HttpResponse, type HttpHandler } from 'msw';
import type { AttemptSignal } from '../api/signals';
import { NOW, resultDetail, testKind } from './fixtures';

const API = '/api/v1';
const HOUR = 3_600_000;

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

function failing(status: string): boolean {
  return status === 'failed' || status === 'broken';
}

/** Signals of a mock result: its failed attempts get the FastMock signals that match the mock
 * journal of `stageC.mockInteractions`, and browser tests a grid signal on their last attempt. */
export function signalsOf(resultId: string): AttemptSignal[] | null {
  const r = resultDetail(resultId);
  if (!r) return null;
  const ui = testKind(resultId) === 'ui';
  const out: AttemptSignal[] = [];
  let id = 1;
  const add = (s: Omit<AttemptSignal, 'id' | 'resultId'>) => {
    out.push({ ...s, id: id++, resultId });
  };
  const last = r.attempts[r.attempts.length - 1];
  for (const a of r.attempts) {
    if (!failing(a.status)) continue;
    const start = a.startedAt ? Date.parse(a.startedAt) : NOW - HOUR;
    const detail = (d: Record<string, string | number | boolean>) => d as AttemptSignal['detail'];
    if (ui) {
      add({ attempt: a.no, source: 'mock', kind: 'stub_error', at: iso(start + 1480), confidence: 'exact', detail: detail({ count: 1, method: 'POST', path: '/payments', status: 502, durationMs: 1210, stub: 'payments-gateway-down', correlation: 'traceId' }) });
      add({ attempt: a.no, source: 'mock', kind: 'unmatched', at: iso(start + 1720), confidence: 'exact', detail: detail({ count: 1, method: 'GET', path: '/orders/SO-2026-118842', status: 404, durationMs: 3, correlation: 'traceId' }) });
      if (a === last) {
        add({ attempt: a.no, source: 'grid', kind: 'browser_oom', at: iso(start + 1900), confidence: 'exact', detail: detail({ node: 'grid-node-3', browser: 'chromium', memoryMb: 2048, sessionId: 'b7ad6b7169203331' }) });
      }
    } else {
      add({ attempt: a.no, source: 'mock', kind: 'unmatched', at: iso(start + 900), confidence: 'window', detail: detail({ count: 1, method: 'GET', path: '/cart', status: 404, durationMs: 2, correlation: 'timeWindow' }) });
    }
  }
  return out;
}

/** Mock routes of this FastProof Analysis area. */
export const signalsHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/results/:id/signals`, ({ params }) => {
    const items = signalsOf(String(params.id));
    if (!items) return HttpResponse.json({ code: 'not_found', message: 'result not found' }, { status: 404 });
    return HttpResponse.json({ items });
  }),
];
