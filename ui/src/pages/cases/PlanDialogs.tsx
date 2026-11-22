import { Alert, Anchor, Button, Chip, Code, Group, Modal, SegmentedControl, Select, Stack, Switch, TagsInput, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconPlayerPlay, IconRefresh } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useStartPlanLaunch, useSyncTestPlan, useUpdateTestPlan, type CaseFilter, type CaseFolder, type PlanLaunch, type PlanSyncDiff, type TestPlan, type TestPlanItem } from '../../api/tm';
import { CopyValue } from '../../components/CopyButton';
import { CASE_STATUSES, CASE_STATUS_LABEL, caseLabel } from '../../lib/tm/filters';
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
export function FilterDialog({ project, plan, etag, folders, onSaved, onClose }: { project: string; plan: TestPlan; etag: string; folders: CaseFolder[]; onSaved: (diff: PlanSyncDiff) => void; onClose: () => void }) {
  const updatePlan = useUpdateTestPlan(project, plan.id);
  const sync = useSyncTestPlan(project, plan.id);
  const f = plan.filter ?? {};
  const [folder, setFolder] = useState<string | null>(f.folderIds?.[0] ?? null);
  const [recursive, setRecursive] = useState(f.recursive !== false);
  const [statuses, setStatuses] = useState<string[]>(f.statuses ?? ['active']);
  const [automation, setAutomation] = useState<string>(f.automation ?? 'all');
  const [tags, setTags] = useState<string[]>(f.tags ?? []);
  const [q, setQ] = useState(f.q ?? '');
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
      await updatePlan.mutateAsync({ patch: { filter }, etag });
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
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={() => void save()} loading={updatePlan.isPending || sync.isPending} data-testid="filter-save">
            Save filter and preview
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

export function StartLaunchDialog({ project, plan, onClose }: { project: string; plan: TestPlan; onClose: () => void }) {
  const start = useStartPlanLaunch(project, plan.id);
  const [name, setName] = useState(`${plan.name} ${new Date().toISOString().slice(0, 10)}`);
  const [envKey, setEnvKey] = useState('staging');
  const [assign, setAssign] = useState<'plan' | 'me' | 'none'>('plan');
  const [includeAutomated, setIncludeAutomated] = useState(plan.counts.automated > 0);
  const [externalId, setExternalId] = useState(() => `plan-${plan.id.slice(0, 8)}-${String(Date.now()).slice(-6)}`);
  const [result, setResult] = useState<PlanLaunch | null>(null);
  const command = `fastproof testplan --launch ${externalId} > testplan.json\nexport ALLURE_TESTPLAN_PATH="$PWD/testplan.json"`;

  if (result) {
    const l = result.launch;
    return (
      <Modal opened onClose={onClose} title="Launch started" size="lg" attributes={{ content: { 'data-testid': 'launch-started' } }}>
        <Stack gap="sm">
          <Text size="sm">
            Launch{' '}
            <Anchor component={Link} to={`/p/${project}/launches/${l.id}`}>
              #{l.number} {l.name}
            </Anchor>{' '}
            holds the manual cases of the plan.
          </Text>
          {includeAutomated && (
            <Stack gap={4}>
              <Text size="sm" fw={600}>
                Run the automated cases in CI
              </Text>
              <Text size="xs" c="dimmed">
                Allure plugins that read <Code>ALLURE_TESTPLAN_PATH</Code> run only the plan's tests; their results join this launch.
              </Text>
              <Group gap={4} wrap="nowrap" align="flex-start">
                <Code block style={{ flex: 1, minWidth: 0, whiteSpace: 'pre-wrap', wordBreak: 'break-all' }} data-testid="testplan-command">
                  {result.ciHint || command}
                </Code>
                <CopyValue value={result.ciHint || command} label="Copy the command" />
              </Group>
            </Stack>
          )}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Close
            </Button>
            <Button component={Link} to={`/p/${project}/launches/${l.id}/run`} leftSection={<IconPlayerPlay size={14} />} data-testid="go-run">
              Start testing
            </Button>
          </Group>
        </Stack>
      </Modal>
    );
  }

  return (
    <Modal opened onClose={onClose} title={`Start a launch of ${plan.name}`} size="lg" attributes={{ content: { 'data-testid': 'start-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          start.mutate(
            { name: name.trim(), externalId, environment: envKey.trim() ? { env: envKey.trim() } : undefined, assign, includeAutomated },
            { onSuccess: setResult, onError: (err) => notifications.show({ color: 'red', title: 'Launch not started', message: errorMessage(err) }) },
          );
        }}
      >
        <Stack gap="sm">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.currentTarget.value)} required maxLength={200} data-testid="start-name" />
          <TextInput label="Environment" value={envKey} onChange={(e) => setEnvKey(e.currentTarget.value)} description="Shown on the launch, e.g. staging or prod-eu" />
          <SegmentedControl
            value={assign}
            onChange={(v) => setAssign(v === 'me' || v === 'none' ? v : 'plan')}
            data={[
              { value: 'plan', label: 'Assignees from the plan' },
              { value: 'me', label: 'Me' },
              { value: 'none', label: 'Nobody' },
            ]}
            aria-label="Assign the cases to"
            fullWidth
          />
          <Switch label={`Include automated cases (${String(plan.counts.automated)})`} checked={includeAutomated} onChange={(e) => setIncludeAutomated(e.currentTarget.checked)} disabled={plan.counts.automated === 0} data-testid="start-automated" />
          {includeAutomated && (
            <Stack gap={4}>
              <TextInput label="External id for CI" value={externalId} onChange={(e) => setExternalId(e.currentTarget.value.replace(/[^A-Za-z0-9._-]/g, ''))} maxLength={128} />
              <Text size="xs" c="dimmed">
                After the start, CI runs:
              </Text>
              <Code block data-testid="testplan-command-preview" style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
                {command}
              </Code>
            </Stack>
          )}
          {plan.counts.manual === 0 && <Alert color="yellow">The plan has no manual cases; the launch only waits for CI results.</Alert>}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={start.isPending} disabled={!name.trim()} leftSection={<IconPlayerPlay size={14} />} data-testid="start-submit">
              Start
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}
