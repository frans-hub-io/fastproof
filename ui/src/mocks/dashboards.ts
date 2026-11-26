import type { DashboardData, DashboardDoc, DashboardSummary, DashboardWidget, DashboardWrite, LaunchPoint, WidgetData, WidgetResult, WidgetSettings } from '../api/dashboards';
import { GRID_COLUMNS, MAX_WIDGETS, WIDGET_CATALOG, WIDGET_TYPES } from '../api/dashboards';
import type { DefectGroup } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { launchResults, launchView, NOW, projectLaunches } from './fixtures';
import { requirementCoverageData } from './requirements';
import { dashboard, flakyOf } from './stageC';

const DAY = 86_400_000;
export const MAX_DASHBOARDS = 50;

interface Stored extends Omit<DashboardDoc, 'owned'> {
  project: string;
}

let seq = 1;
let store: Stored[] = [];

function seed(): Stored[] {
  const w = (id: string, type: DashboardWidget['type'], x: number, y: number, ww: number, h: number, settings: WidgetSettings = {}): DashboardWidget => ({ id, type, title: WIDGET_CATALOG[type].title, settings, x, y, w: ww, h });
  const stamp = new Date(NOW - 2 * DAY).toISOString();
  return [
    {
      project: 'web-shop',
      id: 'd0000000-0000-4000-8000-000000000001',
      name: 'Release health',
      description: 'What to look at before a release',
      owner: 'sam@example.com',
      locked: true,
      revision: 3,
      createdAt: stamp,
      updatedAt: stamp,
      widgets: [
        w('w1', 'passing-rate-summary', 0, 0, 3, 3),
        w('w2', 'launch-statistics', 3, 0, 9, 4, { limit: 12 }),
        w('w3', 'pass-rate-trend', 0, 4, 6, 4),
        w('w4', 'defect-trend', 6, 4, 6, 4),
        w('w5', 'flaky-tests', 0, 8, 6, 5),
        w('w6', 'component-health', 6, 8, 6, 5),
      ],
    },
  ];
}

export function resetDashboards(): void {
  store = seed();
  seq = 2;
}
resetDashboards();

const summary = (d: Stored): DashboardSummary => ({ id: d.id, name: d.name, description: d.description, owner: d.owner, owned: d.owner === 'alex@example.com', locked: d.locked, revision: d.revision, widgetCount: d.widgets.length, createdAt: d.createdAt, updatedAt: d.updatedAt });
const doc = (d: Stored): DashboardDoc => {
  return { id: d.id, name: d.name, description: d.description, owner: d.owner, owned: d.owner === 'alex@example.com', locked: d.locked, revision: d.revision, createdAt: d.createdAt, updatedAt: d.updatedAt, widgets: d.widgets };
};

export function listDashboards(project: string): DashboardSummary[] {
  return store.filter((d) => d.project === project).map(summary);
}

export function getDashboard(project: string, id: string): DashboardDoc | undefined {
  const d = store.find((x) => x.project === project && x.id === id);
  return d ? doc(d) : undefined;
}

export function validateWrite(b: Partial<DashboardWrite>): string | null {
  if (!b.name || b.name.length > 120) return 'name must be 1-120 characters';
  if ((b.description?.length ?? 0) > 2000) return 'description is longer than 2000 characters';
  if (!Array.isArray(b.widgets)) return 'widgets must be a list';
  if (b.widgets.length > MAX_WIDGETS) return `at most ${String(MAX_WIDGETS)} widgets`;
  const ids = new Set<string>();
  for (const w of b.widgets) {
    if (!/^[a-z0-9-]{1,32}$/.test(w.id) || ids.has(w.id)) return 'widget ids must be unique, 1-32 characters of a-z, 0-9 and -';
    ids.add(w.id);
    if (!(WIDGET_TYPES as readonly string[]).includes(w.type)) return `unknown widget type ${w.type}`;
    if (w.title.length > 120) return 'widget title is longer than 120 characters';
    if (w.w < 1 || w.w > GRID_COLUMNS || w.x < 0 || w.x + w.w > GRID_COLUMNS || w.h < 1 || w.h > 12 || w.y < 0) return 'widget does not fit the 12-column grid';
    if (w.settings.days !== undefined && (w.settings.days < 1 || w.settings.days > 730)) return 'days must be 1-730';
    if (w.settings.limit !== undefined && (w.settings.limit < 1 || w.settings.limit > 50)) return 'limit must be 1-50';
  }
  return null;
}

export function createDashboard(project: string, owner: string, b: DashboardWrite): DashboardDoc | 'limit' {
  if (store.filter((d) => d.project === project).length >= MAX_DASHBOARDS) return 'limit';
  const now = new Date().toISOString();
  const d: Stored = { project, id: `d0000000-0000-4000-8000-${String(seq++).padStart(12, '0')}`, name: b.name, description: b.description, owner, locked: false, revision: 1, createdAt: now, updatedAt: now, widgets: b.widgets };
  store.push(d);
  return doc(d);
}

