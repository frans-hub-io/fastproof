import { EmptyState, useCan } from '@franshub/ui-shell';
import { Anchor, Badge, Button, Card, Group, Modal, Stack, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconLock, IconPlus } from '@tabler/icons-react';
import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router';
import { errorMessage } from '../../api/client';
import { templateOverview, useCreateDashboard, useDashboards } from '../../api/dashboards';
import { useProject } from '../../api/hooks';
import { PageHeader } from '../../components/PageHeader';
import { QueryState } from '../../components/QueryState';

function CreateModal({ project, onClose }: { project: string; onClose: () => void }) {
  const [name, setName] = useState('');
  const create = useCreateDashboard(project);
  const navigate = useNavigate();
  const submit = (template: boolean) =>
    create.mutate(
      { name: name.trim(), description: '', widgets: template ? templateOverview() : [] },
      {
        onSuccess: (d) => {
          notifications.show({ color: 'green', message: 'Dashboard created' });
          void navigate(`/p/${project}/dashboards/${d.id}${template ? '' : '?edit=1'}`);
        },
        onError: (e) => notifications.show({ color: 'red', message: errorMessage(e) }),
      },
    );
  return (
    <Modal opened onClose={onClose} title="New dashboard" styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="sm">
        <TextInput label="Name" value={name} maxLength={120} data-autofocus onChange={(e) => setName(e.currentTarget.value)} />
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="default" disabled={!name.trim()} loading={create.isPending} onClick={() => submit(false)}>
            Empty dashboard
          </Button>
          <Button disabled={!name.trim()} loading={create.isPending} onClick={() => submit(true)}>
            From the overview template
          </Button>
        </Group>
        <Text size="xs" c="dimmed">
          The template has the pass rate, status split, trends, flaky and most failing tests. You can change everything afterwards.
        </Text>
      </Stack>
    </Modal>
  );
}

export default function DashboardsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const q = useDashboards(key);
  const canWrite = useCan('dashboards:write', key);
  const [creating, setCreating] = useState(false);
  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Dashboards' }]}
        title="Dashboards"
        subtitle="Charts and tables of launches, tests and failures"
        actions={
          canWrite && (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setCreating(true)}>
              New dashboard
            </Button>
          )
        }
      />
      <Stack gap="sm">
        <Card withBorder radius="md" padding="md">
          <Group justify="space-between" wrap="nowrap">
            <div style={{ minWidth: 0 }}>
              <Anchor component={Link} to={`/p/${key}/dashboard`} fw={600}>
                Project overview
              </Anchor>
              <Text size="sm" c="dimmed">
                Built in: quality trends with branch, environment and date filters
              </Text>
            </div>
            <Badge variant="light">Built in</Badge>
          </Group>
        </Card>
        <QueryState query={q} notFound="Project not found">
          {() =>
            q.data && q.data.length > 0 ? (
              q.data.map((d) => (
                <Card key={d.id} withBorder radius="md" padding="md" data-testid="dashboard-card">
                  <Group justify="space-between" wrap="nowrap" align="flex-start">
                    <div style={{ minWidth: 0 }}>
                      <Group gap={8} wrap="nowrap">
                        <Anchor component={Link} to={`/p/${key}/dashboards/${d.id}`} fw={600} style={{ overflowWrap: 'anywhere' }}>
                          {d.name}
                        </Anchor>
                        {d.locked && (
                          <Badge variant="light" color="yellow" leftSection={<IconLock size={12} />}>
                            Locked
                          </Badge>
                        )}
                      </Group>
                      {d.description && (
                        <Text size="sm" c="dimmed">
                          {d.description}
                        </Text>
                      )}
                      <Text size="xs" c="dimmed" mt={4}>
                        {d.widgetCount} widgets, owner {d.owner}, updated {new Date(d.updatedAt).toLocaleDateString()}
                      </Text>
                    </div>
                  </Group>
                </Card>
              ))
            ) : (
              <EmptyState title="No dashboards yet" description={canWrite ? 'Create one from the overview template or start empty.' : 'Ask a project editor to create one.'} />
            )
          }
        </QueryState>
      </Stack>
      {creating && <CreateModal project={key} onClose={() => setCreating(false)} />}
    </>
  );
}
