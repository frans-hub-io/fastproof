import { Accordion, ActionIcon, Anchor, Badge, Button, Card, Chip, Group, SegmentedControl, SimpleGrid, Stack, Switch, Tabs, Text, TextInput, Tooltip, UnstyledButton } from '@mantine/core';
import { DonutChart } from '@mantine/charts';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { IconAlertTriangle, IconBrandGithub, IconBrandGitlab, IconExternalLink, IconGitBranch, IconGitCommit, IconGitCompare, IconHandFinger, IconListCheck, IconSearch, IconStar, IconStarFilled, IconUpload, IconList, IconListTree } from '@tabler/icons-react';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { useProjectEvents } from '../api/events';
import { flatItems, useLaunch, useProject, useResults, useUploads } from '../api/hooks';
import { useDefectTypeMap, usePatchLaunch } from '../api/stageC';
import type { Counts, Launch, ResultFilters, ResultStatus, ResultSummary, Upload } from '../api/types';
import type { DefectType } from '../api/types-next';
import { RESULT_STATUSES } from '../api/types';
import { DefectChip } from '../components/Defect';
import { GateBadge } from '../components/GateBadge';
import { LoadMore } from '../components/LoadMore';
import { LogConsole } from '../components/LogConsole';
import { PageHeader } from '../components/PageHeader';
import { passRate } from '../lib/counts';
import { QueryState } from '../components/QueryState';
import { LaunchStatusBadge, StatusBadge, StatusBar, StatusDot } from '../components/Status';
import { ciLabel, commitUrl, repoLabel } from '../lib/ci';
import { formatBytes, formatDateTime, formatDuration, formatRelative, isHttpUrl, percent, shortSha } from '../lib/format';
import { stripAnsi } from '../lib/ansi';
import { useListKeys } from '../lib/keys';
import { rememberLaunch } from '../lib/recent';
import { STATUS_LABEL, statusVar, uploadStatusColor } from '../lib/status';
import { Clusters } from './launch/Clusters';
import { Explorer } from './launch/Explorer';
import { TestPlanModal } from './launch/TestPlanModal';
import { TransitionBadge } from './launch/TransitionBadge';
import { LaunchProgress, LiveBadge } from './Launches';
import { Manual } from './launch/Manual';
import { ManualIcon } from './cases/common';

function Meta({ launch }: { launch: Launch }) {
  const cUrl = commitUrl(launch.vcs, launch.ci);
  const provider = (launch.ci?.provider ?? '').toLowerCase();
  const CiIcon = provider.includes('github') ? IconBrandGithub : IconBrandGitlab;
  return (
    <Group gap="md" wrap="wrap" mt={4}>
      {launch.vcs?.branch && (
        <Group gap={4} wrap="nowrap">
          <IconGitBranch size={14} />
          <Text size="sm">{launch.vcs.branch}</Text>
        </Group>
      )}
      {launch.vcs?.commit && (
        <Group gap={4} wrap="nowrap">
          <IconGitCommit size={14} />
          {cUrl ? (
            <Anchor href={cUrl} target="_blank" rel="noopener noreferrer" size="sm" className="fp-mono" data-testid="commit-link">
              {shortSha(launch.vcs.commit)}
            </Anchor>
          ) : (
            <Text size="sm" className="fp-mono">
              {shortSha(launch.vcs.commit)}
            </Text>
          )}
          {launch.vcs.repository && (
            <Text size="sm" c="dimmed">
              in {repoLabel(launch.vcs.repository)}
            </Text>
          )}
        </Group>
      )}
      {launch.ci && (
        <Group gap={4} wrap="nowrap">
          <CiIcon size={14} />
          {isHttpUrl(launch.ci.url) ? (
            <Anchor href={launch.ci.url} target="_blank" rel="noopener noreferrer" size="sm" data-testid="ci-link">
              {ciLabel(launch.ci)} <IconExternalLink size={12} />
            </Anchor>
          ) : (
            <Text size="sm">{ciLabel(launch.ci)}</Text>
          )}
        </Group>
      )}
      <Text size="sm" c="dimmed">
        Started {formatDateTime(launch.startedAt)} ({formatRelative(launch.startedAt)})
      </Text>
      {typeof launch.durationMs === 'number' && (
        <Text size="sm" c="dimmed">
          took {formatDuration(launch.durationMs)}
        </Text>
      )}
    </Group>
  );
}

