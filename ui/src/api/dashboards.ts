import { useDebouncedValue } from '@mantine/hooks';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { projectPath, request } from './client';
import { getVersioned, writeVersioned, type Versioned } from './tm';
import type { Dashboard as TrendData, DefectGroup } from './types-next';

const enc = encodeURIComponent;

export const WIDGET_TYPES = [
  'launch-statistics',
  'overall-statistics',
  'investigated-percentage',
  'launches-comparison',
  'launches-duration',
  'launches-table',
  'failed-trend',
  'non-passed-trend',
  'test-cases-growth',
  'passing-rate-per-launch',
  'passing-rate-summary',
  'flaky-tests',
  'most-failing-tests',
  'popular-patterns',
  'launch-execution',
  'component-health',
  'pass-rate-trend',
  'durations',
  'defect-trend',
  'launches-per-day',
  'cumulative-trend',
  'unique-bugs',
  'activity-stream',
] as const;
export type WidgetType = (typeof WIDGET_TYPES)[number];

export type WidgetView = 'chart' | 'table';
export type WidgetChart = 'bar' | 'line' | 'area';

export interface WidgetSettings {
  days?: number;
  bucket?: 'day' | 'week';
  branch?: string;
  environment?: string;
  limit?: number;
  view?: WidgetView;
  chart?: WidgetChart;
  groupBy?: string;
  launchId?: string;
  attribute?: string;
  /** What the popular-patterns widget counts: error fingerprints (default) or log patterns of rules. */
  patterns?: 'fingerprints' | 'logPatterns';
}

