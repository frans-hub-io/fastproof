import { ActionIcon, Alert, Badge, Button, Card, Checkbox, ColorInput, Group, Modal, MultiSelect, NumberInput, Select, Stack, Switch, Text, TextInput, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconArchive, IconArrowDown, IconArrowBackUp, IconArrowUp, IconPencil, IconPlus, IconTrash } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useAllCaseFields, useDeleteCaseField, useSaveCaseField, type CaseFieldDef, type CaseFieldWrite, type FieldOption, type FieldType, type FieldValue } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { fieldText } from '../lib/tm/versionDiff';

const TYPE_LABEL: Record<FieldType, string> = { select: 'Select', multiselect: 'Multi-select', text: 'Text', number: 'Number', date: 'Date' };
const KEY_RE = /^[a-z][a-z0-9_]{0,62}$/;
const COLOR_RE = /^#[0-9a-fA-F]{6}$/;
const MAX_OPTIONS = 200;
const SHOWN_OPTIONS = 8;

interface OptionDraft extends FieldOption {
  /** Saved options keep their value: cases store it. */
  saved: boolean;
}

interface Draft {
  key: string;
  name: string;
  type: FieldType;
  required: boolean;
  sourceLabel: string;
  defaultValue: FieldValue;
  options: OptionDraft[];
}

function draftOf(f: CaseFieldDef | null): Draft {
  if (!f) return { key: '', name: '', type: 'select', required: false, sourceLabel: '', defaultValue: null, options: [] };
  return { key: f.key, name: f.name, type: f.type, required: f.required, sourceLabel: f.sourceLabel ?? '', defaultValue: f.defaultValue, options: f.options.map((o) => ({ ...o, saved: true })) };
}

function isSelect(t: FieldType): boolean {
  return t === 'select' || t === 'multiselect';
}

function draftErrors(d: Draft, others: readonly CaseFieldDef[], creating: boolean): Record<string, string | null> {
  const values = d.options.map((o) => o.value.trim());
  const dup = values.find((v, i) => v && values.indexOf(v) !== i);
  return {
    key: creating ? (!KEY_RE.test(d.key) ? 'Lowercase letters, digits and _; starts with a letter' : others.some((f) => f.key === d.key) ? 'Another field has this key' : null) : null,
    name: !d.name.trim() ? 'Required' : d.name.length > 200 ? 'At most 200 characters' : null,
    sourceLabel: d.sourceLabel.length > 200 ? 'At most 200 characters' : d.sourceLabel && others.some((f) => !f.archived && f.sourceLabel === d.sourceLabel.trim()) ? 'Another field is mapped to this label' : null,
    options: isSelect(d.type) ? (values.some((v) => !v) ? 'Every option needs a value' : dup ? `"${dup}" is listed twice` : d.options.length > MAX_OPTIONS ? `At most ${String(MAX_OPTIONS)} options` : d.options.some((o) => o.color && !COLOR_RE.test(o.color)) ? 'Colors look like #rrggbb' : null) : null,
  };
}

function defaultOf(d: Draft): FieldValue {
  const v = d.defaultValue;
  if (v === null || v === '') return null;
  const active = new Set(d.options.filter((o) => !o.archived).map((o) => o.value.trim()));
  if (d.type === 'select') return typeof v === 'string' && active.has(v) ? v : null;
  if (d.type === 'multiselect') {
    const list = Array.isArray(v) ? v.filter((x) => active.has(x)) : [];
    return list.length ? list : null;
  }
  return v;
}

function bodyOf(d: Draft): Omit<CaseFieldWrite, 'key' | 'type'> {
  return {
    name: d.name.trim(),
    required: d.required,
    sourceLabel: d.sourceLabel.trim() || null,
    defaultValue: defaultOf(d),
    ...(isSelect(d.type) ? { options: d.options.map(({ value, color, archived }) => ({ value: value.trim(), color: color || null, archived })) } : {}),
  };
}

