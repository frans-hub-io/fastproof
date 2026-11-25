import { http, HttpResponse, type HttpHandler } from 'msw';
import type { ErrorDetail, ErrorPatch, ErrorSort, ErrorState, ErrorSummary, ErrorTimelinePoint } from '../api/errors';
import { NOW, projects } from './fixtures';

const API = '/api/v1';
const DAY = 86_400_000;

interface MockError {
  project: string;
  summary: ErrorSummary;
  aliasOf: string | null;
  resolvedAt: string | null;
  commits: ErrorDetail['commits'];
}

const SEED: { message: string; state: ErrorState; branches: string[]; firstDays: number; lastHours: number; occurrences: number; tests: number; launches: number; regressed?: number; issue?: ErrorSummary['issue']; group?: string }[] = [
  { message: 'java.net.ConnectException: Connection refused: payments.example.com:443', state: 'regressed', branches: ['main', 'release/2.4'], firstDays: 19, lastHours: 1, occurrences: 64, tests: 9, launches: 21, regressed: 2, issue: { url: 'https://tracker.example.com/browse/PAY-1234', key: 'PAY-1234', state: 'closed' }, group: 'productBug' },
  { message: "expected: <200> but was: <503> in CheckoutApiTest.placeOrder", state: 'new', branches: ['feature/checkout-v2'], firstDays: 0, lastHours: 2, occurrences: 3, tests: 3, launches: 1 },
  { message: 'TimeoutError: locator.click: Timeout 30000ms exceeded waiting for [data-test=pay]', state: 'active', branches: ['main', 'feature/checkout-v2', '*'], firstDays: 12, lastHours: 3, occurrences: 41, tests: 4, launches: 17, group: 'automationBug' },
  { message: 'AssertionError: cart total 99.90 != 109.90 (tax rule EU-DE)', state: 'active', branches: ['main'], firstDays: 6, lastHours: 20, occurrences: 12, tests: 2, launches: 6, issue: { url: 'https://tracker.example.com/browse/SHOP-88', key: 'SHOP-88', state: 'open' }, group: 'productBug' },
  { message: 'getaddrinfo ENOTFOUND inventory.internal.example.com', state: 'ignored', branches: ['main', '*'], firstDays: 25, lastHours: 30, occurrences: 120, tests: 30, launches: 25, group: 'systemIssue' },
  { message: 'NullPointerException at OrderMapper.toDto(OrderMapper.java:88)', state: 'resolved', branches: ['main'], firstDays: 28, lastHours: 200, occurrences: 18, tests: 1, launches: 9 },
  { message: 'Stub not found: POST /v1/fraud/score (no matching expectation)', state: 'resolved', branches: ['release/2.4'], firstDays: 14, lastHours: 150, occurrences: 7, tests: 2, launches: 3, group: 'automationBug' },
  { message: 'SQLState 40001: could not serialize access due to concurrent update', state: 'active', branches: ['main'], firstDays: 3, lastHours: 6, occurrences: 5, tests: 1, launches: 4 },
];

const errors: MockError[] = [];

function fingerprint(project: string, i: number): string {
  let h = 0x811c9dc5;
  for (const c of `${project}:${String(i)}`) h = Math.imul(h ^ c.charCodeAt(0), 0x01000193) >>> 0;
  return (h.toString(16).padStart(8, '0') + i.toString(16).padStart(8, '0')).slice(0, 16);
}

function spark(seed: number, occurrences: number, lastHours: number): number[] {
  return Array.from({ length: 14 }, (_, d) => {
    const daysAgo = 13 - d;
    if (daysAgo * 24 < lastHours - 24) return 0;
    return (seed * 7 + d * 3) % 5 === 0 ? 0 : Math.max(0, Math.round(((seed + d) % 4) * (occurrences / 40)));
  });
}