function Chips({ launch }: { launch: Launch }) {
  const env = launch.environment;
  if (!env && !launch.attributes.length) return null;
  return (
    <Group gap={6} wrap="wrap">
      {env?.key && (
        <Badge variant="light" color="fhPurple" radius="sm" style={{ textTransform: 'none' }}>
          env: {env.key}
        </Badge>
      )}
      {env &&
        Object.entries(env.values ?? {}).map(([k, v]) => (
          <Badge key={k} variant="outline" color="gray" radius="sm" style={{ textTransform: 'none' }}>
            {k}={v}
          </Badge>
        ))}
      {launch.attributes.map((a) => (
        <Badge key={`${a.key}:${a.value}`} variant="default" radius="sm" style={{ textTransform: 'none' }}>
          {a.key}: {a.value}
        </Badge>
      ))}
    </Group>
  );
}

function Summary({ launch }: { launch: Launch }) {
  const c = launch.counts;
  const order: ResultStatus[] = ['passed', 'failed', 'broken', 'skipped', 'unknown'];
  const data = order.filter((s) => c[s] > 0).map((s) => ({ name: STATUS_LABEL[s], value: c[s], color: statusVar(s) }));
  return (
    <Card withBorder radius="lg" padding="md">
      <Group align="center" gap="lg" wrap="wrap" justify="center">
        <div style={{ position: 'relative', width: 150, height: 150, flex: 'none' }} data-testid="counts-donut">
          {data.length ? (
            <DonutChart data={data} size={150} thickness={18} withTooltip={false} paddingAngle={2} strokeWidth={0} />
          ) : (
            <div style={{ width: 150, height: 150, borderRadius: '50%', border: '18px solid var(--fh-ui-hairline)' }} />
          )}
          <Tooltip label={`Pass rate = passed / (total − skipped) = ${String(c.passed)} / (${String(c.total)} − ${String(c.skipped)})`} multiline maw={260}>
            <Stack gap={0} align="center" justify="center" style={{ position: 'absolute', inset: 30, borderRadius: '50%', cursor: 'help' }} data-testid="pass-rate" tabIndex={0}>
              <Text fw={800} size="xl" lh={1.1}>
                {Math.round(passRate(c) * 100)}%
              </Text>
              <Text size="xs" c="dimmed">
                pass rate
              </Text>
            </Stack>
          </Tooltip>
        </div>
        <Stack gap="sm" style={{ flex: '1 1 260px', minWidth: 0 }}>
          {launch.status === 'running' ? <LaunchProgress launch={launch} /> : <StatusBar counts={c} height={12} />}
          <SimpleGrid cols={{ base: 4, md: 7 }} spacing="xs">
            <Stat label="Total" value={c.total} />
            {order.map((s) => (
              <Stat key={s} label={STATUS_LABEL[s]} value={c[s]} status={s} total={c.total} />
            ))}
            <Stat label="Flaky" value={c.flaky} status="flaky" total={c.total} />
          </SimpleGrid>
          {c.muted > 0 && (
            <Text size="xs" c="dimmed">
              {c.muted} muted result{c.muted === 1 ? '' : 's'} do not count as failures.
            </Text>
          )}
        </Stack>
      </Group>
    </Card>
  );
}

