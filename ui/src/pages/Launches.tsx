import { Anchor, Autocomplete, Badge, Button, Card, Group, Select, SimpleGrid, Stack, Switch, Text, TextInput, Tooltip } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { IconGitBranch, IconGitCommit, IconHandFinger, IconSearch, IconServer2, IconX } from '@tabler/icons-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { useProjectEvents, type LiveState } from '../api/events';
import { flatItems, useLaunches, useProject } from '../api/hooks';
import type { Launch, LaunchFilters } from '../api/types';
import { LoadMore } from '../components/LoadMore';
import { PageHeader } from '../components/PageHeader';
import { passRate } from '../lib/counts';
import { QueryState } from '../components/QueryState';
import { CountsInline, LaunchStatusBadge, StatusBar } from '../components/Status';
import { formatDuration, formatRelative, shortSha } from '../lib/format';
import { useListKeys } from '../lib/keys';

export function LiveBadge({ state }: { state: LiveState }) {
  if (state === 'unsupported') return null;
  const color = state === 'live' ? 'green' : state === 'connecting' ? 'yellow' : 'gray';
  const label = state === 'live' ? 'Live' : state === 'connecting' ? 'Connecting' : 'Offline';
  return (
    <Tooltip label={state === 'live' ? 'Updates arrive as results are uploaded' : 'Live updates are not connected; data refreshes periodically'}>
      <Badge variant="dot" color={color} size="sm" style={{ textTransform: 'none' }} data-testid="live-state">
        {label}
      </Badge>
    </Tooltip>
  );
}

export function LaunchProgress({ launch }: { launch: Launch }) {
  const pending = launch.uploads.queued + launch.uploads.processing;
  return (
    <Stack gap={4}>
      <StatusBar counts={launch.counts} height={10} live />
      <Text size="xs" c="dimmed" data-testid="launch-progress">
        Running · {launch.counts.total} results so far
        {pending > 0 ? ` · ${String(pending)} upload${pending === 1 ? '' : 's'} in progress` : ''}
      </Text>
    </Stack>
  );
}

/** Marks a launch that carries a manual run: open runs show how many cases are left. */
export function ManualBadge({ manual }: { manual: NonNullable<Launch['manual']> }) {
  const left = manual.untested + manual.inProgress;
  const open = manual.state === 'open';
  const text = open ? (left > 0 ? `manual: ${String(left)} left` : 'manual: open') : 'manual: closed';
  return (
    <Tooltip label={open ? `Manual run open: ${String(manual.total - left)} of ${String(manual.total)} cases finished` : `Manual run closed: ${String(manual.total)} cases`}>
      <Badge size="xs" variant={open ? 'light' : 'outline'} color={open ? 'blue' : 'gray'} leftSection={<IconHandFinger size={10} aria-hidden />} style={{ flex: 'none' }} data-testid="launch-manual" data-state={manual.state} tabIndex={0}>
        {text}
      </Badge>
    </Tooltip>
  );
}

function LaunchRow({ project, launch, index, selected, onSelect }: { project: string; launch: Launch; index: number; selected: boolean; onSelect: (i: number) => void }) {
  const to = `/p/${project}/launches/${launch.id}`;
  return (
    <Card
      withBorder
      padding="sm"
      radius="md"
      className="fp-row"
      data-selected={selected}
      data-list-index={index}
      data-testid="launch-row"
      onMouseEnter={() => onSelect(index)}
    >
      <SimpleGrid cols={{ base: 1, md: 2 }} spacing="sm" verticalSpacing={8}>
        <Stack gap={4} style={{ minWidth: 0 }}>
          <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
            <Anchor component={Link} to={to} fw={700} className="fh-truncate" c="inherit" underline="hover">
              #{launch.number} {launch.name}
            </Anchor>
            <LaunchStatusBadge status={launch.status} />
            {launch.debug && (
              <Badge size="xs" variant="outline" color="gray">
                debug
              </Badge>
            )}
            {launch.manual && <ManualBadge manual={launch.manual} />}
            {launch.uploads.failed > 0 && (
              <Badge size="xs" variant="light" color="red" style={{ flex: 'none' }}>
                {launch.uploads.failed} upload failed
              </Badge>
            )}
          </Group>
          <Group gap="md" wrap="wrap">
            {launch.vcs?.branch && (
              <Group gap={4} wrap="nowrap">
                <IconGitBranch size={14} />
                <Text size="xs" c="dimmed">
                  {launch.vcs.branch}
                </Text>
              </Group>
            )}
            {launch.vcs?.commit && (
              <Group gap={4} wrap="nowrap">
                <IconGitCommit size={14} />
                <Text size="xs" c="dimmed" className="fp-mono">
                  {shortSha(launch.vcs.commit)}
                </Text>
              </Group>
            )}
            {launch.environment && (
              <Group gap={4} wrap="nowrap">
                <IconServer2 size={14} />
                <Text size="xs" c="dimmed">
                  {launch.environment.key}
                </Text>
              </Group>
            )}
            <Text size="xs" c="dimmed">
              {formatRelative(launch.startedAt)}
            </Text>
            {typeof launch.durationMs === 'number' && (
              <Text size="xs" c="dimmed">
                {formatDuration(launch.durationMs)}
              </Text>
            )}
          </Group>
        </Stack>
        <Stack gap={6} justify="center">
          {launch.status === 'running' ? (
            <LaunchProgress launch={launch} />
          ) : (
            <>
              <StatusBar counts={launch.counts} height={10} />
              <Group justify="space-between" gap="xs" wrap="wrap">
                <CountsInline counts={launch.counts} />
                <Text size="xs" c="dimmed">
                  {launch.counts.total} tests · {Math.round(passRate(launch.counts) * 100)}% passed
                </Text>
              </Group>
            </>
          )}
        </Stack>
      </SimpleGrid>
    </Card>
  );
}

