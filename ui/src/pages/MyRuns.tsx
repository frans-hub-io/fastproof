import { Anchor, Badge, Button, Card, Group, Stack, Text } from '@mantine/core';
import { EmptyState, useMe } from '@franshub/ui-shell';
import { IconPlayerPlay } from '@tabler/icons-react';
import { useMemo } from 'react';
import { Link, useParams } from 'react-router';
import { useProject } from '../api/hooks';
import { useMyRunItems, useTmEvents, tmk, type MyRunItem } from '../api/tm';
import { useQueryClient } from '@tanstack/react-query';
import { LoadMore } from '../components/LoadMore';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatRelative } from '../lib/format';
import { caseLabel } from '../lib/tm/filters';
import { RunStatusBadge } from './cases/common';

interface LaunchGroup {
  launchId: string;
  launchNumber: number;
  launchName: string;
  items: MyRunItem[];
}

/** "My runs": the open run items assigned to the signed-in user, grouped by launch. */
export default function MyRunsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const { me } = useMe();
  const q = useMyRunItems(key);
  const qc = useQueryClient();
  useTmEvents(key, {
    runItemUpdated: () => void qc.invalidateQueries({ queryKey: tmk.myRunItems(key) }),
    manualRunUpdated: () => void qc.invalidateQueries({ queryKey: tmk.myRunItems(key) }),
  });
  const groups = useMemo(() => {
    const m = new Map<string, LaunchGroup>();
    for (const it of q.data?.pages.flatMap((p) => p.items) ?? []) {
      const g = m.get(it.launchId) ?? { launchId: it.launchId, launchNumber: it.launchNumber, launchName: it.launchName, items: [] };
      g.items.push(it);
      m.set(it.launchId, g);
    }
    return [...m.values()].sort((a, b) => b.launchNumber - a.launchNumber);
  }, [q.data]);
  const total = groups.reduce((n, g) => n + g.items.length, 0);

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'My runs' }]}
        title="My runs"
        badges={
          total > 0 ? (
            <Badge variant="light" radius="sm" data-testid="my-runs-total">
              {total} open
            </Badge>
          ) : undefined
        }
        subtitle={`Manual cases assigned to ${me?.name ?? 'you'} that are not finished yet, in every open manual run of this project.`}
      />
      <QueryState query={q}>
        {() =>
          groups.length === 0 ? (
            <EmptyState title="Nothing assigned to you" description="When a lead assigns you cases of a manual run, they show up here. Open a launch's Manual tab to see every case of a run." />
          ) : (
            <Stack gap="sm" data-testid="my-runs">
              {groups.map((g) => {
                const first = g.items.find((i) => i.status === 'in_progress') ?? g.items[0];
                return (
                  <Card key={g.launchId} withBorder radius="md" padding={0} data-testid="my-runs-launch" data-launch={g.launchId}>
                    <Group justify="space-between" gap="xs" wrap="wrap" p="sm" style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }}>
                      <Stack gap={2} style={{ minWidth: 0, flex: '1 1 220px' }}>
                        <Anchor component={Link} to={`/p/${key}/launches/${g.launchId}?tab=manual`} fw={600} className="fh-truncate">
                          #{g.launchNumber} {g.launchName}
                        </Anchor>
                        <Text size="xs" c="dimmed">
                          {g.items.length} open case{g.items.length === 1 ? '' : 's'} for you
                        </Text>
                      </Stack>
                      {first && (
                        <Button size="xs" component={Link} to={`/p/${key}/launches/${g.launchId}/run/${first.id}`} leftSection={<IconPlayerPlay size={14} />} data-testid="my-runs-continue">
                          Continue testing
                        </Button>
                      )}
                    </Group>
                    <Stack gap={0} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
                      {g.items.map((i) => (
                        <Group key={i.id} component="li" gap="xs" wrap="wrap" px="sm" py={8} style={{ borderBottom: '1px solid var(--fh-ui-hairline)' }} data-testid="my-run-item">
                          <RunStatusBadge status={i.status} size="xs" />
                          <Anchor component={Link} to={`/p/${key}/launches/${g.launchId}/run/${i.id}`} size="sm" style={{ flex: '1 1 200px', minWidth: 0 }} className="fh-truncate">
                            <Text span size="xs" c="dimmed" className="fp-mono" mr={6}>
                              {caseLabel(i.caseNumber)}
                            </Text>
                            {i.name}
                          </Anchor>
                          <Text size="xs" c="dimmed" className="fh-truncate" style={{ maxWidth: 260 }}>
                            {i.folderPath.join(' / ')}
                          </Text>
                          <Text size="xs" c="dimmed">
                            {formatRelative(i.updatedAt)}
                          </Text>
                        </Group>
                      ))}
                    </Stack>
                  </Card>
                );
              })}
              <LoadMore hasMore={q.hasNextPage} loading={q.isFetchingNextPage} onLoad={() => void q.fetchNextPage()} label="Load more" />
            </Stack>
          )
        }
      </QueryState>
    </>
  );
}