export interface DashboardWidget {
  id: string;
  type: WidgetType;
  title: string;
  settings: WidgetSettings;
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface DashboardSummary {
  id: string;
  name: string;
  description: string;
  owner: string;
  owned: boolean;
  locked: boolean;
  revision: number;
  widgetCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface DashboardDoc extends Omit<DashboardSummary, 'widgetCount'> {
  widgets: DashboardWidget[];
}

export interface DashboardWrite {
  name: string;
  description: string;
  widgets: DashboardWidget[];
}

export interface LaunchPoint {
  id: string;
  number: number;
  name: string;
  status: string;
  startedAt: string;
  finishedAt?: string | null;
  durationMs?: number | null;
  branch?: string | null;
  environment?: string | null;
  total: number;
  passed: number;
  failed: number;
  broken: number;
  skipped: number;
  unknown: number;
  flaky: number;
  muted: number;
}

/** A log pattern marked by rules, with the results and launches it marks. */
export interface LogPatternPoint {
  pattern: string;
  results: number;
  launches: number;
  lastSeenAt: string;
}

export interface PatternPoint {
  fingerprint: string;
  message: string;
  occurrences: number;
  firstSeenAt: string;
  lastSeenAt: string;
}

export interface ComponentPoint {
  name: string;
  total: number;
  passed: number;
  failed: number;
  broken: number;
  skipped: number;
}

export interface CumulativePoint {
  value: string;
  launches: number;
  firstAt: string;
  lastAt: string;
  total: number;
  passed: number;
  failed: number;
  broken: number;
  skipped: number;
  unknown: number;
}

export interface BugPoint {
  id: string;
  trackerId: string;
  trackerType: string;
  key: string;
  url?: string | null;
  title: string;
  state: string;
  closed: boolean;
  links: number;
  tests: number;
  createdAt: string;
}

export interface ActivityPoint {
  id: number;
  at: string;
  principal: string;
  action: string;
}

export type WidgetData =
  | { kind: 'cumulative'; attribute: string; values: CumulativePoint[] }
  | { kind: 'bugs'; items: BugPoint[] }
  | { kind: 'activity'; items: ActivityPoint[] }
  | { kind: 'launches'; launches: LaunchPoint[] }
  | { kind: 'trend'; trend: TrendData }
  | { kind: 'growth'; points: { t: string; added: number }[] }
  | { kind: 'patterns'; items: PatternPoint[] }
  | { kind: 'logPatterns'; items: LogPatternPoint[] }
  | { kind: 'launch'; launch: LaunchPoint | null; defects: Record<DefectGroup, number>; components: ComponentPoint[]; groupBy: string };

export type WidgetResult = { status: 'ok'; data: WidgetData } | { status: 'error'; message: string };

export interface DashboardData {
  generatedAt: string;
  widgets: Record<string, WidgetResult>;
}

export interface WidgetInfo {
  type: WidgetType;
  title: string;
  description: string;
  w: number;
  h: number;
  settings: ReadonlyArray<keyof WidgetSettings>;
  group: 'Launches' | 'Trends' | 'Tests' | 'Failures' | 'Project';
}

const LAUNCH_SETTINGS = ['limit', 'branch', 'environment', 'days'] as const;
const TREND_SETTINGS = ['days', 'bucket', 'branch', 'environment'] as const;

export const WIDGET_CATALOG: Record<WidgetType, WidgetInfo> = {
  'launch-statistics': { type: 'launch-statistics', title: 'Launch statistics', description: 'Results of the latest launches by status, as bars, lines or areas.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS, 'chart'], group: 'Launches' },
  'overall-statistics': { type: 'overall-statistics', title: 'Overall statistics', description: 'All results of the chosen launches by status, as a donut.', w: 4, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'investigated-percentage': { type: 'investigated-percentage', title: 'Investigated percentage', description: 'Share of failures that someone triaged, per bucket.', w: 6, h: 4, settings: [...TREND_SETTINGS], group: 'Failures' },
  'launches-comparison': { type: 'launches-comparison', title: 'Launches comparison', description: 'Status shares of the latest launches side by side, in percent.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'launches-duration': { type: 'launches-duration', title: 'Launches duration', description: 'How long the latest launches took.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'launches-table': { type: 'launches-table', title: 'Launches table', description: 'The latest launches with counts and pass rate.', w: 12, h: 5, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'failed-trend': { type: 'failed-trend', title: 'Failed cases trend', description: 'Failed and broken results per launch.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Trends' },
  'non-passed-trend': { type: 'non-passed-trend', title: 'Non-passed cases trend', description: 'Failed, broken and skipped results per launch.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Trends' },
  'test-cases-growth': { type: 'test-cases-growth', title: 'Test cases growth', description: 'New test cases first seen per bucket, and the running total in the range.', w: 6, h: 4, settings: [...TREND_SETTINGS], group: 'Trends' },
  'passing-rate-per-launch': { type: 'passing-rate-per-launch', title: 'Passing rate per launch', description: 'Pass rate of each of the latest launches.', w: 6, h: 4, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'passing-rate-summary': { type: 'passing-rate-summary', title: 'Passing rate summary', description: 'One pass rate over all chosen launches, with the number of launches and results.', w: 3, h: 3, settings: [...LAUNCH_SETTINGS], group: 'Launches' },
  'flaky-tests': { type: 'flaky-tests', title: 'Flaky tests', description: 'Tests with the highest flakiness score.', w: 6, h: 5, settings: ['branch', 'environment', 'limit'], group: 'Tests' },
  'most-failing-tests': { type: 'most-failing-tests', title: 'Most failing tests', description: 'Tests with the most failed or broken results in the range.', w: 6, h: 5, settings: ['days', 'branch', 'environment', 'limit'], group: 'Tests' },
  'popular-patterns': { type: 'popular-patterns', title: 'Most popular error patterns', description: 'Failure messages that occur most often, or the log patterns rules marked most.', w: 12, h: 5, settings: ['days', 'limit', 'patterns'], group: 'Failures' },
  'launch-execution': { type: 'launch-execution', title: 'Launch execution and defects', description: 'Statuses and defect groups of one launch (the latest by default).', w: 6, h: 4, settings: ['launchId', 'branch', 'environment'], group: 'Failures' },
  'component-health': { type: 'component-health', title: 'Component health', description: 'Pass rate per label value (suite by default) in one launch, as bars or a table.', w: 6, h: 5, settings: ['launchId', 'branch', 'environment', 'groupBy', 'view'], group: 'Tests' },
  'pass-rate-trend': { type: 'pass-rate-trend', title: 'Pass rate trend', description: 'Pass rate per bucket over the range.', w: 6, h: 4, settings: [...TREND_SETTINGS], group: 'Trends' },
  durations: { type: 'durations', title: 'Test durations', description: 'p50 and p95 of result durations per bucket.', w: 6, h: 4, settings: [...TREND_SETTINGS], group: 'Trends' },
  'defect-trend': { type: 'defect-trend', title: 'Failures by defect group', description: 'Failed and broken results by triage group per bucket.', w: 6, h: 4, settings: [...TREND_SETTINGS], group: 'Failures' },
  'launches-per-day': { type: 'launches-per-day', title: 'Launches per bucket', description: 'How many launches started per bucket.', w: 6, h: 3, settings: [...TREND_SETTINGS], group: 'Trends' },
  'cumulative-trend': { type: 'cumulative-trend', title: 'Cumulative trend', description: 'Results of all launches that share a value of a launch attribute (such as build), one bar per value, oldest first.', w: 6, h: 4, settings: ['attribute', 'days', 'limit', 'branch', 'environment'], group: 'Trends' },
  'unique-bugs': { type: 'unique-bugs', title: 'Unique bugs', description: 'Issues of the project tracker with the tests that point to them: open ones, and closed ones changed in the range.', w: 12, h: 5, settings: ['days', 'limit'], group: 'Failures' },
  'activity-stream': { type: 'activity-stream', title: 'Activity stream', description: 'The latest changes recorded in the audit log of the project. Shown only to people who may read the audit log.', w: 6, h: 5, settings: ['days', 'limit'], group: 'Project' },
};

export const PREVIEW_DEBOUNCE_MS = 500;
export const GRID_COLUMNS = 12;
export const MAX_WIDGETS = 24;

export const dk = {
  list: (key: string) => ['dashboards', key] as const,
  doc: (key: string, id: string) => ['dashboards', key, id] as const,
  data: (key: string, id: string) => ['dashboards', key, id, 'data'] as const,
  preview: (key: string, sig: string) => ['dashboards', key, 'preview', sig] as const,
};

const base = (key: string) => `${projectPath(key)}/dashboards`;

export function useDashboards(key: string) {
  return useQuery({ queryKey: dk.list(key), queryFn: () => request<{ items: DashboardSummary[] }>(base(key)).then((r) => r.items) });
}

export function useDashboardDoc(key: string, id: string) {
  return useQuery({ queryKey: dk.doc(key, id), queryFn: () => getVersioned<DashboardDoc>(`${base(key)}/${enc(id)}`) });
}

export function useDashboardData(key: string, id: string, enabled = true) {
  return useQuery({
    queryKey: dk.data(key, id),
    queryFn: () => request<DashboardData>(`${base(key)}/${enc(id)}/data`),
    enabled,
    refetchInterval: 60_000,
  });
}

export function useWidgetPreview(key: string, widgets: DashboardWidget[], enabled: boolean) {
  const sig = JSON.stringify(widgets.map((w) => [w.id, w.type, w.settings]));
  const [settled] = useDebouncedValue({ sig, widgets }, PREVIEW_DEBOUNCE_MS);
  return useQuery({
    queryKey: dk.preview(key, settled.sig),
    queryFn: () => request<DashboardData>(`${projectPath(key)}/dashboard-preview`, { method: 'POST', json: { widgets: settled.widgets } }),
    enabled: enabled && settled.widgets.length > 0,
    placeholderData: (prev) => prev,
  });
}

export function useCreateDashboard(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: DashboardWrite) => request<DashboardDoc>(base(key), { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: dk.list(key) }),
  });
}

export function useSaveDashboard(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ body, etag }: { body: DashboardWrite; etag: string }): Promise<Versioned<DashboardDoc>> => writeVersioned<DashboardDoc>(`${base(key)}/${enc(id)}`, 'PUT', etag, body),
    onSuccess: (v) => {
      qc.setQueryData(dk.doc(key, id), v);
      void qc.invalidateQueries({ queryKey: dk.list(key) });
      void qc.invalidateQueries({ queryKey: dk.data(key, id) });
    },
  });
}

