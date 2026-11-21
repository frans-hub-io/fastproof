import { ActionIcon, Badge, Button, Chip, Drawer, Group, Loader, Menu, SegmentedControl, Stack, Text, TextInput, Tooltip } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useDebouncedValue, useMediaQuery } from '@mantine/hooks';
import {
  IconAlertCircleFilled,
  IconArrowsSort,
  IconBellOff,
  IconBolt,
  IconChevronDown,
  IconChevronRight,
  IconCircleCheckFilled,
  IconCircleMinus,
  IconCircleXFilled,
  IconFoldDown,
  IconFoldUp,
  IconHelpCircleFilled,
  IconListTree,
  IconSearch,
} from '@tabler/icons-react';
import { memo, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react';
import { Link, useSearchParams } from 'react-router';
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso';
import { fetchTreeTests, useExplorerTree } from '../../api/hooks';
import { errorMessage } from '../../api/client';
import { useDefectTypeMap } from '../../api/stageC';
import type { Counts, Launch, ResultStatus, TreeFilters, TreeNode } from '../../api/types';
import { RESULT_STATUSES } from '../../api/types';
import type { DefectType } from '../../api/types-next';
import { DefectChip } from '../../components/Defect';
import { StatusDot } from '../../components/Status';
import {
  allKeys,
  ancestorsOf,
  type ExplorerFilter,
  type ExplorerSort,
  filterActive,
  filterTree,
  flattenTree,
  initialExpansion,
  isComplete,
  keyAction,
  type LazyTests,
  loadExpansion,
  parseSort,
  parseView,
  type Row,
  saveExpansion,
  VIEW_GROUP,
  worstStatus,
} from '../../lib/explorer';
import { formatDuration } from '../../lib/format';
import { isTypingTarget } from '../../lib/keys';
import { STATUS_LABEL, statusVar } from '../../lib/status';
import { ExplorerDetail } from './ExplorerDetail';

const ROW_HEIGHT = 32;
const SPLIT_KEY = 'fastproof.explorer.split';

const ICONS = {
  passed: IconCircleCheckFilled,
  failed: IconCircleXFilled,
  broken: IconAlertCircleFilled,
  skipped: IconCircleMinus,
  unknown: IconHelpCircleFilled,
} as const;

export function StatusIcon({ status, size = 16 }: { status: string | null; size?: number }) {
  const Icon = status && status in ICONS ? ICONS[status as ResultStatus] : IconHelpCircleFilled;
  return <Icon size={size} style={{ color: statusVar(status ?? 'unknown'), flex: 'none' }} aria-label={status ? STATUS_LABEL[status as ResultStatus] : 'No results'} role="img" />;
}

const COUNT_ORDER: (ResultStatus | 'flaky')[] = ['failed', 'broken', 'passed', 'skipped', 'unknown', 'flaky'];

function CountPills({ counts }: { counts: Counts }) {
  return (
    <span className="fp-x-counts" aria-label={COUNT_ORDER.filter((s) => counts[s] > 0).map((s) => `${String(counts[s])} ${s}`).join(', ')}>
      {COUNT_ORDER.filter((s) => counts[s] > 0).map((s) => (
        <span key={s} className="fp-x-count" style={{ color: statusVar(s), background: `color-mix(in srgb, ${statusVar(s)} 14%, transparent)` }} data-count={s} aria-hidden>
          {s === 'flaky' && <IconBolt size={10} style={{ marginRight: 1 }} />}
          {counts[s]}
        </span>
      ))}
    </span>
  );
}

interface RowProps {
  row: Row;
  index: number;
  active: boolean;
  selected: boolean;
  testHref: (id: string) => string;
  types: Map<string, DefectType>;
  onGroup: (index: number, key: string) => void;
  onTest: (index: number, id: string) => void;
  onMore: (nodeKey: string, path: string[]) => void;
}

function MoreRow({ row, onMore }: { row: Extract<Row, { kind: 'more' }>; onMore: (k: string, p: string[]) => void }) {
  const { loading, error, nodeKey, path } = row;
  useEffect(() => {
    if (!loading && !error) onMore(nodeKey, path);
  }, [loading, error, nodeKey, path, onMore]);
  return (
    <Group gap={6} wrap="nowrap" style={{ height: '100%' }} data-testid="explorer-more">
      {error ? (
        <>
          <Text size="xs" c="var(--mantine-color-error)" className="fh-truncate">
            {error}
          </Text>
          <Button size="compact-xs" variant="subtle" onClick={() => onMore(nodeKey, path)}>
            Retry
          </Button>
        </>
      ) : (
        <>
          <Loader size={12} />
          <Text size="xs" c="dimmed">
            Loading {row.remaining ? `${row.remaining.toLocaleString()} more tests` : 'tests'}…
          </Text>
        </>
      )}
    </Group>
  );
}

const ExplorerRow = memo(function ExplorerRow({ row, index, active, selected, testHref, types, onGroup, onTest, onMore }: RowProps) {
  const indent: CSSProperties = { paddingLeft: 6 + row.depth * 16 };
  const common = {
    id: `fp-x-row-${String(index)}`,
    role: 'treeitem',
    'aria-level': row.depth + 1,
    'data-active': active,
    'data-index': index,
    className: 'fp-x-row',
  } as const;
  if (row.kind === 'more') {
    return (
      <div {...common} style={{ ...indent, paddingLeft: 6 + row.depth * 16 + 20 }} aria-busy={row.loading}>
        <MoreRow row={row} onMore={onMore} />
      </div>
    );
  }
  if (row.kind === 'group') {
    const n = row.node;
    const status = worstStatus(n.counts);
    return (
      <div {...common} style={indent} aria-expanded={row.expanded} data-testid="explorer-group" data-status={status ?? ''} data-key={row.key} onClick={() => onGroup(index, row.key)} title={n.path.join(' › ')}>
        <span className="fp-x-chevron" aria-hidden>
          {row.expanded ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
        </span>
        <StatusIcon status={status} size={15} />
        <Text size="sm" fw={600} className="fh-truncate fp-x-name">
          {n.name || '(no name)'}
        </Text>
        <CountPills counts={n.counts} />
        <Text size="xs" c="dimmed" className="fp-x-duration">
          {typeof n.durationMs === 'number' ? formatDuration(n.durationMs) : ''}
        </Text>
      </div>
    );
  }
  const t = row.test;
  return (
    <Link
      {...common}
      to={testHref(t.id)}
      style={indent}
      aria-selected={selected}
      data-selected={selected}
      data-testid="explorer-test"
      data-status={t.status}
      data-id={t.id}
      tabIndex={-1}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        onTest(index, t.id);
      }}
      title={t.name}
    >
      <span className="fp-x-chevron" aria-hidden />
      <StatusIcon status={t.status} size={15} />
      <Text size="sm" className="fh-truncate fp-x-name">
        {t.name}
      </Text>
      <span className="fp-x-marks">
        {(t.attemptCount ?? 1) > 1 && (
          <Tooltip label={`${String(t.attemptCount)} attempts`}>
            <Badge size="xs" variant="light" color="gray" radius="sm" data-testid="explorer-retries">
              ×{t.attemptCount}
            </Badge>
          </Tooltip>
        )}
        {t.flakyInRun && (
          <Tooltip label="Flaky in this launch: failed, then passed on retry">
            <span className="fp-x-mark" style={{ color: statusVar('flaky') }} data-testid="explorer-flaky">
              <IconBolt size={14} aria-label="flaky" />
            </span>
          </Tooltip>
        )}
        {t.muted && (
          <Tooltip label="Muted: does not count as a failure">
            <span className="fp-x-mark" data-testid="explorer-muted">
              <IconBellOff size={14} aria-label="muted" />
            </span>
          </Tooltip>
        )}
        {t.defect && <DefectChip defect={t.defect} types={types} size="xs" />}
      </span>
      <Text size="xs" c="dimmed" className="fp-x-duration">
        {t.status === 'skipped' && !t.durationMs ? '' : formatDuration(t.durationMs)}
      </Text>
    </Link>
  );
});

