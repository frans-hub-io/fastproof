import { describe, expect, it } from 'vitest';
import type { Counts, ResultStatus, TreeNode, TreeTest } from '../api/types';
import {
  allKeys,
  ancestorsOf,
  extraCount,
  failureKeys,
  filterTree,
  flattenTree,
  groupByOf,
  groupName,
  initialExpansion,
  isComplete,
  keyAction,
  type LazyTests,
  loadExpansion,
  nodeKey,
  pageGroupBy,
  parseExplorerParams,
  parseLevels,
  parseSort,
  parseView,
  type Row,
  saveExpansion,
  toggleValue,
  validLabelName,
  worstStatus,
} from './explorer';

function counts(tests: TreeTest[], children: TreeNode[] = []): Counts {
  const c: Counts = { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };
  for (const t of tests) {
    c.total++;
    c[t.status]++;
    if (t.flakyInRun) c.flaky++;
    if (t.muted) c.muted++;
  }
  for (const ch of children) for (const k of Object.keys(c) as (keyof Counts)[]) c[k] += ch.counts[k];
  return c;
}

function t(id: string, status: ResultStatus, durationMs = 100, extra: Partial<TreeTest> = {}): TreeTest {
  return { id, name: id, status, durationMs, ...extra };
}

function group(path: string[], children: TreeNode[], tests: TreeTest[] = []): TreeNode {
  const dur = tests.reduce((s, x) => s + (x.durationMs ?? 0), 0) + children.reduce((s, c) => s + (c.durationMs ?? 0), 0);
  return { name: path[path.length - 1] ?? '', path, children, tests: tests.length ? tests : null, testCount: tests.length, counts: counts(tests, children), durationMs: dur };
}

/** checkout.spec.ts › Checkout › Card (pays failed, refunds passed); Checkout (totals skipped); api › Orders (creates passed). */
function sample(): TreeNode {
  const card = group(['checkout.spec.ts', 'Checkout', 'Card'], [], [t('pays', 'failed', 300), t('refunds', 'passed', 50, { flakyInRun: true, attemptCount: 2 })]);
  const describe = group(['checkout.spec.ts', 'Checkout'], [card], [t('shows totals', 'skipped', 0)]);
  const file = group(['checkout.spec.ts'], [describe]);
  const orders = group(['api', 'Orders'], [], [t('creates', 'passed', 900)]);
  const api = group(['api'], [orders]);
  return group([], [file, api]);
}

const k = (...p: string[]) => nodeKey(p);

function names(rows: Row[]): string[] {
  return rows.map((r) => (r.kind === 'group' ? `${'  '.repeat(r.depth)}[${r.node.name}]` : r.kind === 'test' ? `${'  '.repeat(r.depth)}${r.test.name}` : `${'  '.repeat(r.depth)}…${String(r.remaining)}`));
}

