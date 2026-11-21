import { Anchor, Autocomplete, Card, Group, Progress, SimpleGrid, Stack, Table, Text, Tooltip } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useMemo } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { useProject } from '../api/hooks';
import { useFlaky } from '../api/stageC';
import type { FlakyTest } from '../api/types-next';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { useLaunchList } from '../lib/launches';
import { STATUS_LABEL, statusVar } from '../lib/status';

function Strip({ statuses }: { statuses: FlakyTest['lastStatuses'] }) {
  const oldestFirst = [...statuses].reverse();
  return (
    <div className="fp-history" data-testid="flaky-strip" aria-label={`Last runs, oldest first: ${oldestFirst.join(', ')}`}>
      {oldestFirst.map((s, i) => (
        <Tooltip key={i} label={STATUS_LABEL[s]}>
          <span className="fp-history-cell" style={{ background: statusVar(s), width: 10, height: 16, cursor: 'default', display: 'inline-block' }} />
        </Tooltip>
      ))}
    </div>
  );
}

export default function FlakyPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const branch = sp.get('branch') ?? '';
  const environment = sp.get('environment') ?? '';
  const q = useFlaky(key, { branch: branch || undefined, environment: environment || undefined, limit: 100 });
  const { launches } = useLaunchList(key);
  const branches = useMemo(() => [...new Set(launches.map((l) => l.vcs?.branch).filter((b): b is string => !!b))], [launches]);
  const envs = useMemo(() => [...new Set(launches.map((l) => l.environment?.key).filter((e): e is string => !!e))], [launches]);
  const items = useMemo(() => [...(q.data ?? [])].sort((a, b) => b.score - a.score), [q.data]);

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
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Flaky tests' }]}
        title="Flaky tests"
        subtitle="Tests whose outcome changes between runs without a code change, by flakiness score"
      />
      <Card withBorder radius="md" padding="sm" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="xs">
          <Autocomplete aria-label="Branch" placeholder="All branches" data={branches} value={branch} onChange={(v) => set('branch', v)} clearable />
          <Autocomplete aria-label="Environment" placeholder="All environments" data={envs} value={environment} onChange={(v) => set('environment', v)} clearable />
        </SimpleGrid>
      </Card>
      <QueryState query={q} notFound="Project not found">
        {() =>
          items.length === 0 ? (
            <EmptyState title="No flaky tests" description="No test changed its outcome often enough on this branch and environment." />
          ) : (
            <Card withBorder radius="md" padding={0}>
              <div className="fp-table-wrap">
                <Table highlightOnHover verticalSpacing="xs" fz="sm" miw={680} data-testid="flaky-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Test</Table.Th>
                      <Table.Th w={170}>Score</Table.Th>
                      <Table.Th w={190} style={{ whiteSpace: 'nowrap' }}>Last runs, oldest → newest</Table.Th>
                      <Table.Th w={120}>Last flaky</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {items.map((f) => (
                      <Table.Tr key={f.seriesKey} data-testid="flaky-row">
                        <Table.Td style={{ maxWidth: 0 }}>
                          <Stack gap={0} style={{ minWidth: 0 }}>
                            <Anchor component={Link} to={`/p/${key}/test-cases/${encodeURIComponent(f.testCaseId)}`} size="sm" fw={600} className="fh-truncate">
                              {f.name}
                            </Anchor>
                          </Stack>
                        </Table.Td>
                        <Table.Td>
                          <Group gap="xs" wrap="nowrap">
                            <Progress value={f.score * 100} color="var(--fp-status-flaky)" size="sm" radius="xl" style={{ flex: 1 }} aria-label={`Flakiness ${String(Math.round(f.score * 100))}%`} />
                            <Text size="xs" w={36} ta="right">
                              {Math.round(f.score * 100)}%
                            </Text>
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Strip statuses={f.lastStatuses} />
                        </Table.Td>
                        <Table.Td>
                          <Tooltip label={formatDateTime(f.lastFlakyAt)} disabled={!f.lastFlakyAt}>
                            <Text size="xs" c="dimmed">
                              {formatRelative(f.lastFlakyAt)}
                            </Text>
                          </Tooltip>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            </Card>
          )
        }
      </QueryState>
    </>
  );
}
