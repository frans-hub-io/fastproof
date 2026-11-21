import { ActionIcon, Alert, Badge, Button, Card, Code, Group, Modal, NumberInput, Select, SimpleGrid, Stack, Switch, TagsInput, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconEdit, IconInfoCircle, IconPlus, IconShieldCheck, IconTrash } from '@tabler/icons-react';
import { useState } from 'react';
import { useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useSaveSettings, useSettings } from '../api/stageC';
import type { Gate, GateMetric, GateOp, GateRule, ProjectSettings } from '../api/types-next';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';

const METRICS: { value: GateMetric; label: string; unit: string }[] = [
  { value: 'failed', label: 'Failed results', unit: 'results' },
  { value: 'broken', label: 'Broken results', unit: 'results' },
  { value: 'newFailures', label: 'New failures', unit: 'results' },
  { value: 'passRate', label: 'Pass rate', unit: '%' },
  { value: 'flaky', label: 'Flaky results', unit: 'results' },
  { value: 'duration', label: 'Duration', unit: 'seconds' },
];

const OPS: { value: GateOp; label: string }[] = [
  { value: 'lte', label: '≤' },
  { value: 'lt', label: '<' },
  { value: 'gte', label: '≥' },
  { value: 'gt', label: '>' },
];

const OP_LABEL: Record<GateOp, string> = { gt: '>', gte: '≥', lt: '<', lte: '≤' };

function metricOf(m: GateMetric) {
  return METRICS.find((x) => x.value === m) ?? { value: m, label: m, unit: '' };
}

function ruleText(r: GateRule): string {
  const m = metricOf(r.metric);
  const unit = m.unit === '%' ? '%' : m.unit === 'seconds' ? ' s' : '';
  return `${m.label} ${OP_LABEL[r.op]} ${String(r.value)}${unit}${r.baseline ? ` (vs ${r.baseline})` : ''}`;
}

