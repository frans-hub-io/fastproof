import { Anchor, Autocomplete, Card, Group, SimpleGrid, Stack, Table, Text, Tooltip } from '@mantine/core';
import { BarChart } from '@mantine/charts';
import { EmptyState } from '@franshub/ui-shell';
import { useMemo } from 'react';
import { Link, useNavigate, useParams, useSearchParams } from 'react-router';
import { useProject, useResult, useTestCaseHistory } from '../api/hooks';
import type { ResultStatus } from '../api/types';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { StatusBadge } from '../components/Status';
import { formatDateTime, formatDuration, formatRelative, percent } from '../lib/format';
import { historyStats } from '../lib/history';
import { STATUS_LABEL, statusVar } from '../lib/status';

const CHART_STATUSES: ResultStatus[] = ['passed', 'failed', 'broken', 'skipped', 'unknown'];

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <Card withBorder radius="md" padding="sm">
      <Text size="xs" c="dimmed">
        {label}
      </Text>
      <Text fw={800} size="xl">
        {value}
      </Text>
      {hint && (
        <Text size="xs" c="dimmed">
          {hint}
        </Text>
      )}
    </Card>
  );
}

export default function TestCasePage() {
  const { key = '', id = '' } = useParams();
  const [sp, setSp] = useSearchParams();
  const navigate = useNavigate();
  const branch = sp.get('branch') ?? '';
  const environment = sp.get('environment') ?? '';
  const project = useProject(key);
  const all = useTestCaseHistory(key, id, { limit: 200 });
  const q = useTestCaseHistory(key, id, { branch: branch || undefined, environment: environment || undefined, limit: 200 });
  const items = useMemo(() => q.data ?? [], [q.data]);
  const latest = useResult(key, all.data?.[0]?.resultId ?? '');
  const name = latest.data?.name ?? 'Test case';

  const branches = useMemo(() => [...new Set((all.data ?? []).map((i) => i.branch).filter((x): x is string => !!x))].sort(), [all.data]);
  const envs = useMemo(() => [...new Set((all.data ?? []).map((i) => i.environment).filter((x): x is string => !!x))].sort(), [all.data]);
  const stats = useMemo(() => historyStats(items), [items]);
  const chronological = useMemo(() => [...items].reverse(), [items]);
  const chart = useMemo(
    () =>
      chronological.map((i) => {
        const row: Record<string, string | number> = { launch: `#${String(i.launchNumber)}` };
        for (const s of CHART_STATUSES) row[s] = s === i.status && typeof i.durationMs === 'number' ? Math.round(i.durationMs) / 1000 : 0;
        return row;
      }),
    [chronological],
  );

  const set = (k: string, v: string) =>
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        if (v) n.set(k, v);
        else n.delete(k);
        return n;
      },
      { replace: true },
    );

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Test case' }]}
        title={name}
        subtitle={
          <Stack gap={2}>
            {latest.data && (
              <Text size="sm" className="fp-mono" style={{ wordBreak: 'break-all' }}>
                {latest.data.fullName}
              </Text>
            )}
            <Text size="xs" c="dimmed" className="fp-mono">
              {id}
            </Text>
          </Stack>
        }
      />
      <Card withBorder radius="md" padding="sm" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
          <Autocomplete aria-label="Branch" placeholder="All branches" data={branches} value={branch} onChange={(v) => set('branch', v)} clearable />
          <Autocomplete aria-label="Environment" placeholder="All environments" data={envs} value={environment} onChange={(v) => set('environment', v)} clearable />
        </SimpleGrid>
      </Card>
      <QueryState query={q} notFound="Test case not found">
        {() =>
          items.length === 0 ? (
            <EmptyState title="No runs match" description="No launch on this branch and environment ran this test." />
          ) : (
            <Stack gap="md">
              <SimpleGrid cols={{ base: 2, md: 4 }} spacing="sm">
                <Stat label="Runs" value={String(stats.runs)} hint={`${String(stats.executed)} executed`} />
                <Stat label="Pass rate" value={percent(stats.passRate, 1)} />
                <Stat label="Status flips" value={percent(stats.flipRate, 1)} hint="share of runs that changed pass/fail" />
                <Stat label="Average duration" value={formatDuration(stats.avg)} hint={stats.lastFailure ? `last failure ${formatRelative(stats.lastFailure.startedAt)}` : 'no failures'} />
              </SimpleGrid>
              <Card withBorder radius="md" padding="sm">
                <Text size="sm" fw={600} mb={8}>
                  Status timeline
                </Text>
                <div className="fp-history" data-testid="status-timeline">
                  {chronological.map((i) => (
                    <Tooltip key={i.resultId} label={`#${String(i.launchNumber)} · ${STATUS_LABEL[i.status]} · ${i.branch ?? ''} · ${formatDateTime(i.startedAt)}`}>
                      <button type="button" className="fp-history-cell" style={{ background: statusVar(i.status) }} aria-label={`Launch #${String(i.launchNumber)}: ${i.status}`} onClick={() => void navigate(`/p/${key}/results/${i.resultId}`)} />
                    </Tooltip>
                  ))}
                </div>
              </Card>
              <Card withBorder radius="md" padding="sm">
                <Text size="sm" fw={600} mb={8}>
                  Duration, seconds
                </Text>
                <BarChart
                  h={220}
                  data={chart}
                  dataKey="launch"
                  type="stacked"
                  series={CHART_STATUSES.map((s) => ({ name: s, label: STATUS_LABEL[s], color: statusVar(s) }))}
                  withLegend={false}
                  tickLine="none"
                  gridAxis="x"
                  valueFormatter={(v) => (v ? `${v.toFixed(2)} s` : '')}
                  data-testid="duration-chart"
                />
              </Card>
              <Card withBorder radius="md" padding={0}>
                <div className="fp-table-wrap">
                  <Table highlightOnHover verticalSpacing="xs" fz="sm" miw={560}>
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th>Launch</Table.Th>
                        <Table.Th>Status</Table.Th>
                        <Table.Th>Branch</Table.Th>
                        <Table.Th>Environment</Table.Th>
                        <Table.Th>Duration</Table.Th>
                        <Table.Th>Started</Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {items.map((i) => (
                        <Table.Tr key={i.resultId} data-testid="history-row">
                          <Table.Td>
                            <Anchor component={Link} to={`/p/${key}/results/${i.resultId}`} size="sm">
                              #{i.launchNumber}
                            </Anchor>
                          </Table.Td>
                          <Table.Td>
                            <StatusBadge status={i.status} />
                          </Table.Td>
                          <Table.Td>{i.branch ?? '—'}</Table.Td>
                          <Table.Td>{i.environment ?? '—'}</Table.Td>
                          <Table.Td>{formatDuration(i.durationMs)}</Table.Td>
                          <Table.Td>
                            <Group gap={4} wrap="nowrap">
                              <Text size="sm">{formatRelative(i.startedAt)}</Text>
                            </Group>
                          </Table.Td>
                        </Table.Tr>
                      ))}
                    </Table.Tbody>
                  </Table>
                </div>
              </Card>
            </Stack>
          )
        }
      </QueryState>
    </>
  );
}