export function saveDashboard(project: string, id: string, revision: number, b: DashboardWrite): DashboardDoc | 'missing' | 'stale' {
  const d = store.find((x) => x.project === project && x.id === id);
  if (!d) return 'missing';
  if (d.revision !== revision) return 'stale';
  Object.assign(d, { name: b.name, description: b.description, widgets: b.widgets, revision: d.revision + 1, updatedAt: new Date().toISOString() });
  return doc(d);
}

export function lockDashboard(project: string, id: string, revision: number, locked: boolean): DashboardDoc | 'missing' | 'stale' {
  const d = store.find((x) => x.project === project && x.id === id);
  if (!d) return 'missing';
  if (d.revision !== revision) return 'stale';
  d.locked = locked;
  d.revision++;
  return doc(d);
}

export function deleteDashboard(project: string, id: string, revision: number): 'ok' | 'missing' | 'stale' {
  const d = store.find((x) => x.project === project && x.id === id);
  if (!d) return 'missing';
  if (d.revision !== revision) return 'stale';
  store = store.filter((x) => x !== d);
  return 'ok';
}

export function cloneDashboard(project: string, id: string, owner: string, name: string | undefined, target: string): DashboardDoc | 'missing' | 'limit' {
  const src = getDashboard(project, id);
  if (!src) return 'missing';
  return createDashboard(target, owner, { name: name ?? `${src.name} (copy)`, description: src.description, widgets: src.widgets });
}

function points(project: string, s: WidgetSettings, defaultLimit = 10): LaunchPoint[] {
  const limit = s.limit ?? defaultLimit;
  const from = NOW - (s.days ?? 30) * DAY;
  return projectLaunches(project)
    .filter((d) => !d.launch.debug && Date.parse(d.launch.startedAt) >= from)
    .filter((d) => (!s.branch || d.launch.vcs?.branch === s.branch) && (!s.environment || d.launch.environment?.key === s.environment))
    .slice(0, limit)
    .map((d) => {
      const l = launchView(d);
      return {
        id: l.id,
        number: l.number,
        name: l.name,
        status: l.status,
        startedAt: l.startedAt,
        finishedAt: l.finishedAt,
        durationMs: l.durationMs,
        branch: l.vcs?.branch ?? null,
        environment: l.environment?.key ?? null,
        total: l.counts.total,
        passed: l.counts.passed,
        failed: l.counts.failed,
        broken: l.counts.broken,
        skipped: l.counts.skipped,
        unknown: l.counts.unknown,
        flaky: l.counts.flaky,
        muted: l.counts.muted,
      };
    })
    .reverse();
}

