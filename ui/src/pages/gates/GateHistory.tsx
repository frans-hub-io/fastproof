import { Anchor, Badge, Card, Group, SegmentedControl, Select, SimpleGrid, Stack, Table, Text, TextInput } from '@mantine/core';
import { EmptyState } from '@franshub/ui-shell';
import { useState } from 'react';
import { Link } from 'react-router';
import { flatItems } from '../../api/hooks';
import { useGateHistory, type GateHistoryGate, type GateHistoryItem } from '../../api/gatesHistory';
import { useSettings } from '../../api/stageC';
import { LoadMore } from '../../components/LoadMore';
import { QueryState } from '../../components/QueryState';
import { formatDateTime, formatDuration } from '../../lib/format';
import { actualText, metricOf } from '../../lib/gates';

const DAYS = [
  { value: '7', label: '7 days' },
  { value: '30', label: '30 days' },
  { value: '90', label: '90 days' },
  { value: '365', label: '1 year' },
];

function GateSummary({ g }: { g: GateHistoryGate }) {
  const rate = g.evaluations ? Math.round((g.failed * 1000) / g.evaluations) / 10 : 0;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="gate-history-gate">
      <Stack gap={4}>
        <Group justify="space-between" wrap="nowrap" gap="xs">
          <Text fw={600} truncate>
            {g.gate}
          </Text>
          <Badge variant="light" color={g.failed ? 'red' : 'green'} radius="sm" style={{ textTransform: 'none' }}>
            {String(rate)}% failed
          </Badge>
        </Group>
        <Text size="sm" c="dimmed">
          {String(g.evaluations)} evaluations, {String(g.failed)} failed, {String(g.flips)} flips between passed and failed
        </Text>
        <Text size="sm">
          Back to green in {g.meanSecondsToGreen === null || g.meanSecondsToGreen === undefined ? '—' : formatDuration(g.meanSecondsToGreen * 1000)} on average
        </Text>
        {g.topFailingMetric && (
          <Text size="sm" c="dimmed">
            Fails most on {metricOf(g.topFailingMetric).label.toLowerCase()}
          </Text>
        )}
      </Stack>
    </Card>
  );
}

function Row({ project, item }: { project: string; item: GateHistoryItem }) {
  return (
    <Table.Tr data-testid="gate-history-row">
      <Table.Td>
        <Anchor component={Link} to={`/p/${project}/launches/${item.launchId}`} size="sm">
          #{item.launchNumber}
        </Anchor>
      </Table.Td>
      <Table.Td className="fp-mono">{item.branch ?? '—'}</Table.Td>
      <Table.Td>{formatDateTime(item.evaluatedAt)}</Table.Td>
      <Table.Td>
        <Badge variant="light" color={item.status === 'passed' ? 'green' : item.status === 'failed' ? 'red' : 'yellow'} radius="sm" style={{ textTransform: 'none' }}>
          {item.status}
        </Badge>
      </Table.Td>
      <Table.Td>
        {item.failed.length === 0 ? (
          <Text size="sm" c="dimmed">
            —
          </Text>
        ) : (
          <Stack gap={2}>
            {item.failed.map((f, i) => (
              <Text key={i} size="xs">
                {f.gate}: {metricOf(f.metric).label}{' '}
                <span className="fp-mono">
                  {actualText(f.metric, f.actual)} (rule {String(f.expected)})
                </span>
              </Text>
            ))}
          </Stack>
        )}
      </Table.Td>
    </Table.Tr>
  );
}

/** Gate evaluations over time: aggregates per gate and branch, and every stored verdict. */
export function GateHistory({ project }: { project: string }) {
  const [gate, setGate] = useState<string | null>(null);
  const [branch, setBranch] = useState('');
  const [days, setDays] = useState('30');
  const settings = useSettings(project);
  const q = useGateHistory(project, { gate: gate ?? undefined, branch: branch.trim() || undefined, days: Number(days) });
  const first = q.data?.pages[0];
  const items = flatItems(q.data);
  return (
    <Stack gap="md" data-testid="gate-history">
      <Group gap="sm" align="flex-end" wrap="wrap">
        <Select label="Gate" placeholder="All gates" clearable data={(settings.data?.gates ?? []).map((g) => g.name)} value={gate} onChange={setGate} comboboxProps={{ withinPortal: true }} data-testid="gate-history-gate-filter" />
        <TextInput label="Branch" placeholder="main" value={branch} onChange={(e) => setBranch(e.currentTarget.value)} maxLength={256} data-testid="gate-history-branch-filter" />
        <SegmentedControl value={days} onChange={setDays} data={DAYS} aria-label="Window" />
      </Group>
      <QueryState query={q}>
        {() => {
          if (!first || items.length === 0) {
            return <EmptyState title="No gate evaluations" description="Nothing was evaluated in this window; finished launches are checked against the gates of the project." />;
          }
          return (
            <Stack gap="md">
              <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }} spacing="md">
                {first.gates.map((g) => (
                  <GateSummary key={g.gate} g={g} />
                ))}
              </SimpleGrid>
              {first.branches.length > 0 && (
                <Group gap="xs" wrap="wrap" data-testid="gate-history-branches">
                  {first.branches.slice(0, 12).map((b) => (
                    <Badge key={b.branch} variant="default" radius="sm" style={{ textTransform: 'none' }}>
                      {b.branch}: {String(b.failed)} of {String(b.evaluations)} failed
                    </Badge>
                  ))}
                </Group>
              )}
              <div className="fp-table-wrap">
                <Table verticalSpacing="xs" highlightOnHover>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Launch</Table.Th>
                      <Table.Th>Branch</Table.Th>
                      <Table.Th>Evaluated</Table.Th>
                      <Table.Th>Verdict</Table.Th>
                      <Table.Th>Failed rules</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {items.map((i) => (
                      <Row key={i.launchId} project={project} item={i} />
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
              <LoadMore hasMore={q.hasNextPage} loading={q.isFetchingNextPage} onLoad={() => void q.fetchNextPage()} />
            </Stack>
          );
        }}
      </QueryState>
    </Stack>
  );
}
