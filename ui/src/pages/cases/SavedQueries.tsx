import { ActionIcon, Button, Group, Menu, Modal, Stack, Switch, Text, TextInput } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconBookmark, IconChevronDown, IconDeviceFloppy, IconPencil, IconSettings, IconTrash } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage } from '../../api/client';
import { useCreateSavedQuery, useDeleteSavedQuery, useSavedQueries, useUpdateSavedQuery, type SavedQuery } from '../../api/queries';

function SaveDialog({ project, query, onClose }: { project: string; query: string; onClose: () => void }) {
  const create = useCreateSavedQuery(project);
  const canWrite = useCan('cases:write', project);
  const [name, setName] = useState('');
  const [shared, setShared] = useState(false);
  return (
    <Modal opened onClose={onClose} title="Save the query" attributes={{ content: { 'data-testid': 'save-query-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!name.trim()) return;
          create.mutate(
            { entity: 'cases', name: name.trim(), query, shared: canWrite && shared },
            {
              onSuccess: () => {
                notifications.show({ color: 'green', title: 'Query saved', message: name.trim() });
                onClose();
              },
              onError: (err) => notifications.show({ color: 'red', title: 'Query not saved', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} maxLength={100} required data-autofocus data-testid="save-query-name" />
          <Text size="xs" c="dimmed" className="fp-mono" style={{ overflowWrap: 'anywhere' }}>
            {query}
          </Text>
          {canWrite && <Switch label="Share with the project" checked={shared} onChange={(e) => setShared(e.currentTarget.checked)} data-testid="save-query-shared" />}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={create.isPending} disabled={!name.trim()} data-testid="save-query-submit">
              Save
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

function ManageRow({ project, q, canChange }: { project: string; q: SavedQuery; canChange: boolean }) {
  const update = useUpdateSavedQuery(project);
  const del = useDeleteSavedQuery(project);
  const [name, setName] = useState<string | null>(null);
  const fail = (title: string) => (err: unknown) => notifications.show({ color: 'red', title, message: errorMessage(err) });
  return (
    <Group gap="xs" wrap="nowrap" align="center" data-testid="saved-query-row">
      <Stack gap={0} style={{ flex: 1, minWidth: 0 }}>
        {name === null ? (
          <Text size="sm" fw={600} truncate>
            {q.name}
          </Text>
        ) : (
          <TextInput
            size="xs"
            aria-label="Query name"
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            maxLength={100}
            data-autofocus
            data-testid="saved-query-rename-input"
            onKeyDown={(e) => {
              if (e.key === 'Escape') setName(null);
              if (e.key === 'Enter' && name.trim()) update.mutate({ id: q.id, patch: { name: name.trim() } }, { onSuccess: () => setName(null), onError: fail('Not renamed') });
            }}
          />
        )}
        <Text size="xs" c="dimmed" className="fp-mono" truncate>
          {q.query}
        </Text>
      </Stack>
      {canChange && (
        <>
          <ActionIcon variant="subtle" aria-label={`Rename ${q.name}`} onClick={() => setName(q.name)} data-testid="saved-query-rename">
            <IconPencil size={16} />
          </ActionIcon>
          <ActionIcon
            variant="subtle"
            color="red"
            aria-label={`Delete ${q.name}`}
            loading={del.isPending}
            onClick={() => del.mutate(q.id, { onError: fail('Not deleted') })}
            data-testid="saved-query-delete"
          >
            <IconTrash size={16} />
          </ActionIcon>
        </>
      )}
    </Group>
  );
}

function ManageDialog({ project, items, onClose }: { project: string; items: SavedQuery[]; onClose: () => void }) {
  const canWrite = useCan('cases:write', project);
  return (
    <Modal opened onClose={onClose} title="Saved queries" size="lg" attributes={{ content: { 'data-testid': 'manage-queries-dialog' } }}>
      <Stack gap="sm">
        {items.length === 0 && (
          <Text size="sm" c="dimmed">
            No saved queries yet.
          </Text>
        )}
        {items.map((q) => (
          <ManageRow key={q.id} project={project} q={q} canChange={q.shared ? canWrite : q.mine} />
        ))}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

/** The menu next to the query bar: apply, save and manage saved queries. */
export function SavedQueries({ project, query, onApply }: { project: string; query: string; onApply: (query: string) => void }) {
  const saved = useSavedQueries(project);
  const [dialog, setDialog] = useState<'save' | 'manage' | null>(null);
  const items = saved.data ?? [];
  const mine = items.filter((q) => q.mine && !q.shared);
  const shared = items.filter((q) => q.shared);
  const row = (q: SavedQuery) => (
    <Menu.Item key={q.id} onClick={() => onApply(q.query)} data-testid="saved-query-item">
      <Text size="sm" truncate maw={280}>
        {q.name}
      </Text>
    </Menu.Item>
  );
  return (
    <>
      <Menu withinPortal position="bottom-end">
        <Menu.Target>
          <Button variant="default" leftSection={<IconBookmark size={14} />} rightSection={<IconChevronDown size={12} />} data-testid="saved-queries">
            Saved
          </Button>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Label>My queries</Menu.Label>
          {mine.length ? (
            mine.map(row)
          ) : (
            <Menu.Item disabled>None yet</Menu.Item>
          )}
          <Menu.Label>Shared</Menu.Label>
          {shared.length ? (
            shared.map(row)
          ) : (
            <Menu.Item disabled>None yet</Menu.Item>
          )}
          <Menu.Divider />
          <Menu.Item leftSection={<IconDeviceFloppy size={14} />} disabled={!query.trim()} onClick={() => setDialog('save')} data-testid="save-query">
            Save current query…
          </Menu.Item>
          <Menu.Item leftSection={<IconSettings size={14} />} disabled={!items.length} onClick={() => setDialog('manage')} data-testid="manage-queries">
            Manage…
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
      {dialog === 'save' && <SaveDialog project={project} query={query.trim()} onClose={() => setDialog(null)} />}
      {dialog === 'manage' && <ManageDialog project={project} items={items} onClose={() => setDialog(null)} />}
    </>
  );
}