function dataOf(project: string, w: DashboardWidget): WidgetData {
  const s = w.settings;
  switch (w.type) {
    case 'launch-statistics':
    case 'overall-statistics':
    case 'launches-comparison':
    case 'launches-duration':
    case 'launches-table':
    case 'failed-trend':
    case 'non-passed-trend':
    case 'passing-rate-per-launch':
    case 'passing-rate-summary':
      return { kind: 'launches', launches: points(project, s, w.type === 'launches-table' ? 10 : 12) };
    case 'flaky-tests':
    case 'most-failing-tests':
    case 'pass-rate-trend':
    case 'durations':
    case 'defect-trend':
    case 'launches-per-day':
    case 'investigated-percentage': {
      const t = dashboard(project, { branch: s.branch, environment: s.environment, bucket: s.bucket, from: new Date(NOW - (s.days ?? 30) * DAY).toISOString().slice(0, 10), to: new Date(NOW).toISOString().slice(0, 10) });
      const limit = s.limit ?? 10;
      return { kind: 'trend', trend: { ...t, flakyTop: flakyOf(project, s.branch, s.environment, limit), mostFailing: t.mostFailing.slice(0, limit) } };
    }
    case 'test-cases-growth': {
      const days = Math.min(s.days ?? 30, 60);
      return { kind: 'growth', points: Array.from({ length: days }, (_, i) => ({ t: new Date(NOW - (days - 1 - i) * DAY).toISOString().slice(0, 10), added: (i * 7) % 5 })) };
    }
    case 'popular-patterns':
      if (s.patterns === 'logPatterns') {
        return {
          kind: 'logPatterns',
          items: [
            { pattern: 'oom', results: 41, launches: 9 },
            { pattern: 'db-timeout', results: 17, launches: 6 },
            { pattern: 'grid-node-lost', results: 5, launches: 2 },
          ]
            .slice(0, s.limit ?? 10)
            .map((p, i) => ({ ...p, lastSeenAt: new Date(NOW - i * DAY).toISOString() })),
        };
      }
      return {
        kind: 'patterns',
        items: ['TimeoutError: waiting for locator("#checkout")', 'AssertionError: expected 200 but got 503', 'ECONNRESET while calling payments', 'Element is not attached to the DOM'].slice(0, s.limit ?? 10).map((message, i) => ({
          fingerprint: `fp${String(i)}`,
          message,
          occurrences: 120 - i * 25,
          firstSeenAt: new Date(NOW - 20 * DAY).toISOString(),
          lastSeenAt: new Date(NOW - i * DAY).toISOString(),
        })),
      };
    case 'cumulative-trend': {
      if (!s.attribute) return { kind: 'cumulative', attribute: '', values: [] };
      const values = ['1.4.0', '1.4.1', '1.5.0'].map((value, i) => ({ value, launches: 3 + i, firstAt: new Date(NOW - (9 - 3 * i) * DAY).toISOString(), lastAt: new Date(NOW - (7 - 3 * i) * DAY).toISOString(), total: 120 + 10 * i, passed: 100 + 8 * i, failed: 12 - i, broken: 4, skipped: 3, unknown: 1 }));
      return { kind: 'cumulative', attribute: s.attribute, values: values.slice(-(s.limit ?? 10)) };
    }
    case 'unique-bugs':
      return {
        kind: 'bugs',
        items: [
          { id: 'b1', trackerId: 'jira', trackerType: 'jira', key: 'SHOP-1234', url: 'https://issues.example.com/browse/SHOP-1234', title: 'Checkout returns 500 for empty carts', state: 'In Progress', closed: false, links: 2, tests: 7, createdAt: new Date(NOW - 5 * DAY).toISOString() },
          { id: 'b2', trackerId: 'jira', trackerType: 'jira', key: 'SHOP-1180', url: 'javascript:alert(1)', title: 'Payment timeout under load <b>not bold</b>', state: 'Open', closed: false, links: 1, tests: 3, createdAt: new Date(NOW - 12 * DAY).toISOString() },
          { id: 'b3', trackerId: 'jira', trackerType: 'jira', key: 'OPS-77', url: 'https://issues.example.com/browse/OPS-77', title: 'Flaky runner clock', state: 'Done', closed: true, links: 1, tests: 1, createdAt: new Date(NOW - 20 * DAY).toISOString() },
        ].slice(0, s.limit ?? 10),
      };
    case 'requirement-coverage':
      return requirementCoverageData(project, s);
    case 'activity-stream':
      return {
        kind: 'activity',
        items: ['launch.merge', 'dashboard.update', 'tracker.issue.create', 'project.settings.urls', 'dashboard.create'].map((action, i) => ({ id: 100 - i, at: new Date(NOW - i * 3_600_000).toISOString(), principal: i % 2 ? 'alex@example.com' : 'ci-bot', action })).slice(0, s.limit ?? 10),
      };
    case 'launch-execution':
    case 'component-health': {
      const all = points(project, { ...s, limit: 50 }, 50);
      const launch = (s.launchId ? all.find((p) => p.id === s.launchId) : undefined) ?? all[all.length - 1] ?? null;
      const defects = Object.fromEntries(DEFECT_GROUPS.map((g: DefectGroup) => [g, 0])) as Record<DefectGroup, number>;
      const comps = new Map<string, { name: string; total: number; passed: number; failed: number; broken: number; skipped: number }>();
      if (launch) {
        for (const r of launchResults(launch.id)) {
          const name = r.name.split(/[ .:>/]/)[0] || 'other';
          const c = comps.get(name) ?? { name, total: 0, passed: 0, failed: 0, broken: 0, skipped: 0 };
          c.total++;
          if (r.status === 'passed') c.passed++;
          else if (r.status === 'failed') c.failed++;
          else if (r.status === 'broken') c.broken++;
          else if (r.status === 'skipped') c.skipped++;
          comps.set(name, c);
          if (r.status === 'failed' || r.status === 'broken') defects[r.defect?.group ?? 'toInvestigate']++;
        }
      }
      return { kind: 'launch', launch, defects, components: [...comps.values()].sort((a, b) => b.total - a.total).slice(0, 20), groupBy: s.groupBy || 'suite' };
    }
  }
}

export function dashboardData(project: string, widgets: DashboardWidget[], canReadAudit = true): DashboardData {
  const out: Record<string, WidgetResult> = {};
  for (const w of widgets) {
    if (w.type === 'activity-stream' && !canReadAudit) out[w.id] = { status: 'error', message: 'The activity stream is shown to people who may read the audit log (audit:read).' };
    else if (w.settings.branch === 'broken-branch') out[w.id] = { status: 'error', message: 'the widget could not be computed' };
    else out[w.id] = { status: 'ok', data: dataOf(project, w) };
  }
  return { generatedAt: new Date().toISOString(), widgets: out };
}
