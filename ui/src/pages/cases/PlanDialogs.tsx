import { Anchor, Button, Chip, Group, Modal, SegmentedControl, Select, Stack, Switch, TagsInput, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconRefresh } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useSyncTestPlan, useUpdateTestPlan, type CaseFilter, type CaseFolder, type PlanSyncDiff, type TestPlan, type TestPlanItem } from '../../api/tm';
import { CASE_STATUSES, CASE_STATUS_LABEL, caseLabel } from '../../lib/tm/filters';
import { QueryBar } from './QueryBar';
import { caseLink, folderPathLabel } from '../../lib/tm/ui';

export function SyncDialog({ project, plan, items, diff, onClose }: { project: string; plan: TestPlan; items: TestPlanItem[]; diff: PlanSyncDiff; onClose: () => void }) {
  const sync = useSyncTestPlan(project, plan.id);
  const names = new Map(items.map((i) => [i.caseNumber, i.name] as const));
  const none = !diff.added.length && !diff.removed.length;
  return (
    <Modal opened onClose={onClose} title="Sync with the filter" size="lg" attributes={{ content: { 'data-testid': 'sync-dialog' } }}>
      <Stack gap="sm">
        {none ? (
          <Text size="sm">The plan already matches its filter.</Text>
        ) : (
          <>
            <Text size="sm">Applying the filter changes the plan like this:</Text>
            <Stack gap={4}>
              <Text size="sm" fw={600} c="var(--fp-diff-add-fg)" data-testid="sync-added">
                + {diff.added.length} added
              </Text>
              {diff.added.slice(0, 50).map((n) => (
                <Anchor key={n} component={Link} to={caseLink(project, n)} size="sm" className="fp-mono" target="_blank">
                  + {caseLabel(n)}
                </Anchor>
              ))}
              <Text size="sm" fw={600} c="var(--fp-diff-remove-fg)" data-testid="sync-removed">
                − {diff.removed.length} removed (added by the filter, no longer match)
              </Text>
              {diff.removed.slice(0, 50).map((n) => (
                <Text key={n} size="sm">
                  − <span className="fp-mono">{caseLabel(n)}</span> {names.get(n) ?? ''}
                </Text>
              ))}
            </Stack>
            <Text size="xs" c="dimmed">
              Cases you added by hand stay.
            </Text>
          </>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {none ? 'Close' : 'Cancel'}
          </Button>
          {!none && (
            <Button
              leftSection={<IconRefresh size={14} />}
              loading={sync.isPending}
              data-testid="sync-apply"
              onClick={() =>
                sync.mutate(false, {
                  onSuccess: (d) => {
                    notifications.show({ color: 'green', title: 'Plan synced', message: `+${String(d.added.length)} / −${String(d.removed.length)}` });
                    onClose();
                  },
                  onError: (e) => notifications.show({ color: 'red', title: 'Not synced', message: errorMessage(e) }),
                })
              }
            >
              Apply
            </Button>
          )}
        </Group>
      </Stack>
    </Modal>
  );
}

/** "Add by filter": saves the plan's structured filter and shows the sync diff before applying it. */
export function FilterDialog({
  project,
  plan,
  etag,
  folders,
  onSaved,
  onClose,
}: {
  project: string;
  plan: TestPlan;
  etag: string;
  folders: CaseFolder[];
  onSaved: (diff: PlanSyncDiff) => void;
  onClose: () => void;
}) {
  const updatePlan = useUpdateTestPlan(project, plan.id);
  const sync = useSyncTestPlan(project, plan.id);
  const f = plan.filter ?? {};
  const [folder, setFolder] = useState<string | null>(f.folderIds?.[0] ?? null);
  const [recursive, setRecursive] = useState(f.recursive !== false);
  const [statuses, setStatuses] = useState<string[]>(f.statuses ?? ['active']);
  const [automation, setAutomation] = useState<string>(f.automation ?? 'all');
  const [tags, setTags] = useState<string[]>(f.tags ?? []);
  const [q, setQ] = useState(f.q ?? '');
  const [mode, setMode] = useState<'filter' | 'query'>(plan.query ? 'query' : 'filter');
  const [query, setQuery] = useState(plan.query ?? '');
  const [validity, setValidity] = useState<'empty' | 'checking' | 'ok' | 'invalid'>('empty');
  const folderData = useMemo(() => folders.map((x) => ({ value: x.id, label: folderPathLabel(folders, x.id) })).sort((a, b) => a.label.localeCompare(b.label)), [folders]);
  const filter: CaseFilter = {
    ...(folder ? { folderIds: [folder], recursive } : {}),
    ...(statuses.length ? { statuses: statuses as CaseFilter['statuses'] } : {}),
    ...(automation === 'manual' || automation === 'automated' ? { automation } : {}),
    ...(tags.length ? { tags } : {}),
    ...(q.trim() ? { q: q.trim() } : {}),
  };
  const save = async () => {
    try {
      await updatePlan.mutateAsync({ patch: mode === 'query' ? { query: query.trim(), filter: null } : { filter, query: null }, etag });
      const diff = await sync.mutateAsync(true);
      onSaved(diff);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Filter not saved', message: errorMessage(e) });
    }
  };
  return (
    <Modal opened onClose={onClose} title="Add cases by a filter" size="lg" attributes={{ content: { 'data-testid': 'filter-dialog' } }}>
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          The plan keeps this filter. Sync adds new matching cases and removes the ones the filter added that no longer match.
        </Text>
        <SegmentedControl
          size="xs"
          value={mode}
          onChange={(v) => setMode(v === 'query' ? 'query' : 'filter')}
          data={[
            { value: 'filter', label: 'Filter' },
            { value: 'query', label: 'Query' },
          ]}
          aria-label="Filter mode"
          w="fit-content"
          data-testid="filter-dialog-mode"
        />
        {mode === 'query' ? (
          <QueryBar project={project} value={query} onChange={setQuery} onApply={() => query.trim() && validity !== 'invalid' && void save()} applyLabel={null} onValidity={setValidity} />
        ) : (
          <>
            <Group gap="sm" wrap="wrap" align="flex-end">
              <Select label="Folder" data={folderData} value={folder} onChange={setFolder} searchable clearable w={320} comboboxProps={{ withinPortal: true }} data-testid="filter-folder" />
              <Switch label="Include subfolders" checked={recursive} onChange={(e) => setRecursive(e.currentTarget.checked)} />
            </Group>
            <Chip.Group multiple value={statuses} onChange={setStatuses}>
              <Group gap={6}>
                {CASE_STATUSES.map((s) => (
                  <Chip key={s} value={s} size="xs" variant="outline">
                    {CASE_STATUS_LABEL[s]}
                  </Chip>
                ))}
              </Group>
            </Chip.Group>
            <SegmentedControl
              size="xs"
              value={automation}
              onChange={setAutomation}
              data={[
                { value: 'all', label: 'Manual and automated' },
                { value: 'manual', label: 'Manual' },
                { value: 'automated', label: 'Automated' },
              ]}
              aria-label="Automation"
              w="fit-content"
            />
            <TagsInput label="All of these tags" value={tags} onChange={setTags} />
            <TextInput label="Name contains" value={q} onChange={(e) => setQ(e.currentTarget.value)} />
          </>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={() => void save()} loading={updatePlan.isPending || sync.isPending} disabled={mode === 'query' && (!query.trim() || validity === 'invalid')} data-testid="filter-save">
            Save filter and preview
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}
