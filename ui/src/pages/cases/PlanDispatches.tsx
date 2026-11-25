import { Anchor, Badge, Card, Group, Stack, Text } from '@mantine/core';
import { IconExternalLink } from '@tabler/icons-react';
import { Link } from 'react-router';
import { usePlanDispatches, variablesLine, type PlanDispatch } from '../../api/planDispatches';
import { formatDateTime, formatRelative, isHttpUrl } from '../../lib/format';

const COLOR: Record<PlanDispatch['status'], string> = { dispatching: 'blue', dispatched: 'green', failed: 'red' };

/** The pipeline of a dispatch: a link when the server gave a URL on the configured CI origin, else the pipeline id. */
function Pipeline({ ciRef }: { ciRef: string | null | undefined }) {
  if (!ciRef) return null;
  if (isHttpUrl(ciRef)) {
    return (
      <Anchor href={ciRef} target="_blank" rel="noopener noreferrer" size="xs" data-testid="dispatch-pipeline">
        <Group gap={2} component="span" wrap="nowrap">
          Pipeline <IconExternalLink size={12} />
        </Group>
      </Anchor>
    );
  }
  return (
    <Text size="xs" c="dimmed" data-testid="dispatch-pipeline">
      pipeline {ciRef}
    </Text>
  );
}

/** CI runs started from the plan, newest first (`GET /test-plans/{id}/dispatches`). Nothing is shown until there is one. */
export function PlanDispatches({ projectKey, planId }: { projectKey: string; planId: string }) {
  const q = usePlanDispatches(projectKey, planId);
  const items = q.data?.items ?? [];
  if (items.length === 0) return null;
  return (
    <Stack gap={6} mt="lg" component="section" aria-labelledby="plan-dispatches-title" data-testid="plan-dispatches">
      <Text size="sm" fw={600} component="h3" m={0} id="plan-dispatches-title">
        Runs in CI
      </Text>
      <Stack gap={4}>
        {items.map((d) => (
          <Card key={d.id} withBorder radius="md" padding="xs" data-testid="plan-dispatch">
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Group gap="xs" wrap="nowrap" style={{ minWidth: 0, flex: '1 1 240px' }}>
                <Anchor component={Link} to={`/p/${projectKey}/launches/${d.launchId}`} fw={600} size="sm">
                  {d.launchNumber != null ? `#${String(d.launchNumber)}` : 'Launch'}
                </Anchor>
                {d.environment && (
                  <Badge size="sm" variant="light" radius="sm">
                    {d.environment}
                  </Badge>
                )}
                <Badge size="sm" variant="light" color={COLOR[d.status]} radius="sm" data-testid="dispatch-status">
                  {d.status}
                </Badge>
                <Pipeline ciRef={d.ciRef} />
              </Group>
              <Text size="xs" c="dimmed" title={formatDateTime(d.createdAt)}>
                {d.provider} · {d.gitRef} · {d.requestedBy} · {formatRelative(d.createdAt)}
              </Text>
            </Group>
            {Object.keys(d.variables).length > 0 && (
              <Text size="xs" c="dimmed" mt={4} style={{ wordBreak: 'break-word' }}>
                {variablesLine(d.variables)}
              </Text>
            )}
            {d.error && (
              <Text size="xs" c="red" mt={4} data-testid="dispatch-error">
                {d.error}
              </Text>
            )}
          </Card>
        ))}
      </Stack>
    </Stack>
  );
}