function useSplit(): [number, (n: number) => void] {
  const [split, setSplit] = useState<number>(() => {
    try {
      const v = Number(window.localStorage.getItem(SPLIT_KEY));
      return v >= 20 && v <= 75 ? v : 42;
    } catch {
      return 42;
    }
  });
  const set = useCallback((n: number) => {
    const v = Math.min(75, Math.max(20, Math.round(n)));
    setSplit(v);
    try {
      window.localStorage.setItem(SPLIT_KEY, String(v));
    } catch {
      /* storage is optional */
    }
  }, []);
  return [split, set];
}

interface Scoped<T> {
  scope: string;
  value: T;
}

/** The launch tree down to the tests, with the selected test beside it. */
export function Explorer({ project, launch }: { project: string; launch: Launch }) {
  const [sp, setSp] = useSearchParams();
  const phone = useMediaQuery('(max-width: 48em)');
  const view = parseView(sp.get('view'));
  const sort = parseSort(sp.get('sort'));
  const selected = sp.get('test');
  const statusParam = sp.get('status') ?? '';
  const statuses = useMemo(() => statusParam.split(',').filter(Boolean), [statusParam]);
  const flaky = sp.get('flaky') === 'true';
  const [text, setText] = useState(sp.get('q') ?? '');
  const [debounced] = useDebouncedValue(text, 250);
  const filter: ExplorerFilter = useMemo(() => ({ statuses, flaky, text: debounced }), [statuses, flaky, debounced]);
  const groupBy = VIEW_GROUP[view];
  const running = launch.status === 'running';
  const types = useDefectTypeMap(project);

  const setParam = useCallback(
    (k: string, v: string | null, push = false) =>
      setSp(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (v) next.set(k, v);
          else next.delete(k);
          return next;
        },
        { replace: !push },
      ),
    [setSp],
  );

  useEffect(() => {
    if ((sp.get('q') ?? '') !== debounced) setParam('q', debounced || null);
  }, [debounced, sp, setParam]);

  const urlQ = sp.get('q') ?? '';
  const [seenQ, setSeenQ] = useState(urlQ);
  if (urlQ !== seenQ) {
    setSeenQ(urlQ);
    if (urlQ !== debounced) setText(urlQ);
  }

  const [reveal] = useState(selected ?? undefined);
  const baseFilters: TreeFilters = useMemo(() => ({ groupBy, reveal }), [groupBy, reveal]);
  const base = useExplorerTree(project, launch.id, baseFilters, running);
  const complete = base.data ? isComplete(base.data) : true;
  const serverFiltered = !complete && filterActive(filter);
  const serverFilters: TreeFilters = useMemo(
    () => ({ groupBy, status: statusParam || undefined, q: debounced.trim() || undefined, flaky: flaky || undefined, reveal }),
    [groupBy, statusParam, debounced, flaky, reveal],
  );
  const filtered = useExplorerTree(project, launch.id, serverFilters, running, serverFiltered);
  const lazyFilters = serverFiltered ? serverFilters : baseFilters;
  const source = serverFiltered ? filtered : base;
  const tree: TreeNode | null = useMemo(() => {
    if (serverFiltered) return filtered.data ?? null;
    return base.data ? filterTree(base.data, filter) : null;
  }, [serverFiltered, filtered.data, base.data, filter]);
  const settled = !!tree && !source.isPlaceholderData;

  const lazyScope = `${groupBy}|${serverFiltered ? JSON.stringify(serverFilters) : ''}`;
  const [lazyState, setLazyState] = useState<Scoped<Map<string, LazyTests>>>({ scope: lazyScope, value: new Map() });
  const lazy = lazyState.scope === lazyScope ? lazyState.value : null;
  const guard = useRef<{ scope: string; inflight: Set<string>; next: Map<string, string | null> }>({ scope: lazyScope, inflight: new Set(), next: new Map() });

  const loadMore = useCallback(
    (nodeKey: string, path: string[]) => {
      const scope = lazyScope;
      if (guard.current.scope !== scope) guard.current = { scope, inflight: new Set(), next: new Map() };
      const g = guard.current;
      if (g.inflight.has(nodeKey) || g.next.get(nodeKey) === null) return;
      g.inflight.add(nodeKey);
      const cursor = g.next.get(nodeKey);
      const update = (fn: (prev: LazyTests | undefined) => LazyTests) =>
        setLazyState((s) => {
          if (guard.current.scope !== scope) return s;
          const map = new Map(s.scope === scope ? s.value : []);
          map.set(nodeKey, fn(map.get(nodeKey)));
          return { scope, value: map };
        });
      update((p) => ({ items: p?.items ?? [], next: p?.next ?? null, loaded: p?.loaded ?? false, loading: true, error: null }));
      fetchTreeTests(project, launch.id, lazyFilters, path, cursor ?? null).then(
        (page) => {
          if (guard.current !== g) return;
          g.inflight.delete(nodeKey);
          g.next.set(nodeKey, page.next ?? null);
          update((p) => {
            const seen = new Set((p?.items ?? []).map((t) => t.id));
            return { items: [...(p?.items ?? []), ...page.items.filter((t) => !seen.has(t.id))], next: page.next ?? null, loaded: true, loading: false, error: null };
          });
        },
        (e: unknown) => {
          if (guard.current !== g) return;
          g.inflight.delete(nodeKey);
          update((p) => ({ items: p?.items ?? [], next: p?.next ?? null, loaded: p?.loaded ?? false, loading: false, error: errorMessage(e) }));
        },
      );
    },
    [project, launch.id, lazyFilters, lazyScope],
  );

  const storageKey = `fastproof.explorer.${launch.id}.${view}`;
  const [expState, setExpState] = useState<Scoped<Set<string>> | null>(null);
  if (base.data && !base.isPlaceholderData && expState?.scope !== storageKey) {
    const keys = loadExpansion(storageKey) ?? initialExpansion(base.data);
    if (selected) for (const k of ancestorsOf(base.data, selected) ?? []) keys.add(k);
    setExpState({ scope: storageKey, value: keys });
  }
  const filterScope = filterActive(filter) ? `${view}|${JSON.stringify(filter)}` : '';
  const [filterExp, setFilterExp] = useState<Scoped<Set<string>> | null>(null);
  if (filterScope && tree && settled && filterExp?.scope !== filterScope) {
    setFilterExp({ scope: filterScope, value: new Set(allKeys(tree, 5000)) });
  }
  const usingFilterExp = !!filterScope && filterExp?.scope === filterScope;
  const expanded = useMemo(() => (usingFilterExp ? filterExp.value : expState?.scope === storageKey ? expState.value : new Set<string>()), [usingFilterExp, filterExp, expState, storageKey]);

  const setExpanded = useCallback(
    (fn: (prev: Set<string>) => Set<string>) => {
      if (usingFilterExp) {
        setFilterExp((s) => (s ? { scope: s.scope, value: fn(s.value) } : s));
        return;
      }
      setExpState((s) => {
        const value = fn(s?.scope === storageKey ? s.value : new Set());
        saveExpansion(storageKey, value);
        return { scope: storageKey, value };
      });
    },
    [usingFilterExp, storageKey],
  );

  const toggle = useCallback(
    (key: string, open?: boolean) =>
      setExpanded((prev) => {
        const next = new Set(prev);
        const want = open ?? !next.has(key);
        if (want) next.add(key);
        else next.delete(key);
        return next;
      }),
    [setExpanded],
  );

  const rows = useMemo(() => (tree ? flattenTree(tree, { expanded, sort, lazy: lazy ?? undefined }) : []), [tree, expanded, sort, lazy]);
  const [active, setActive] = useState(-1);
  const virtuoso = useRef<VirtuosoHandle>(null);
  const grid = useRef<HTMLDivElement>(null);
  const treeEl = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!selected || !tree) return;
    const keys = ancestorsOf(tree, selected, lazy ?? undefined);
    if (keys?.some((k) => !expanded.has(k))) {
      setExpanded((prev) => new Set([...prev, ...keys]));
    }
  }, [selected, tree, lazy, expanded, setExpanded]);

  const selectedIndex = useMemo(() => (selected ? rows.findIndex((r) => r.kind === 'test' && r.test.id === selected) : -1), [rows, selected]);
  const revealed = useRef<string | null>(null);
  const [initialIndex, setInitialIndex] = useState(-1);
  if (initialIndex < 0 && selectedIndex >= 0 && reveal === selected) setInitialIndex(selectedIndex);
  useEffect(() => {
    if (selectedIndex < 0 || revealed.current === selected) return;
    const first = revealed.current === null && reveal === selected;
    revealed.current = selected;
    setActive(selectedIndex);
    if (first) grid.current?.scrollIntoView({ block: 'start' });
    const frame = window.requestAnimationFrame(() => virtuoso.current?.scrollIntoView({ index: selectedIndex, align: 'center' }));
    return () => window.cancelAnimationFrame(frame);
  }, [selectedIndex, selected, reveal]);

  const select = useCallback(
    (id: string | null) =>
      setSp(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (id) next.set('test', id);
          else next.delete('test');
          next.delete('attempt');
          next.delete('section');
          return next;
        },
        { replace: false },
      ),
    [setSp],
  );

  const testHref = useCallback(
    (id: string) => {
      const next = new URLSearchParams(sp);
      next.set('test', id);
      next.delete('attempt');
      next.delete('section');
      return `?${next.toString()}`;
    },
    [sp],
  );

  const onGroup = useCallback(
    (index: number, key: string) => {
      setActive(index);
      toggle(key);
    },
    [toggle],
  );
  const onTest = useCallback(
    (index: number, id: string) => {
      setActive(index);
      revealed.current = id;
      select(id);
    },
    [select],
  );

  const moveTo = useCallback(
    (index: number) => {
      setActive(index);
      virtuoso.current?.scrollIntoView({ index });
    },
    [],
  );

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Escape' && selected) {
      e.preventDefault();
      select(null);
      return;
    }
    const start = active >= 0 ? active : selectedIndex;
    const a = keyAction(rows, start, e.key);
    if (!a) return;
    e.preventDefault();
    if ('toggle' in a) toggle(a.toggle, a.open);
    if ('select' in a) {
      revealed.current = a.select;
      select(a.select);
    }
    moveTo(a.index);
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || !selected || isTypingTarget(e.target) || e.defaultPrevented) return;
      if (document.querySelector('[role="dialog"]')) return;
      select(null);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [selected, select]);

  const [split, setSplit] = useSplit();
  const onResize = (e: ReactPointerEvent<HTMLDivElement>) => {
    const box = grid.current?.getBoundingClientRect();
    if (!box) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => setSplit(((ev.clientX - box.left) / box.width) * 100);
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  const c = launch.counts;
  const toolbar = (
    <Stack gap={8} p="xs" className="fp-x-toolbar">
      <Group gap="xs" wrap="nowrap" justify="space-between">
        <SegmentedControl
          size="xs"
          value={view}
          onChange={(v) => {
            setActive(-1);
            setParam('view', v === 'suites' ? null : v);
          }}
          data={[
            { value: 'suites', label: 'Suites' },
            { value: 'features', label: 'Features' },
            { value: 'packages', label: 'Packages' },
          ]}
          aria-label="Group by"
          data-testid="explorer-view"
        />
        <Group gap={2} wrap="nowrap">
          <Menu position="bottom-end" withinPortal>
            <Menu.Target>
              <Tooltip label={`Sort by ${sort}`}>
                <ActionIcon variant="subtle" color="gray" aria-label={`Sort by ${sort}`} data-testid="explorer-sort">
                  <IconArrowsSort size={16} />
                </ActionIcon>
              </Tooltip>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Label>Sort by</Menu.Label>
              {(['name', 'status', 'duration'] as ExplorerSort[]).map((s) => (
                <Menu.Item key={s} onClick={() => setParam('sort', s === 'name' ? null : s)} data-active={sort === s} fw={sort === s ? 700 : 400}>
                  {s === 'name' ? 'Name' : s === 'status' ? 'Status (failures first)' : 'Duration (slowest first)'}
                </Menu.Item>
              ))}
            </Menu.Dropdown>
          </Menu>
          <Tooltip label="Expand all">
            <ActionIcon variant="subtle" color="gray" aria-label="Expand all" onClick={() => tree && setExpanded(() => new Set(allKeys(tree)))} data-testid="explorer-expand-all">
              <IconFoldDown size={16} />
            </ActionIcon>
          </Tooltip>
          <Tooltip label="Collapse all">
            <ActionIcon variant="subtle" color="gray" aria-label="Collapse all" onClick={() => setExpanded(() => new Set())} data-testid="explorer-collapse-all">
              <IconFoldUp size={16} />
            </ActionIcon>
          </Tooltip>
        </Group>
      </Group>
      <TextInput
        size="xs"
        aria-label="Search tests"
        placeholder="Search tests and groups"
        leftSection={<IconSearch size={14} />}
        rightSection={(base.isFetching || filtered.isFetching) && !base.isPending ? <Loader size={12} /> : undefined}
        value={text}
        onChange={(e) => setText(e.currentTarget.value)}
        data-testid="explorer-search"
      />
      <Chip.Group multiple value={statuses} onChange={(v) => setParam('status', v.join(',') || null)}>
        <Group gap={4} wrap="wrap">
          {RESULT_STATUSES.filter((s) => c[s] > 0 || statuses.includes(s)).map((s) => (
            <Chip key={s} value={s} size="xs" variant="outline" data-testid={`explorer-chip-${s}`}>
              <Group gap={4} wrap="nowrap" component="span">
                <StatusDot status={s} size={8} />
                {STATUS_LABEL[s]} {c[s]}
              </Group>
            </Chip>
          ))}
          {(c.flaky > 0 || flaky) && (
            <Chip checked={flaky} onChange={(v) => setParam('flaky', v ? 'true' : null)} size="xs" variant="outline" data-testid="explorer-chip-flaky">
              <Group gap={4} wrap="nowrap" component="span">
                <StatusDot status="flaky" size={8} />
                Flaky {c.flaky}
              </Group>
            </Chip>
          )}
        </Group>
      </Chip.Group>
    </Stack>
  );

  const loading = base.isPending || (serverFiltered && filtered.isPending && !filtered.data);
  const error = source.isError ? errorMessage(source.error) : null;
  const list = loading ? (
    <Group justify="center" p="lg">
      <Loader size="sm" />
    </Group>
  ) : error ? (
    <Text size="sm" c="var(--mantine-color-error)" p="md">
      {error}
    </Text>
  ) : !rows.length ? (
    <EmptyState title={filterActive(filter) ? 'No tests match' : 'No results yet'} description={filterActive(filter) ? 'Change the search or the status filters.' : undefined} />
  ) : (
    <div
      ref={treeEl}
      role="tree"
      aria-label="Tests of the launch"
      aria-activedescendant={active >= 0 && active < rows.length ? `fp-x-row-${String(active)}` : undefined}
      tabIndex={0}
      onKeyDown={onKeyDown}
      onFocus={() => {
        if (active < 0) setActive(selectedIndex >= 0 ? selectedIndex : 0);
      }}
      className="fp-x-tree"
      data-testid="explorer-tree"
      data-busy={source.isPlaceholderData || source.isFetching ? 'true' : 'false'}
    >
      <Virtuoso
        ref={virtuoso}
        data={rows}
        computeItemKey={(_, r) => r.key}
        fixedItemHeight={ROW_HEIGHT}
        increaseViewportBy={ROW_HEIGHT * 20}
        useWindowScroll={phone}
        initialTopMostItemIndex={initialIndex >= 0 ? { index: initialIndex, align: 'center' } : 0}
        style={phone ? undefined : { height: '100%' }}
        itemContent={(i, r) => <ExplorerRow row={r} index={i} active={i === active} selected={r.kind === 'test' && r.test.id === selected} testHref={testHref} types={types} onGroup={onGroup} onTest={onTest} onMore={loadMore} />}
      />
    </div>
  );

  const total = tree?.counts.total ?? 0;
  const footer = (
    <Group gap="xs" justify="space-between" px="xs" py={4} className="fp-x-footer" wrap="nowrap">
      <Text size="xs" c="dimmed" className="fh-truncate">
        {filterActive(filter) ? `${total.toLocaleString()} of ${c.total.toLocaleString()} tests` : `${c.total.toLocaleString()} tests`}
        {typeof tree?.durationMs === 'number' ? ` · ${formatDuration(tree.durationMs)} in total` : ''}
      </Text>
      <Text size="xs" c="dimmed" visibleFrom="sm" className="fh-truncate" title="↑ ↓ move, → ← open and close groups, Enter shows a test, Esc closes it">
        Keys: ↑↓ → ← Enter Esc
      </Text>
    </Group>
  );

  const detail = selected ? (
    <ExplorerDetail project={project} launch={launch} resultId={selected} onClose={() => select(null)} />
  ) : (
    <Stack align="center" justify="center" gap="xs" h="100%" p="xl" data-testid="explorer-empty-detail">
      <IconListTree size={36} style={{ color: 'var(--mantine-color-dimmed)' }} />
      <Text fw={600}>Pick a test</Text>
      <Text size="sm" c="dimmed" ta="center" maw={360}>
        Its steps, error, attachments and history open here. Groups with failures are already open.
      </Text>
    </Stack>
  );

  if (phone) {
    return (
      <div className="fp-explorer-phone" data-testid="explorer">
        {toolbar}
        {list}
        {footer}
        <Drawer opened={!!selected} onClose={() => select(null)} position="right" size="100%" title={<Text fw={600}>Test</Text>} data-testid="explorer-drawer" padding="sm">
          {selected && <ExplorerDetail project={project} launch={launch} resultId={selected} />}
        </Drawer>
      </div>
    );
  }

  return (
    <div ref={grid} className="fp-explorer" style={{ '--fp-x-split': `${String(split)}%` } as CSSProperties} data-testid="explorer">
      <div className="fp-x-left">
        {toolbar}
        <div className="fp-x-list">{list}</div>
        {footer}
      </div>
      <div
        className="fp-x-resizer"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the tree"
        aria-valuemin={20}
        aria-valuemax={75}
        aria-valuenow={split}
        tabIndex={0}
        onPointerDown={onResize}
        onKeyDown={(e) => {
          if (e.key === 'ArrowLeft') setSplit(split - 3);
          if (e.key === 'ArrowRight') setSplit(split + 3);
        }}
      />
      <div className="fp-x-right" data-testid="explorer-detail-pane">
        {detail}
      </div>
    </div>
  );
}

