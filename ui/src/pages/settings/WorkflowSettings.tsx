import { Alert, Badge, Button, Card, Checkbox, Group, Modal, MultiSelect, NumberInput, Select, Stack, Switch, Text, TextInput } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconPlus, IconTrash } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage } from '../../api/client';
import {
  DEFAULT_REVIEW,
  statusesInUse,
  useCaseWorkflow,
  useSaveCaseWorkflow,
  useSaveReviewPolicy,
  type CaseReviewSettings,
  type CaseWorkflowFlow,
  type CaseWorkflowSettings,
  type CaseWorkflowStatus,
  type CaseWorkflowTransition,
  type StatusInUse,
  type WorkflowGuard,
} from '../../api/reviews';
import { useTmJob, type TmJob } from '../../api/tm';
import { QueryState } from '../../components/QueryState';
import { Section } from '../cases/common';
import { CATEGORIES_LIST, validateWorkflow } from '../../lib/tm/reviewUtil';

const COLOR_RE = /^#[0-9a-fA-F]{6}$/;
const GUARDS: { value: WorkflowGuard; label: string }[] = [
  { value: 'approved', label: 'Version is approved' },
  { value: 'requiredFields', label: 'Required fields are filled' },
  { value: 'notAuthor', label: 'Not the author' },
];
const PERMISSIONS = [
  { value: 'cases:write', label: 'cases:write' },
  { value: 'cases:review', label: 'cases:review' },
];

function statusOptions(w: CaseWorkflowSettings, withAny: boolean): { value: string; label: string }[] {
  const out = w.statuses.filter((s) => s.key).map((s) => ({ value: s.key, label: s.name ? `${s.name} (${s.key})` : s.key }));
  return withAny ? [{ value: '*', label: 'Any state' }, ...out] : out;
}

function FlowEditor({ title, testid, flow, workflow, onChange, disabled }: { title: string; testid: string; flow: CaseWorkflowFlow; workflow: CaseWorkflowSettings; onChange: (f: CaseWorkflowFlow) => void; disabled: boolean }) {
  const setT = (i: number, t: CaseWorkflowTransition) => onChange({ ...flow, transitions: flow.transitions.map((x, j) => (j === i ? t : x)) });
  const keys = new Set(workflow.statuses.map((s) => s.key));
  return (
    <Card withBorder radius="md" padding="sm" data-testid={testid}>
      <Stack gap="sm">
        <Group justify="space-between" align="flex-end" wrap="wrap">
          <Text fw={600} size="sm" component="h4" m={0}>
            {title}
          </Text>
          <Select label="Initial state" aria-label={`${title}: initial state`} data={statusOptions(workflow, false)} value={keys.has(flow.initial) ? flow.initial : null} onChange={(v) => v && onChange({ ...flow, initial: v })} disabled={disabled} allowDeselect={false} w={{ base: '100%', xs: 240 }} comboboxProps={{ withinPortal: true }} data-testid={`${testid}-initial`} />
        </Group>
        {flow.transitions.map((t, i) => (
          <Card key={i} withBorder radius="sm" padding="xs" data-testid={`${testid}-transition`}>
            <Stack gap="xs">
              <Group gap="xs" align="flex-end" wrap="wrap">
                <MultiSelect label="From" data={statusOptions(workflow, true)} value={t.from} onChange={(v) => setT(i, { ...t, from: v.includes('*') && v.length > 1 ? (t.from.includes('*') ? v.filter((x) => x !== '*') : ['*']) : v })} disabled={disabled} w={{ base: '100%', sm: 260 }} comboboxProps={{ withinPortal: true }} data-testid="transition-from" />
                <Select label="To" data={statusOptions(workflow, true)} value={t.to || null} onChange={(v) => setT(i, { ...t, to: v ?? '' })} disabled={disabled} allowDeselect={false} w={{ base: '100%', sm: 220 }} comboboxProps={{ withinPortal: true }} data-testid="transition-to" />
                <Select label="Permission" data={PERMISSIONS} value={t.permission ?? 'cases:write'} onChange={(v) => setT(i, { ...t, permission: v === 'cases:review' ? 'cases:review' : 'cases:write' })} disabled={disabled} allowDeselect={false} w={{ base: '100%', sm: 160 }} comboboxProps={{ withinPortal: true }} data-testid="transition-permission" />
                <MultiSelect label="Conditions" placeholder={t.requires?.length ? undefined : 'None'} data={GUARDS} value={t.requires ?? []} onChange={(v) => setT(i, { ...t, requires: v })} disabled={disabled} w={{ base: '100%', sm: 280 }} comboboxProps={{ withinPortal: true }} data-testid="transition-requires" />
                {!disabled && (
                  <Button size="xs" variant="subtle" color="red" leftSection={<IconTrash size={14} />} onClick={() => onChange({ ...flow, transitions: flow.transitions.filter((_, j) => j !== i) })} aria-label={`Remove transition ${String(i + 1)} of ${title}`} data-testid="transition-remove">
                    Remove
                  </Button>
                )}
              </Group>
            </Stack>
          </Card>
        ))}
        {flow.transitions.length === 0 && (
          <Text size="sm" c="dimmed">
            No transitions: cases of this kind stay in their state.
          </Text>
        )}
        {!disabled && (
          <Group>
            <Button size="xs" variant="default" leftSection={<IconPlus size={14} />} disabled={flow.transitions.length >= 100} onClick={() => onChange({ ...flow, transitions: [...flow.transitions, { from: [], to: '', permission: 'cases:write', requires: [] }] })} data-testid={`${testid}-add`}>
              Add transition
            </Button>
          </Group>
        )}
      </Stack>
    </Card>
  );
}