describe('explorer tree', () => {
  it('parses the view and sort, defaulting to suites by name', () => {
    expect(parseView('features')).toBe('features');
    expect(parseView('nope')).toBe('suites');
    expect(parseView(null)).toBe('suites');
    expect(parseSort('duration')).toBe('duration');
    expect(parseSort('x')).toBe('name');
  });

  it('shows the worst status of a group', () => {
    const root = sample();
    expect(worstStatus(root.counts)).toBe('failed');
    expect(worstStatus(root.children[1]?.counts ?? root.counts)).toBe('passed');
    expect(worstStatus({ total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 })).toBeNull();
  });

  it('flattens expanded groups, groups before tests, down to the leaves', () => {
    const root = sample();
    const rows = flattenTree(root, { expanded: new Set([k('checkout.spec.ts'), k('checkout.spec.ts', 'Checkout'), k('checkout.spec.ts', 'Checkout', 'Card')]), sort: 'name' });
    expect(names(rows)).toEqual(['[api]', '[checkout.spec.ts]', '  [Checkout]', '    [Card]', '      pays', '      refunds', '    shows totals']);
    const card = rows[3];
    expect(card?.kind === 'group' && card.expanded).toBe(true);
    expect(rows[4]?.parent).toBe(3);
    expect(rows[6]?.parent).toBe(2);
  });

  it('sorts by status with failures first and by duration with the slowest first', () => {
    const root = sample();
    const all = new Set(allKeys(root));
    expect(names(flattenTree(root, { expanded: all, sort: 'status' })).slice(0, 2)).toEqual(['[checkout.spec.ts]', '  [Checkout]']);
    const byDuration = names(flattenTree(root, { expanded: all, sort: 'duration' }));
    expect(byDuration[0]).toBe('[api]');
    expect(byDuration.indexOf('      pays')).toBeLessThan(byDuration.indexOf('      refunds'));
  });

  it('opens the paths with failures at first', () => {
    const root = sample();
    expect(failureKeys(root)).toEqual([k('checkout.spec.ts'), k('checkout.spec.ts', 'Checkout'), k('checkout.spec.ts', 'Checkout', 'Card')]);
    const single = group([], [group(['only'], [group(['only', 'inner'], [], [t('a', 'passed')])])]);
    expect([...initialExpansion(single)]).toEqual([k('only'), k('only', 'inner')]);
  });

  it('filters by status and keeps the ancestors with recomputed counts', () => {
    const f = filterTree(sample(), { statuses: ['failed'], flaky: false, text: '' });
    expect(f.counts.total).toBe(1);
    expect(f.children.map((c) => c.name)).toEqual(['checkout.spec.ts']);
    const describe = f.children[0]?.children[0];
    expect(describe?.name).toBe('Checkout');
    expect(describe?.tests).toEqual([]);
    expect(describe?.children[0]?.tests?.map((x) => x.id)).toEqual(['pays']);
    expect(describe?.counts.failed).toBe(1);
    expect(describe?.durationMs).toBe(300);
  });

  it('searches test names and group names; a matching group keeps its subtree', () => {
    const byTest = filterTree(sample(), { statuses: [], flaky: false, text: 'REFUND' });
    expect(byTest.counts.total).toBe(1);
    expect(ancestorsOf(byTest, 'refunds')).toEqual([k('checkout.spec.ts'), k('checkout.spec.ts', 'Checkout'), k('checkout.spec.ts', 'Checkout', 'Card')]);
    const byGroup = filterTree(sample(), { statuses: [], flaky: false, text: 'orders' });
    expect(byGroup.counts.total).toBe(1);
    expect(byGroup.children.map((c) => c.name)).toEqual(['api']);
    const flaky = filterTree(sample(), { statuses: [], flaky: true, text: 'checkout' });
    expect(flaky.counts.total).toBe(1);
    expect(flaky.counts.flaky).toBe(1);
    const none = filterTree(sample(), { statuses: ['broken'], flaky: false, text: '' });
    expect(none.children).toEqual([]);
    expect(flattenTree(none, { expanded: new Set(), sort: 'name' })).toEqual([]);
  });

  it('pages groups that are not in the response', () => {
    const lazyGroup: TreeNode = { name: 'Big', path: ['Big'], children: [], testCount: 1200, counts: { total: 1200, passed: 1200, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 } };
    const root = group([], [lazyGroup]);
    expect(isComplete(root)).toBe(false);
    expect(isComplete(sample())).toBe(true);
    const expanded = new Set([k('Big')]);
    expect(names(flattenTree(root, { expanded, sort: 'name' }))).toEqual(['[Big]', '  …1200']);
    const lazy = new Map<string, LazyTests>([[k('Big'), { items: [t('x1', 'passed'), t('x0', 'passed')], next: 'c', loading: false, error: null, loaded: true }]]);
    expect(names(flattenTree(root, { expanded, sort: 'name', lazy }))).toEqual(['[Big]', '  x0', '  x1', '  …1198']);
    expect(ancestorsOf(root, 'x1', lazy)).toEqual([k('Big')]);
    const done = new Map<string, LazyTests>([[k('Big'), { items: [t('x0', 'passed')], next: null, loading: false, error: null, loaded: true }]]);
    expect(names(flattenTree(root, { expanded, sort: 'name', lazy: done }))).toEqual(['[Big]', '  x0']);
  });

  it('moves with the arrows, opens and closes groups and selects tests', () => {
    const root = sample();
    const rows = flattenTree(root, { expanded: new Set([k('checkout.spec.ts')]), sort: 'name' });
    expect(names(rows)).toEqual(['[api]', '[checkout.spec.ts]', '  [Checkout]']);
    expect(keyAction(rows, -1, 'ArrowDown')).toEqual({ index: 0 });
    expect(keyAction(rows, 2, 'ArrowDown')).toEqual({ index: 2 });
    expect(keyAction(rows, 0, 'ArrowUp')).toEqual({ index: 0 });
    expect(keyAction(rows, 0, 'ArrowRight')).toEqual({ index: 0, toggle: k('api'), open: true });
    expect(keyAction(rows, 1, 'ArrowRight')).toEqual({ index: 2 });
    expect(keyAction(rows, 1, 'ArrowLeft')).toEqual({ index: 1, toggle: k('checkout.spec.ts'), open: false });
    expect(keyAction(rows, 2, 'ArrowLeft')).toEqual({ index: 1 });
    expect(keyAction(rows, 0, 'ArrowLeft')).toBeNull();
    expect(keyAction(rows, 2, 'End')).toEqual({ index: 2 });
    expect(keyAction(rows, 2, 'Home')).toEqual({ index: 0 });
    expect(keyAction(rows, 2, 'x')).toBeNull();
    const open = flattenTree(root, { expanded: new Set(allKeys(root)), sort: 'name' });
    const pays = open.findIndex((r) => r.kind === 'test' && r.test.id === 'pays');
    expect(keyAction(open, pays, 'Enter')).toEqual({ index: pays, select: 'pays' });
    expect(keyAction(open, pays, 'ArrowRight')).toBeNull();
    expect(keyAction(open, pays, 'ArrowLeft')).toEqual({ index: open[pays]?.parent });
    expect(keyAction([], 0, 'ArrowDown')).toBeNull();
  });

  it('remembers the expansion in the session and survives a broken storage', () => {
    saveExpansion('fastproof.explorer.test', new Set([k('a'), k('a', 'b')]));
    expect([...(loadExpansion('fastproof.explorer.test') ?? [])]).toEqual([k('a'), k('a', 'b')]);
    window.sessionStorage.setItem('fastproof.explorer.bad', '{not json');
    expect(loadExpansion('fastproof.explorer.bad')).toBeNull();
    expect(loadExpansion('fastproof.explorer.none')).toBeNull();
    const original = Object.getOwnPropertyDescriptor(window, 'sessionStorage');
    Object.defineProperty(window, 'sessionStorage', {
      configurable: true,
      get() {
        throw new Error('blocked');
      },
    });
    try {
      expect(loadExpansion('fastproof.explorer.test')).toBeNull();
      expect(() => saveExpansion('fastproof.explorer.test', new Set(['x']))).not.toThrow();
    } finally {
      if (original) Object.defineProperty(window, 'sessionStorage', original);
    }
  });
});

