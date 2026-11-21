import type { Counts, ResultStatus, TreeGroupBy, TreeNode, TreeTest } from '../api/types';

export type ExplorerView = 'suites' | 'features' | 'packages';
export type ExplorerSort = 'name' | 'status' | 'duration';

export const VIEWS: readonly ExplorerView[] = ['suites', 'features', 'packages'];
export const VIEW_GROUP: Record<ExplorerView, TreeGroupBy> = { suites: 'suite', features: 'feature', packages: 'package' };
export const SORTS: readonly ExplorerSort[] = ['name', 'status', 'duration'];

export function parseView(v: string | null): ExplorerView {
  return (VIEWS as readonly string[]).includes(v ?? '') ? (v as ExplorerView) : 'suites';
}

export function parseSort(v: string | null): ExplorerSort {
  return (SORTS as readonly string[]).includes(v ?? '') ? (v as ExplorerSort) : 'name';
}

export interface ExplorerFilter {
  statuses: readonly string[];
  flaky: boolean;
  text: string;
}

export function filterActive(f: ExplorerFilter): boolean {
  return f.statuses.length > 0 || f.flaky || f.text.trim() !== '';
}

/** A stable key of a group. */
export function nodeKey(path: readonly string[]): string {
  return JSON.stringify(path);
}

const RANK: Record<string, number> = { failed: 0, broken: 1, unknown: 2, skipped: 3, passed: 4 };

export function statusRank(s: string): number {
  return RANK[s] ?? 2;
}

/** The status a group shows: the worst one under it. */
export function worstStatus(c: Counts): ResultStatus | null {
  if (c.failed) return 'failed';
  if (c.broken) return 'broken';
  if (c.unknown) return 'unknown';
  if (c.passed) return 'passed';
  if (c.skipped) return 'skipped';
  return null;
}

export function failing(c: Counts): number {
  return c.failed + c.broken;
}

function empty(): Counts {
  return { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 };
}

function addTest(c: Counts, t: TreeTest): void {
  c.total++;
  const s: ResultStatus = t.status in RANK ? t.status : 'unknown';
  c[s]++;
  if (t.flakyInRun) c.flaky++;
  if (t.muted) c.muted++;
}

function addCounts(a: Counts, b: Counts): void {
  a.total += b.total;
  a.passed += b.passed;
  a.failed += b.failed;
  a.broken += b.broken;
  a.skipped += b.skipped;
  a.unknown += b.unknown;
  a.flaky += b.flaky;
  a.muted += b.muted;
}

/** Tests directly in the node that are not in the response (read page by page). */
export function lazyCount(n: TreeNode): number {
  const count = n.testCount ?? 0;
  return n.tests ? 0 : count;
}

/** Whether every test of the tree is in it, so it can be filtered here. */
export function isComplete(n: TreeNode): boolean {
  if (lazyCount(n) > 0) return false;
  return n.children.every(isComplete);
}

function matches(t: TreeTest, f: ExplorerFilter, needle: string, ancestorMatched: boolean): boolean {
  if (f.statuses.length && !f.statuses.includes(t.status)) return false;
  if (f.flaky && !t.flakyInRun) return false;
  if (!needle || ancestorMatched) return true;
  return t.name.toLowerCase().includes(needle);
}

/**
 * The tree with only the tests that match: groups stay when a test under them matches (or, for
 * the text, when their own name does), with counts and durations of what is left.
 */
