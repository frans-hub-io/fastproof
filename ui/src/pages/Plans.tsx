import { Badge, Button, Card, Group, Modal, Stack, Switch, Text, Textarea, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useDebouncedValue } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconPlus, IconSearch } from '@tabler/icons-react';
import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useCaseFolders, useCreateManagedPlan, useManagedPlans } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatRelative } from '../lib/format';
import { describeFilter } from '../lib/tm/filters';
import { folderPathLabel } from '../lib/tm/ui';

function NewPlanDialog({ project, onClose }: { project: string; onClose: () => void }) {
  const create = useCreateManagedPlan(project);
  const navigate = useNavigate();
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  return (
    <Modal opened onClose={onClose} title="New test plan">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate(
            { name: name.trim(), description },
            {
              onSuccess: (p) => void navigate(`/p/${project}/plans/${p.id}`),
              onError: (err) => notifications.show({ color: 'red', title: 'Plan not created', message: errorMessage(err) }),
            },
          );
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} required maxLength={200} data-autofocus data-testid="plan-name" />
          <Textarea label="Description" value={description} onChange={(e) => setDescription(e.currentTarget.value)} autosize minRows={2} maxLength={4000} />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={!name.trim()} loading={create.isPending} data-testid="plan-create">
              Create and add cases
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

export default function PlansPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('plans:write', key);
  const [q, setQ] = useState('');
  const [debounced] = useDebouncedValue(q, 300);
  const [archived, setArchived] = useState(false);
  const plans = useManagedPlans(key, { q: debounced || undefined, archived });
  const folders = useCaseFolders(key);
  const [creating, setCreating] = useState(false);
  const list = plans.data ?? [];
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Test plans' }]}
        title="Test plans"
        subtitle="Long-lived sets of cases, chosen by hand or kept in sync with a filter, run as manual or combined launches."
        actions={
          canWrite && (
            <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setCreating(true)} data-testid="new-plan">
              New plan
            </Button>
          )
        }
      />
      <Stack gap="sm">
        <Group gap="sm" wrap="wrap">
          <TextInput aria-label="Search plans" placeholder="Search plans" leftSection={<IconSearch size={14} />} value={q} onChange={(e) => setQ(e.currentTarget.value)} style={{ flex: '1 1 240px' }} />
          <Switch label="Archived" checked={archived} onChange={(e) => setArchived(e.currentTarget.checked)} />
        </Group>
        <QueryState query={plans}>
          {() =>
            list.length === 0 ? (
              <EmptyState title={archived ? 'No archived plans' : 'No test plans yet'} description="A plan groups cases for a release, a smoke check or a regression run." />
            ) : (
              <Stack gap="xs">
                {list.map((p) => (
                  <Card key={p.id} withBorder radius="md" padding="sm" component={Link} to={`/p/${key}/plans/${p.id}`} data-testid="plan-card" className="fp-tm-plan-card">
                    <Group justify="space-between" gap="xs" wrap="wrap" align="flex-start">
                      <Stack gap={2} style={{ minWidth: 0, flex: '1 1 260px' }}>
                        <Text fw={600}>{p.name}</Text>
                        {p.description && (
                          <Text size="sm" c="dimmed" lineClamp={2}>
                            {p.description}
                          </Text>
                        )}
                        <Text size="xs" c="dimmed">
                          {p.filter ? `Filter: ${describeFilter(p.filter, (id) => folderPathLabel(folders.data, id))}` : 'Chosen by hand'} · updated {formatRelative(p.updatedAt ?? p.createdAt)}
                        </Text>
                      </Stack>
                      <Group gap={6}>
                        <Badge variant="light" radius="sm">
                          {p.counts.total} cases
                        </Badge>
                        <Badge variant="outline" color="gray" radius="sm">
                          {p.counts.manual} manual
                        </Badge>
                        <Badge variant="outline" color="gray" radius="sm">
                          {p.counts.automated} automated
                        </Badge>
                      </Group>
                    </Group>
                  </Card>
                ))}
              </Stack>
            )
          }
        </QueryState>
      </Stack>
      {creating && <NewPlanDialog project={key} onClose={() => setCreating(false)} />}
    </>
  );
}
