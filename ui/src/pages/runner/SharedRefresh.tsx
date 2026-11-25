import { Alert, Button, Group, Text } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconRefresh } from '@tabler/icons-react';
import { useParams } from 'react-router';
import { errorMessage } from '../../api/client';
import { sharedKey, useRefreshRunItem } from '../../api/sharedSteps';
import { useRunItem } from '../../api/tm';

function Refresh({ projectKey, itemId, outdated }: { projectKey: string; itemId: string; outdated: string[] }) {
  const refresh = useRefreshRunItem(projectKey, itemId);
  return (
    <Alert color="yellow" variant="light" mb="sm" title="A shared step has changed since this case was added to the run" className="fp-no-print" data-testid="shared-refresh">
      <Group gap="xs" wrap="wrap">
        <Text size="sm">
          {outdated.join(', ')} {outdated.length === 1 ? 'has' : 'have'} a newer version. This case has no results yet, so it can take the current version.
        </Text>
        <Button
          size="compact-sm"
          leftSection={<IconRefresh size={14} />}
          loading={refresh.isPending}
          onClick={() =>
            refresh.mutate(undefined, {
              onSuccess: () => notifications.show({ color: 'green', title: 'Steps updated', message: 'The case now uses the current version of its shared steps.' }),
              onError: (e) => notifications.show({ color: 'red', title: 'Not updated', message: errorMessage(e) }),
            })
          }
          data-testid="shared-refresh-button"
        >
          Update to current version
        </Button>
      </Group>
    </Alert>
  );
}

/**
 * Above the manual runner: when the open case was pinned to older versions of its shared steps and has not started
 * (status untested, no step results), offers to take the current versions. Started items keep their steps.
 */
export function SharedRefresh({ projectKey }: { projectKey: string; launchId: string }) {
  const { itemId } = useParams();
  const q = useRunItem(projectKey, itemId);
  const item = q.data?.value;
  if (!itemId || !item || item.status !== 'untested' || item.stepResults.length > 0) return null;
  const outdated = item.sharedPins.filter((p) => p.current > p.version).map((p) => `${sharedKey(p.number)} v${String(p.version)} → v${String(p.current)}`);
  if (outdated.length === 0) return null;
  return <Refresh projectKey={projectKey} itemId={itemId} outdated={outdated} />;
}