function StatusEditor({ workflow, onChange, disabled }: { workflow: CaseWorkflowSettings; onChange: (w: CaseWorkflowSettings) => void; disabled: boolean }) {
  const setS = (i: number, s: CaseWorkflowStatus) => onChange({ ...workflow, statuses: workflow.statuses.map((x, j) => (j === i ? s : x)) });
  return (
    <Stack gap="xs" data-testid="workflow-statuses">
      {workflow.statuses.map((s, i) => {
        const n = String(i + 1);
        return (
          <Card key={i} withBorder radius="sm" padding="xs" data-testid="workflow-status">
            <Group gap="xs" align="flex-end" wrap="wrap">
              <TextInput label="Key" aria-label={`Status ${n} key`} value={s.key} onChange={(e) => setS(i, { ...s, key: e.currentTarget.value })} disabled={disabled} maxLength={32} w={{ base: '45%', sm: 150 }} className="fp-mono" data-testid="status-key" />
              <TextInput label="Name" aria-label={`Status ${n} name`} value={s.name} onChange={(e) => setS(i, { ...s, name: e.currentTarget.value })} disabled={disabled} w={{ base: '50%', sm: 180 }} data-testid="status-name" />
              <Select label="Category" aria-label={`Status ${n} category`} data={CATEGORIES_LIST} value={s.category} onChange={(v) => v && setS(i, { ...s, category: v })} disabled={disabled} allowDeselect={false} w={{ base: '45%', sm: 130 }} comboboxProps={{ withinPortal: true }} data-testid="status-category" />
              <TextInput label="Colour" aria-label={`Status ${n} colour`} placeholder="#1c7ed6" value={s.color ?? ''} onChange={(e) => setS(i, { ...s, color: e.currentTarget.value.trim() || null })} disabled={disabled} w={{ base: '45%', sm: 120 }} leftSection={<span aria-hidden style={{ width: 12, height: 12, borderRadius: 3, background: s.color && COLOR_RE.test(s.color) ? s.color : 'transparent', border: '1px solid var(--fh-ui-hairline)' }} />} data-testid="status-color" />
              <Checkbox label="Blocks runs" aria-label={`Status ${n} blocks runs`} checked={s.blocksRuns === true} onChange={(e) => setS(i, { ...s, blocksRuns: e.currentTarget.checked })} disabled={disabled} mb={6} data-testid="status-blocks" />
              {!disabled && (
                <Button size="xs" variant="subtle" color="red" leftSection={<IconTrash size={14} />} disabled={workflow.statuses.length <= 1} onClick={() => onChange({ ...workflow, statuses: workflow.statuses.filter((_, j) => j !== i) })} aria-label={`Remove status ${n}`} data-testid="status-remove">
                  Remove
                </Button>
              )}
            </Group>
          </Card>
        );
      })}
      {!disabled && (
        <Group>
          <Button size="xs" variant="default" leftSection={<IconPlus size={14} />} disabled={workflow.statuses.length >= 20} onClick={() => onChange({ ...workflow, statuses: [...workflow.statuses, { key: '', name: '', category: 'draft', color: null, blocksRuns: false }] })} data-testid="status-add">
            Add status
          </Button>
        </Group>
      )}
    </Stack>
  );
}