function DefaultInput({ d, onChange }: { d: Draft; onChange: (v: FieldValue) => void }) {
  const opts = d.options.filter((o) => !o.archived && o.value.trim()).map((o) => o.value.trim());
  const common = { label: 'Default value', description: 'Set on new cases', 'data-testid': 'field-default' };
  switch (d.type) {
    case 'select':
      return <Select {...common} data={[...new Set(opts)]} value={typeof d.defaultValue === 'string' ? d.defaultValue : null} onChange={onChange} clearable comboboxProps={{ withinPortal: true }} />;
    case 'multiselect':
      return <MultiSelect {...common} data={[...new Set(opts)]} value={Array.isArray(d.defaultValue) ? d.defaultValue : []} onChange={(v) => onChange(v.length ? v : null)} comboboxProps={{ withinPortal: true }} />;
    case 'number':
      return <NumberInput {...common} value={typeof d.defaultValue === 'number' ? d.defaultValue : ''} onChange={(v) => onChange(typeof v === 'number' ? v : null)} />;
    case 'date':
      return <TextInput {...common} type="date" value={typeof d.defaultValue === 'string' ? d.defaultValue : ''} onChange={(e) => onChange(e.currentTarget.value || null)} />;
    default:
      return <TextInput {...common} value={typeof d.defaultValue === 'string' ? d.defaultValue : ''} onChange={(e) => onChange(e.currentTarget.value || null)} maxLength={500} />;
  }
}

function OptionsEditor({ d, onChange, error }: { d: Draft; onChange: (o: OptionDraft[]) => void; error: string | null }) {
  const set = (i: number, patch: Partial<OptionDraft>) => onChange(d.options.map((o, j) => (j === i ? { ...o, ...patch } : o)));
  return (
    <Stack gap={6} data-testid="field-options">
      <Text size="sm" fw={500}>
        Options
      </Text>
      {d.options.length === 0 && (
        <Text size="xs" c="dimmed">
          No options yet.
        </Text>
      )}
      {d.options.map((o, i) => (
        <Group key={i} gap={6} wrap="wrap" align="center" data-testid="field-option">
          <TextInput aria-label={`Option ${String(i + 1)} value`} value={o.value} onChange={(e) => set(i, { value: e.currentTarget.value })} readOnly={o.saved} maxLength={200} style={{ flex: '1 1 140px' }} data-testid="field-option-value" />
          <ColorInput aria-label={`Option ${String(i + 1)} color`} placeholder="Color" value={o.color ?? ''} onChange={(v) => set(i, { color: v || null })} format="hex" withEyeDropper={false} w={130} />
          {o.saved ? (
            <Checkbox label="Archived" checked={o.archived} onChange={(e) => set(i, { archived: e.currentTarget.checked })} data-testid="field-option-archived" />
          ) : (
            <ActionIcon variant="subtle" color="red" aria-label={`Remove option ${String(i + 1)}`} onClick={() => onChange(d.options.filter((_, j) => j !== i))}>
              <IconTrash size={14} />
            </ActionIcon>
          )}
        </Group>
      ))}
      {error && (
        <Text size="xs" c="var(--mantine-color-error)" role="alert">
          {error}
        </Text>
      )}
      <Button size="compact-xs" variant="subtle" leftSection={<IconPlus size={12} />} onClick={() => onChange([...d.options, { value: '', color: null, archived: false, saved: false }])} w="fit-content" disabled={d.options.length >= MAX_OPTIONS} data-testid="field-option-add">
        Add option
      </Button>
      <Text size="xs" c="dimmed">
        Saved option values cannot be renamed, because cases store them; archive an option to hide it from new choices.
      </Text>
    </Stack>
  );
}