export function filterTree(root: TreeNode, f: ExplorerFilter): TreeNode {
  if (!filterActive(f)) return root;
  const needle = f.text.trim().toLowerCase();
  const walk = (n: TreeNode, ancestorMatched: boolean, isRoot: boolean): TreeNode | null => {
    const own = !isRoot && !!needle && n.name.toLowerCase().includes(needle);
    const matched = ancestorMatched || own;
    const children = n.children.map((c) => walk(c, matched, false)).filter((c): c is TreeNode => c !== null);
    const tests = (n.tests ?? []).filter((t) => matches(t, f, needle, matched));
    if (!isRoot && !children.length && !tests.length) return null;
    const counts = empty();
    let duration: number | null = null;
    for (const c of children) {
      addCounts(counts, c.counts);
      if (typeof c.durationMs === 'number') duration = (duration ?? 0) + c.durationMs;
    }
    for (const t of tests) {
      addTest(counts, t);
      if (typeof t.durationMs === 'number') duration = (duration ?? 0) + t.durationMs;
    }
    return { ...n, children, tests: n.tests ? tests : n.tests, testCount: n.tests ? tests.length : n.testCount, counts, durationMs: duration };
  };
  return walk(root, false, true) ?? root;
}

/** Keys of the groups on the way to a failed or broken result. */
export function failureKeys(root: TreeNode): string[] {
  const out: string[] = [];
  const walk = (n: TreeNode) => {
    for (const c of n.children) {
      if (failing(c.counts) > 0) {
        out.push(nodeKey(c.path));
        walk(c);
      }
    }
  };
  walk(root);
  return out;
}

/** Keys of every group, at most `limit`. */
export function allKeys(root: TreeNode, limit = 20_000): string[] {
  const out: string[] = [];
  const stack = [...root.children];
  while (stack.length && out.length < limit) {
    const n = stack.pop();
    if (!n) break;
    out.push(nodeKey(n.path));
    stack.push(...n.children);
  }
  return out;
}

/** Keys of the chain of single groups from the top (opened so a one-suite launch is not a click away). */
export function singleChainKeys(root: TreeNode): string[] {
  const out: string[] = [];
  let n = root;
  while (n.children.length === 1 && !(n.tests?.length ?? 0)) {
    const only = n.children[0];
    if (!only) break;
    out.push(nodeKey(only.path));
    n = only;
  }
  return out;
}

/** The initial expansion: paths with failures and the chain of single groups. */
export function initialExpansion(root: TreeNode): Set<string> {
  return new Set([...singleChainKeys(root), ...failureKeys(root)]);
}

/** Keys of the groups above a loaded test, or null when it is not in the tree. */
export function ancestorsOf(root: TreeNode, id: string, lazy?: ReadonlyMap<string, LazyTests>): string[] | null {
  const walk = (n: TreeNode, keys: string[]): string[] | null => {
    const tests = n.tests ?? lazy?.get(nodeKey(n.path))?.items ?? [];
    if (tests.some((t) => t.id === id)) return keys;
    for (const c of n.children) {
      const found = walk(c, [...keys, nodeKey(c.path)]);
      if (found) return found;
    }
    return null;
  };
  return walk(root, []);
}

export interface LazyTests {
  items: TreeTest[];
  next: string | null;
  loading: boolean;
  error: string | null;
  loaded: boolean;
}

export interface GroupRow {
  kind: 'group';
  key: string;
  node: TreeNode;
  depth: number;
  expanded: boolean;
  parent: number;
}

export interface TestRow {
  kind: 'test';
  key: string;
  test: TreeTest;
  depth: number;
  parent: number;
}

export interface MoreRow {
  kind: 'more';
  key: string;
  nodeKey: string;
  path: string[];
  depth: number;
  remaining: number;
  loading: boolean;
  error: string | null;
  parent: number;
}

export type Row = GroupRow | TestRow | MoreRow;

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function compareNodes(sort: ExplorerSort) {
  return (a: TreeNode, b: TreeNode): number => {
    if (sort === 'status') {
      const d = statusRank(worstStatus(a.counts) ?? 'unknown') - statusRank(worstStatus(b.counts) ?? 'unknown');
      if (d) return d;
      const f = failing(b.counts) - failing(a.counts);
      if (f) return f;
    } else if (sort === 'duration') {
      const d = (b.durationMs ?? 0) - (a.durationMs ?? 0);
      if (d) return d;
    }
    return collator.compare(a.name, b.name);
  };
}