function Stat({ label, value, status, total }: { label: string; value: number; status?: string; total?: number }) {
  return (
    <Stack gap={0} data-testid={`stat-${label.toLowerCase()}`}>
      <Group gap={6} wrap="nowrap">
        {status && <StatusDot status={status} size={8} />}
        <Text size="xs" c="dimmed">
          {label}
        </Text>
      </Group>
      <Text fw={700} size="lg">
        {value}
      </Text>
      {total !== undefined && total > 0 && (
        <Text size="xs" c="dimmed">
          {percent(value, total)}
        </Text>
      )}
    </Stack>
  );
}

function UploadRow({ u }: { u: Upload }) {
  return (
    <Stack gap={4} py={6} style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }} data-testid="upload-row">
      <Group justify="space-between" wrap="wrap" gap="xs">
        <Group gap="xs" wrap="wrap">
          <Badge size="sm" variant="light" color={uploadStatusColor(u.status)} radius="sm" style={{ textTransform: 'none' }}>
            {u.status}
          </Badge>
          <Text size="sm" fw={600}>
            {u.format}
          </Text>
          <Text size="xs" c="dimmed">
            {formatBytes(u.sizeBytes)}
          </Text>
        </Group>
        <Text size="xs" c="dimmed">
          {u.counts.results} results · {u.counts.attachments} attachments{u.counts.ignored ? ` · ${String(u.counts.ignored)} ignored` : ''} · {formatRelative(u.createdAt)}
        </Text>
      </Group>
      {u.error && (
        <Text size="sm" c="var(--mantine-color-error)" data-testid="upload-error">
          <IconAlertTriangle size={14} style={{ verticalAlign: -2 }} /> {u.error.message}{' '}
          <Text span size="xs" c="dimmed" className="fp-mono">
            ({u.error.code}
            {u.error.limit ? `, limit ${u.error.limit}` : ''})
          </Text>
        </Text>
      )}
      {u.warnings.map((w) => (
        <Text key={w} size="xs" c="var(--fh-semantic-warning-fg)">
          {w}
        </Text>
      ))}
    </Stack>
  );
}

function Uploads({ project, launch }: { project: string; launch: Launch }) {
  const q = useUploads(project, launch.id);
  const list = q.data ?? [];
  const failed = list.filter((u) => u.status === 'failed').length;
  return (
    <Accordion variant="contained" radius="lg" defaultValue={failed ? 'uploads' : null}>
      <Accordion.Item value="uploads">
        <Accordion.Control icon={<IconUpload size={16} />}>
          <Group gap="xs">
            <Text fw={600}>Uploads</Text>
            <Text size="sm" c="dimmed">
              {q.isPending ? '…' : String(list.length)}
            </Text>
            {failed > 0 && (
              <Badge size="sm" color="red" variant="light">
                {failed} failed
              </Badge>
            )}
          </Group>
        </Accordion.Control>
        <Accordion.Panel>
          <QueryState query={q}>{() => (list.length ? list.map((u) => <UploadRow key={u.id} u={u} />) : <Text size="sm" c="dimmed">No uploads.</Text>)}</QueryState>
        </Accordion.Panel>
      </Accordion.Item>
    </Accordion>
  );
}

