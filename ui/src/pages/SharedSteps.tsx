import { Badge, Button, Card, Group, Modal, Stack, Switch, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconPlus, IconSearch } from '@tabler/icons-react';
import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { sharedKey, useCreateSharedStep, useSharedSteps } from '../api/sharedSteps';
import { LiveRegion } from './cases/common';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatRelative } from '../lib/format';
import { newStepId } from '../lib/tm/steps';
import { draftProblem, type SharedDraft } from '../lib/tm/shared';
import { SharedStepForm } from './shared/SharedStepForm';

function NewSharedStepDialog({ project, onClose }: { project: string; onClose: () => void }) {
  const create = useCreateSharedStep(project);
  const navigate = useNavigate();
  const [draft, setDraft] = useState<SharedDraft>({ name: '', description: '', steps: [{ id: newStepId(), action: '', expected: '', data: '', attachments: [], steps: [] }] });
  const [announce, setAnnounce] = useState('');
  const problem = draftProblem(draft);
  return (
    <Modal opened onClose={onClose} title="New shared step" size="xl">
      <LiveRegion message={announce} />
      <form
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate(
            { name: draft.name.trim(), description: draft.description, steps: draft.steps },
            {
              onSuccess: (s) => void navigate(`/p/${project}/shared-steps/${String(s.number)}`),
              onError: (err) => notifications.show({ color: 'red', title: 'Shared step not created', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <SharedStepForm project={project} draft={draft} onChange={setDraft} onAnnounce={setAnnounce} />
          <Group justify="space-between">
            <Text size="xs" c="dimmed">
              {problem ?? 'Cases reference it by number; each save is a new version that new runs pick up.'}
            </Text>
            <Group>
              <Button variant="default" onClick={onClose}>
                Cancel
              </Button>
              <Button type="submit" disabled={!!problem} loading={create.isPending} data-testid="shared-create">
                Create
              </Button>
            </Group>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

export default function SharedStepsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('cases:write', key);
  const [q, setQ] = useState('');
  const [debounced] = useDebouncedValue(q, 300);
  const [archived, setArchived] = useState(false);
  const list = useSharedSteps(key, { q: debounced || undefined, archived });
  const [creating, setCreating] = useState(false);
  const items = list.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Shared steps' }]}
        title="Shared steps"
        subtitle="Steps that many cases reuse, such as logging in. Edit one in one place; cases and new runs pick up its current version."
        actions={
          canWrite && (
            <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setCreating(true)} data-testid="new-shared-step">
              New shared step
            </Button>
          )
        }
      />
      <Stack gap="sm">
        <Group gap="sm" wrap="wrap">
          <TextInput aria-label="Search shared steps" placeholder="Search by SS- number or name" leftSection={<IconSearch size={14} />} value={q} onChange={(e) => setQ(e.currentTarget.value)} style={{ flex: '1 1 240px' }} data-testid="shared-search" />
          <Switch label="Archived" checked={archived} onChange={(e) => setArchived(e.currentTarget.checked)} />
        </Group>
        <QueryState query={list}>
          {() =>
            items.length === 0 ? (
              <EmptyState title={archived ? 'No archived shared steps' : q ? 'No shared step matches' : 'No shared steps yet'} description="Extract steps from a case, or create a shared step here and insert it into cases with Ctrl+Shift+S." />
            ) : (
              <Stack gap="xs">
                {items.map((s) => (
                  <Card key={s.number} withBorder radius="md" padding="sm" component={Link} to={`/p/${key}/shared-steps/${String(s.number)}`} data-testid="shared-card" className="fp-tm-plan-card">
                    <Group justify="space-between" gap="xs" wrap="wrap" align="flex-start">
                      <Group gap="xs" wrap="nowrap" style={{ minWidth: 0, flex: '1 1 260px' }}>
                        <Badge variant="light" radius="sm" className="fp-mono">
                          {sharedKey(s.number)}
                        </Badge>
                        <Stack gap={2} style={{ minWidth: 0 }}>
                          <Text fw={600} className="fh-truncate">
                            {s.name}
                          </Text>
                          <Text size="xs" c="dimmed">
                            v{s.version} · updated {formatRelative(s.updatedAt)}
                          </Text>
                        </Stack>
                      </Group>
                      <Group gap={6}>
                        {s.aiUnreviewed && (
                          <Badge variant="light" color="grape" radius="sm">
                            AI draft
                          </Badge>
                        )}
                        {s.archived && (
                          <Badge variant="outline" color="gray" radius="sm">
                            archived
                          </Badge>
                        )}
                        <Badge variant="outline" color="gray" radius="sm">
                          {s.stepCount} steps
                        </Badge>
                        <Badge variant="light" radius="sm">
                          {s.cases} cases
                        </Badge>
                      </Group>
                    </Group>
                  </Card>
                ))}
                {list.hasNextPage && (
                  <Button variant="subtle" size="xs" onClick={() => void list.fetchNextPage()} loading={list.isFetchingNextPage}>
                    Load more
                  </Button>
                )}
              </Stack>
            )
          }
        </QueryState>
      </Stack>
      {creating && <NewSharedStepDialog project={key} onClose={() => setCreating(false)} />}
    </>
  );
}
