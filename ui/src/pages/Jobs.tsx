import { Badge, Card, Group, Stack, Table, Text, Tooltip } from '@mantine/core';
import { EmptyState, useCan, useMe } from '@franshub/ui-shell';
import { IconLock } from '@tabler/icons-react';
import { useJobs } from '../api/stageC';
import type { ScheduledJob } from '../api/types';
import { PageHeader } from '../components/PageHeader';
import { Loading, QueryState } from '../components/QueryState';
import { formatDateTime, formatDuration, formatRelative } from '../lib/format';

function StatusCell({ job }: { job: ScheduledJob }) {
  const s = job.lastStatus;
  if (!s) return <Badge variant="outline" color="gray" radius="sm" style={{ textTransform: 'none' }}>never ran</Badge>;
  const ok = s === 'ok' || s === 'success' || s === 'succeeded' || s === 'done';
  return (
    <Badge variant="light" color={ok ? 'green' : 'red'} radius="sm" style={{ textTransform: 'none' }} data-testid="job-status">
      {s}
    </Badge>
  );
}

function leased(job: ScheduledJob): boolean {
  return !!job.leaseOwner && !!job.leaseUntil && Date.parse(job.leaseUntil) > Date.now();
}

function JobsTable({ jobs }: { jobs: ScheduledJob[] }) {
  return (
    <Card withBorder radius="md" padding={0}>
      <div className="fp-table-wrap">
        <Table highlightOnHover verticalSpacing="xs" fz="sm" miw={860} data-testid="jobs-table">
          <Table.Thead>
            <Table.Tr>
              <Table.Th>Job</Table.Th>
              <Table.Th>Last run</Table.Th>
              <Table.Th>Next run</Table.Th>
              <Table.Th ta="right">Runs / failures</Table.Th>
              <Table.Th>Last error</Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {jobs.map((j) => (
              <Table.Tr key={j.name} data-testid="job-row">
                <Table.Td>
                  <Stack gap={2}>
                    <Text size="sm" fw={600} className="fp-mono">
                      {j.name}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {j.schedule}
                    </Text>
                  </Stack>
                </Table.Td>
                <Table.Td>
                  <Stack gap={4}>
                    <Group gap={6} wrap="nowrap">
                      <StatusCell job={j} />
                      {leased(j) && (
                        <Tooltip label={`Lease held until ${formatDateTime(j.leaseUntil)}`}>
                          <Badge variant="dot" color="blue" radius="sm" style={{ textTransform: 'none' }} data-testid="job-running">
                            running
                          </Badge>
                        </Tooltip>
                      )}
                    </Group>
                    <Tooltip label={formatDateTime(j.lastStartedAt)} disabled={!j.lastStartedAt}>
                      <Text size="xs" c="dimmed">
                        {formatRelative(j.lastStartedAt)}
                        {typeof j.lastDurationMs === 'number' ? ` · took ${formatDuration(j.lastDurationMs)}` : ''}
                      </Text>
                    </Tooltip>
                    {leased(j) && (
                      <Tooltip label={`Lease held by ${j.leaseOwner ?? ''} until ${formatDateTime(j.leaseUntil)}`}>
                        <Text size="xs" c="dimmed" className="fh-truncate" style={{ maxWidth: 240 }}>
                          on {j.leaseOwner}
                        </Text>
                      </Tooltip>
                    )}
                  </Stack>
                </Table.Td>
                <Table.Td>
                  <Tooltip label={formatDateTime(j.nextRunAt)}>
                    <Text size="sm">{formatRelative(j.nextRunAt)}</Text>
                  </Tooltip>
                </Table.Td>
                <Table.Td ta="right">
                  <Text size="sm">
                    {j.runCount.toLocaleString()}
                    <Text span size="sm" c={j.failureCount > 0 ? 'var(--fp-status-failed)' : 'dimmed'}>
                      {' '}
                      / {j.failureCount}
                    </Text>
                  </Text>
                </Table.Td>
                <Table.Td style={{ maxWidth: 280 }}>
                  {j.lastError ? (
                    <Tooltip label={j.lastError} multiline maw={420}>
                      <Text size="xs" className="fp-mono fh-truncate" c="var(--mantine-color-error)">
                        {j.lastError}
                      </Text>
                    </Tooltip>
                  ) : (
                    <Text size="xs" c="dimmed">
                      —
                    </Text>
                  )}
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </div>
    </Card>
  );
}

export default function JobsPage() {
  const { loading } = useMe();
  const allowed = useCan('settings:write');
  const q = useJobs(allowed);
  return (
    <>
      <PageHeader crumbs={[{ label: 'Administration' }, { label: 'Scheduler jobs' }]} title="Scheduler jobs" subtitle="Periodic jobs of the scheduler: rollups, retention, cleanup. Refreshes every 30 seconds." />
      {loading ? (
        <Loading />
      ) : !allowed ? (
        <EmptyState icon={<IconLock size={28} />} title="Access denied" description="Scheduler jobs need the global settings:write permission." />
      ) : (
        <QueryState query={q}>{() => (q.data?.length ? <JobsTable jobs={q.data} /> : <EmptyState title="No scheduled jobs" />)}</QueryState>
      )}
    </>
  );
}