function ResultRow({ project, r, index, selected, onHover, types }: { project: string; r: ResultSummary; index: number; selected: boolean; onHover: (i: number) => void; types: Map<string, DefectType> }) {
  const manualTo = r.source === 'manual' && r.runItemId ? `/p/${project}/launches/${r.launchId}/run/${r.runItemId}` : null;
  return (
    <UnstyledButton
      component={Link}
      to={manualTo ?? `/p/${project}/results/${r.id}`}
      className="fp-row"
      data-selected={selected}
      data-list-index={index}
      data-testid="result-row"
      onMouseEnter={() => onHover(index)}
      style={{ display: 'block', padding: '8px 10px', borderBottom: '1px solid var(--fh-ui-hairline)' }}
    >
      <Group gap="sm" wrap="nowrap" align="flex-start">
        <div style={{ flex: 'none', width: 78 }}>
          <StatusBadge status={r.status} />
        </div>
        <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
          <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
            {r.source === 'manual' && <ManualIcon />}
            <Text size="sm" fw={600} className="fh-truncate">
              {r.name}
            </Text>
            {r.flakyInRun && <StatusBadge status="flaky" size="xs" />}
            {r.muted && (
              <Tooltip label={r.muteUntil ? `Muted until ${formatDateTime(r.muteUntil)}` : 'Muted'}>
                <Badge size="xs" variant="outline" color="gray" style={{ flex: 'none' }}>
                  muted
                </Badge>
              </Tooltip>
            )}
            <TransitionBadge transition={r.transition} />
            {r.attemptCount > 1 && (
              <Tooltip label={`${String(r.attemptCount)} attempts`}>
                <Badge size="xs" variant="light" color="gray">
                  ×{r.attemptCount}
                </Badge>
              </Tooltip>
            )}
          </Group>
          <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
            {r.defect && <DefectChip defect={r.defect} types={types} size="xs" />}
            <Text size="xs" c="dimmed" className="fh-truncate">
              {r.suitePath.join(' › ')}
            </Text>
          </Group>
          {r.message && (r.status === 'failed' || r.status === 'broken') && (
            <Text size="xs" className="fp-mono fh-truncate" c="var(--mantine-color-error)">
              {stripAnsi(r.message).split('\n')[0]}
            </Text>
          )}
        </Stack>
        <Text size="xs" c="dimmed" style={{ flex: 'none' }}>
          {formatDuration(r.durationMs)}
        </Text>
      </Group>
    </UnstyledButton>
  );
}

function Results({ project, launch }: { project: string; launch: Launch }) {
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const statusParam = sp.get('status') ?? '';
  const statuses = statusParam.split(',').filter(Boolean);
  const [text, setText] = useState(sp.get('q') ?? '');
  const [debounced] = useDebouncedValue(text, 300);
  const flaky = sp.get('flaky') === 'true';
  const label = sp.get('label') ?? '';
  const source = sp.get('source') ?? '';
  const filters: ResultFilters = useMemo(
    () => ({ status: statusParam || undefined, q: debounced || undefined, flaky: flaky || undefined, label: label || undefined, source: source || undefined }),
    [statusParam, debounced, flaky, label, source],
  );
  const q = useResults(project, launch.id, filters);
  const types = useDefectTypeMap(project);
  const items = flatItems(q.data);
  const [selected, setSelected] = useState(-1);

  const set = useCallback(
    (k: string, v: string | null) =>
      setSp(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (v) next.set(k, v);
          else next.delete(k);
          return next;
        },
        { replace: true },
      ),
    [setSp],
  );

  useEffect(() => {
    if ((sp.get('q') ?? '') !== debounced) set('q', debounced || null);
  }, [debounced, sp, set]);

  const open = useCallback(
    (i: number) => {
      const r = items[i];
      if (r) void navigate(`/p/${project}/results/${r.id}`);
    },
    [items, navigate, project],
  );
  useListKeys({ count: items.length, index: selected, setIndex: setSelected, onOpen: open });

  const c: Counts = launch.counts;
  return (
    <Stack gap="sm">
      <Group gap="sm" wrap="wrap" align="center">
        <Chip.Group multiple value={statuses} onChange={(v) => set('status', v.join(',') || null)}>
          <Group gap={6} wrap="wrap">
            {RESULT_STATUSES.map((s) => (
              <Chip key={s} value={s} size="xs" variant="outline" data-testid={`chip-${s}`}>
                <Group gap={4} wrap="nowrap" component="span">
                  <StatusDot status={s} size={8} />
                  {STATUS_LABEL[s]} {c[s]}
                </Group>
              </Chip>
            ))}
          </Group>
        </Chip.Group>
        <Switch size="xs" label={`Flaky only (${String(c.flaky)})`} checked={flaky} onChange={(e) => set('flaky', e.currentTarget.checked ? 'true' : null)} />
        {launch.manual && (
          <SegmentedControl
            size="xs"
            value={source || 'all'}
            onChange={(v) => set('source', v === 'manual' || v === 'automated' ? v : null)}
            data={[
              { value: 'all', label: 'All' },
              { value: 'automated', label: 'Automated' },
              { value: 'manual', label: 'Manual' },
            ]}
            aria-label="Result source"
            data-testid="source-filter"
          />
        )}
      </Group>
      <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
        <TextInput aria-label="Search results" placeholder="Search by name or error" leftSection={<IconSearch size={16} />} value={text} onChange={(e) => setText(e.currentTarget.value)} />
        <TextInput aria-label="Label filter" placeholder="Label name:value, e.g. severity:blocker" value={label} onChange={(e) => set('label', e.currentTarget.value || null)} />
      </SimpleGrid>
      <QueryState query={q}>
        {() =>
          items.length === 0 ? (
            <EmptyState title="No results match" description="Change the filters to see more results." />
          ) : (
            <Card withBorder radius="md" padding={0} data-testid="results-list">
              {items.map((r, i) => (
                <ResultRow key={r.id} project={project} r={r} index={i} selected={i === selected} onHover={setSelected} types={types} />
              ))}
              <LoadMore hasMore={q.hasNextPage} loading={q.isFetchingNextPage} onLoad={() => void q.fetchNextPage()} />
            </Card>
          )
        }
      </QueryState>
      <Text size="xs" c="dimmed" visibleFrom="sm">
        {items.length} shown. Tip: j / k move the selection, Enter opens it.
      </Text>
    </Stack>
  );
}