function build(): void {
  if (errors.length) return;
  for (const p of projects()) {
    SEED.forEach((s, i) => {
      const fp = fingerprint(p.key, i);
      const first = new Date(NOW - s.firstDays * DAY - 3_600_000).toISOString();
      const last = new Date(NOW - s.lastHours * 3_600_000).toISOString();
      errors.push({
        project: p.key,
        aliasOf: null,
        resolvedAt: s.state === 'resolved' ? new Date(NOW - (s.lastHours - 30) * 3_600_000).toISOString() : null,
        commits: s.state === 'new' || s.state === 'regressed' ? { lastGood: '3f2a9c1d8e7b6a5f', first: 'a1b2c3d4e5f60718', compareUrl: 'https://github.com/example/web-shop/compare/3f2a9c1d8e7b6a5f...a1b2c3d4e5f60718' } : null,
        summary: {
          fingerprint: fp,
          message: s.message,
          state: s.state,
          stateAt: s.state === 'new' ? first : last,
          firstSeenAt: first,
          lastSeenAt: last,
          occurrences: s.occurrences,
          tests: s.tests,
          launches: s.launches,
          branches: s.branches,
          regressedCount: s.regressed ?? 0,
          aliasOf: null,
          aliases: 0,
          issue: s.issue ?? null,
          group: s.group ?? null,
          spark: spark(i, s.occurrences, s.lastHours),
        },
      });
    });
  }
}

export function resetErrors(): void {
  errors.length = 0;
}

function error(status: number, code: string, message: string) {
  return HttpResponse.json({ code, message }, { status });
}

function find(project: string, fp: string): MockError | undefined {
  build();
  return errors.find((e) => e.project === project && e.summary.fingerprint === fp);
}

function summaryOf(e: MockError): ErrorSummary {
  const aliases = errors.filter((a) => a.project === e.project && a.aliasOf === e.summary.fingerprint);
  return {
    ...e.summary,
    aliasOf: e.aliasOf,
    aliases: aliases.length,
    occurrences: e.summary.occurrences + aliases.reduce((n, a) => n + a.summary.occurrences, 0),
    stateAt: e.summary.state === 'resolved' ? e.resolvedAt : e.summary.stateAt,
  };
}

function timeline(e: MockError): ErrorTimelinePoint[] {
  const points: ErrorTimelinePoint[] = [];
  const s = e.summary.spark;
  for (let d = 30; d >= 0; d--) {
    const day = new Date(NOW - d * DAY).toISOString().slice(0, 10);
    const v = d < 14 ? (s[13 - d] ?? 0) : new Date(e.summary.firstSeenAt).getTime() <= NOW - d * DAY ? (d * 3) % 4 : 0;
    points.push({ day, occurrences: v, launches: v ? Math.max(1, Math.round(v / 2)) : 0 });
  }
  return points;
}

function detailOf(e: MockError): ErrorDetail {
  const s = summaryOf(e);
  const to = new Date(NOW).toISOString();
  const from = new Date(NOW - 30 * DAY).toISOString();
  return {
    ...s,
    aliasList: errors.filter((a) => a.project === e.project && a.aliasOf === s.fingerprint).map((a) => a.summary.fingerprint),
    branchList: s.branches.map((b, i) => ({
      branch: b,
      firstSeenAt: s.firstSeenAt,
      lastSeenAt: i === 0 ? s.lastSeenAt : new Date(new Date(s.lastSeenAt).getTime() - (i + 1) * DAY).toISOString(),
      lastLaunchId: null,
      occurrences: Math.max(1, Math.round(s.launches / (i + 1))),
      launchesSince: s.state === 'resolved' ? 10 + i : i,
    })),
    commits: e.commits,
    mttrSeconds: s.state === 'resolved' && e.resolvedAt ? Math.round((new Date(e.resolvedAt).getTime() - new Date(s.firstSeenAt).getTime()) / 1000) : null,
    decisions: s.group
      ? [
          {
            fingerprint: s.fingerprint,
            defectTypeId: '0199aaaa-0000-7000-8000-00000000d001',
            group: s.group,
            defectName: s.group === 'productBug' ? 'Product bug' : s.group === 'automationBug' ? 'Automation bug' : 'System issue',
            messageNorm: s.message,
            frames: [],
            confirmed: 4,
            overridden: 0,
            comment: null,
            issueUrl: s.issue?.url ?? null,
            issueKey: s.issue?.key ?? null,
            testCases: s.tests,
            branches: s.branches,
            environments: ['staging'],
            firstDecidedAt: s.firstSeenAt,
            lastDecidedAt: s.lastSeenAt,
            lastDecidedBy: 'alice@example.com',
            sample: null,
          },
        ]
      : [],
    testList: Array.from({ length: Math.min(5, s.tests) }, (_, i) => ({
      testCaseId: `0199aaaa-0000-7000-8000-${(0xe00 + i).toString(16).padStart(12, '0')}`,
      name: ['placeOrder', 'payWithCard', 'applyCoupon', 'refundOrder', 'cartTotals'][i] ?? `test ${String(i)}`,
      occurrences: Math.max(1, Math.round(s.occurrences / (i + 2))),
    })),
    timeline: timeline(e),
    window: { backend: 'postgres', from, to, partial: false },
  };
}

