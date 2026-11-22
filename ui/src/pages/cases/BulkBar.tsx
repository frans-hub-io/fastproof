import { Button, Group, Menu, Modal, MultiSelect, NumberInput, Paper, Select, Stack, TagsInput, Text, TextInput } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconArrowBackUp, IconChevronDown, IconFolderShare, IconListCheck, IconTag, IconX } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage, projectPath, request } from '../../api/client';
import { useBulkCases, useCaseFolders, useManagedPlans, type CaseBulk, type CaseFieldDef, type FieldValue } from '../../api/tm';
import { CASE_STATUSES, CASE_STATUS_LABEL } from '../../lib/tm/filters';
import { FolderPicker } from './common';

const SKIP_TEXT: Record<string, string> = { ai_self_review: 'AI drafts you created need another person', field_from_code: 'field set from code' };

function FieldValueInput({ def, value, onChange }: { def: CaseFieldDef; value: FieldValue; onChange: (v: FieldValue) => void }) {
  const opts = def.options.filter((o) => !o.archived).map((o) => o.value);
  switch (def.type) {
    case 'select':
      return <Select label="Value" data={opts} value={typeof value === 'string' ? value : null} onChange={(v) => onChange(v)} comboboxProps={{ withinPortal: true }} clearable />;
    case 'multiselect':
      return <MultiSelect label="Values" data={opts} value={Array.isArray(value) ? value : []} onChange={onChange} comboboxProps={{ withinPortal: true }} />;
    case 'number':
      return <NumberInput label="Value" value={typeof value === 'number' ? value : ''} onChange={(v) => onChange(typeof v === 'number' ? v : null)} />;
    case 'date':
      return <TextInput label="Value" type="date" value={typeof value === 'string' ? value : ''} onChange={(e) => onChange(e.currentTarget.value || null)} />;
    default:
      return <TextInput label="Value" value={typeof value === 'string' ? value : ''} onChange={(e) => onChange(e.currentTarget.value || null)} />;
  }
}

type Dialog = 'move' | 'field' | 'addTags' | 'removeTags' | 'plan' | null;