function FieldDialog({ project, field, all, onClose }: { project: string; field: CaseFieldDef | null; all: readonly CaseFieldDef[]; onClose: () => void }) {
  const save = useSaveCaseField(project);
  const [d, setD] = useState<Draft>(() => draftOf(field));
  const creating = !field;
  const others = useMemo(() => all.filter((f) => f.id !== field?.id), [all, field]);
  const errors = draftErrors(d, others, creating);
  const valid = Object.values(errors).every((e) => !e);
  const set = (patch: Partial<Draft>) => setD((x) => ({ ...x, ...patch }));
  const submit = () => {
    const body = bodyOf(d);
    const nextPosition = Math.max(-1, ...all.map((f) => f.position)) + 1;
    save.mutate(creating ? { body: { ...body, key: d.key, type: d.type, position: nextPosition } } : { id: field.id, body }, {
      onSuccess: () => {
        notifications.show({ color: 'green', title: creating ? 'Field created' : 'Field saved', message: d.name.trim() });
        onClose();
      },
      onError: (e) => notifications.show({ color: 'red', title: 'Field not saved', message: errorMessage(e) }),
    });
  };
  return (
    <Modal opened onClose={onClose} title={creating ? 'New case field' : `Edit ${field.name}`} size="lg" attributes={{ content: { 'data-testid': 'field-dialog' } }}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (valid) submit();
        }}
      >
        <Stack gap="sm">
          <Group grow align="flex-start" wrap="wrap">
            <TextInput label="Name" value={d.name} onChange={(e) => set({ name: e.currentTarget.value })} error={d.name ? errors.name : null} required maxLength={200} data-autofocus data-testid="field-name" />
            <TextInput
              label="Key"
              description={creating ? 'Used in the API, filters and imports; cannot change later' : 'Cannot change'}
              value={d.key}
              onChange={(e) => set({ key: e.currentTarget.value })}
              error={d.key ? errors.key : null}
              readOnly={!creating}
              required
              classNames={{ input: 'fp-mono' }}
              data-testid="field-key"
            />
          </Group>
          <Group grow align="flex-start" wrap="wrap">
            <Select
              label="Type"
              description={creating ? 'Cannot change later' : 'Cannot change'}
              data={(Object.keys(TYPE_LABEL) as FieldType[]).map((t) => ({ value: t, label: TYPE_LABEL[t] }))}
              value={d.type}
              onChange={(v) => {
                const t = (Object.keys(TYPE_LABEL) as FieldType[]).find((x) => x === v);
                if (t) set({ type: t, defaultValue: null });
              }}
              disabled={!creating}
              allowDeselect={false}
              comboboxProps={{ withinPortal: true }}
              data-testid="field-type"
            />
            <TextInput
              label="Source label (optional)"
              description="Allure label whose value the code sync writes here"
              value={d.sourceLabel}
              onChange={(e) => set({ sourceLabel: e.currentTarget.value })}
              error={errors.sourceLabel}
              maxLength={200}
              classNames={{ input: 'fp-mono' }}
              data-testid="field-source"
            />
          </Group>
          {isSelect(d.type) && <OptionsEditor d={d} onChange={(options) => set({ options })} error={errors.options ?? null} />}
          <DefaultInput d={d} onChange={(defaultValue) => set({ defaultValue })} />
          <Switch label="Required: a case needs a value before it leaves draft" checked={d.required} onChange={(e) => set({ required: e.currentTarget.checked })} data-testid="field-required" />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" loading={save.isPending} disabled={!valid} data-testid="field-save">
              {creating ? 'Create' : 'Save'}
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}

