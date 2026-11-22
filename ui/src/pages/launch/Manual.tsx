import { Anchor, Badge, Button, Card, Chip, Group, Menu, Modal, Radio, SimpleGrid, Stack, Table, Text, Tooltip } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconChevronDown, IconDownload, IconHandFinger, IconLock, IconLockOpen, IconPlayerPlay, IconPrinter, IconRobot } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { downloadAuthed, manualReportUrl, useManualRunState, useRunItems, useTmEvents, tmk, type RunItemSummary, type RunStatus, type TmLaunch } from '../../api/tm';
import { useQueryClient } from '@tanstack/react-query';
import { QueryState } from '../../components/QueryState';
import { formatDateTime } from '../../lib/format';
import { caseLabel } from '../../lib/tm/filters';
import { RUN_STATUS_LABEL } from '../../lib/tm/steps';
import { runStatusVar } from '../../lib/tm/ui';
import { RunStatusBadge, RunStatusIcon } from '../cases/common';

const ORDER: RunStatus[] = ['passed', 'failed', 'blocked', 'skipped', 'in_progress', 'untested'];

function ProgressBar({ counts, total }: { counts: Record<RunStatus, number>; total: number }) {
  const label = ORDER.filter((s) => counts[s] > 0)
    .map((s) => `${String(counts[s])} ${RUN_STATUS_LABEL[s].toLowerCase()}`)
    .join(', ');
  return (
    <div className="fp-statusbar" style={{ height: 14 }} role="img" aria-label={total ? `${String(total)} manual cases: ${label}` : 'No manual cases'} data-testid="manual-progress">
      {ORDER.filter((s) => counts[s] > 0).map((s) => (
        <Tooltip key={s} label={`${RUN_STATUS_LABEL[s]}: ${String(counts[s])}`}>
          <span style={{ width: `${String((counts[s] / (total || 1)) * 100)}%`, background: runStatusVar(s) }} />
        </Tooltip>
      ))}
    </div>
  );
}

function countBy(items: RunItemSummary[]): Record<RunStatus, number> {
  const c: Record<RunStatus, number> = { untested: 0, in_progress: 0, passed: 0, failed: 0, blocked: 0, skipped: 0 };
  for (const i of items) c[i.status]++;
  return c;
}

