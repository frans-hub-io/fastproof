import { Alert, Anchor, Badge, Button, Card, Code, Group, SimpleGrid, Stack, Table, Text, TextInput, Title, Tooltip } from '@mantine/core';
import { BarChart } from '@mantine/charts';
import { notifications } from '@mantine/notifications';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { IconArrowMerge, IconArrowsSplit, IconEye, IconEyeOff, IconGitCompare } from '@tabler/icons-react';
import { useState } from 'react';
import { Link, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { formatSpan, isFingerprint, useError, usePatchError, type ErrorDetail, type ErrorPatch } from '../api/errors';
import { useProject } from '../api/hooks';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDate, formatDateTime, formatRelative, isHttpUrl, shortSha } from '../lib/format';
import { BranchBadges, ErrorStateBadge, IssueLink } from './errors/parts';

const GROUP_LABEL: Record<string, string> = {
  productBug: 'product bug',
  automationBug: 'automation bug',
  systemIssue: 'system issue',
  noDefect: 'no defect',
  toInvestigate: 'to investigate',
};

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <Card withBorder radius="md" padding="sm">
      <Text size="xs" c="dimmed">
        {label}
      </Text>
      <Tooltip label={hint} disabled={!hint}>
        <Text size="lg" fw={700} data-testid={`error-stat-${label.toLowerCase().replace(/\s+/g, '-')}`}>
          {value}
        </Text>
      </Tooltip>
    </Card>
  );
}

function Commits({ d }: { d: ErrorDetail }) {
  const c = d.commits;
  if (!c || (!c.first && !c.lastGood)) return null;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="error-commits">
      <Group gap="xs" wrap="wrap">
        <IconGitCompare size={16} />
        <Text size="sm">
          {d.state === 'regressed' ? 'Came back' : 'Appeared'} between <Code>{c.lastGood ? shortSha(c.lastGood) : 'the first launch'}</Code> and <Code>{c.first ? shortSha(c.first) : '?'}</Code>
        </Text>
        {c.compareUrl && isHttpUrl(c.compareUrl) && (
          <Anchor href={c.compareUrl} target="_blank" rel="noopener noreferrer" size="sm" data-testid="error-compare">
            Compare commits
          </Anchor>
        )}
      </Group>
    </Card>
  );
}

function Aliases({ project, d, canEdit, patch, busy }: { project: string; d: ErrorDetail; canEdit: boolean; patch: (p: ErrorPatch, done: string) => void; busy: boolean }) {
  const [target, setTarget] = useState('');
  const valid = isFingerprint(target.trim()) && target.trim() !== d.fingerprint;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="error-aliases">
      <Text size="sm" fw={600} mb={6}>
        Merged fingerprints
      </Text>
      <Text size="xs" c="dimmed" mb="xs">
        Fingerprints that are the same error (for example after a change of the message normalizer) count as one. Merging is undone by splitting.
      </Text>
      {d.aliasList.length === 0 ? (
        <Text size="sm" c="dimmed">
          None.
        </Text>
      ) : (
        <Stack gap={4} mb="xs">
          {d.aliasList.map((a) => (
            <Group key={a} gap="xs" wrap="nowrap">
              <Anchor component={Link} to={`/p/${project}/errors/${a}`} size="sm" className="fp-mono">
                {a}
              </Anchor>
            </Group>
          ))}
        </Stack>
      )}
      {canEdit && !d.aliasOf && (
        <Group gap="xs" align="flex-end" wrap="wrap">
          <TextInput
            label="Merge into another error"
            description="The fingerprint of the error this one is the same as"
            placeholder="16 hex digits"
            value={target}
            onChange={(e) => setTarget(e.currentTarget.value)}
            error={target && !valid ? 'A fingerprint of another error: 16 hex digits' : undefined}
            style={{ flex: '1 1 220px' }}
            data-testid="error-merge-target"
          />
          <Button size="sm" variant="default" leftSection={<IconArrowMerge size={14} />} disabled={!valid || busy} onClick={() => patch({ aliasOf: target.trim() }, 'Merged')} data-testid="error-merge">
            Merge
          </Button>
        </Group>
      )}
    </Card>
  );
}