function FieldRow({ f, index, count, canWrite, busy, onEdit, onMove, onArchive }: { f: CaseFieldDef; index: number; count: number; canWrite: boolean; busy: boolean; onEdit: () => void; onMove: (dir: -1 | 1) => void; onArchive: (archived: boolean) => void }) {
  const active = f.options.filter((o) => !o.archived);
  return (
    <Card withBorder radius="md" padding="sm" data-testid="field-row" data-key={f.key} data-archived={f.archived}>
      <Group justify="space-between" gap="xs" wrap="wrap" align="flex-start">
        <Stack gap={4} style={{ minWidth: 0, flex: '1 1 240px' }}>
          <Group gap={6} wrap="wrap">
            <Text fw={600} size="sm" component="h3" m={0}>
              {f.name}
            </Text>
            <Badge size="xs" variant="light" radius="sm">
              {TYPE_LABEL[f.type]}
            </Badge>
            {f.builtIn && (
              <Badge size="xs" variant="default" radius="sm">
                built-in
              </Badge>
            )}
            {f.required && (
              <Badge size="xs" variant="default" radius="sm">
                required
              </Badge>
            )}
            {f.archived && (
              <Badge size="xs" variant="light" color="gray" radius="sm">
                archived
              </Badge>
            )}
          </Group>
          <Text size="xs" c="dimmed">
            <span className="fp-mono">{f.key}</span>
            {f.sourceLabel ? (
              <>
                {' · from the '}
                <span className="fp-mono">{f.sourceLabel}</span> label
              </>
            ) : (
              ''
            )}
            {fieldText(f.defaultValue) ? ` · default ${fieldText(f.defaultValue)}` : ''}
          </Text>
          {isSelect(f.type) && (
            <Group gap={4} wrap="wrap" data-testid="field-row-options">
              {active.slice(0, SHOWN_OPTIONS).map((o) => (
                <Badge key={o.value} size="xs" variant="default" radius="sm" leftSection={o.color ? <span aria-hidden style={{ display: 'inline-block', width: 8, height: 8, borderRadius: 4, background: o.color }} /> : undefined}>
                  {o.value}
                </Badge>
              ))}
              {active.length > SHOWN_OPTIONS && (
                <Text size="xs" c="dimmed">
                  +{active.length - SHOWN_OPTIONS} more
                </Text>
              )}
              {active.length === 0 && (
                <Text size="xs" c="dimmed">
                  no options
                </Text>
              )}
            </Group>
          )}
        </Stack>
        {canWrite && (
          <Group gap={2} wrap="nowrap">
            <Tooltip label="Move up">
              <ActionIcon variant="subtle" size="lg" aria-label={`Move ${f.name} up`} disabled={busy || index === 0} onClick={() => onMove(-1)} data-testid="field-up">
                <IconArrowUp size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Move down">
              <ActionIcon variant="subtle" size="lg" aria-label={`Move ${f.name} down`} disabled={busy || index === count - 1} onClick={() => onMove(1)} data-testid="field-down">
                <IconArrowDown size={16} />
              </ActionIcon>
            </Tooltip>
            <Tooltip label="Edit">
              <ActionIcon variant="subtle" size="lg" aria-label={`Edit ${f.name}`} onClick={onEdit} data-testid="field-edit">
                <IconPencil size={16} />
              </ActionIcon>
            </Tooltip>
            {f.archived ? (
              <Tooltip label="Restore">
                <ActionIcon variant="subtle" size="lg" aria-label={`Restore ${f.name}`} disabled={busy} onClick={() => onArchive(false)} data-testid="field-restore">
                  <IconArrowBackUp size={16} />
                </ActionIcon>
              </Tooltip>
            ) : (
              <Tooltip label="Archive">
                <ActionIcon variant="subtle" size="lg" color="red" aria-label={`Archive ${f.name}`} disabled={busy} onClick={() => onArchive(true)} data-testid="field-archive">
                  <IconArchive size={16} />
                </ActionIcon>
              </Tooltip>
            )}
          </Group>
        )}
      </Group>
    </Card>
  );
}