function LaunchLogs({ project, launch }: { project: string; launch: Launch }) {
  const q = useResults(project, launch.id, {});
  const results = useMemo(() => flatItems(q.data).map((r) => ({ id: r.id, name: r.name, clientIds: r.clientIds })), [q.data]);
  return <LogConsole project={project} launchId={launch.id} live={launch.status === 'running'} results={results} fileName={`${launch.name}-${String(launch.number)}-logs.txt`} />;
}

function ImportantToggle({ project, launch }: { project: string; launch: Launch }) {
  const canWrite = useCan('launches:write', project);
  const patch = usePatchLaunch(project, launch.id);
  const important = launch.important;
  if (!canWrite) {
    return important ? (
      <Tooltip label="Kept by retention">
        <Badge size="sm" variant="light" color="yellow" leftSection={<IconStarFilled size={11} />} style={{ textTransform: 'none' }}>
          important
        </Badge>
      </Tooltip>
    ) : null;
  }
  return (
    <Tooltip label={important ? 'Important: retention keeps it. Click to unmark' : 'Mark as important so retention keeps it'}>
      <ActionIcon variant="subtle" color={important ? 'yellow' : 'gray'} onClick={() => patch.mutate({ important: !important })} loading={patch.isPending} aria-label={important ? 'Unmark important' : 'Mark important'} aria-pressed={important} data-testid="important-toggle">
        {important ? <IconStarFilled size={18} /> : <IconStar size={18} />}
      </ActionIcon>
    </Tooltip>
  );
}