function Timeline({ d }: { d: ErrorDetail }) {
  const data = d.timeline.map((p) => ({ day: formatDate(p.day).replace(/, \d{4}$/, ''), occurrences: p.occurrences, launches: p.launches }));
  return (
    <Card withBorder radius="md" padding="sm" data-testid="error-timeline">
      <Group justify="space-between" mb={8} wrap="wrap">
        <Text size="sm" fw={600}>
          Occurrences per day
        </Text>
        <Text size="xs" c="dimmed">
          {d.window.backend === 'clickhouse' ? 'ClickHouse' : 'PostgreSQL, last 30 days'}
          {d.window.partial ? ' (partial)' : ''}
        </Text>
      </Group>
      {d.timeline.every((p) => p.occurrences === 0) ? (
        <Text size="sm" c="dimmed">
          No occurrences in this window.
        </Text>
      ) : (
        <BarChart
          h={180}
          data={data}
          dataKey="day"
          series={[{ name: 'occurrences', label: 'Occurrences', color: 'var(--fp-status-failed)' }]}
          withLegend={false}
          tickLine="none"
          gridAxis="y"
          withTooltip
        />
      )}
    </Card>
  );
}

/** Unique error (FastProof Analysis): where and when it occurs, its decisions and history. */
export default function ErrorPage() {
  const { key = '', fingerprint = '' } = useParams();
  const project = useProject(key);
  const q = useError(key, fingerprint);
  const canEdit = useCan('results:triage', key);
  const m = usePatchError(key, fingerprint);
  const patch = (p: ErrorPatch, done: string) =>
    m.mutate(p, {
      onSuccess: () => notifications.show({ color: 'green', message: done }),
      onError: (e) => notifications.show({ color: 'red', title: 'Not changed', message: errorMessage(e) }),
    });
  const d = q.data;
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Unique errors', to: `/p/${key}/errors` }, { label: fingerprint }]}
        title="Unique error"
        subtitle={d ? <span className="fp-mono">{fingerprint}</span> : 'One error: where and when it occurs, its decisions and history'}
        badges={d && <ErrorStateBadge state={d.state} size="md" />}
        actions={
          d &&
          canEdit &&
          !d.aliasOf && (
            <Group gap="xs">
              {d.state === 'ignored' ? (
                <Button size="sm" variant="default" leftSection={<IconEye size={14} />} loading={m.isPending} onClick={() => patch({ state: 'active' }, 'Error activated')} data-testid="error-activate">
                  Activate
                </Button>
              ) : (
                <Tooltip label="Environment noise: left out of new errors and notifications" multiline w={240}>
                  <Button size="sm" variant="default" leftSection={<IconEyeOff size={14} />} loading={m.isPending} onClick={() => patch({ state: 'ignored' }, 'Error ignored')} data-testid="error-ignore">
                    Ignore
                  </Button>
                </Tooltip>
              )}
            </Group>
          )
        }
      />
      <QueryState query={q} notFound="Error not found">
        {() =>
          !d ? null : (
            <Stack gap="md" data-testid="error-detail">
              <Card withBorder radius="md" padding="sm">
                <Text size="sm" className="fp-mono" style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }} data-testid="error-message">
                  {d.message || '(no message)'}
                </Text>
                <Group gap="xs" mt="xs" wrap="wrap">
                  <BranchBadges branches={d.branches} max={6} />
                  <IssueLink issue={d.issue} />
                  {d.group && (
                    <Badge size="sm" variant="light" color="grape" tt="none">
                      decided as {GROUP_LABEL[d.group] ?? d.group}
                    </Badge>
                  )}
                </Group>
              </Card>
              {d.aliasOf && (
                <Alert color="blue" variant="light" title="Merged into another error" data-testid="error-alias-of">
                  <Group gap="xs" wrap="wrap">
                    <Text size="sm">
                      This fingerprint counts as{' '}
                      <Anchor component={Link} to={`/p/${key}/errors/${d.aliasOf}`} className="fp-mono">
                        {d.aliasOf}
                      </Anchor>
                      .
                    </Text>
                    {canEdit && (
                      <Button size="xs" variant="default" leftSection={<IconArrowsSplit size={14} />} loading={m.isPending} onClick={() => patch({ aliasOf: null }, 'Split')} data-testid="error-split">
                        Split
                      </Button>
                    )}
                  </Group>
                </Alert>
              )}
              <SimpleGrid cols={{ base: 2, sm: 3, md: 6 }} spacing="xs">
                <Stat label="First seen" value={formatRelative(d.firstSeenAt)} hint={formatDateTime(d.firstSeenAt)} />
                <Stat label="Last seen" value={formatRelative(d.lastSeenAt)} hint={formatDateTime(d.lastSeenAt)} />
                <Stat label="Occurrences" value={String(d.occurrences)} hint="Failed and broken results with this error" />
                <Stat label="Launches" value={String(d.launches)} hint="Analysed launches it occurred in" />
                <Stat label="Regressions" value={String(d.regressedCount)} hint="Times it came back after it was resolved" />
                <Stat label="Time to resolve" value={formatSpan(d.mttrSeconds)} hint="From its first occurrence (or its last regression) to the first launch without it" />
              </SimpleGrid>
              <Commits d={d} />
              <Timeline d={d} />
              <SimpleGrid cols={{ base: 1, md: 2 }} spacing="md">
                <Card withBorder radius="md" padding={0} data-testid="error-branches">
                  <Text size="sm" fw={600} p="sm" pb={4}>
                    Branches
                  </Text>
                  <div className="fp-table-wrap">
                    <Table verticalSpacing={6} fz="sm" miw={300}>
                      <Table.Thead>
                        <Table.Tr>
                          <Table.Th>Branch</Table.Th>
                          <Table.Th>Last seen</Table.Th>
                          <Table.Th ta="right">Launches</Table.Th>
                          <Table.Th ta="right">
                            <Tooltip label="Finished launches of the branch since it was last seen there">
                              <span>Since</span>
                            </Tooltip>
                          </Table.Th>
                        </Table.Tr>
                      </Table.Thead>
                      <Table.Tbody>
                        {d.branchList.map((b) => (
                          <Table.Tr key={b.branch}>
                            <Table.Td>{b.branch === '*' ? <Text size="sm" c="dimmed">other branches</Text> : b.branch || <Text size="sm" c="dimmed">no branch</Text>}</Table.Td>
                            <Table.Td>
                              {b.lastLaunchId ? (
                                <Anchor component={Link} to={`/p/${key}/launches/${b.lastLaunchId}`} size="sm">
                                  {formatRelative(b.lastSeenAt)}
                                </Anchor>
                              ) : (
                                formatRelative(b.lastSeenAt)
                              )}
                            </Table.Td>
                            <Table.Td ta="right">{b.occurrences}</Table.Td>
                            <Table.Td ta="right">{b.launchesSince}</Table.Td>
                          </Table.Tr>
                        ))}
                      </Table.Tbody>
                    </Table>
                  </div>
                </Card>
                <Card withBorder radius="md" padding={0} data-testid="error-tests">
                  <Text size="sm" fw={600} p="sm" pb={4}>
                    Tests, last 30 days
                  </Text>
                  {d.testList.length === 0 ? (
                    <Text size="sm" c="dimmed" px="sm" pb="sm">
                      No occurrences in the last 30 days.
                    </Text>
                  ) : (
                    <div className="fp-table-wrap">
                      <Table verticalSpacing={6} fz="sm">
                        <Table.Tbody>
                          {d.testList.map((t) => (
                            <Table.Tr key={t.testCaseId}>
                              <Table.Td style={{ maxWidth: 0 }}>
                                <Anchor component={Link} to={`/p/${key}/test-cases/${encodeURIComponent(t.testCaseId)}`} size="sm" className="fh-truncate">
                                  {t.name}
                                </Anchor>
                              </Table.Td>
                              <Table.Td ta="right" w={60}>
                                {t.occurrences}
                              </Table.Td>
                            </Table.Tr>
                          ))}
                        </Table.Tbody>
                      </Table>
                    </div>
                  )}
                </Card>
              </SimpleGrid>
              <Card withBorder radius="md" padding="sm" data-testid="error-decisions">
                <Title order={4} size="sm" mb={6}>
                  Decisions
                </Title>
                {d.decisions.length === 0 ? (
                  <EmptyState title="No decisions yet" description="When people triage this error, their decisions are kept here and suggested for the next occurrences." />
                ) : (
                  <Stack gap={6}>
                    {d.decisions.map((m) => (
                      <Group key={`${m.fingerprint}-${m.defectTypeId}`} gap="xs" wrap="wrap" justify="space-between">
                        <Group gap="xs" wrap="wrap">
                          <Badge variant="light" color="grape">
                            {m.defectName || m.group}
                          </Badge>
                          <Text size="sm">
                            confirmed {m.confirmed}×{m.overridden > 0 ? `, overridden ${String(m.overridden)}×` : ''}
                          </Text>
                          {m.issueKey && <Text size="sm">{m.issueKey}</Text>}
                        </Group>
                        <Text size="xs" c="dimmed">
                          by {m.lastDecidedBy}, {formatRelative(m.lastDecidedAt)}
                        </Text>
                      </Group>
                    ))}
                  </Stack>
                )}
              </Card>
              <Aliases project={key} d={d} canEdit={canEdit} patch={patch} busy={m.isPending} />
            </Stack>
          )
        }
      </QueryState>
    </>
  );
}