function RemapDialog({ used, workflow, busy, onCancel, onSubmit }: { used: StatusInUse[]; workflow: CaseWorkflowSettings; busy: boolean; onCancel: () => void; onSubmit: (m: Record<string, string>) => void }) {
  const [map, setMap] = useState<Record<string, string | null>>({});
  const ready = used.every((u) => map[u.key]);
  return (
    <Modal opened onClose={onCancel} title="Move cases to other states" size="lg">
      <Stack gap="sm" data-testid="remap-dialog">
        <Text size="sm">Some cases are in states that the new workflow drops or puts in another category. Choose where each group goes; a background job moves them.</Text>
        {used.map((u) => (
          <Select key={u.key} label={`${u.key}: ${String(u.cases)} ${u.cases === 1 ? 'case' : 'cases'} move to`} data={statusOptions(workflow, false)} value={map[u.key] ?? null} onChange={(v) => setMap((m) => ({ ...m, [u.key]: v }))} comboboxProps={{ withinPortal: true }} allowDeselect={false} data-testid="remap-select" data-key={u.key} />
        ))}
        <Group justify="flex-end">
          <Button variant="default" onClick={onCancel}>
            Cancel
          </Button>
          <Button disabled={!ready} loading={busy} onClick={() => onSubmit(Object.fromEntries(Object.entries(map).filter((e): e is [string, string] => !!e[1])))} data-testid="remap-confirm">
            Save and move the cases
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

function JobNote({ project, first }: { project: string; first: TmJob }) {
  const job = useTmJob(project, first.id);
  const j = job.data ?? first;
  const total = j.progress.total ?? j.progress.done;
  return (
    <Alert color={j.status === 'failed' ? 'red' : j.status === 'done' ? 'teal' : 'blue'} variant="light" role="status" data-testid="remap-job" data-status={j.status}>
      Moving cases: {j.status}, {j.progress.done} of {total} done.
    </Alert>
  );
}

function WorkflowForm({ project, configured, initial, canWrite, job, onJob }: { project: string; configured: boolean; initial: CaseWorkflowSettings; canWrite: boolean; job: TmJob | null; onJob: (j: TmJob) => void }) {
  const save = useSaveCaseWorkflow(project);
  const [draft, setDraft] = useState<CaseWorkflowSettings>(() => structuredClone(initial));
  const [remap, setRemap] = useState<StatusInUse[] | null>(null);
  const [removing, setRemoving] = useState(false);
  const errors = configured ? validateWorkflow(draft) : [];
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

  const send = (workflow: CaseWorkflowSettings | null, map?: Record<string, string>) =>
    save.mutate(
      { workflow, ...(map ? { remap: map } : {}) },
      {
        onSuccess: (r) => {
          setRemap(null);
          setRemoving(false);
          if (r.job) onJob(r.job);
          notifications.show({ color: 'green', title: workflow ? 'Workflow saved' : 'Workflow removed', message: workflow ? 'Cases follow the new states.' : 'Cases no longer have a workflow state.' });
        },
        onError: (e) => {
          const used = statusesInUse(e);
          if (used && workflow) {
            setRemap(used);
            return;
          }
          setRemoving(false);
          notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) });
        },
      },
    );

  if (!configured) {
    return (
      <Stack gap="sm" data-testid="workflow-unconfigured">
        <Text size="sm">This project has no workflow: cases only have a status (draft, active or outdated). A workflow adds named states, the moves between them and rules such as "needs an approved version".</Text>
        <Group gap={6} wrap="wrap" aria-label="Suggested states">
          {initial.statuses.map((s) => (
            <Badge key={s.key} variant="outline" color="gray" tt="none" leftSection={<span aria-hidden style={{ width: 8, height: 8, borderRadius: '50%', background: s.color ?? 'transparent', display: 'inline-block' }} />}>
              {s.name}
            </Badge>
          ))}
        </Group>
        {canWrite ? (
          <Group>
            <Button size="xs" loading={save.isPending} onClick={() => send(initial)} data-testid="workflow-use-suggested">
              Use the suggested workflow
            </Button>
          </Group>
        ) : (
          <Text size="sm" c="dimmed">
            Needs the settings:write permission to change.
          </Text>
        )}
      </Stack>
    );
  }

  return (
    <Stack gap="md" data-testid="workflow-form">
      {job && <JobNote project={project} first={job} />}
      <StatusEditor workflow={draft} onChange={setDraft} disabled={!canWrite} />
      <FlowEditor title="Manual cases" testid="flow-manual" flow={draft.manual} workflow={draft} onChange={(f) => setDraft({ ...draft, manual: f })} disabled={!canWrite} />
      <FlowEditor title="Automated cases" testid="flow-automated" flow={draft.automated} workflow={draft} onChange={(f) => setDraft({ ...draft, automated: f })} disabled={!canWrite} />
      {errors.length > 0 && (
        <Alert color="red" variant="light" title="Fix before saving" data-testid="workflow-errors">
          <Stack gap={2} component="ul" m={0} pl="md">
            {errors.map((e) => (
              <li key={e}>{e}</li>
            ))}
          </Stack>
        </Alert>
      )}
      {canWrite && (
        <Group gap="xs">
          <Button size="xs" disabled={!dirty || errors.length > 0} loading={save.isPending && !removing} onClick={() => send(draft)} data-testid="workflow-save">
            Save workflow
          </Button>
          <Button size="xs" variant="default" disabled={!dirty} onClick={() => setDraft(structuredClone(initial))} data-testid="workflow-reset">
            Discard changes
          </Button>
          <Button size="xs" variant="subtle" color="red" onClick={() => setRemoving(true)} data-testid="workflow-remove">
            Remove workflow
          </Button>
        </Group>
      )}
      {remap && <RemapDialog used={remap} workflow={draft} busy={save.isPending} onCancel={() => setRemap(null)} onSubmit={(m) => send(draft, m)} />}
      <Modal opened={removing} onClose={() => setRemoving(false)} title="Remove the workflow">
        <Stack gap="sm">
          <Text size="sm">Cases keep their status (draft, active or outdated) but lose their named state. Transitions and their rules stop applying. This cannot be undone.</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={() => setRemoving(false)}>
              Cancel
            </Button>
            <Button color="red" loading={save.isPending} onClick={() => send(null)} data-testid="workflow-remove-confirm">
              Remove workflow
            </Button>
          </Group>
        </Stack>
      </Modal>
    </Stack>
  );
}

