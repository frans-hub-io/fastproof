import { Group, SegmentedControl, Stack, Switch, Text, UnstyledButton } from '@mantine/core';
import { Link } from 'react-router';
import type { RunItemSummary, RunStatus } from '../../api/tm';
import { caseLabel } from '../../lib/tm/filters';
import { RUN_STATUS_LABEL } from '../../lib/tm/steps';
import { RunStatusIcon } from '../cases/common';
import { isOpen } from './types';

export type QueueFilter = 'all' | 'open' | 'done';

export function RunQueue({ project, launchId, items, currentId, statusOf, mine, onMine, filter, onFilter, onPick }: { project: string; launchId: string; items: RunItemSummary[]; currentId: string | null; statusOf: (i: RunItemSummary) => RunStatus; mine: boolean; onMine: (v: boolean) => void; filter: QueueFilter; onFilter: (f: QueueFilter) => void; onPick?: () => void }) {
  const open = items.filter((i) => isOpen(statusOf(i))).length;
  return (
    <Stack gap="xs" data-testid="run-queue">
      <Group justify="space-between" gap="xs">
        <Text size="sm" fw={600} component="h2" m={0}>
          Queue
        </Text>
        <Text size="xs" c="dimmed">
          {open} open of {items.length}
        </Text>
      </Group>
      <Switch size="xs" label="Assigned to me" checked={mine} onChange={(e) => onMine(e.currentTarget.checked)} data-testid="queue-mine" />
      <SegmentedControl
        size="xs"
        fullWidth
        value={filter}
        onChange={(v) => onFilter(v === 'open' || v === 'done' ? v : 'all')}
        data={[
          { value: 'open', label: 'Open' },
          { value: 'done', label: 'Done' },
          { value: 'all', label: 'All' },
        ]}
        aria-label="Show"
      />
      <nav aria-label="Cases of this run">
        <ol className="fp-tm-queue">
          {items.map((i) => {
            const s = statusOf(i);
            const current = i.id === currentId;
            return (
              <li key={i.id}>
                <UnstyledButton
                  component={Link}
                  to={`/p/${project}/launches/${launchId}/run/${i.id}`}
                  replace
                  className="fp-row fp-tm-queue-item"
                  data-selected={current}
                  aria-current={current ? 'true' : undefined}
                  data-testid="queue-item"
                  data-status={s}
                  onClick={onPick}
                >
                  <RunStatusIcon status={s} size={15} />
                  <Stack gap={0} style={{ minWidth: 0, flex: 1 }}>
                    <Text size="sm" className="fh-truncate" fw={current ? 700 : 400}>
                      <Text span size="xs" c="dimmed" className="fp-mono" mr={6}>
                        {caseLabel(i.caseNumber)}
                      </Text>
                      {i.name}
                    </Text>
                    <Text size="xs" c="dimmed" className="fh-truncate">
                      {RUN_STATUS_LABEL[s]}
                      {i.assignee ? ` · ${i.assignee}` : ''}
                      {i.executionNo > 1 ? ` · run ${String(i.executionNo)}` : ''}
                    </Text>
                  </Stack>
                </UnstyledButton>
              </li>
            );
          })}
        </ol>
        {!items.length && (
          <Text size="sm" c="dimmed">
            Nothing here.
          </Text>
        )}
      </nav>
    </Stack>
  );
}
