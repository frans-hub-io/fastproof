import { ActionIcon, Alert, Badge, Button, Card, Code, Group, Modal, SimpleGrid, Stack, Switch, Tabs, TagsInput, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconEdit, IconInfoCircle, IconPlus, IconShieldCheck, IconTrash } from '@tabler/icons-react';
import { useState } from 'react';
import { useParams } from 'react-router';
import { useAnalysisCapabilities } from '../api/analysis';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { useSaveSettings, useSettings } from '../api/stageC';
import type { Gate, GateMetric, ProjectSettings } from '../api/types-next';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { GateHistory } from './gates/GateHistory';
import { GLOB, availableMetrics, ruleText, rulesValid } from '../lib/gates';
import { RuleRows } from './gates/RuleRows';
import { GateSimulation } from './gates/GateSimulation';

function GateEditor({ initial, taken, metrics, onSave, onDelete, onCancel, saving }: { initial: Gate; taken: string[]; metrics: GateMetric[]; onSave: (g: Gate) => void; onDelete?: () => void; onCancel: () => void; saving: boolean }) {
  const [g, setG] = useState<Gate>(initial);
  const name = g.name.trim();
  const nameError = !name ? 'Required' : name.length > 100 ? 'At most 100 characters' : taken.includes(name) ? 'Another gate has this name' : null;
  const branchError = (g.branches ?? []).some((b) => !GLOB.test(b)) ? 'Use branch names or globs like release/*' : null;
  const valid = !nameError && !branchError && rulesValid(g.rules);
  return (
    <Stack gap="sm">
      <TextInput label="Name" value={g.name} onChange={(e) => setG({ ...g, name: e.currentTarget.value })} error={nameError} maxLength={100} data-testid="gate-name" data-autofocus />
      <TagsInput
        label="Branches"
        description="Globs; empty applies the gate to every branch"
        placeholder="main, release/*"
        value={g.branches ?? []}
        onChange={(v) => setG({ ...g, branches: v.length ? v : null })}
        error={branchError}
        maxTags={20}
        comboboxProps={{ withinPortal: true }}
      />
      <Switch label="Enabled" checked={g.enabled} onChange={(e) => setG({ ...g, enabled: e.currentTarget.checked })} />
      <Text size="sm" fw={600}>
        Rules (all must hold)
      </Text>
      <RuleRows rules={g.rules} onChange={(rules) => setG({ ...g, rules })} metrics={metrics} />
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
  const caps = useAnalysisCapabilities(key).data;
  const analytics = caps?.features.gateAnalytics === true;
  const [tab, setTab] = useState('gates');

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

  const list = (
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
  );

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Quality gates' }]}
        title="Quality gates"
        subtitle="Pass or fail a finished launch on its numbers; CI waits for the verdict."
        actions={
          canWrite && q.data ? (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setEditing({ index: null, gate: { name: '', enabled: true, rules: [{ metric: 'failed', op: 'lte', value: 0 }] } })} data-testid="gate-add">
              Add gate
            </Button>
          ) : undefined
        }
      />
      {analytics ? (
        <Tabs value={tab} onChange={(v) => setTab(v ?? 'gates')} keepMounted={false}>
          <Tabs.List mb="md">
            <Tabs.Tab value="gates">Gates</Tabs.Tab>
            <Tabs.Tab value="history" data-testid="tab-gate-history">
              History
            </Tabs.Tab>
            <Tabs.Tab value="simulation" data-testid="tab-gate-simulation">
              Simulation
            </Tabs.Tab>
          </Tabs.List>
          <Tabs.Panel value="gates">{list}</Tabs.Panel>
          <Tabs.Panel value="history">
            <GateHistory project={key} />
          </Tabs.Panel>
          <Tabs.Panel value="simulation">
            <GateSimulation project={key} />
          </Tabs.Panel>
        </Tabs>
      ) : (
        list
      )}
      <Modal opened={!!editing} onClose={() => setEditing(null)} title={editing?.index === null ? 'New gate' : 'Edit gate'} size="lg" fullScreen={phone}>
        {editing && q.data && (() => {
          const settings = q.data;
          return (
          <GateEditor
            initial={editing.gate}
            metrics={availableMetrics(analytics, editing.gate.rules)}
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
