import { ActionIcon, Alert, Anchor, Button, Card, Group, Menu, SegmentedControl, Stack, Table, Text, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconBellRinging, IconDots, IconExternalLink, IconRefresh, IconUnlink } from '@tabler/icons-react';
import { useSearchParams, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import type { TrackerIssue, TrackerIssueFilter } from '../api/trackers';
import { TRACKER_LABEL, useDismissTrackerIssue, useSyncTrackerIssue, useTrackerIssues, useUnmuteTrackerIssue, useUntrackIssue } from '../api/trackers';
import { IssueStateBadge } from '../components/IssueState';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';

type View = 'all' | 'open' | 'closed' | 'suggested';

const FILTER: Record<View, TrackerIssueFilter> = {
  all: {},
  open: { state: 'open' },
  closed: { state: 'closed' },
  suggested: { unmuteSuggested: true },
};

function Suggestion({ project, issue, canUnmute }: { project: string; issue: TrackerIssue; canUnmute: boolean }) {
  const unmute = useUnmuteTrackerIssue(project);
  const dismiss = useDismissTrackerIssue(project);
  return (
    <Alert color="teal" variant="light" icon={<IconBellRinging size={18} />} title={`${issue.key ?? ''} ${issue.title}`.trim()} data-testid="unmute-suggestion">
      <Stack gap="xs">
        <Text size="sm">
          {TRACKER_LABEL[issue.trackerType] ?? issue.trackerType} closed this issue{issue.closedAt ? ` ${formatRelative(issue.closedAt)}` : ''}, and tests linked to it are still muted. Unmute them so
          their failures count again?
        </Text>
        <Group gap="xs">
          {canUnmute ? (
            <Button
              size="xs"
              loading={unmute.isPending}
              onClick={() =>
                unmute.mutate(issue.id, {
                  onSuccess: (r) =>
                    notifications.show({
                      color: 'green',
                      message: `${String(r.tests)} test${r.tests === 1 ? '' : 's'} unmuted`,
                    }),
                  onError: (e) =>
                    notifications.show({
                      color: 'red',
                      title: 'Not unmuted',
                      message: errorMessage(e),
                    }),
                })
              }
              data-testid="unmute-linked"
            >
              Unmute linked tests
            </Button>
          ) : (
            <Text size="xs" c="dimmed">
              Unmuting needs the results:mute permission.
            </Text>
          )}
          <Button size="xs" variant="default" loading={dismiss.isPending} onClick={() => dismiss.mutate(issue.id)} data-testid="keep-muted">
            Keep muted
          </Button>
          {issue.url && (
            <Anchor href={issue.url} target="_blank" rel="noopener noreferrer nofollow" size="xs">
              Open the issue <IconExternalLink size={11} />
            </Anchor>
          )}
        </Group>
      </Stack>
    </Alert>
  );
}

function Row({ project, issue, canTriage, canIssues }: { project: string; issue: TrackerIssue; canTriage: boolean; canIssues: boolean }) {
  const sync = useSyncTrackerIssue(project);
  const untrack = useUntrackIssue(project);
  return (
    <Table.Tr data-testid="issue-row">
      <Table.Td style={{ maxWidth: 0 }}>
        <Stack gap={0} style={{ minWidth: 0 }}>
          {issue.url ? (
            <Anchor href={issue.url} target="_blank" rel="noopener noreferrer nofollow" size="sm" fw={600} className="fh-truncate">
              <span className="fp-mono">{issue.key}</span> {issue.title}
            </Anchor>
          ) : (
            <Text size="sm" fw={600} className="fh-truncate">
              {issue.title || 'Creating…'}
            </Text>
          )}
          <Text size="xs" c="dimmed" className="fh-truncate">
            {TRACKER_LABEL[issue.trackerType] ?? issue.trackerType} · {issue.trackerId} · by {issue.createdBy}, {formatRelative(issue.createdAt)}
          </Text>
        </Stack>
      </Table.Td>
      <Table.Td>
        <IssueStateBadge issue={issue} />
      </Table.Td>
      <Table.Td visibleFrom="sm">
        <Tooltip label={issue.syncedAt ? formatDateTime(issue.syncedAt) : 'Not read yet'}>
          <Text size="xs" c={issue.syncError ? 'orange' : 'dimmed'}>
            {issue.syncError ? 'read failed' : issue.syncedAt ? formatRelative(issue.syncedAt) : '–'}
          </Text>
        </Tooltip>
      </Table.Td>
      <Table.Td>
        {canTriage && (
          <Menu position="bottom-end" withinPortal>
            <Menu.Target>
              <ActionIcon variant="subtle" aria-label={`Actions for ${issue.key ?? issue.title}`} data-testid="issue-actions">
                <IconDots size={16} />
              </ActionIcon>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item
                leftSection={<IconRefresh size={14} />}
                disabled={issue.category === 'creating'}
                onClick={() =>
                  sync.mutate(issue.id, {
                    onError: (e) =>
                      notifications.show({
                        color: 'red',
                        title: 'Not read',
                        message: errorMessage(e),
                      }),
                  })
                }
              >
                Read the state now
              </Menu.Item>
              <Menu.Item
                color="red"
                disabled={!canIssues}
                leftSection={<IconUnlink size={14} />}
                onClick={() =>
                  untrack.mutate(issue.id, {
                    onSuccess: () =>
                      notifications.show({
                        message: `${issue.key ?? 'The issue'} is no longer followed`,
                      }),
                  })
                }
              >
                Stop following
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        )}
      </Table.Td>
    </Table.Tr>
  );
}

export default function IssuesPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const [sp, setSp] = useSearchParams();
  const view = (['all', 'open', 'closed', 'suggested'].includes(sp.get('view') ?? '') ? sp.get('view') : 'all') as View;
  const q = useTrackerIssues(key, FILTER[view]);
  const suggested = useTrackerIssues(key, FILTER.suggested);
  const canTriage = useCan('results:triage', key);
  const canMute = useCan('results:mute', key);
  const canIssues = useCan('issues:write', key);
  const suggestions = suggested.data?.items ?? [];
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Issues' }]}
        title="Issues"
        subtitle="Issues created or linked from FastProof, with the state their tracker reports"
      />
      {suggestions.length > 0 && (
        <Stack gap="xs" mb="md">
          {suggestions.slice(0, 5).map((i) => (
            <Suggestion key={i.id} project={key} issue={i} canUnmute={canMute && canTriage} />
          ))}
        </Stack>
      )}
      <Group mb="md">
        <SegmentedControl
          value={view}
          onChange={(v) =>
            setSp(
              (prev) => {
                const n = new URLSearchParams(prev);
                if (v === 'all') n.delete('view');
                else n.set('view', v);
                return n;
              },
              { replace: true },
            )
          }
          data={[
            { value: 'all', label: 'All' },
            { value: 'open', label: 'Open' },
            { value: 'closed', label: 'Closed' },
            {
              value: 'suggested',
              label: `Muted after close${suggestions.length ? ` (${String(suggestions.length)})` : ''}`,
            },
          ]}
          data-testid="issues-view"
        />
      </Group>
      <QueryState query={q} notFound="Project not found">
        {() =>
          (q.data?.items ?? []).length === 0 ? (
            <EmptyState title="No issues" description="Create an issue from a failed result or a failure cluster with Triage; it shows up here with its state." />
          ) : (
            <Card withBorder radius="md" padding={0}>
              <div className="fp-table-wrap">
                <Table highlightOnHover verticalSpacing="xs" fz="sm" data-testid="issues-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Issue</Table.Th>
                      <Table.Th w={130}>State</Table.Th>
                      <Table.Th w={100} visibleFrom="sm">
                        Read
                      </Table.Th>
                      <Table.Th w={48} aria-label="Actions" />
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {(q.data?.items ?? []).map((i) => (
                      <Row key={i.id} project={key} issue={i} canTriage={canTriage} canIssues={canIssues} />
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
