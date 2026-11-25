import { Alert, Badge, Button, Group, Menu, Modal, Stack, Tabs, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconArrowBackUp, IconDots, IconInfoCircle, IconListDetails, IconPencil, IconTrash, IconUsers, IconVersions } from '@tabler/icons-react';
import { useState } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { sharedKey, useArchiveSharedStep, useInlineSharedStep, useRestoreSharedStep, useSharedStep, useUpdateSharedStep, type SharedStep } from '../api/sharedSteps';
import { isConflict } from '../api/tm';
import { CaseHtml } from './cases/CaseHtml';
import { LiveRegion } from './cases/common';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatDateTime, formatRelative } from '../lib/format';
import { draftProblem, type SharedDraft } from '../lib/tm/shared';
import { SharedStepForm } from './shared/SharedStepForm';
import { SharedStepUsages } from './shared/SharedStepUsages';
import { SharedStepVersions } from './shared/SharedStepVersions';
import { SharedStepView } from './shared/SharedStepView';

type Confirm = 'inline' | 'purge' | null;

function SharedStepBody({ project, projectName, s, etag, reload }: { project: string; projectName: string; s: SharedStep; etag: string; reload: () => void }) {
  const canWrite = useCan('cases:write', project);
  const canPurge = useCan('settings:write', project);
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const tab = params.get('tab') ?? 'steps';
  const setTab = (t: string | null) => setParams(t && t !== 'steps' ? { tab: t } : {}, { replace: true });
  const update = useUpdateSharedStep(project, s.number);
  const archive = useArchiveSharedStep(project, s.number);
  const restore = useRestoreSharedStep(project, s.number);
  const inline = useInlineSharedStep(project, s.number);
  const [editing, setEditing] = useState<{ draft: SharedDraft; etag: string } | null>(null);
  const [conflict, setConflict] = useState(false);
  const [confirm, setConfirm] = useState<Confirm>(null);
  const [announce, setAnnounce] = useState('');
  const problem = editing ? draftProblem(editing.draft) : null;
  const archived = s.archivedAt !== null;

  const startEdit = () => {
    setConflict(false);
    setEditing({ draft: { name: s.name, description: s.description, steps: structuredClone(s.steps) }, etag });
  };

  const save = () => {
    if (!editing) return;
    update.mutate(
      { body: { name: editing.draft.name.trim(), description: editing.draft.description, steps: editing.draft.steps }, etag: editing.etag },
      {
        onSuccess: (r) => {
          setEditing(null);
          notifications.show({ color: 'green', title: 'Saved', message: r.value.version === s.version ? 'No change to save.' : `Version ${String(r.value.version)}. New runs use it; started runs keep their steps.` });
        },
        onError: (e) => {
          if (isConflict(e)) setConflict(true);
          else notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) });
        },
      },
    );
  };

  const doInline = () =>
    inline.mutate(undefined, {
      onSuccess: (r) => {
        setConfirm(null);
        notifications.show({
          color: 'green',
          title: r.kind === 'job' ? 'Inlining started' : 'Inlined',
          message: r.kind === 'job' ? 'A background job replaces the references; follow it on the Jobs page.' : `${String(r.result.updated)} cases now hold the steps themselves${r.result.skipped ? `, ${String(r.result.skipped)} left as they were` : ''}.`,
        });
        reload();
      },
      onError: (e) => notifications.show({ color: 'red', title: 'Not inlined', message: errorMessage(e) }),
    });

  const doPurge = () =>
    archive.mutate(true, {
      onSuccess: () => void navigate(`/p/${project}/shared-steps`),
      onError: (e) => {
        setConfirm(null);
        notifications.show({ color: 'red', title: 'Not deleted', message: errorMessage(e) });
      },
    });

  return (
    <>
      <LiveRegion message={announce} />
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: projectName, to: `/p/${project}` }, { label: 'Shared steps', to: `/p/${project}/shared-steps` }, { label: sharedKey(s.number) }]}
        title={
          <span data-testid="shared-title">
            <span className="fp-mono">{sharedKey(s.number)}</span> {s.name}
          </span>
        }
        badges={
          <Group gap={6}>
            <Badge variant="light" radius="sm">
              v{s.version}
            </Badge>
            {archived && (
              <Badge variant="outline" color="gray" radius="sm">
                archived
              </Badge>
            )}
            {s.aiUnreviewed && (
              <Badge variant="light" color="grape" radius="sm">
                AI draft: a person reviews it before cases can use it
              </Badge>
            )}
            <Badge variant="outline" color="gray" radius="sm" data-testid="shared-usage-count">
              {s.usage.cases} cases / {s.usage.openRuns} open runs
            </Badge>
          </Group>
        }
        subtitle={
          <span title={formatDateTime(s.updatedAt)}>
            Updated {formatRelative(s.updatedAt)} by {s.updatedBy ?? 'unknown'}
          </span>
        }
        actions={
          canWrite && (
            <Group gap="xs">
              {!editing && !archived && (
                <Button size="xs" leftSection={<IconPencil size={14} />} onClick={startEdit} data-testid="shared-edit">
                  Edit
                </Button>
              )}
              {editing && (
                <>
                  <Button size="xs" variant="default" onClick={() => setEditing(null)} data-testid="shared-cancel">
                    Cancel
                  </Button>
                  <Button size="xs" onClick={save} disabled={!!problem} loading={update.isPending} data-testid="shared-save">
                    Save as a new version
                  </Button>
                </>
              )}
              {!editing && (
                <Menu position="bottom-end" withinPortal>
                  <Menu.Target>
                    <Button size="xs" variant="default" px={8} aria-label="More actions" data-testid="shared-more">
                      <IconDots size={16} />
                    </Button>
                  </Menu.Target>
                  <Menu.Dropdown>
                    {archived ? (
                      <Menu.Item leftSection={<IconArrowBackUp size={14} />} onClick={() => restore.mutate(undefined, { onError: (e) => notifications.show({ color: 'red', title: 'Not restored', message: errorMessage(e) }) })} data-testid="shared-restore">
                        Restore from the archive
                      </Menu.Item>
                    ) : (
                      <Menu.Item leftSection={<IconArchive size={14} />} onClick={() => archive.mutate(false, { onError: (e) => notifications.show({ color: 'red', title: 'Not archived', message: errorMessage(e) }) })} data-testid="shared-archive">
                        Archive (cases keep using it)
                      </Menu.Item>
                    )}
                    <Menu.Item leftSection={<IconListDetails size={14} />} onClick={() => setConfirm('inline')} disabled={s.usage.cases === 0} data-testid="shared-inline">
                      Inline into {s.usage.cases} cases
                    </Menu.Item>
                    {canPurge && (
                      <Menu.Item color="red" leftSection={<IconTrash size={14} />} onClick={() => setConfirm('purge')} data-testid="shared-purge">
                        Delete for good
                      </Menu.Item>
                    )}
                  </Menu.Dropdown>
                </Menu>
              )}
            </Group>
          )
        }
      />
      {conflict && (
        <Alert color="yellow" variant="light" mb="sm" title="Someone else saved this shared step" data-testid="shared-conflict">
          <Group gap="xs">
            <Text size="sm">Reload to see their version; your edits stay in the form until you do.</Text>
            <Button
              size="compact-sm"
              onClick={() => {
                setEditing(null);
                setConflict(false);
                reload();
              }}
            >
              Discard mine and reload
            </Button>
          </Group>
        </Alert>
      )}
      {editing ? (
        <Stack gap="xs">
          <SharedStepForm project={project} draft={editing.draft} onChange={(draft) => setEditing({ ...editing, draft })} onAnnounce={setAnnounce} />
          {problem && (
            <Text size="xs" c="red">
              {problem}
            </Text>
          )}
        </Stack>
      ) : (
        <Tabs value={tab} onChange={setTab} keepMounted={false}>
          <Tabs.List>
            <Tabs.Tab value="steps" leftSection={<IconListDetails size={14} />}>
              Steps
            </Tabs.Tab>
            <Tabs.Tab value="versions" leftSection={<IconVersions size={14} />} rightSection={<Badge size="xs" variant="light">{s.version}</Badge>}>
              Versions
            </Tabs.Tab>
            <Tabs.Tab value="usages" leftSection={<IconUsers size={14} />} rightSection={<Badge size="xs" variant="light">{s.usage.cases}</Badge>}>
              Used by
            </Tabs.Tab>
          </Tabs.List>
          <Tabs.Panel value="steps" pt="md">
            <Stack gap="md">
              {s.description && <CaseHtml html={s.html.description} project={project} />}
              <SharedStepView project={project} steps={s.steps} html={s.html.steps} />
              <Group gap={4} c="dimmed">
                <IconInfoCircle size={14} />
                <Text size="xs" c="dimmed">
                  A case references this shared step without a version and runs its current one; a run keeps the version it started with.
                </Text>
              </Group>
            </Stack>
          </Tabs.Panel>
          <Tabs.Panel value="versions" pt="md">
            <SharedStepVersions project={project} s={s} etag={etag} canWrite={canWrite} onRestored={reload} />
          </Tabs.Panel>
          <Tabs.Panel value="usages" pt="md">
            <SharedStepUsages project={project} number={s.number} />
          </Tabs.Panel>
        </Tabs>
      )}
      <Modal opened={confirm === 'inline'} onClose={() => setConfirm(null)} title={`Inline ${sharedKey(s.number)}`}>
        <Stack gap="sm">
          <Text size="sm">
            Every case that uses it gets the steps of its current version as its own steps, as a new case version. This cannot be undone in one step; restore the case versions to go back.
          </Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setConfirm(null)}>
              Cancel
            </Button>
            <Button loading={inline.isPending} onClick={doInline} data-testid="shared-inline-confirm">
              Inline into {s.usage.cases} cases
            </Button>
          </Group>
        </Stack>
      </Modal>
      <Modal opened={confirm === 'purge'} onClose={() => setConfirm(null)} title={`Delete ${sharedKey(s.number)} for good`}>
        <Stack gap="sm">
          <Text size="sm">Deleting removes the shared step with all its versions. It is refused while a case uses it or an open run pins it; archive it instead to keep history.</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setConfirm(null)}>
              Cancel
            </Button>
            <Button color="red" loading={archive.isPending} onClick={doPurge} data-testid="shared-purge-confirm">
              Delete
            </Button>
          </Group>
        </Stack>
      </Modal>
    </>
  );
}

export default function SharedStepPage() {
  const { key = '', n = '' } = useParams();
  const num = /^\d+$/.test(n) ? Number(n) : 0;
  const project = useProject(key);
  const q = useSharedStep(key, num);
  return (
    <QueryState query={q} notFound="Shared step not found">
      {() => (q.data ? <SharedStepBody key={q.data.value.number} project={key} projectName={project.data?.name ?? key} s={q.data.value} etag={q.data.etag} reload={() => void q.refetch()} /> : null)}
    </QueryState>
  );
}
