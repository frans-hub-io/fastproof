import { Anchor, Autocomplete, Badge, Button, Card, Group, Select, SimpleGrid, Stack, Table, Text, TextInput, Tooltip } from '@mantine/core';
import { useDebouncedValue, useMediaQuery } from '@mantine/hooks';
import { EmptyState } from '@franshub/ui-shell';
import { IconSearch } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link, useParams, useSearchParams } from 'react-router';
import { ERROR_SORT_LABEL, ERROR_STATE_LABEL, ERROR_STATES, useErrors, type ErrorSort, type ErrorState } from '../api/errors';
import { useProject } from '../api/hooks';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { useLaunchList } from '../lib/launches';
import { BranchBadges, ErrorStateBadge, IssueLink, Sparkline } from './errors/parts';

const PAGE = 50;

/** Unique errors (FastProof Analysis): the errors of the project with their state across branches. */
export default function ErrorsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const state = (sp.get('state') ?? '') as ErrorState | '';
  const branch = sp.get('branch') ?? '';
  const sort = (sp.get('sort') ?? 'lastSeen') as ErrorSort;
  const [text, setText] = useState(sp.get('q') ?? '');
  const [q] = useDebouncedValue(text.trim(), 300);
  const search = q.length >= 3 ? q : undefined;
  const [cursors, setCursors] = useState<string[]>([]);
  const before = cursors.at(-1);
  const list = useErrors(key, { state: state || undefined, branch: branch || undefined, q: search, sort, before, limit: PAGE });
  const narrow = useMediaQuery('(max-width: 48em)');
  const { launches } = useLaunchList(key);
  const branches = useMemo(() => [...new Set(launches.map((l) => l.vcs?.branch).filter((b): b is string => !!b))], [launches]);

  const set = (k: string, v: string) => {
    setCursors([]);
    setSp(
      (prev) => {
        const n = new URLSearchParams(prev);
        if (v) n.set(k, v);
        else n.delete(k);
        return n;
      },
      { replace: true },
    );
  };

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Unique errors' }]}
        title="Unique errors"
        subtitle="Errors of this project by fingerprint, with their state across branches"
      />
      <Card withBorder radius="md" padding="sm" mb="md">
        <SimpleGrid cols={{ base: 1, sm: 2, md: 4 }} spacing="xs">
          <TextInput
            aria-label="Search messages"
            placeholder="Search messages (3+ characters)"
            leftSection={<IconSearch size={14} />}
            value={text}
            onChange={(e) => {
              setText(e.currentTarget.value);
              setCursors([]);
            }}
            data-testid="errors-search"
          />
          <Select
            aria-label="State"
            placeholder="All states"
            data={ERROR_STATES.map((s) => ({ value: s, label: ERROR_STATE_LABEL[s] }))}
            value={state || null}
            onChange={(v) => set('state', v ?? '')}
            clearable
            data-testid="errors-state"
          />
          <Autocomplete aria-label="Branch" placeholder="All branches" data={branches} value={branch} onChange={(v) => set('branch', v)} clearable data-testid="errors-branch" />
          <Select
            aria-label="Sort"
            data={(Object.keys(ERROR_SORT_LABEL) as ErrorSort[]).map((s) => ({ value: s, label: ERROR_SORT_LABEL[s] }))}
            value={sort}
            onChange={(v) => set('sort', v && v !== 'lastSeen' ? v : '')}
            allowDeselect={false}
            data-testid="errors-sort"
          />
        </SimpleGrid>
        {text.trim().length > 0 && text.trim().length < 3 && (
          <Text size="xs" c="dimmed" mt={6}>
            Type at least 3 characters to search.
          </Text>
        )}
      </Card>
      <QueryState query={list} notFound="Project not found">
        {() => {
          const items = list.data?.items ?? [];
          if (items.length === 0 && cursors.length === 0)
            return (
              <EmptyState
                title="No unique errors"
                description={state || branch || search ? 'No error matches these filters.' : 'Errors appear here once launches with failures are analysed.'}
              />
            );
          return (
            <Stack gap="sm">
              {narrow ? (
                <Stack gap="xs" data-testid="errors-cards">
                  {items.map((e) => (
                    <Card key={e.fingerprint} withBorder radius="md" padding="sm" data-testid="error-row">
                      <Stack gap={6}>
                        <Group justify="space-between" gap="xs" wrap="nowrap">
                          <ErrorStateBadge state={e.state} />
                          <Sparkline values={e.spark} label={`Occurrences in the last 14 days: ${e.spark.join(', ')}`} />
                        </Group>
                        <Anchor component={Link} to={`/p/${key}/errors/${e.fingerprint}`} size="sm" fw={600} lineClamp={3} style={{ wordBreak: 'break-word' }}>
                          {e.message || '(no message)'}
                        </Anchor>
                        <Text size="xs" c="dimmed" className="fp-mono">
                          {e.fingerprint}
                          {e.aliases > 0 ? ` · +${String(e.aliases)} merged` : ''}
                        </Text>
                        <BranchBadges branches={e.branches} />
                        <Group gap="md" wrap="wrap">
                          <Text size="xs" c="dimmed">
                            last {formatRelative(e.lastSeenAt)} · first {formatRelative(e.firstSeenAt)}
                          </Text>
                          <Text size="xs" c="dimmed">
                            {e.launches} {e.launches === 1 ? 'launch' : 'launches'} · {e.tests} {e.tests === 1 ? 'test' : 'tests'}
                          </Text>
                          {e.issue && <IssueLink issue={e.issue} />}
                        </Group>
                      </Stack>
                    </Card>
                  ))}
                </Stack>
              ) : (
              <Card withBorder radius="md" padding={0}>
                <div className="fp-table-wrap">
                  <Table highlightOnHover verticalSpacing="xs" fz="sm" miw={920} data-testid="errors-table">
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th>Error</Table.Th>
                        <Table.Th w={100}>State</Table.Th>
                        <Table.Th w={150}>Branches</Table.Th>
                        <Table.Th w={86}>First seen</Table.Th>
                        <Table.Th w={86}>Last seen</Table.Th>
                        <Table.Th w={54} ta="right">
                          Tests
                        </Table.Th>
                        <Table.Th w={72} ta="right">
                          Launches
                        </Table.Th>
                        <Table.Th w={116}>Issue</Table.Th>
                        <Table.Th w={84}>14 days</Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {items.map((e) => (
                        <Table.Tr key={e.fingerprint} data-testid="error-row">
                          <Table.Td style={{ maxWidth: 0 }}>
                            <Stack gap={2} style={{ minWidth: 0 }}>
                              <Anchor component={Link} to={`/p/${key}/errors/${e.fingerprint}`} size="sm" fw={600} lineClamp={2} title={e.message} style={{ wordBreak: 'break-word' }}>
                                {e.message || '(no message)'}
                              </Anchor>
                              <Group gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
                                <Text size="xs" c="dimmed" className="fp-mono fh-truncate" style={{ minWidth: 0 }}>
                                  {e.fingerprint}
                                </Text>
                                {e.aliases > 0 && (
                                  <Badge size="xs" variant="light" color="gray">
                                    +{e.aliases} merged
                                  </Badge>
                                )}
                              </Group>
                            </Stack>
                          </Table.Td>
                          <Table.Td>
                            <ErrorStateBadge state={e.state} />
                          </Table.Td>
                          <Table.Td>
                            <BranchBadges branches={e.branches} />
                          </Table.Td>
                          <Table.Td>
                            <Tooltip label={formatDateTime(e.firstSeenAt)}>
                              <Text size="xs">{formatRelative(e.firstSeenAt)}</Text>
                            </Tooltip>
                          </Table.Td>
                          <Table.Td>
                            <Tooltip label={formatDateTime(e.lastSeenAt)}>
                              <Text size="xs">{formatRelative(e.lastSeenAt)}</Text>
                            </Tooltip>
                          </Table.Td>
                          <Table.Td ta="right">{e.tests}</Table.Td>
                          <Table.Td ta="right">{e.launches}</Table.Td>
                          <Table.Td>
                            <IssueLink issue={e.issue} />
                          </Table.Td>
                          <Table.Td>
                            <Sparkline values={e.spark} label={`Occurrences in the last 14 days: ${e.spark.join(', ')}`} />
                          </Table.Td>
                        </Table.Tr>
                      ))}
                    </Table.Tbody>
                  </Table>
                </div>
              </Card>
              )}
              <Group justify="space-between">
                <Text size="xs" c="dimmed">
                  Page {cursors.length + 1}
                  {list.isFetching ? ' · loading…' : ''}
                </Text>
                <Group gap="xs">
                  <Button size="xs" variant="default" disabled={cursors.length === 0} onClick={() => setCursors((c) => c.slice(0, -1))}>
                    Previous
                  </Button>
                  <Button size="xs" variant="default" disabled={!list.data?.next} onClick={() => list.data?.next && setCursors((c) => [...c, list.data.next ?? ''])} data-testid="errors-next">
                    Next
                  </Button>
                </Group>
              </Group>
            </Stack>
          );
        }}
      </QueryState>
    </>
  );
}