export function useDeleteDashboard(key: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ id, etag }: { id: string; etag: string }) => request<unknown>(`${base(key)}/${enc(id)}`, { method: 'DELETE', headers: { 'If-Match': etag } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: dk.list(key) }),
  });
}

export function useLockDashboard(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ locked, etag }: { locked: boolean; etag: string }) => request<DashboardDoc>(`${base(key)}/${enc(id)}/lock`, { method: 'PUT', json: { locked }, headers: { 'If-Match': etag } }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: dk.doc(key, id) });
      void qc.invalidateQueries({ queryKey: dk.list(key) });
    },
  });
}

export function useCloneDashboard(key: string, id: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name?: string; targetProject?: string }) => request<DashboardDoc & { project: string }>(`${base(key)}/${enc(id)}/clone`, { method: 'POST', json: body }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['dashboards'] }),
  });
}

/** Places widgets in reading order on the 12-column grid without gaps to the left. */
export function packWidgets(ws: DashboardWidget[]): DashboardWidget[] {
  let x = 0;
  let y = 0;
  let rowH = 0;
  return ws.map((w) => {
    const width = Math.min(Math.max(1, w.w), GRID_COLUMNS);
    if (x + width > GRID_COLUMNS) {
      x = 0;
      y += rowH;
      rowH = 0;
    }
    const placed = { ...w, w: width, x, y };
    x += width;
    rowH = Math.max(rowH, w.h);
    return placed;
  });
}

export function newWidget(type: WidgetType, existing: DashboardWidget[]): DashboardWidget {
  const info = WIDGET_CATALOG[type];
  let n = existing.length + 1;
  const ids = new Set(existing.map((w) => w.id));
  while (ids.has(`w${String(n)}`)) n++;
  return { id: `w${String(n)}`, type, title: info.title, settings: {}, x: 0, y: 0, w: info.w, h: info.h };
}

export function templateOverview(): DashboardWidget[] {
  const order: WidgetType[] = ['passing-rate-summary', 'overall-statistics', 'pass-rate-trend', 'launch-statistics', 'defect-trend', 'durations', 'flaky-tests', 'most-failing-tests'];
  const out: DashboardWidget[] = [];
  for (const t of order) out.push(newWidget(t, out));
  return packWidgets(out);
}