function PolicyForm({ project, initial, canWrite }: { project: string; initial: CaseReviewSettings; canWrite: boolean }) {
  const save = useSaveReviewPolicy(project);
  const [p, setP] = useState<CaseReviewSettings>({ ...DEFAULT_REVIEW, ...initial });
  const dirty = JSON.stringify(p) !== JSON.stringify({ ...DEFAULT_REVIEW, ...initial });
  const off = p.mode === 'off';
  return (
    <Stack gap="sm" data-testid="review-policy">
      <Text size="sm" c="dimmed">
        When review is on, a new version of a case waits for approval before it counts as the approved version.
      </Text>
      <Group gap="sm" align="flex-start" wrap="wrap">
        <Select
          label="Review"
          data={[
            { value: 'off', label: 'Off' },
            { value: 'manual', label: 'Manual cases only' },
            { value: 'all', label: 'All cases and shared steps' },
          ]}
          value={p.mode}
          onChange={(v) => v && setP({ ...p, mode: v })}
          disabled={!canWrite}
          allowDeselect={false}
          w={{ base: '100%', xs: 240 }}
          comboboxProps={{ withinPortal: true }}
          data-testid="policy-mode"
        />
        <NumberInput label="Approvals needed" min={1} max={5} allowDecimal={false} value={p.approvals} onChange={(v) => setP({ ...p, approvals: Math.min(5, Math.max(1, Number(v) || 1)) })} disabled={!canWrite || off} w={{ base: '100%', xs: 160 }} data-testid="policy-approvals" />
        <Select
          label="An approval resets on"
          data={[
            { value: 'content', label: 'Content changes' },
            { value: 'any', label: 'Any change' },
          ]}
          value={p.resetOn}
          onChange={(v) => v && setP({ ...p, resetOn: v })}
          disabled={!canWrite || off}
          allowDeselect={false}
          w={{ base: '100%', xs: 200 }}
          comboboxProps={{ withinPortal: true }}
          data-testid="policy-reset"
        />
        <Select
          label="Imported changes"
          data={[
            { value: 'review', label: 'Need review' },
            { value: 'bypass', label: 'Skip review' },
          ]}
          value={p.imports}
          onChange={(v) => v && setP({ ...p, imports: v })}
          disabled={!canWrite || off}
          allowDeselect={false}
          w={{ base: '100%', xs: 200 }}
          comboboxProps={{ withinPortal: true }}
          data-testid="policy-imports"
        />
      </Group>
      <Switch label="Authors may approve their own changes" checked={p.allowSelfApproval} onChange={(e) => setP({ ...p, allowSelfApproval: e.currentTarget.checked })} disabled={!canWrite || off} data-testid="policy-self" />
      {canWrite ? (
        <Group>
          <Button
            size="xs"
            disabled={!dirty}
            loading={save.isPending}
            onClick={() =>
              save.mutate(p, {
                onSuccess: () => notifications.show({ color: 'green', title: 'Review policy saved', message: p.mode === 'off' ? 'Review is off.' : `${String(p.approvals)} approval${p.approvals === 1 ? '' : 's'} needed.` }),
                onError: (e) => notifications.show({ color: 'red', title: 'Not saved', message: errorMessage(e) }),
              })
            }
            data-testid="policy-save"
          >
            Save review policy
          </Button>
        </Group>
      ) : (
        <Text size="sm" c="dimmed">
          Needs the settings:write permission to change.
        </Text>
      )}
    </Stack>
  );
}

export function WorkflowSettings({ projectKey }: { projectKey: string }) {
  const canWrite = useCan('settings:write', projectKey);
  const view = useCaseWorkflow(projectKey);
  const [job, setJob] = useState<TmJob | null>(null);
  return (
    <QueryState query={view}>
      {() =>
        view.data && (
          <Stack gap="xl" data-testid="workflow-settings">
            <Section title="Workflow">
              <WorkflowForm key={JSON.stringify([view.data.configured, view.data.workflow])} project={projectKey} configured={view.data.configured} initial={view.data.workflow} canWrite={canWrite} job={job} onJob={setJob} />
            </Section>
            <Section title="Review policy">
              <PolicyForm key={JSON.stringify(view.data.review)} project={projectKey} initial={view.data.review} canWrite={canWrite} />
            </Section>
          </Stack>
        )
      }
    </QueryState>
  );
}