function CloseDialog({ project, launch, onClose }: { project: string; launch: TmLaunch; onClose: () => void }) {
  const state = useManualRunState(project, launch.id);
  const [untestedTo, setUntestedTo] = useState<'skipped' | 'leave'>('skipped');
  const open = (launch.manual?.untested ?? 0) + (launch.manual?.inProgress ?? 0);
  return (
    <Modal opened onClose={onClose} title="Close the manual run">
      <Stack gap="sm">
        <Text size="sm">Closing re-evaluates the quality gate and sends the notifications. You can reopen it later.</Text>
        {open > 0 && (
          <Radio.Group value={untestedTo} onChange={(v) => setUntestedTo(v === 'leave' ? 'leave' : 'skipped')} label={`${String(open)} cases are not finished`}>
            <Stack gap={6} mt={6}>
              <Radio value="skipped" label="Mark them skipped" />
              <Radio value="leave" label="Leave them untested (they do not count)" />
            </Stack>
          </Radio.Group>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button
            loading={state.isPending}
            data-testid="manual-close-confirm"
            onClick={() =>
              state.mutate(
                { close: true, untestedTo },
                {
                  onSuccess: () => {
                    notifications.show({ color: 'green', title: 'Manual run closed', message: '' });
                    onClose();
                  },
                  onError: (e) => notifications.show({ color: 'red', title: 'Not closed', message: errorMessage(e) }),
                },
              )
            }
          >
            Close the run
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

/** The launch's Manual tab: progress, assignees, the run report and closing the run. */
export function Manual({ project, launch }: { project: string; launch: TmLaunch }) {
  const items = useRunItems(project, launch.id, {});
  const qc = useQueryClient();
  const canRun = useCan('runs:execute', project);
  const canReopen = useCan('plans:write', project);
  const state = useManualRunState(project, launch.id);
  const [closing, setClosing] = useState(false);
  const [statuses, setStatuses] = useState<string[]>([]);
  const list = useMemo(() => items.data ?? [], [items.data]);
  const counts = countBy(list);
  const closed = launch.manual?.state === 'closed';

  useTmEvents(project, {
    runItemUpdated: (e) => {
      if (e.launchId === launch.id) void qc.invalidateQueries({ queryKey: tmk.runItems(project, launch.id) });
    },
    manualRunUpdated: (e) => {
      if (e.launchId === launch.id) void qc.invalidateQueries({ queryKey: tmk.launch(project, launch.id) });
    },
  });

  const byAssignee = useMemo(() => {
    const m = new Map<string, RunItemSummary[]>();
    for (const i of list) {
      const k = i.assignee ?? '';
      m.set(k, [...(m.get(k) ?? []), i]);
    }
    return [...m.entries()].sort(([a], [b]) => (a === '' ? 1 : b === '' ? -1 : a.localeCompare(b)));
  }, [list]);

  const manualDone = counts.passed + counts.failed + counts.blocked + counts.skipped;
  const automatedTotal = Math.max(0, launch.counts.total - manualDone);
  const shown = statuses.length ? list.filter((i) => statuses.includes(i.status)) : list;
  const firstOpen = list.find((i) => i.status === 'untested' || i.status === 'in_progress');

  return (
    <QueryState query={items}>
      {() => (
        <Stack gap="md" data-testid="manual-tab">
          <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="sm" className="fp-no-print">
            <Card withBorder radius="lg" padding="sm">
              <Group gap="xs">
                <IconRobot size={18} aria-hidden />
                <Text size="sm" fw={600}>
                  Automated
                </Text>
              </Group>
              <Text size="xl" fw={700}>
                {automatedTotal}
              </Text>
              <Text size="xs" c="dimmed">
                results from CI
              </Text>
            </Card>
            <Card withBorder radius="lg" padding="sm">
              <Group gap="xs">
                <IconHandFinger size={18} aria-hidden />
                <Text size="sm" fw={600}>
                  Manual
                </Text>
                <Badge size="xs" variant="light" color={closed ? 'gray' : 'blue'} leftSection={closed ? <IconLock size={10} /> : <IconLockOpen size={10} />}>
                  {closed ? 'closed' : 'open'}
                </Badge>
              </Group>
              <Text size="xl" fw={700} data-testid="manual-done">
                {manualDone} / {list.length}
              </Text>
              <Text size="xs" c="dimmed">
                cases finished
              </Text>
            </Card>
          </SimpleGrid>
          <Stack gap={6}>
            <ProgressBar counts={counts} total={list.length} />
            <Group gap="md" wrap="wrap">
              {ORDER.map((s) => (
                <Group key={s} gap={4} wrap="nowrap">
                  <RunStatusIcon status={s} size={14} />
                  <Text size="xs">
                    {RUN_STATUS_LABEL[s]} {counts[s]}
                  </Text>
                </Group>
              ))}
            </Group>
          </Stack>
          <Group gap="xs" wrap="wrap" className="fp-no-print">
            {!closed && canRun && (
              <Button size="xs" component={Link} to={firstOpen ? `/p/${project}/launches/${launch.id}/run/${firstOpen.id}` : `/p/${project}/launches/${launch.id}/run`} leftSection={<IconPlayerPlay size={14} />} data-testid="continue-testing">
                Continue testing
              </Button>
            )}
            {!closed && canRun && (
              <Button size="xs" variant="default" leftSection={<IconLock size={14} />} onClick={() => setClosing(true)} data-testid="manual-close">
                Close manual run
              </Button>
            )}
            {closed && canReopen && (
              <Button size="xs" variant="default" leftSection={<IconLockOpen size={14} />} loading={state.isPending} onClick={() => state.mutate({ close: false }, { onError: (e) => notifications.show({ color: 'red', title: 'Not reopened', message: errorMessage(e) }) })} data-testid="manual-reopen">
                Reopen
              </Button>
            )}
            <Menu withinPortal>
              <Menu.Target>
                <Button size="xs" variant="default" leftSection={<IconDownload size={14} />} rightSection={<IconChevronDown size={12} />}>
                  Report
                </Button>
              </Menu.Target>
              <Menu.Dropdown>
                <Menu.Item onClick={() => void downloadAuthed(manualReportUrl(project, launch.id, 'csv'), `manual-report-${String(launch.number)}.csv`).catch(() => undefined)}>
                  CSV
                </Menu.Item>
                <Menu.Item onClick={() => void downloadAuthed(manualReportUrl(project, launch.id, 'json'), `manual-report-${String(launch.number)}.json`).catch(() => undefined)}>
                  JSON
                </Menu.Item>
              </Menu.Dropdown>
            </Menu>
            <Button size="xs" variant="default" leftSection={<IconPrinter size={14} />} onClick={() => window.print()} data-testid="manual-print">
              Print report
            </Button>
          </Group>
          <Stack gap={6}>
            <Text size="sm" fw={600} component="h3" m={0}>
              By assignee
            </Text>
            <div className="fp-table-wrap">
              <Table withTableBorder verticalSpacing={4} miw={520} data-testid="manual-assignees">
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>Assignee</Table.Th>
                    <Table.Th>Progress</Table.Th>
                    {ORDER.map((s) => (
                      <Table.Th key={s} ta="right">
                        {RUN_STATUS_LABEL[s]}
                      </Table.Th>
                    ))}
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {byAssignee.map(([who, its]) => {
                    const c = countBy(its);
                    return (
                      <Table.Tr key={who || 'none'}>
                        <Table.Td>{who || 'Unassigned'}</Table.Td>
                        <Table.Td w={160}>
                          <ProgressBar counts={c} total={its.length} />
                        </Table.Td>
                        {ORDER.map((s) => (
                          <Table.Td key={s} ta="right">
                            {c[s]}
                          </Table.Td>
                        ))}
                      </Table.Tr>
                    );
                  })}
                </Table.Tbody>
              </Table>
            </div>
          </Stack>
          <Stack gap={6}>
            <Group justify="space-between" wrap="wrap" className="fp-no-print">
              <Text size="sm" fw={600} component="h3" m={0}>
                Cases
              </Text>
              <Chip.Group multiple value={statuses} onChange={setStatuses}>
                <Group gap={4}>
                  {ORDER.map((s) => (
                    <Chip key={s} value={s} size="xs" variant="outline">
                      {RUN_STATUS_LABEL[s]}
                    </Chip>
                  ))}
                </Group>
              </Chip.Group>
            </Group>
            <div className="fp-table-wrap">
              <Table withTableBorder verticalSpacing={4} miw={560} data-testid="manual-items" className="fp-print-report">
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>Case</Table.Th>
                    <Table.Th>Status</Table.Th>
                    <Table.Th>Assignee</Table.Th>
                    <Table.Th>Run</Table.Th>
                    <Table.Th>Updated</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {shown.map((i) => (
                    <Table.Tr key={i.id} data-testid="manual-item">
                      <Table.Td>
                        <Anchor component={Link} to={`/p/${project}/launches/${launch.id}/run/${i.id}`} size="sm">
                          <Text span size="xs" c="dimmed" className="fp-mono" mr={6}>
                            {caseLabel(i.caseNumber)}
                          </Text>
                          {i.name}
                        </Anchor>
                        <Text size="xs" c="dimmed">
                          {i.folderPath.join(' / ')}
                        </Text>
                      </Table.Td>
                      <Table.Td>
                        <RunStatusBadge status={i.status} size="xs" />
                      </Table.Td>
                      <Table.Td>
                        <Text size="sm">{i.assignee ?? '—'}</Text>
                      </Table.Td>
                      <Table.Td>{i.executionNo}</Table.Td>
                      <Table.Td>
                        <Text size="xs" c="dimmed">
                          {formatDateTime(i.updatedAt)}
                        </Text>
                      </Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </div>
          </Stack>
          {closing && <CloseDialog project={project} launch={launch} onClose={() => setClosing(false)} />}
        </Stack>
      )}
    </QueryState>
  );
}