const FILTER_KEYS = ['branch', 'environment', 'status', 'attr', 'q'] as const;

function readFilters(sp: URLSearchParams): LaunchFilters {
  const f: LaunchFilters = {};
  for (const k of FILTER_KEYS) {
    const v = sp.get(k);
    if (v) f[k] = v;
  }
  f.debug = sp.get('debug') === 'true';
  return f;
}

export default function LaunchesPage() {
  const { key = '' } = useParams();
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const project = useProject(key);
  const live = useProjectEvents(key);
  const [text, setText] = useState(sp.get('q') ?? '');
  const [debounced] = useDebouncedValue(text, 300);
  const filters = useMemo(() => readFilters(sp), [sp]);
  const launches = useLaunches(key, filters);
  const items = flatItems(launches.data);
  const [selected, setSelected] = useState(-1);

  const set = useCallback(
    (k: string, v: string | null) => {
      setSp(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (v) next.set(k, v);
          else next.delete(k);
          return next;
        },
        { replace: true },
      );
    },
    [setSp],
  );

  useEffect(() => {
    if ((sp.get('q') ?? '') !== debounced) set('q', debounced || null);
  }, [debounced, sp, set]);

  const branches = useMemo(() => [...new Set(items.map((l) => l.vcs?.branch).filter((b): b is string => !!b))].sort(), [items]);
  const envs = useMemo(() => [...new Set(items.map((l) => l.environment?.key).filter((b): b is string => !!b))].sort(), [items]);
  const attrs = useMemo(() => [...new Set(items.flatMap((l) => l.attributes.map((a) => `${a.key}:${a.value}`)))].sort(), [items]);

  const open = useCallback(
    (i: number) => {
      const l = items[i];
      if (l) void navigate(`/p/${key}/launches/${l.id}`);
    },
    [items, key, navigate],
  );
  useListKeys({ count: items.length, index: selected, setIndex: setSelected, onOpen: open });

  const anyFilter = FILTER_KEYS.some((k) => sp.get(k)) || sp.get('debug') === 'true';
  const name = project.data?.name ?? key;

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: name }]}
        title={name}
        badges={<LiveBadge state={live} />}
        subtitle={project.data?.description ?? undefined}
      />
      <Card withBorder padding="sm" radius="md" mb="md">
        <SimpleGrid cols={{ base: 2, md: 3, lg: 6 }} spacing="xs">
          <TextInput
            aria-label="Search launches"
            placeholder="Search launches"
            leftSection={<IconSearch size={16} />}
            value={text}
            onChange={(e) => setText(e.currentTarget.value)}
          />
          <Autocomplete aria-label="Branch" placeholder="Branch" data={branches} value={sp.get('branch') ?? ''} onChange={(v) => set('branch', v || null)} clearable />
          <Autocomplete aria-label="Environment" placeholder="Environment" data={envs} value={sp.get('environment') ?? ''} onChange={(v) => set('environment', v || null)} clearable />
          <Select
            aria-label="Status"
            placeholder="Any status"
            data={[
              { value: 'running', label: 'Running' },
              { value: 'finished', label: 'Finished' },
              { value: 'interrupted', label: 'Interrupted' },
            ]}
            value={sp.get('status')}
            onChange={(v) => set('status', v)}
            clearable
          />
          <Autocomplete aria-label="Attribute" placeholder="Attribute key:value" data={attrs} value={sp.get('attr') ?? ''} onChange={(v) => set('attr', v || null)} clearable />
          <Group gap="xs" wrap="nowrap" justify="space-between">
            <Switch label="Debug only" checked={sp.get('debug') === 'true'} onChange={(e) => set('debug', e.currentTarget.checked ? 'true' : null)} />
            {anyFilter && (
              <Button
                variant="subtle"
                size="xs"
                leftSection={<IconX size={14} />}
                onClick={() => {
                  setText('');
                  setSp(new URLSearchParams(), { replace: true });
                }}
              >
                Clear
              </Button>
            )}
          </Group>
        </SimpleGrid>
      </Card>
      <QueryState query={project} notFound="Project not found">
        {() => (
          <QueryState query={launches}>
            {() =>
              items.length === 0 ? (
                anyFilter ? (
                  <EmptyState title="No launches match" description="Change or clear the filters." />
                ) : (
                  <EmptyState title="No launches yet" description="Upload test results from CI with the fastproof CLI or the upload API, and the launches show up here." />
                )
              ) : (
                <Stack gap="xs" data-testid="launch-list">
                  {items.map((l, i) => (
                    <LaunchRow key={l.id} project={key} launch={l} index={i} selected={i === selected} onSelect={setSelected} />
                  ))}
                  <LoadMore hasMore={launches.hasNextPage} loading={launches.isFetchingNextPage} onLoad={() => void launches.fetchNextPage()} label="Load older launches" />
                  <Text size="xs" c="dimmed" ta="center" visibleFrom="sm">
                    Tip: j / k move the selection, Enter opens it.
                  </Text>
                </Stack>
              )
            }
          </QueryState>
        )}
      </QueryState>
    </>
  );
}