const GLOB = /^[A-Za-z0-9._/*-]{1,200}$/;

function ruleError(r: GateRule): string | null {
  if (!Number.isFinite(r.value)) return 'Value is required';
  if (r.metric === 'passRate' && (r.value < 0 || r.value > 100)) return 'Pass rate is 0–100';
  if (r.value < 0) return 'Value cannot be negative';
  if (r.baseline && !GLOB.test(r.baseline)) return 'Baseline must be a branch name';
  return null;
}

function GateEditor({ initial, taken, onSave, onDelete, onCancel, saving }: { initial: Gate; taken: string[]; onSave: (g: Gate) => void; onDelete?: () => void; onCancel: () => void; saving: boolean }) {
  const [g, setG] = useState<Gate>(initial);
  const name = g.name.trim();
  const nameError = !name ? 'Required' : name.length > 100 ? 'At most 100 characters' : taken.includes(name) ? 'Another gate has this name' : null;
  const branchError = (g.branches ?? []).some((b) => !GLOB.test(b)) ? 'Use branch names or globs like release/*' : null;
  const rErrors = g.rules.map(ruleError);
  const valid = !nameError && !branchError && g.rules.length > 0 && g.rules.length <= 20 && rErrors.every((e) => e === null);
  const setRule = (i: number, r: Partial<GateRule>) => setG({ ...g, rules: g.rules.map((x, j) => (j === i ? { ...x, ...r } : x)) });
  return (
    <Stack gap="sm">
      <TextInput label="Name" value={g.name} onChange={(e) => setG({ ...g, name: e.currentTarget.value })} error={nameError} maxLength={100} data-testid="gate-name" data-autofocus />
      <TagsInput
        label="Branches"
        description="Globs; empty applies the gate to every branch"
        placeholder="main, release/*"
        value={g.branches ?? []}
        onChange={(v) => setG({ ...g, branches: v })}
        error={branchError}
        maxTags={20}
        comboboxProps={{ withinPortal: true }}
      />
      <Switch label="Enabled" checked={g.enabled} onChange={(e) => setG({ ...g, enabled: e.currentTarget.checked })} />
      <Text size="sm" fw={600}>
        Rules (all must hold)
      </Text>
      {g.rules.map((r, i) => {
        const m = metricOf(r.metric);
        return (
          <Card key={i} withBorder radius="sm" padding="xs" data-testid="gate-rule-row">
            <Stack gap={6}>
              <SimpleGrid cols={{ base: 2, sm: 4 }} spacing="xs">
                <Select aria-label="Metric" data={METRICS.map(({ value, label }) => ({ value, label }))} value={r.metric} onChange={(v) => v && setRule(i, { metric: v })} allowDeselect={false} comboboxProps={{ withinPortal: true }} data-testid="gate-rule-metric" />
                <Select aria-label="Comparison" data={OPS} value={r.op} onChange={(v) => v && setRule(i, { op: v })} allowDeselect={false} comboboxProps={{ withinPortal: true }} />
                <NumberInput
                  aria-label="Value"
                  value={r.value}
                  onChange={(v) => setRule(i, { value: typeof v === 'number' ? v : Number.NaN })}
                  min={0}
                  max={r.metric === 'passRate' ? 100 : undefined}
                  decimalScale={r.metric === 'passRate' ? 1 : 0}
                  rightSection={
                    <Text size="xs" c="dimmed">
                      {m.unit === 'results' ? '' : m.unit === 'seconds' ? 's' : m.unit}
                    </Text>
                  }
                  data-testid="gate-rule-value"
                />
                <TextInput aria-label="Baseline branch" placeholder="baseline (optional)" value={r.baseline ?? ''} onChange={(e) => setRule(i, { baseline: e.currentTarget.value || null })} />
              </SimpleGrid>
              <Group justify="space-between" wrap="nowrap" gap="xs">
                <Text size="xs" c={rErrors[i] ? 'red' : 'dimmed'}>
                  {rErrors[i] ?? (r.baseline ? `Compared with the last finished launch on ${r.baseline}` : (m.unit === 'results' ? 'Number of results' : m.unit === 'seconds' ? 'Launch duration in seconds' : 'Percent of executed (non-skipped) results'))}
                </Text>
                <ActionIcon variant="subtle" color="red" aria-label="Remove the rule" onClick={() => setG({ ...g, rules: g.rules.filter((_, j) => j !== i) })} disabled={g.rules.length === 1}>
                  <IconTrash size={16} />
                </ActionIcon>
              </Group>
            </Stack>
          </Card>
        );
      })}
      {g.rules.length < 20 && (
        <Group>
          <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={() => setG({ ...g, rules: [...g.rules, { metric: 'failed', op: 'lte', value: 0 }] })} data-testid="gate-add-rule">
            Add rule
          </Button>
        </Group>
      )}
      <Group justify="space-between" mt="xs">
        {onDelete ? (
          <Button variant="subtle" color="red" leftSection={<IconTrash size={16} />} onClick={onDelete} loading={saving}>
            Delete gate
          </Button>
        ) : (
          <span />
        )}
        <Group gap="xs">
          <Button variant="default" onClick={onCancel}>
            Cancel
          </Button>
          <Button onClick={() => onSave({ ...g, name, rules: g.rules.map((r) => (r.baseline ? r : { metric: r.metric, op: r.op, value: r.value })) })} disabled={!valid} loading={saving} data-testid="gate-save">
            Save gate
          </Button>
        </Group>
      </Group>
    </Stack>
  );
}

function GateCard({ gate, canWrite, onToggle, onEdit }: { gate: Gate; canWrite: boolean; onToggle: (v: boolean) => void; onEdit: () => void }) {
  return (
    <Card withBorder radius="md" padding="md" data-testid="gate-card" style={{ opacity: gate.enabled ? 1 : 0.7 }}>
      <Stack gap="xs">
        <Group justify="space-between" wrap="nowrap" gap="xs">
          <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
            <IconShieldCheck size={18} style={{ flex: 'none' }} />
            <Text fw={700} className="fh-truncate">
              {gate.name}
            </Text>
            {!gate.enabled && (
              <Badge size="xs" variant="outline" color="gray" style={{ textTransform: 'none' }}>
                disabled
              </Badge>
            )}
          </Group>
          {canWrite && (
            <Group gap="xs" wrap="nowrap">
              <Switch size="sm" aria-label={`Enable ${gate.name}`} checked={gate.enabled} onChange={(e) => onToggle(e.currentTarget.checked)} />
              <ActionIcon variant="subtle" aria-label={`Edit ${gate.name}`} onClick={onEdit}>
                <IconEdit size={16} />
              </ActionIcon>
            </Group>
          )}
        </Group>
        <Group gap={6} wrap="wrap">
          <Text size="xs" c="dimmed">
            Branches:
          </Text>
          {gate.branches?.length ? (
            gate.branches.map((b) => (
              <Badge key={b} size="sm" variant="default" radius="sm" className="fp-mono" style={{ textTransform: 'none' }}>
                {b}
              </Badge>
            ))
          ) : (
            <Text size="xs">all</Text>
          )}
        </Group>
        <Stack gap={2}>
          {gate.rules.map((r, i) => (
            <Text key={i} size="sm" data-testid="gate-rule">
              • {ruleText(r)}
            </Text>
          ))}
        </Stack>
      </Stack>
    </Card>
  );
}

export default function GatesPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canWrite = useCan('settings:write', key);
  const q = useSettings(key);
  const save = useSaveSettings(key);
  const phone = useMediaQuery('(max-width: 48em)');
  const [editing, setEditing] = useState<{ index: number | null; gate: Gate } | null>(null);

  const persist = (settings: ProjectSettings, gates: Gate[], message: string, after?: () => void) =>
    save.mutate(
      { ...settings, gates },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message });
          after?.();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Gates not saved', message: errorMessage(e) }),
      },
    );

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Quality gates' }]}
        title="Quality gates"
        subtitle="Pass or fail a finished launch on its numbers; CI waits for the verdict."
        actions={
          canWrite && q.data ? (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setEditing({ index: null, gate: { name: '', enabled: true, branches: [], rules: [{ metric: 'failed', op: 'lte', value: 0 }] } })} data-testid="gate-add">
              Add gate
            </Button>
          ) : undefined
        }
      />
      <Stack gap="md">
        <Alert variant="light" color="gray" icon={<IconInfoCircle size={18} />} title="How gates are evaluated">
          <Stack gap={6}>
            <Text size="sm">
              When a launch finishes, every enabled gate whose branch globs match the launch branch is checked; a gate passes when all its rules hold, and the launch passes when every matching gate does.
              The launch page shows the result next to its status; it stays pending while the launch runs.
            </Text>
            <Text size="sm">
              In CI, <Code>fastproof gate --launch &lt;externalId&gt; --wait 10m</Code> prints the summary and exits 0 when the gates pass, 1 when they fail and 2 on errors.
            </Text>
          </Stack>
        </Alert>
        <QueryState query={q}>
          {() => {
            const settings = q.data;
            if (!settings) return null;
            const gates = settings.gates;
            if (!gates.length) return <EmptyState title="No gates yet" description={canWrite ? 'Add a gate to fail launches that break your quality bar.' : 'Project admins can add gates.'} />;
            return (
              <SimpleGrid cols={{ base: 1, md: 2 }} spacing="md" data-testid="gates-list">
                {gates.map((g, i) => (
                  <GateCard
                    key={`${g.name}:${String(i)}`}
                    gate={g}
                    canWrite={canWrite}
                    onEdit={() => setEditing({ index: i, gate: structuredClone(g) })}
                    onToggle={(v) => persist(settings, gates.map((x, j) => (j === i ? { ...x, enabled: v } : x)), `${g.name} ${v ? 'enabled' : 'disabled'}`)}
                  />
                ))}
              </SimpleGrid>
            );
          }}
        </QueryState>
      </Stack>
      <Modal opened={!!editing} onClose={() => setEditing(null)} title={editing?.index === null ? 'New gate' : 'Edit gate'} size="lg" fullScreen={phone}>
        {editing && q.data && (() => {
          const settings = q.data;
          return (
          <GateEditor
            initial={editing.gate}
            taken={q.data.gates.filter((_, j) => j !== editing.index).map((g) => g.name)}
            saving={save.isPending}
            onCancel={() => setEditing(null)}
            onDelete={
              editing.index === null
                ? undefined
                : () => persist(settings, settings.gates.filter((_, j) => j !== editing.index), `${editing.gate.name} deleted`, () => setEditing(null))
            }
            onSave={(gate) => {
              const gates = editing.index === null ? [...settings.gates, gate] : settings.gates.map((x, j) => (j === editing.index ? gate : x));
              persist(settings, gates, `${gate.name} saved`, () => setEditing(null));
            }}
          />
          );
        })()}
      </Modal>
    </>
  );
}
