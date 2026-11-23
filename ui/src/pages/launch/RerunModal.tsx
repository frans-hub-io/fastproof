import { Alert, Anchor, Badge, Button, Chip, Code, Group, Modal, Stack, Text } from '@mantine/core';
import { IconExternalLink } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useRerun, useReruns } from '../../api/stageC';
import type { Launch, ResultStatus } from '../../api/types';
import { RESULT_STATUSES } from '../../api/types';
import { StatusDot } from '../../components/Status';
import { isHttpUrl } from '../../lib/format';
import { STATUS_LABEL } from '../../lib/status';

export type RerunTarget = { kind: 'failed' } | { kind: 'selected'; resultIds: string[] };

export function RerunModal({ project, launch, target, opened, onClose }: { project: string; launch: Launch; target: RerunTarget; opened: boolean; onClose: () => void }) {
  const info = useReruns(project, launch.id);
  const rerun = useRerun(project, launch.id);
  const [statuses, setStatuses] = useState<string[]>(['failed', 'broken']);
  const counted = statuses.reduce((n, s) => n + (RESULT_STATUSES.includes(s as ResultStatus) ? launch.counts[s as ResultStatus] : 0), 0);
  const selected = target.kind === 'selected';
  const count = selected ? target.resultIds.length : counted;
  const done = rerun.data;
  const close = () => {
    rerun.reset();
    onClose();
  };
  const provider = info.data?.provider;
  return (
    <Modal opened={opened} onClose={close} title={selected ? 'Rerun selected tests' : 'Rerun failed tests'} size="lg" styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="md" data-testid="rerun-modal">
        <Text size="sm" c="dimmed">
          FastProof makes a test plan (<Code>testplan.json</Code>) and triggers the CI of this project{provider ? ` (${provider})` : ''}. The new launch starts with a prepared external id and is linked to this launch.
        </Text>
        {!selected && (
          <Stack gap={6}>
            <Text size="sm" fw={500}>
              Results with status
            </Text>
            <Chip.Group multiple value={statuses} onChange={setStatuses}>
              <Group gap={6}>
                {RESULT_STATUSES.map((s) => (
                  <Chip key={s} value={s} size="xs" variant="outline" disabled={!!done}>
                    <Group gap={4} component="span" wrap="nowrap">
                      <StatusDot status={s} size={8} />
                      {STATUS_LABEL[s]} {launch.counts[s]}
                    </Group>
                  </Chip>
                ))}
              </Group>
            </Chip.Group>
          </Stack>
        )}
        <Text size="sm" data-testid="rerun-count">
          {count} test{count === 1 ? '' : 's'} will run again.
        </Text>
        {rerun.isError && (
          <Alert color="red" variant="light" title="Not started" data-testid="rerun-error">
            {errorMessage(rerun.error)}
          </Alert>
        )}
        {done && (
          <Alert color="green" variant="light" title="CI triggered" data-testid="rerun-done">
            <Stack gap={4}>
              <Text size="sm">
                {done.testCount} test{done.testCount === 1 ? '' : 's'} sent. The new launch will use external id <Code>{done.externalId}</Code>.
              </Text>
              {done.ciRef && isHttpUrl(done.ciRef) && (
                <Anchor href={done.ciRef} target="_blank" rel="noopener noreferrer" size="sm">
                  <Group gap={4} component="span">
                    Open the pipeline <IconExternalLink size={14} />
                  </Group>
                </Anchor>
              )}
            </Stack>
          </Alert>
        )}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={close}>
            {done ? 'Close' : 'Cancel'}
          </Button>
          {!done && (
            <Button
              onClick={() => rerun.mutate(selected ? { mode: 'selected', resultIds: target.resultIds } : { mode: 'failed', statuses })}
              loading={rerun.isPending}
              disabled={count === 0 || (!selected && statuses.length === 0)}
              data-testid="rerun-start"
            >
              Rerun
            </Button>
          )}
        </Group>
      </Stack>
    </Modal>
  );
}

export function RerunsPanel({ project, launch }: { project: string; launch: Launch }) {
  const q = useReruns(project, launch.id);
  const items = q.data?.items ?? [];
  const of = q.data?.rerunOf;
  if (!items.length && !of) return null;
  return (
    <Stack gap={4} data-testid="reruns-panel">
      {of && (
        <Text size="sm" data-testid="rerun-of">
          Rerun of{' '}
          <Anchor component={Link} to={`/p/${project}/launches/${of.sourceLaunchId}`}>
            #{of.sourceLaunchNumber}
          </Anchor>{' '}
          ({of.testCount} test{of.testCount === 1 ? '' : 's'}, {of.mode})
        </Text>
      )}
      {items.map((r) => (
        <Group key={r.id} gap="xs" wrap="wrap" data-testid="rerun-row">
          <Badge size="sm" variant="light" color={r.status === 'dispatched' ? 'green' : r.status === 'failed' ? 'red' : 'gray'} style={{ textTransform: 'none' }}>
            {r.status}
          </Badge>
          <Text size="sm">
            {r.testCount} {r.mode} test{r.testCount === 1 ? '' : 's'} via {r.provider}
          </Text>
          {r.launchId ? (
            <Anchor component={Link} to={`/p/${project}/launches/${r.launchId}`} size="sm">
              launch #{r.launchNumber}
            </Anchor>
          ) : (
            r.status === 'dispatched' && (
              <Text size="xs" c="dimmed">
                waiting for the launch
              </Text>
            )
          )}
          {r.error && (
            <Text size="xs" c="var(--mantine-color-error)">
              {r.error}
            </Text>
          )}
        </Group>
      ))}
    </Stack>
  );
}