describe('explorer grouping and filters', () => {
  it('maps the presets and a custom grouping to groupBy', () => {
    expect(parseView('owner')).toBe('owner');
    expect(parseView('custom')).toBe('custom');
    expect(groupByOf('suites', [])).toBe('suite');
    expect(groupByOf('features', [])).toBe('epic,feature,story');
    expect(groupByOf('severity', ['x'])).toBe('severity');
    expect(groupByOf('custom', [])).toBe('suite');
    expect(groupByOf('custom', ['team', 'owner'])).toBe('team,owner');
    expect(groupByOf('custom', ['feature']), 'a label named like a preset').toBe('label:feature');
  });

  it('keeps valid, distinct label names, five at most', () => {
    expect(parseLevels('epic, feature ,epic,bad name,,x.y')).toEqual(['epic', 'feature', 'x.y']);
    expect(parseLevels('a,b,c,d,e,f')).toEqual(['a', 'b', 'c', 'd', 'e']);
    expect(parseLevels(null)).toEqual([]);
    expect(validLabelName('ünïcode_1')).toBe(true);
    expect(validLabelName('a'.repeat(65))).toBe(false);
    expect(validLabelName('ж'.repeat(33)), 'the server counts bytes').toBe(false);
    expect(validLabelName('')).toBe(false);
  });

  it('pages groups by the levels the tree reports', () => {
    const root = { name: 'root', path: [], counts: counts([]), children: [] } as TreeNode;
    expect(pageGroupBy('epic,feature,story', { ...root, levels: ['feature', 'story'] })).toBe('feature,story');
    expect(pageGroupBy('epic,package', { ...root, levels: ['package'] })).toBe('label:package');
    expect(pageGroupBy('suite', root)).toBe('suite');
    expect(pageGroupBy('owner', null)).toBe('owner');
  });

  it('reads the filters from the URL and counts those set', () => {
    const p = parseExplorerParams(new URLSearchParams('retried=true&muted=false&severity=critical,(none)&minDurationMs=1500&maxDurationMs=x&env=staging&source=manual&message=%20%20&newFailures=1'));
    expect(p).toMatchObject({ retried: true, muted: false, severity: 'critical,(none)', minDurationMs: 1500, env: 'staging', source: 'manual' });
    expect(p.maxDurationMs).toBeUndefined();
    expect(p.message).toBeUndefined();
    expect(p.newFailures).toBeUndefined();
    expect(extraCount(p), 'env is a scope, not a filter').toBe(5);
    expect(extraCount(parseExplorerParams(new URLSearchParams('')))).toBe(0);
  });

  it('toggles list values and names the group of missing labels', () => {
    expect(toggleValue('a,b', 'c', true)).toBe('a,b,c');
    expect(toggleValue('a,b', 'a', false)).toBe('b');
    expect(toggleValue('a', 'a', false)).toBeNull();
    expect(toggleValue(undefined, 'a', true)).toBe('a');
    expect(groupName('')).toBe('(none)');
    expect(groupName('Shop')).toBe('Shop');
  });
});