function compareTests(sort: ExplorerSort) {
  return (a: TreeTest, b: TreeTest): number => {
    if (sort === 'status') {
      const d = statusRank(a.status) - statusRank(b.status);
      if (d) return d;
    } else if (sort === 'duration') {
      const d = (b.durationMs ?? 0) - (a.durationMs ?? 0);
      if (d) return d;
    }
    return collator.compare(a.name, b.name) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  };
}

export interface FlattenOptions {
  expanded: ReadonlySet<string>;
  sort: ExplorerSort;
  lazy?: ReadonlyMap<string, LazyTests>;
}

/** The visible rows: groups first, then their tests, down the expanded groups. */
export function flattenTree(root: TreeNode, o: FlattenOptions): Row[] {
  const rows: Row[] = [];
  const byNode = compareNodes(o.sort);
  const byTest = compareTests(o.sort);
  const visit = (n: TreeNode, depth: number, parent: number) => {
    for (const c of [...n.children].sort(byNode)) {
      const key = nodeKey(c.path);
      const expanded = o.expanded.has(key);
      const index = rows.length;
      rows.push({ kind: 'group', key, node: c, depth, expanded, parent });
      if (expanded) visit(c, depth + 1, index);
    }
    const key = nodeKey(n.path);
    const lazy = n.tests ? null : (o.lazy?.get(key) ?? null);
    const tests = n.tests ?? lazy?.items ?? [];
    for (const t of [...tests].sort(byTest)) rows.push({ kind: 'test', key: t.id, test: t, depth, parent });
    const total = n.testCount ?? tests.length;
    if (!n.tests && total > 0 && (!lazy?.loaded || lazy.next !== null)) {
      rows.push({ kind: 'more', key: `${key}:more`, nodeKey: key, path: n.path, depth, remaining: Math.max(0, total - tests.length), loading: lazy?.loading ?? false, error: lazy?.error ?? null, parent });
    }
  };
  visit(root, 0, -1);
  return rows;
}

export type KeyAction = { index: number } | { index: number; toggle: string; open: boolean } | { index: number; select: string } | null;

/** What a key does on the row at `index` (WAI-ARIA tree pattern). */
export function keyAction(rows: readonly Row[], index: number, key: string): KeyAction {
  if (!rows.length) return null;
  const at = Math.min(Math.max(index, 0), rows.length - 1);
  const row = rows[at];
  if (!row) return null;
  switch (key) {
    case 'ArrowDown':
      return { index: index < 0 ? 0 : Math.min(rows.length - 1, at + 1) };
    case 'ArrowUp':
      return { index: Math.max(0, at - 1) };
    case 'Home':
      return { index: 0 };
    case 'End':
      return { index: rows.length - 1 };
    case 'ArrowRight':
      if (row.kind !== 'group') return null;
      if (!row.expanded) return { index: at, toggle: row.key, open: true };
      return { index: Math.min(rows.length - 1, at + 1) };
    case 'ArrowLeft':
      if (row.kind === 'group' && row.expanded) return { index: at, toggle: row.key, open: false };
      return row.parent >= 0 ? { index: row.parent } : null;
    case 'Enter':
    case ' ':
      if (row.kind === 'group') return { index: at, toggle: row.key, open: !row.expanded };
      if (row.kind === 'test') return { index: at, select: row.test.id };
      return null;
    default:
      return null;
  }
}

/** Remembered expansion, per launch and view, in the session (absent storage is fine). */
export function loadExpansion(storageKey: string): Set<string> | null {
  try {
    const raw = window.sessionStorage.getItem(storageKey);
    if (!raw) return null;
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? new Set(v.filter((x): x is string => typeof x === 'string')) : null;
  } catch {
    return null;
  }
}

export const MAX_REMEMBERED = 5000;

export function saveExpansion(storageKey: string, keys: ReadonlySet<string>): void {
  try {
    window.sessionStorage.setItem(storageKey, JSON.stringify([...keys].slice(0, MAX_REMEMBERED)));
  } catch {
    /* storage is optional */
  }
}
