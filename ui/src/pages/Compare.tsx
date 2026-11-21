import { ActionIcon, Anchor, Badge, Card, Group, SimpleGrid, Stack, Table, Tabs, Text, Tooltip, UnstyledButton } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { IconArrowsLeftRight, IconTrendingDown, IconTrendingUp } from '@tabler/icons-react';
import { useEffect, useMemo, useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { useProject } from '../api/hooks';
import { useCompare, useDefectTypeMap } from '../api/stageC';
import type { ResultSummary } from '../api/types';
import type { DurationChange, LaunchComparison } from '../api/types-next';
import { DefectChip } from '../components/Defect';
import { LaunchSelect } from '../components/LaunchSelect';
import { PageHeader } from '../components/PageHeader';
import { Loading, QueryState } from '../components/QueryState';
import { StatusBadge } from '../components/Status';
import { formatDuration } from '../lib/format';
import { stripAnsi } from '../lib/ansi';
import { useLaunchList } from '../lib/launches';

type Section = Exclude<keyof LaunchComparison, 'durationChanges' | 'truncated'>;

const SECTIONS: { key: Section | 'durationChanges'; label: string; color: string; empty: string }[] = [
  { key: 'newFailures', label: 'New failures', color: 'red', empty: 'Nothing that passed in A fails in B.' },
  { key: 'fixed', label: 'Fixed', color: 'green', empty: 'Nothing that failed in A passes in B.' },
  { key: 'stillFailing', label: 'Still failing', color: 'orange', empty: 'No test fails in both launches.' },
  { key: 'newTests', label: 'New tests', color: 'blue', empty: 'B has no tests that A did not run.' },
  { key: 'missing', label: 'Missing', color: 'grape', empty: 'B ran every test that A ran.' },
  { key: 'durationChanges', label: 'Duration changes', color: 'violet', empty: 'No test got much faster or slower.' },
];

function ResultRows({ project, items }: { project: string; items: ResultSummary[] }) {
  const types = useDefectTypeMap(project);
  return (
    <Stack gap={0}>
      {items.map((r) => (
        <UnstyledButton
          key={r.id}
          component={Link}
          to={`/p/${project}/results/${r.id}`}
          className="fp-row"
          data-testid="compare-row"
          style={{ display: 'block', padding: '8px 10px', borderBottom: '1px solid var(--fh-ui-hairline)' }}
        >
          <Group gap="sm" wrap="nowrap" align="flex-start">
            <div style={{ flex: 'none', width: 78 }}>
              <StatusBadge status={r.status} />
            </div>
            <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
              <Group gap={6} wrap="wrap" style={{ minWidth: 0 }}>
                <Text size="sm" fw={600} className="fh-truncate" style={{ minWidth: 0, maxWidth: '100%' }}>
                  {r.name}
                </Text>
                {r.defect && (r.status === 'failed' || r.status === 'broken') && <DefectChip defect={r.defect} types={types} size="xs" />}
              </Group>
              <Text size="xs" c="dimmed" className="fh-truncate">
                {r.suitePath.join(' › ')}
              </Text>
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
      ))}
    </Stack>
  );
}

function DurationRows({ project, items }: { project: string; items: DurationChange[] }) {
  return (
    <div className="fp-table-wrap">
      <Table verticalSpacing="xs" fz="sm" miw={520} highlightOnHover>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Test</Table.Th>
            <Table.Th w={120} ta="right">
              Change
            </Table.Th>
            <Table.Th w={90} ta="right">
              Ratio
            </Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {items.map((d) => {
            const slower = d.deltaMs > 0;
            const color = slower ? 'var(--fp-status-failed)' : 'var(--fp-status-passed)';
            return (
              <Table.Tr key={`${d.resultIdA}:${d.resultIdB}`} data-testid="duration-row">
                <Table.Td style={{ maxWidth: 0 }}>
                  <Anchor component={Link} to={`/p/${project}/results/${d.resultIdB}`} size="sm" className="fh-truncate" style={{ display: 'block' }}>
                    {d.name}
                  </Anchor>
                </Table.Td>
                <Table.Td ta="right">
                  <Group gap={4} justify="flex-end" wrap="nowrap" style={{ color }}>
                    {slower ? <IconTrendingUp size={14} aria-label="slower" /> : <IconTrendingDown size={14} aria-label="faster" />}
                    <Text size="sm" span>
                      {slower ? '+' : '−'}
                      {formatDuration(Math.abs(d.deltaMs))}
                    </Text>
                  </Group>
                </Table.Td>
                <Table.Td ta="right">
                  <Text size="sm" c="dimmed">
                    ×{d.ratio.toFixed(2)}
                  </Text>
                </Table.Td>
              </Table.Tr>
            );
          })}
        </Table.Tbody>
      </Table>
    </div>
  );
}

function Result({ project, data }: { project: string; data: LaunchComparison }) {
  const first = SECTIONS.find((s) => data[s.key].length > 0)?.key ?? 'newFailures';
  const [tab, setTab] = useState<string>(first);
  return (
    <Stack gap="md">
      <Tabs value={tab} onChange={(v) => setTab(v ?? first)} keepMounted={false} variant="pills" radius="md">
        <Tabs.List data-testid="compare-summary">
          {SECTIONS.map((s) => (
            <Tabs.Tab
              key={s.key}
              value={s.key}
              data-testid={`compare-count-${s.key}`}
              rightSection={
                <Badge size="sm" variant={tab === s.key ? 'white' : 'light'} color={s.color} radius="sm">
                  {data[s.key].length}
                </Badge>
              }
            >
              {s.label}
            </Tabs.Tab>
          ))}
        </Tabs.List>
        {SECTIONS.map((s) => (
          <Tabs.Panel key={s.key} value={s.key} pt="md">
            {data[s.key].length === 0 ? (
              <Text size="sm" c="dimmed">
                {s.empty}
              </Text>
            ) : (
              <Card withBorder radius="md" padding={0} data-testid={`compare-section-${s.key}`}>
                {s.key === 'durationChanges' ? <DurationRows project={project} items={data.durationChanges} /> : <ResultRows project={project} items={data[s.key]} />}
              </Card>
            )}
          </Tabs.Panel>
        ))}
      </Tabs>
      {data.truncated && (
        <Text size="xs" c="dimmed" data-testid="compare-truncated">
          Some lists were cut at their limit.
        </Text>
      )}
    </Stack>
  );
}

export default function ComparePage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const { launches, pending } = useLaunchList(key);
  const a = sp.get('a') ?? '';
  const b = sp.get('b') ?? '';

  useEffect(() => {
    if ((a && b) || pending || launches.length < 2) return;
    let nextA = a;
    let nextB = b;
    if (!nextB) {
      const finished = launches.filter((l) => l.status !== 'running' && l.id !== nextA);
      nextB = finished[0]?.id ?? '';
    }
    if (!nextA) {
      const i = launches.findIndex((l) => l.id === nextB);
      nextA = launches.slice(i + 1).find((l) => l.status !== 'running')?.id ?? '';
    }
    if (!nextA || !nextB) return;
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        n.set('a', nextA);
        n.set('b', nextB);
        return n;
      },
      { replace: true },
    );
  }, [a, b, launches, pending, setSp]);

  const q = useCompare(key, a, b);
  const byId = useMemo(() => new Map(launches.map((l) => [l.id, l] as const)), [launches]);
  const la = byId.get(a);
  const lb = byId.get(b);

  const set = (k: 'a' | 'b', v: string | null) =>
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        if (v) n.set(k, v);
        else n.delete(k);
        return n;
      },
      { replace: true },
    );
  const swap = () =>
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        n.set('a', b);
        n.set('b', a);
        return n;
      },
      { replace: true },
    );

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Compare launches' }]}
        title="Compare launches"
        subtitle={la && lb ? `#${String(la.number)} (A) → #${String(lb.number)} (B): what changed in B` : 'Pick a base launch A and a launch B to compare with it'}
      />
      <Card withBorder radius="md" padding="sm" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs" style={{ position: 'relative' }}>
          <LaunchSelect project={key} label="Base (A)" value={a || null} onChange={(v) => set('a', v)} exclude={b} placeholder="Pick a launch" data-testid="compare-a" />
          <LaunchSelect project={key} label="Compared (B)" value={b || null} onChange={(v) => set('b', v)} exclude={a} placeholder="Pick a launch" data-testid="compare-b" />
        </SimpleGrid>
        <Group justify="space-between" mt="xs" wrap="wrap" gap="xs">
          <Group gap="md">
            {la && (
              <Anchor component={Link} to={`/p/${key}/launches/${la.id}`} size="xs">
                Open A
              </Anchor>
            )}
            {lb && (
              <Anchor component={Link} to={`/p/${key}/launches/${lb.id}`} size="xs">
                Open B
              </Anchor>
            )}
          </Group>
          <Tooltip label="Swap A and B">
            <ActionIcon variant="light" onClick={swap} disabled={!a || !b} aria-label="Swap A and B">
              <IconArrowsLeftRight size={16} />
            </ActionIcon>
          </Tooltip>
        </Group>
      </Card>
      {!a || !b ? (
        pending ? (
          <Loading />
        ) : (
          <EmptyState title="Pick two launches" description="Choose the base launch A and the launch B to see new failures, fixes and duration changes." />
        )
      ) : a === b ? (
        <EmptyState title="Pick two different launches" />
      ) : (
        <QueryState query={q} notFound="Launch not found">
          {() => (q.data ? <Result key={`${a}:${b}`} project={key} data={q.data} /> : null)}
        </QueryState>
      )}
    </>
  );
}
