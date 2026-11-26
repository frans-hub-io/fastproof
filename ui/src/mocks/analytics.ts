import { http, HttpResponse, type HttpHandler } from 'msw';
import type {
  AnalyticsWindow,
  DurationRegressions,
  FailureAnalytics,
  FailureGroup,
  FailureGroupBy,
  FlakyTrend,
  MttrStats,
  Portfolio,
  RetryCosts,
  SlowSteps,
} from '../api/analytics';
import { NOW, projects } from './fixtures';

const API = '/api/v1';
const DAY = 86_400_000;

/** `localStorage['fastproof-mock-clickhouse'] = 'on'` makes the mock answer as a server with ClickHouse. */
export const MOCK_CLICKHOUSE_KEY = 'fastproof-mock-clickhouse';

/** Tests set this to answer as a server with ClickHouse without the browser storage. */
export const mockAnalytics = { clickhouse: false };

export function resetAnalytics(): void {
  mockAnalytics.clickhouse = false;
}

function clickhouse(): boolean {
  if (mockAnalytics.clickhouse) return true;
  try {
    return typeof window !== 'undefined' && window.localStorage.getItem(MOCK_CLICKHOUSE_KEY) === 'on';
  } catch {
    return false;
  }
}

function error(status: number, code: string, message: string) {
  return HttpResponse.json({ code, message }, { status });
}

function window_(url: URL): AnalyticsWindow {
  const to = url.searchParams.get('to') ? Date.parse(url.searchParams.get('to') ?? '') : NOW;
  const askedFrom = url.searchParams.get('from') ? Date.parse(url.searchParams.get('from') ?? '') : to - 30 * DAY;
  const ch = clickhouse();
  const max = ch ? 365 : 30;
  const floor = to - max * DAY;
  const partial = askedFrom < floor;
  const w: AnalyticsWindow = { backend: ch ? 'clickhouse' : 'postgres', from: new Date(Math.max(askedFrom, floor)).toISOString(), to: new Date(to).toISOString(), partial };
  if (partial) w.reason = ch ? 'at most 365 days' : 'PostgreSQL answers the last 30 days; longer history and the portfolio need ClickHouse';
  return w;
}

function days(w: AnalyticsWindow): number {
  return Math.max(1, Math.round((Date.parse(w.to) - Date.parse(w.from)) / DAY));
}

function wave(seed: number, n: number, scale: number): number[] {
  return Array.from({ length: n }, (_, i) => Math.max(0, Math.round(scale * (0.6 + 0.4 * Math.sin((i + seed) / 2.2)) + ((i * 7 + seed * 3) % 5) - 2)));
}

const ERRORS: { key: string; label: string; weight: number }[] = [
  { key: '3f2a9c1d8e7b6a50', label: 'java.net.ConnectException: Connection refused: payments.example.com:443', weight: 64 },
  { key: 'a1b2c3d4e5f60718', label: 'TimeoutError: locator.click: Timeout 30000ms exceeded waiting for [data-test=pay]', weight: 41 },
  { key: '0c9d8e7f6a5b4c3d', label: 'AssertionError: cart total 99.90 != 109.90 (tax rule EU-DE)', weight: 22 },
  { key: '5e4d3c2b1a098776', label: 'expected: <200> but was: <503> in CheckoutApiTest.placeOrder', weight: 13 },
  { key: '9f8e7d6c5b4a3920', label: 'SQLState 40001: could not serialize access due to concurrent update', weight: 6 },
];

const GROUPS: Record<FailureGroupBy, { key: string; label: string; weight: number }[]> = {
  fingerprint: ERRORS,
  defectGroup: [
    { key: 'toInvestigate', label: 'To investigate', weight: 52 },
    { key: 'productBug', label: 'Product bug', weight: 38 },
    { key: 'automationBug', label: 'Automation bug', weight: 33 },
    { key: 'systemIssue', label: 'System issue', weight: 19 },
    { key: 'noDefect', label: 'No defect', weight: 4 },
  ],
  suite: [
    { key: 'checkout / payments', label: 'checkout / payments', weight: 71 },
    { key: 'catalog / search', label: 'catalog / search', weight: 30 },
    { key: 'admin / exports', label: 'admin / exports', weight: 12 },
  ],
  env: [
    { key: 'linux-chrome', label: 'linux-chrome', weight: 80 },
    { key: 'linux-firefox', label: 'linux-firefox', weight: 33 },
    { key: '', label: '(no environment)', weight: 4 },
  ],
};