export default function LaunchPage() {
  const { key = '', id = '' } = useParams();
  const project = useProject(key);
  const q = useLaunch(key, id);
  const live = useProjectEvents(key);
  const [sp, setSp] = useSearchParams();
  const asked = sp.get('tab');
  const tab = asked === 'tree' || (!asked && !sp.get('label')) ? 'explorer' : (asked ?? 'results');
  const [planOpen, setPlanOpen] = useState(false);
  const canPlan = useCan('launches:write', key);

  useEffect(() => {
    if (q.data) rememberLaunch({ project: key, id: q.data.id, number: q.data.number, name: q.data.name });
  }, [q.data?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const setTab = (t: string | null) =>
    setSp(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (!t || t === 'explorer') next.delete('tab');
        else next.set('tab', t);
        if (t !== 'explorer') {
          next.delete('test');
          next.delete('attempt');
          next.delete('section');
        }
        return next;
      },
      { replace: true },
    );

  return (
    <QueryState query={q} notFound="Launch not found">
      {() => {
        const l = q.data;
        if (!l) return null;
        return (
          <>
            <PageHeader
              crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: `#${String(l.number)}` }]}
              title={`#${String(l.number)} ${l.name}`}
              badges={
                <>
                  <LaunchStatusBadge status={l.status} />
                  {l.status === 'running' && <LiveBadge state={live} />}
                  <GateBadge project={key} launch={l} />
                  <ImportantToggle project={key} launch={l} />
                  {l.debug && (
                    <Badge variant="outline" color="gray" size="sm">
                      debug
                    </Badge>
                  )}
                </>
              }
              subtitle={<Meta launch={l} />}
              actions={
                <>
                  <Button component={Link} to={`/p/${key}/compare?b=${l.id}`} variant="default" size="xs" leftSection={<IconGitCompare size={14} />}>
                    Compare
                  </Button>
                  {canPlan && (
                    <Button variant="default" size="xs" leftSection={<IconListCheck size={14} />} onClick={() => setPlanOpen(true)} data-testid="open-test-plan">
                      Test plan
                    </Button>
                  )}
                </>
              }
            />
            {planOpen && <TestPlanModal project={key} launch={l} opened={planOpen} onClose={() => setPlanOpen(false)} initialStatuses={(sp.get('status') ?? '').split(',').filter((x): x is ResultStatus => (RESULT_STATUSES as readonly string[]).includes(x))} />}
            <Stack gap="md">
              <Chips launch={l} />
              <Summary launch={l} />
              <Uploads project={key} launch={l} />
              <Tabs value={tab} onChange={setTab} keepMounted={false}>
                <Tabs.List>
                  <Tabs.Tab value="explorer" leftSection={<IconListTree size={14} />} data-testid="tab-explorer">
                    Tree
                  </Tabs.Tab>
                  <Tabs.Tab value="results" leftSection={<IconList size={14} />}>
                    Results
                  </Tabs.Tab>
                  <Tabs.Tab value="failures" rightSection={l.counts.failed + l.counts.broken > 0 ? <Badge size="xs" color="red" variant="light">{l.counts.failed + l.counts.broken}</Badge> : undefined}>
                    Failures
                  </Tabs.Tab>
                  <Tabs.Tab value="logs" leftSection={l.status === 'running' ? <span className="fp-live-dot" aria-hidden /> : undefined} data-testid="tab-logs">
                    {l.status === 'running' ? 'Live' : 'Logs'}
                  </Tabs.Tab>
                  {l.manual && (
                    <Tabs.Tab value="manual" leftSection={<IconHandFinger size={14} />} data-testid="tab-manual">
                      Manual
                    </Tabs.Tab>
                  )}
                </Tabs.List>
                <Tabs.Panel value="explorer" pt="md">
                  <Explorer project={key} launch={l} />
                </Tabs.Panel>
                <Tabs.Panel value="results" pt="md">
                  <Results project={key} launch={l} />
                </Tabs.Panel>
                <Tabs.Panel value="failures" pt="md">
                  <Clusters project={key} launch={l} />
                </Tabs.Panel>
                <Tabs.Panel value="logs" pt="md">
                  <LaunchLogs project={key} launch={l} />
                </Tabs.Panel>
                <Tabs.Panel value="manual" pt="md">
                  <Manual project={key} launch={l} />
                </Tabs.Panel>
              </Tabs>
            </Stack>
          </>
        );
      }}
    </QueryState>
  );
}
