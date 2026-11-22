import { Button, Chip, CloseButton, Group, Modal, MultiSelect, Pill, SegmentedControl, Select, Stack, Switch, TagsInput, Text, TextInput } from '@mantine/core';
import { useDebouncedValue } from '@mantine/hooks';
import { IconFilter, IconSearch } from '@tabler/icons-react';
import { useEffect, useState } from 'react';
import type { CaseFieldDef } from '../../api/tm';
import { CASE_STATUSES, CASE_STATUS_LABEL, type CaseListState } from '../../lib/tm/filters';

const STALE: { value: string; label: string }[] = [
  { value: '', label: 'Any time' },
  { value: '7', label: 'Not run for 7 days' },
  { value: '30', label: 'Not run for 30 days' },
  { value: '90', label: 'Not run for 90 days' },
];

function FieldFilterDialog({ fields, onAdd, onClose }: { fields: CaseFieldDef[]; onAdd: (values: string[]) => void; onClose: () => void }) {
  const usable = fields.filter((f) => !f.archived && (f.type === 'select' || f.type === 'multiselect' || f.type === 'text'));
  const [key, setKey] = useState<string | null>(usable[0]?.key ?? null);
  const def = usable.find((f) => f.key === key);
  const [values, setValues] = useState<string[]>([]);
  const [text, setText] = useState('');
  const chosen = def?.type === 'text' ? (text.trim() ? [text.trim()] : []) : values;
  return (
    <Modal opened onClose={onClose} title="Filter by a field">
      <Stack gap="sm">
        <Select label="Field" data={usable.map((f) => ({ value: f.key, label: f.name }))} value={key} onChange={(v) => { setKey(v); setValues([]); }} allowDeselect={false} comboboxProps={{ withinPortal: true }} data-testid="field-filter-key" />
        {def?.type === 'text' ? (
          <TextInput label="Value" value={text} onChange={(e) => setText(e.currentTarget.value)} data-testid="field-filter-text" />
        ) : (
          <MultiSelect label="Any of" data={(def?.options ?? []).filter((o) => !o.archived).map((o) => o.value)} value={values} onChange={setValues} searchable comboboxProps={{ withinPortal: true }} data-testid="field-filter-values" />
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!def || !chosen.length} onClick={() => def && onAdd(chosen.map((v) => `${def.key}:${v}`))} data-testid="field-filter-add">
            Add filter
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

export function CaseFilters({ state, fields, onChange }: { state: CaseListState; fields: CaseFieldDef[]; onChange: (s: Partial<CaseListState>) => void }) {
  const [q, setQ] = useState(state.q);
  const [debounced] = useDebouncedValue(q, 300);
  const [fieldDialog, setFieldDialog] = useState(false);
  useEffect(() => {
    if (debounced !== state.q) onChange({ q: debounced });
  }, [debounced]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setQ(state.q);
  }, [state.q]);
  const label = (f: string) => {
    const i = f.indexOf(':');
    const def = fields.find((d) => d.key === f.slice(0, i));
    return `${def?.name ?? f.slice(0, i)}: ${f.slice(i + 1) || '—'}`;
  };
  return (
    <Stack gap="xs" data-testid="case-filters">
      <Group gap="xs" wrap="wrap" align="center">
        <TextInput aria-label="Search cases" placeholder="Search by name or TC-number" leftSection={<IconSearch size={16} />} value={q} onChange={(e) => setQ(e.currentTarget.value)} style={{ flex: '1 1 220px' }} data-testid="case-search" />
        <SegmentedControl
          size="xs"
          value={state.automation ?? 'all'}
          onChange={(v) => onChange({ automation: v === 'manual' || v === 'automated' ? v : null })}
          data={[
            { value: 'all', label: 'All' },
            { value: 'manual', label: 'Manual' },
            { value: 'automated', label: 'Automated' },
          ]}
          aria-label="Automation"
        />
      </Group>
      <Group gap={6} wrap="wrap" align="center">
        <Chip.Group multiple value={state.statuses} onChange={(v) => onChange({ statuses: v.filter((s): s is CaseListState['statuses'][number] => (CASE_STATUSES as readonly string[]).includes(s)) })}>
          {CASE_STATUSES.map((s) => (
            <Chip key={s} value={s} size="xs" variant="outline" data-testid={`chip-status-${s}`}>
              {CASE_STATUS_LABEL[s]}
            </Chip>
          ))}
        </Chip.Group>
        <Chip size="xs" variant="outline" checked={state.aiDraft} onChange={(v) => onChange({ aiDraft: v })} data-testid="chip-ai-draft">
          AI draft
        </Chip>
        <Select size="xs" aria-label="Last run" data={STALE} value={state.staleDays ? String(state.staleDays) : ''} onChange={(v) => onChange({ staleDays: v ? Number(v) : null })} w={170} allowDeselect={false} comboboxProps={{ withinPortal: true }} />
        <TagsInput size="xs" aria-label="Tags" placeholder={state.tags.length ? '' : 'Tags'} value={state.tags} onChange={(v) => onChange({ tags: v.slice(0, 20) })} w={180} clearable />
        <Button size="compact-xs" variant="default" leftSection={<IconFilter size={12} />} onClick={() => setFieldDialog(true)} data-testid="field-filter-open">
          Field
        </Button>
        <Switch size="xs" label="Trash" checked={state.archived} onChange={(e) => onChange({ archived: e.currentTarget.checked })} data-testid="toggle-archived" />
      </Group>
      {state.fields.length > 0 && (
        <Group gap={4} wrap="wrap">
          <Text size="xs" c="dimmed">
            Fields:
          </Text>
          {state.fields.map((f) => (
            <Pill key={f} withRemoveButton onRemove={() => onChange({ fields: state.fields.filter((x) => x !== f) })} removeButtonProps={{ 'aria-label': `Remove filter ${label(f)}` }} data-testid="field-filter-pill">
              {label(f)}
            </Pill>
          ))}
          <CloseButton size="xs" aria-label="Clear field filters" onClick={() => onChange({ fields: [] })} />
        </Group>
      )}
      {fieldDialog && (
        <FieldFilterDialog
          fields={fields}
          onClose={() => setFieldDialog(false)}
          onAdd={(values) => {
            onChange({ fields: [...new Set([...state.fields, ...values])] });
            setFieldDialog(false);
          }}
        />
      )}
    </Stack>
  );
}
