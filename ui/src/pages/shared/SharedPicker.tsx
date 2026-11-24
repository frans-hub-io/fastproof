import { Badge, Button, Group, Loader, Modal, Stack, Text, TextInput } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconSearch } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage, projectPath, request } from '../../api/client';
import { sharedKey, useSharedSteps, type SharedStep, type SharedStepBrief, type SharedStepSummary } from '../../api/sharedSteps';
import { briefOf } from '../../lib/tm/shared';

/** Search the project's shared steps by `SS-` number or name and pick one to reference. */
export function SharedPicker({ project, opened, onClose, onPick }: { project: string; opened: boolean; onClose: () => void; onPick: (brief: SharedStepBrief) => void }) {
  const [q, setQ] = useState('');
  const [debounced] = useDebouncedValue(q, 250);
  const list = useSharedSteps(project, { q: debounced || undefined }, opened);
  const [busy, setBusy] = useState<number | null>(null);
  const items: SharedStepSummary[] = list.data?.pages.flatMap((p) => p.items) ?? [];

  const pick = async (s: SharedStepSummary) => {
    setBusy(s.number);
    try {
      const full = await request<SharedStep>(`${projectPath(project)}/shared-steps/${String(s.number)}`);
      onPick(briefOf(full));
      onClose();
    } catch (err) {
      notifications.show({ color: 'red', title: 'Shared step not inserted', message: errorMessage(err) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Modal opened={opened} onClose={onClose} title="Insert a shared step" size="lg">
      <Stack gap="sm">
        <TextInput
          data-autofocus
          aria-label="Search shared steps"
          placeholder="Search by SS- number or name"
          leftSection={<IconSearch size={14} />}
          value={q}
          onChange={(e) => setQ(e.currentTarget.value)}
          onKeyDown={(e) => {
            const first = items[0];
            if (e.key === 'Enter' && first && busy === null) {
              e.preventDefault();
              void pick(first);
            }
          }}
          data-testid="shared-pick-search"
        />
        {list.isPending ? (
          <Loader size="sm" />
        ) : items.length === 0 ? (
          <Text size="sm" c="dimmed">
            No shared steps match. Create one on the Shared steps page, or extract steps from a case.
          </Text>
        ) : (
          <Stack gap={4} role="list" aria-label="Shared steps">
            {items.map((s) => (
              <Button key={s.number} variant="default" justify="flex-start" fullWidth h="auto" py={8} loading={busy === s.number} disabled={busy !== null && busy !== s.number} onClick={() => void pick(s)} role="listitem" data-testid="shared-pick" styles={{ label: { width: '100%', whiteSpace: 'normal' } }}>
                <Group gap={8} wrap="nowrap" align="flex-start" style={{ width: '100%' }}>
                  <Badge variant="light" radius="sm" className="fp-mono" style={{ flex: 'none' }}>
                    {sharedKey(s.number)}
                  </Badge>
                  <Stack gap={0} align="flex-start" style={{ minWidth: 0, flex: 1 }}>
                    <Text size="sm" ta="left" style={{ overflowWrap: 'anywhere' }}>
                      {s.name}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {s.stepCount} steps · {s.cases} cases
                    </Text>
                  </Stack>
                </Group>
              </Button>
            ))}
            {list.hasNextPage && (
              <Button variant="subtle" size="xs" onClick={() => void list.fetchNextPage()} loading={list.isFetchingNextPage}>
                Load more
              </Button>
            )}
          </Stack>
        )}
      </Stack>
    </Modal>
  );
}