const SORT_KEY: Record<ErrorSort, (e: ErrorSummary) => number> = {
  lastSeen: (e) => new Date(e.lastSeenAt).getTime(),
  firstSeen: (e) => new Date(e.firstSeenAt).getTime(),
  occurrences: (e) => e.occurrences,
  launches: (e) => e.launches,
};

/** Mock routes of unique errors. */
export const errorsHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/errors`, ({ params, request }) => {
    build();
    const key = String(params.key);
    if (!projects().some((p) => p.key === key)) return error(404, 'not_found', 'project not found');
    const url = new URL(request.url);
    const state = url.searchParams.get('state');
    const branch = url.searchParams.get('branch');
    const q = url.searchParams.get('q')?.trim().toLowerCase();
    const sort = (url.searchParams.get('sort') ?? 'lastSeen') as ErrorSort;
    if (q !== undefined && q.length < 3) return error(400, 'bad_request', 'q needs at least 3 characters');
    if (!(sort in SORT_KEY)) return error(400, 'bad_request', 'unknown sort');
    const limit = Math.min(500, Math.max(1, Number(url.searchParams.get('limit') ?? 50)));
    const before = url.searchParams.get('before');
    let items = errors
      .filter((e) => e.project === key && !e.aliasOf)
      .map(summaryOf)
      .filter((e) => !state || e.state === state)
      .filter((e) => !branch || e.branches.includes(branch))
      .filter((e) => !q || e.message.toLowerCase().includes(q));
    items.sort((a, b) => SORT_KEY[sort](b) - SORT_KEY[sort](a) || (a.fingerprint < b.fingerprint ? 1 : -1));
    if (before) {
      const i = items.findIndex((e) => e.fingerprint === before);
      items = i >= 0 ? items.slice(i + 1) : [];
    }
    const page = items.slice(0, limit);
    return HttpResponse.json({ items: page, next: items.length > limit ? (page.at(-1)?.fingerprint ?? null) : null });
  }),
  http.get(`${API}/projects/:key/errors/:fp`, ({ params }) => {
    const e = find(String(params.key), String(params.fp));
    if (!e) return error(404, 'not_found', 'error not found');
    return HttpResponse.json(detailOf(e));
  }),
  http.get(`${API}/projects/:key/errors/:fp/timeline`, ({ params }) => {
    const e = find(String(params.key), String(params.fp));
    if (!e) return error(404, 'not_found', 'error not found');
    const d = detailOf(e);
    return HttpResponse.json({ window: d.window, items: d.timeline });
  }),
  http.patch(`${API}/projects/:key/errors/:fp`, async ({ params, request }) => {
    const key = String(params.key);
    const e = find(key, String(params.fp));
    if (!e) return error(404, 'not_found', 'error not found');
    const body = (await request.json()) as ErrorPatch;
    if (body.state === undefined && body.aliasOf === undefined) return error(400, 'bad_request', 'give state or aliasOf');
    if (typeof body.aliasOf === 'string') {
      const t = find(key, body.aliasOf);
      const canon = t ? (t.aliasOf ?? t.summary.fingerprint) : null;
      if (!t || canon === e.summary.fingerprint) return error(409, 'alias_cycle', 'aliasOf would make a cycle');
      for (const a of errors.filter((x) => x.project === key && x.aliasOf === e.summary.fingerprint)) a.aliasOf = canon;
      e.aliasOf = canon;
    } else if (body.aliasOf === null) {
      e.aliasOf = null;
    }
    if (body.state) {
      if (e.aliasOf) return error(400, 'bad_request', 'an alias has no state of its own');
      e.summary.state = body.state === 'ignored' ? 'ignored' : e.summary.regressedCount > 0 ? 'regressed' : 'active';
      e.summary.stateAt = new Date().toISOString();
    }
    return HttpResponse.json(detailOf(e));
  }),
];