function failures(w: AnalyticsWindow, by: FailureGroupBy, limit: number, branch: string | null): FailureAnalytics {
  const scale = (days(w) / 30) * (branch ? 0.4 : 1);
  const buckets = Math.min(14, days(w));
  const rows = GROUPS[by].slice(0, limit).map((g, i) => {
    const trend = wave(i + 1, buckets, (g.weight * scale) / buckets);
    const failures = trend.reduce((a, b) => a + b, 0);
    return { ...g, failures, trend };
  });
  const total = rows.reduce((a, r) => a + r.failures, 0) + Math.round(3 * scale);
  const items: FailureGroup[] = rows
    .filter((r) => r.failures > 0)
    .map((r, i) => ({
      key: r.key,
      label: r.label,
      failures: r.failures,
      tests: Math.max(1, Math.round(r.failures / (3 + i))),
      launches: Math.max(1, Math.round(r.failures / 2)),
      share: total ? Math.round((r.failures / total) * 1000) / 1000 : 0,
      trend: r.trend,
    }))
    .sort((a, b) => b.failures - a.failures);
  return { window: w, groupBy: by, total, items };
}

function flaky(w: AnalyticsWindow): FlakyTrend {
  const n = days(w);
  const start = Date.parse(w.from);
  return {
    window: w,
    items: Array.from({ length: n }, (_, i) => {
      const runs = 380 + ((i * 37) % 90);
      return { day: new Date(start + (i + 1) * DAY).toISOString().slice(0, 10), runs, flaky: Math.round(runs * (0.02 + 0.015 * Math.sin(i / 3))) };
    }),
  };
}

function mttr(w: AnalyticsWindow): MttrStats {
  const k = days(w) / 30;
  const counts = [2, 5, 9, 13, 16, 18, 19].map((c) => Math.round(c * k));
  return { window: w, resolved: counts[6] ?? 0, p50Seconds: 140_000, p90Seconds: 1_300_000, buckets: [3600, 21_600, 86_400, 259_200, 604_800, 2_592_000, 7_776_000].map((le, i) => ({ le, count: counts[i] ?? 0 })) };
}

function retries(w: AnalyticsWindow): RetryCosts {
  const k = days(w) / 30;
  const items = [
    { suite: 'checkout / payments', wastedMs: Math.round(5_400_000 * k), retries: Math.round(310 * k), tests: 14 },
    { suite: 'catalog / search', wastedMs: Math.round(1_900_000 * k), retries: Math.round(122 * k), tests: 6 },
    { suite: 'admin / exports', wastedMs: Math.round(420_000 * k), retries: Math.round(18 * k), tests: 2 },
  ];
  return { window: w, totalWastedMs: items.reduce((a, b) => a + b.wastedMs, 0), items };
}

function slowSteps(w: AnalyticsWindow): SlowSteps {
  return {
    window: w,
    items: [
      { name: 'wait for payment confirmation', p50Ms: 4_200, p95Ms: 11_800, count: 912 },
      { name: 'open checkout page', p50Ms: 1_900, p95Ms: 6_300, count: 2_410 },
      { name: 'seed catalog fixtures', p50Ms: 2_700, p95Ms: 4_100, count: 380 },
      { name: 'log in as customer', p50Ms: 640, p95Ms: 2_200, count: 2_890 },
      { name: 'export orders CSV', p50Ms: 820, p95Ms: 1_500, count: 120 },
    ],
  };
}