/** Project settings → Case fields: the definitions of the fields every case carries. */
export default function CaseFieldsPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('settings:write', key);
  const fields = useAllCaseFields(key);
  const save = useSaveCaseField(key);
  const del = useDeleteCaseField(key);
  const [showArchived, setShowArchived] = useState(false);
  const [dialog, setDialog] = useState<null | { field: CaseFieldDef | null }>(null);
  const [moving, setMoving] = useState(false);
  const all = useMemo(() => [...(fields.data ?? [])].sort((a, b) => a.position - b.position || a.key.localeCompare(b.key)), [fields.data]);
  const shown = showArchived ? all : all.filter((f) => !f.archived);
  const archivedCount = all.filter((f) => f.archived).length;

  /** Reorders by renumbering the whole list (positions may repeat), writing only the positions that change. */
  const move = async (f: CaseFieldDef, dir: -1 | 1) => {
    const order = [...shown];
    const i = order.findIndex((x) => x.id === f.id);
    const j = i + dir;
    const other = order[j];
    if (i < 0 || !other) return;
    order[i] = other;
    order[j] = f;
    const hidden = all.filter((x) => !order.includes(x));
    const next = [...order, ...hidden];
    setMoving(true);
    try {
      for (const [pos, x] of next.entries()) if (x.position !== pos) await save.mutateAsync({ id: x.id, body: { position: pos } });
    } catch (e) {
      notifications.show({ color: 'red', title: 'Order not saved', message: errorMessage(e) });
    } finally {
      setMoving(false);
    }
  };

  const archive = (f: CaseFieldDef, archived: boolean) => {
    const done = { onSuccess: () => notifications.show({ color: 'green', title: archived ? 'Field archived' : 'Field restored', message: archived ? `${f.name}: values on cases are kept` : f.name }), onError: (e: unknown) => notifications.show({ color: 'red', title: 'Not changed', message: errorMessage(e) }) };
    if (archived) del.mutate(f.id, done);
    else save.mutate({ id: f.id, body: { archived: false } }, done);
  };

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Settings', to: `/p/${key}/settings` }, { label: 'Case fields' }]}
        title="Case fields"
        badges={
          !canWrite ? (
            <Badge variant="outline" color="gray" size="sm" data-testid="read-only">
              read-only
            </Badge>
          ) : undefined
        }
        subtitle={
          <>
            Fields every case of this project carries, in this order on the case page. Built-in fields can be renamed and archived; their key and type stay. Values are set on the{' '}
            <Link to={`/p/${key}/cases`}>test cases</Link>, filters and imports use the key.
          </>
        }
        actions={
          canWrite ? (
            <Button size="xs" leftSection={<IconPlus size={14} />} onClick={() => setDialog({ field: null })} data-testid="field-new">
              New field
            </Button>
          ) : undefined
        }
      />
      <QueryState query={fields}>
        {() => (
          <Stack gap="xs" data-testid="field-list">
            <Group justify="space-between" gap="xs" wrap="wrap">
              <Text size="sm" c="dimmed">
                {all.length - archivedCount} active{archivedCount ? `, ${String(archivedCount)} archived` : ''}
              </Text>
              <Switch label="Show archived fields" checked={showArchived} onChange={(e) => setShowArchived(e.currentTarget.checked)} disabled={!archivedCount && !showArchived} data-testid="field-show-archived" />
            </Group>
            {!canWrite && (
              <Alert variant="light" color="gray">
                Changing case fields needs the settings:write permission.
              </Alert>
            )}
            {shown.length === 0 ? (
              <EmptyState title="No fields" description="Create one with New field." />
            ) : (
              shown.map((f, i) => <FieldRow key={f.id} f={f} index={i} count={shown.length} canWrite={canWrite} busy={moving || save.isPending || del.isPending} onEdit={() => setDialog({ field: f })} onMove={(dir) => void move(f, dir)} onArchive={(a) => archive(f, a)} />)
            )}
          </Stack>
        )}
      </QueryState>
      {dialog && <FieldDialog key={dialog.field?.id ?? 'new'} project={key} field={dialog.field} all={all} onClose={() => setDialog(null)} />}
    </>
  );
}
