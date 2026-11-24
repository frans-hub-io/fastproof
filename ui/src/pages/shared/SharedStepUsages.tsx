import { Anchor, Badge, Button, Card, Group, Stack, Text } from '@mantine/core';
import { Link } from 'react-router';
import { useSharedStepUsages } from '../../api/sharedSteps';
import { QueryState } from '../../components/QueryState';

export function SharedStepUsages({ project, number }: { project: string; number: number }) {
  const usages = useSharedStepUsages(project, number);
  const items = usages.data?.pages.flatMap((p) => p.items) ?? [];
  const openRuns = usages.data?.pages[0]?.openRuns ?? 0;
  return (
    <QueryState query={usages}>
      {() => (
        <Stack gap="xs" data-testid="shared-usages">
          <Text size="sm" c="dimmed">
            {items.length === 0 && !usages.hasNextPage ? 'No case uses this shared step.' : 'Cases whose current or approved version references it.'} {openRuns} open {openRuns === 1 ? 'run pins' : 'runs pin'} a version of it.
          </Text>
          {items.map((u) => (
            <Card key={u.caseNumber} withBorder radius="md" padding="xs" data-testid="shared-usage">
              <Group justify="space-between" gap="xs" wrap="nowrap">
                <Anchor component={Link} to={`/p/${project}/cases/${String(u.caseNumber)}`} size="sm" className="fh-truncate">
                  <span className="fp-mono">TC-{u.caseNumber}</span> {u.name}
                </Anchor>
                <Group gap={6} wrap="nowrap">
                  <Badge size="xs" variant="light" radius="sm" color="gray">
                    {u.status}
                  </Badge>
                  {u.archived && (
                    <Badge size="xs" variant="outline" radius="sm" color="gray">
                      archived
                    </Badge>
                  )}
                </Group>
              </Group>
            </Card>
          ))}
          {usages.hasNextPage && (
            <Button variant="subtle" size="xs" onClick={() => void usages.fetchNextPage()} loading={usages.isFetchingNextPage}>
              Load more
            </Button>
          )}
        </Stack>
      )}
    </QueryState>
  );
}
