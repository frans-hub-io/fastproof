import { Badge, Button, Menu, Stack, Text, TextInput } from '@mantine/core';
import { useCan, useMe } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconCheck, IconDots, IconDownload, IconFilter, IconPencil, IconPlayerPlay, IconRefresh, IconX } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { isConflict, planTestplanUrl, useArchiveTestPlan, useCaseFolders, useSyncTestPlan, useTestPlan, useTestPlanItems, useUpdateTestPlan, type PlanSyncDiff } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { formatRelative } from '../lib/format';
import { describeFilter } from '../lib/tm/filters';
import { folderPathLabel } from '../lib/tm/ui';
import { FilterDialog, StartLaunchDialog, SyncDialog } from './cases/PlanDialogs';
import { PlanItems } from './cases/PlanItems';
import { PlanTree } from './cases/PlanTree';

const PEOPLE_HINT = ['maria@example.com', 'oleh@example.com', 'sam@example.com'];

export default function PlanPage() {
  const { key = '', id = '' } = useParams();
  const project = useProject(key);
  const navigate = useNavigate();
  const { me } = useMe();
  const canWrite = useCan('plans:write', key);
  const canRun = useCan('runs:execute', key);
  const q = useTestPlan(key, id);
  const items = useTestPlanItems(key, id);
  const folders = useCaseFolders(key);
  const update = useUpdateTestPlan(key, id);
  const archive = useArchiveTestPlan(key, id);
  const sync = useSyncTestPlan(key, id);
  const [dialog, setDialog] = useState<null | 'filter' | 'start'>(null);
  const [diff, setDiff] = useState<PlanSyncDiff | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);

  const people = useMemo(() => {
    const set = new Set<string>([...(me?.name ? [me.name] : []), ...PEOPLE_HINT]);
    for (const it of items.data ?? []) if (it.assignee) set.add(it.assignee);
    return [...set];
  }, [items.data, me]);

  const preview = () =>
    sync.mutate(true, {
      onSuccess: setDiff,
      onError: (e) => notifications.show({ color: 'red', title: 'Sync preview failed', message: errorMessage(e) }),
    });

  return (
    <QueryState query={q} notFound="Test plan not found">
      {() => {
        const v = q.data;
        if (!v) return null;
        const plan = v.value;
        const readOnly = !canWrite || plan.archived;
        const rename = (name: string) =>
          update.mutate(
            { patch: { name }, etag: v.etag },
            {
              onSuccess: () => setRenaming(null),
              onError: (e) => notifications.show({ color: 'red', title: 'Not renamed', message: isConflict(e) ? 'The plan changed meanwhile; reload the page.' : errorMessage(e) }),
            },
          );
        return (
          <>
            <PageHeader
              crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Test plans', to: `/p/${key}/plans` }, { label: plan.name }]}
              title={
                renaming !== null ? (
                  <form
                    onSubmit={(e) => {
                      e.preventDefault();
                      if (renaming.trim()) rename(renaming.trim());
                    }}
                    style={{ display: 'flex', gap: 6 }}
                  >
                    <TextInput aria-label="Plan name" value={renaming} onChange={(e) => setRenaming(e.currentTarget.value)} maxLength={200} autoFocus w="min(520px, 70vw)" />
                    <Button type="submit" size="sm" variant="light" aria-label="Save the name" loading={update.isPending}>
                      <IconCheck size={14} />
                    </Button>
                    <Button size="sm" variant="default" aria-label="Cancel renaming" onClick={() => setRenaming(null)}>
                      <IconX size={14} />
                    </Button>
                  </form>
                ) : (
                  plan.name
                )
              }
              badges={
                <>
                  <Badge variant="light" radius="sm" data-testid="plan-count-total">
                    {plan.counts.total} cases
                  </Badge>
                  <Badge variant="outline" color="gray" radius="sm" data-testid="plan-count-manual">
                    {plan.counts.manual} manual
                  </Badge>
                  <Badge variant="outline" color="gray" radius="sm" data-testid="plan-count-automated">
                    {plan.counts.automated} automated
                  </Badge>
                  {plan.archived && (
                    <Badge color="red" variant="light" radius="sm">
                      archived
                    </Badge>
                  )}
                </>
              }
              subtitle={
                <Stack gap={2}>
                  {plan.description && <Text size="sm">{plan.description}</Text>}
                  <Text size="xs" c="dimmed" data-testid="plan-filter">
                    {plan.filter ? `Filter: ${describeFilter(plan.filter, (fid) => folderPathLabel(folders.data, fid))}` : 'Chosen by hand'} · revision {plan.revision} · updated {formatRelative(plan.updatedAt ?? plan.createdAt)}
                  </Text>
                </Stack>
              }
              actions={
                <>
                  {canRun && !plan.archived && (
                    <Button size="xs" leftSection={<IconPlayerPlay size={14} />} onClick={() => setDialog('start')} disabled={plan.counts.total === 0} data-testid="plan-start">
                      Start launch
                    </Button>
                  )}
                  {!readOnly && (
                    <Button size="xs" variant="default" leftSection={<IconFilter size={14} />} onClick={() => setDialog('filter')} data-testid="plan-filter-open">
                      {plan.filter ? 'Edit filter' : 'Add by filter'}
                    </Button>
                  )}
                  {!readOnly && plan.filter && (
                    <Button size="xs" variant="default" leftSection={<IconRefresh size={14} />} onClick={preview} loading={sync.isPending} data-testid="plan-sync">
                      Sync
                    </Button>
                  )}
                  <Menu withinPortal position="bottom-end">
                    <Menu.Target>
                      <Button size="xs" variant="default" px={8} aria-label="More plan actions">
                        <IconDots size={14} />
                      </Button>
                    </Menu.Target>
                    <Menu.Dropdown>
                      <Menu.Item component="a" href={planTestplanUrl(key, plan.id)} download="testplan.json" leftSection={<IconDownload size={14} />} disabled={plan.counts.automated === 0}>
                        testplan.json (automated cases)
                      </Menu.Item>
                      {!readOnly && (
                        <Menu.Item leftSection={<IconPencil size={14} />} onClick={() => setRenaming(plan.name)}>
                          Rename
                        </Menu.Item>
                      )}
                      {!readOnly && (
                        <Menu.Item
                          color="red"
                          leftSection={<IconArchive size={14} />}
                          onClick={() =>
                            archive.mutate(undefined, {
                              onSuccess: () => {
                                notifications.show({ color: 'green', title: 'Plan archived', message: plan.name });
                                void navigate(`/p/${key}/plans`);
                              },
                            })
                          }
                        >
                          Archive
                        </Menu.Item>
                      )}
                    </Menu.Dropdown>
                  </Menu>
                </>
              }
            />
            <QueryState query={items}>
              {() => (
                <div className="fp-tm-plan">
                  {!readOnly && (
                    <aside className="fp-tm-plan-tree" aria-label="Choose cases">
                      <PlanTree project={key} planId={plan.id} folders={folders.data ?? []} items={items.data ?? []} readOnly={readOnly} />
                    </aside>
                  )}
                  <section style={{ minWidth: 0 }} aria-label="Plan composition">
                    <PlanItems project={key} planId={plan.id} items={items.data ?? []} folders={folders.data ?? []} people={people} readOnly={readOnly} />
                  </section>
                </div>
              )}
            </QueryState>
            {dialog === 'filter' && (
              <FilterDialog
                project={key}
                plan={plan}
                etag={v.etag}
                folders={folders.data ?? []}
                onClose={() => setDialog(null)}
                onSaved={(d) => {
                  setDialog(null);
                  setDiff(d);
                }}
              />
            )}
            {dialog === 'start' && <StartLaunchDialog project={key} plan={plan} onClose={() => setDialog(null)} />}
            {diff && <SyncDialog project={key} plan={plan} items={items.data ?? []} diff={diff} onClose={() => setDiff(null)} />}
          </>
        );
      }}
    </QueryState>
  );
}