export function BulkBar({ project, numbers, fields, archived, onDone }: { project: string; numbers: number[]; fields: CaseFieldDef[]; archived: boolean; onDone: () => void }) {
  const bulk = useBulkCases(project);
  const folders = useCaseFolders(project);
  const plans = useManagedPlans(project, {});
  const [dialog, setDialog] = useState<Dialog>(null);
  const [fieldKey, setFieldKey] = useState<string | null>(null);
  const [fieldValue, setFieldValue] = useState<FieldValue>(null);
  const [tags, setTags] = useState<string[]>([]);
  const [planId, setPlanId] = useState<string | null>(null);
  const [planBusy, setPlanBusy] = useState(false);
  const editable = fields.filter((f) => !f.archived);
  const def = editable.find((f) => f.key === fieldKey);

  const run = (body: Omit<CaseBulk, 'numbers'>, label: string) =>
    bulk.mutate(
      { ...body, numbers },
      {
        onSuccess: (r) => {
          const skipped = r.skipped.filter((s) => s.code !== 'unchanged');
          notifications.show({
            color: skipped.length ? 'yellow' : 'green',
            title: label,
            message: `${String(r.updated)} updated${skipped.length ? `, ${String(skipped.length)} skipped (${[...new Set(skipped.map((s) => SKIP_TEXT[s.code] ?? s.code))].join(', ')})` : ''}`,
          });
          setDialog(null);
          onDone();
        },
        onError: (e) => notifications.show({ color: 'red', title: `${label} failed`, message: errorMessage(e) }),
      },
    );

  const addToPlan = async () => {
    if (!planId) return;
    setPlanBusy(true);
    try {
      await request(`${projectPath(project)}/test-plans/${encodeURIComponent(planId)}/items`, { method: 'POST', json: { add: numbers } });
      notifications.show({ color: 'green', title: 'Added to the plan', message: `${String(numbers.length)} cases` });
      setDialog(null);
      onDone();
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not added', message: errorMessage(e) });
    } finally {
      setPlanBusy(false);
    }
  };

  return (
    <Paper withBorder radius="md" p="xs" className="fp-tm-bulkbar" data-testid="bulk-bar" role="region" aria-label="Bulk actions">
      <Group gap="xs" wrap="wrap">
        <Text size="sm" fw={600} aria-live="polite">
          {numbers.length} selected
        </Text>
        <Button size="compact-sm" variant="default" leftSection={<IconFolderShare size={14} />} onClick={() => setDialog('move')} data-testid="bulk-move">
          Move…
        </Button>
        <Menu withinPortal>
          <Menu.Target>
            <Button size="compact-sm" variant="default" rightSection={<IconChevronDown size={12} />} data-testid="bulk-status">
              Status
            </Button>
          </Menu.Target>
          <Menu.Dropdown>
            {CASE_STATUSES.map((s) => (
              <Menu.Item key={s} onClick={() => run({ action: 'setStatus', value: s }, `Status set to ${CASE_STATUS_LABEL[s]}`)}>
                {CASE_STATUS_LABEL[s]}
              </Menu.Item>
            ))}
          </Menu.Dropdown>
        </Menu>
        <Button size="compact-sm" variant="default" onClick={() => setDialog('field')} data-testid="bulk-field">
          Set field…
        </Button>
        <Menu withinPortal>
          <Menu.Target>
            <Button size="compact-sm" variant="default" leftSection={<IconTag size={14} />} rightSection={<IconChevronDown size={12} />} data-testid="bulk-tags">
              Tags
            </Button>
          </Menu.Target>
          <Menu.Dropdown>
            <Menu.Item onClick={() => setDialog('addTags')}>Add tags…</Menu.Item>
            <Menu.Item onClick={() => setDialog('removeTags')}>Remove tags…</Menu.Item>
          </Menu.Dropdown>
        </Menu>
        <Button size="compact-sm" variant="default" leftSection={<IconListCheck size={14} />} onClick={() => setDialog('plan')} data-testid="bulk-plan">
          Add to plan…
        </Button>
        {archived ? (
          <Button size="compact-sm" variant="default" leftSection={<IconArrowBackUp size={14} />} onClick={() => run({ action: 'restore' }, 'Restored')} loading={bulk.isPending} data-testid="bulk-restore">
            Restore
          </Button>
        ) : (
          <Button size="compact-sm" variant="default" color="red" leftSection={<IconArchive size={14} />} onClick={() => run({ action: 'archive' }, 'Moved to the trash')} loading={bulk.isPending} data-testid="bulk-archive">
            Trash
          </Button>
        )}
        <Button size="compact-sm" variant="subtle" leftSection={<IconX size={14} />} onClick={onDone}>
          Clear
        </Button>
      </Group>
      <FolderPicker opened={dialog === 'move'} onClose={() => setDialog(null)} folders={folders.data ?? []} busy={bulk.isPending} onPick={(id) => run({ action: 'move', value: id }, 'Moved')} title={`Move ${String(numbers.length)} cases to`} />
      <Modal opened={dialog === 'field'} onClose={() => setDialog(null)} title={`Set a field on ${String(numbers.length)} cases`}>
        <Stack gap="sm">
          <Select label="Field" data={editable.map((f) => ({ value: f.key, label: f.name }))} value={fieldKey} onChange={(v) => { setFieldKey(v); setFieldValue(null); }} comboboxProps={{ withinPortal: true }} />
          {def && <FieldValueInput def={def} value={fieldValue} onChange={setFieldValue} />}
          <Text size="xs" c="dimmed">
            Automated cases whose field comes from code are skipped.
          </Text>
          <Group justify="flex-end">
            <Button disabled={!def} loading={bulk.isPending} onClick={() => def && run({ action: 'setField', value: { key: def.key, value: fieldValue } }, `${def.name} set`)}>
              Apply
            </Button>
          </Group>
        </Stack>
      </Modal>
      <Modal opened={dialog === 'addTags' || dialog === 'removeTags'} onClose={() => setDialog(null)} title={dialog === 'addTags' ? 'Add tags' : 'Remove tags'}>
        <Stack gap="sm">
          <TagsInput label="Tags" value={tags} onChange={(v) => setTags(v.slice(0, 50))} data-autofocus />
          <Group justify="flex-end">
            <Button disabled={!tags.length} loading={bulk.isPending} onClick={() => run({ action: dialog === 'addTags' ? 'addTags' : 'removeTags', value: tags }, dialog === 'addTags' ? 'Tags added' : 'Tags removed')}>
              Apply
            </Button>
          </Group>
        </Stack>
      </Modal>
      <Modal opened={dialog === 'plan'} onClose={() => setDialog(null)} title={`Add ${String(numbers.length)} cases to a plan`}>
        <Stack gap="sm">
          <Select label="Test plan" data={(plans.data ?? []).map((p) => ({ value: p.id, label: p.name }))} value={planId} onChange={setPlanId} searchable comboboxProps={{ withinPortal: true }} data-testid="bulk-plan-select" />
          <Group justify="flex-end">
            <Button disabled={!planId} loading={planBusy} onClick={() => void addToPlan()} data-testid="bulk-plan-add">
              Add
            </Button>
          </Group>
        </Stack>
      </Modal>
    </Paper>
  );
}