function durations(to: number): DurationRegressions {
  const w: AnalyticsWindow = { backend: clickhouse() ? 'clickhouse' : 'postgres', from: new Date(to - 35 * DAY).toISOString(), to: new Date(to).toISOString(), partial: !clickhouse() };
  if (!clickhouse()) w.reason = 'PostgreSQL compares the last 7 days with the 23 days before; ClickHouse uses 28';
  return {
    window: w,
    items: [
      { seriesId: '0199bbbb-0000-7000-8000-000000000001', testCaseId: '0199bbbb-0000-7000-8000-0000000000a1', name: 'CheckoutTest.paysWithSavedCard', p50RecentMs: 8_400, p50BeforeMs: 3_100, ratio: 2.71, runs: 64 },
      { seriesId: '0199bbbb-0000-7000-8000-000000000002', testCaseId: '0199bbbb-0000-7000-8000-0000000000a2', name: 'SearchTest.filtersByBrand', p50RecentMs: 2_050, p50BeforeMs: 1_210, ratio: 1.694, runs: 88 },
      { seriesId: '0199bbbb-0000-7000-8000-000000000003', testCaseId: '0199bbbb-0000-7000-8000-0000000000a3', name: 'OrdersApiTest.exportsCsv', p50RecentMs: 960, p50BeforeMs: 700, ratio: 1.371, runs: 31 },
    ],
  };
}

function portfolio(w: AnalyticsWindow): Portfolio {
  const k = days(w) / 30;
  return {
    window: w,
    items: projects().map((p, i) => ({
      key: p.key,
      name: p.name,
      launches: Math.round((120 - i * 25) * k),
      passRate: i === 3 ? null : Math.round((0.97 - i * 0.04) * 1000) / 1000,
      failures: Math.round((140 + i * 60) * k),
      newErrors: 3 + i,
      regressedErrors: i % 2,
      flakyTests: 6 + i * 3,
      mttrP50Seconds: i === 3 ? null : 60_000 + i * 90_000,
    })),
  };
}

function knownProject(key: string): boolean {
  return projects().some((p) => p.key === key);
}

function limitOf(url: URL, def: number): number | null {
  const l = url.searchParams.get('limit');
  if (l === null) return def;
  const n = Number(l);
  return Number.isInteger(n) && n >= 1 && n <= 100 ? n : null;
}

/** Failure analytics, the portfolio and cross-project similar failures over the mock data. */
export const analyticsHandlers: HttpHandler[] = [
  http.get(`${API}/projects/:key/analytics/:what`, ({ params, request }) => {
    const key = String(params.key);
    if (!knownProject(key)) return error(404, 'not_found', 'project not found');
    const url = new URL(request.url);
    const limit = limitOf(url, 20);
    if (limit === null) return error(400, 'bad_request', 'limit must be 1..=100');
    const w = window_(url);
    switch (String(params.what)) {
      case 'failures': {
        const by = (url.searchParams.get('groupBy') ?? 'fingerprint') as FailureGroupBy;
        if (!(by in GROUPS)) return error(400, 'bad_request', 'invalid query: unknown groupBy');
        return HttpResponse.json(failures(w, by, limit, url.searchParams.get('branch')));
      }
      case 'durations':
        return HttpResponse.json(durations(Date.parse(w.to)));
      case 'retries':
        return HttpResponse.json(retries(w));
      case 'flaky-trend':
        return HttpResponse.json(flaky(w));
      case 'mttr':
        return HttpResponse.json(mttr(w));
      case 'slow-steps':
        return HttpResponse.json(slowSteps(w));
      default:
        return error(404, 'not_found', 'resource not found');
    }
  }),
  http.get(`${API}/analytics/portfolio`, ({ request }) => {
    if (!clickhouse()) return error(409, 'clickhouse_required', 'this view needs ClickHouse');
    return HttpResponse.json(portfolio(window_(new URL(request.url))));
  }),
  http.get(`${API}/analytics/similar`, () => {
    if (!clickhouse()) return error(409, 'clickhouse_required', 'this view needs ClickHouse');
    return HttpResponse.json({ items: [] });
  }),
];
