import { ActionIcon, Alert, Anchor, Badge, Button, Card, Divider, Drawer, Group, Modal, MultiSelect, NumberInput, Select, Stack, Switch, Text, Textarea, TextInput, Tooltip } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconArrowDown, IconArrowUp, IconFlask, IconPencil, IconPlayerPlay, IconPlus, IconTrash } from '@tabler/icons-react';
import { useMemo, useState } from 'react';
import { Link, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { flatItems, useProject, useResults } from '../api/hooks';
import { useApplyRule, useDefectTypeMap, useDefectTypes, useDeleteRule, useRules, useSaveRule, useTestRule } from '../api/stageC';
import type { ResultStatus } from '../api/types';
import { RESULT_STATUSES } from '../api/types';
import type { DefectType, Rule, RuleInput } from '../api/types-next';
import { DEFECT_GROUPS } from '../api/types-next';
import { DefectChip } from '../components/Defect';
import { LaunchSelect } from '../components/LaunchSelect';
import { PageHeader } from '../components/PageHeader';
import { QueryState } from '../components/QueryState';
import { StatusDot } from '../components/Status';
import { GROUP_LABEL } from '../lib/defects';
import { formatRelative } from '../lib/format';
import { useLaunchList } from '../lib/launches';
import { STATUS_LABEL } from '../lib/status';

const MAX_REGEX = 1000;

function regexError(src: string): string | null {
  if (!src) return null;
  if (src.length > MAX_REGEX) return `At most ${String(MAX_REGEX)} characters`;
  try {
    new RegExp(src);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : 'Invalid regular expression';
  }
}

interface Draft {
  name: string;
  enabled: boolean;
  messageRegex: string;
  traceRegex: string;
  statuses: string[];
  environment: string;
  fingerprint: string;
  testCaseId: string;
  defectTypeId: string | null;
  comment: string;
  issueUrl: string;
  issueKey: string;
  muteDays: number | '';
  accept: boolean;
}

function draftOf(r?: Rule): Draft {
  return {
    name: r?.name ?? '',
    enabled: r?.enabled ?? true,
    messageRegex: r?.match.messageRegex ?? '',
    traceRegex: r?.match.traceRegex ?? '',
    statuses: r?.match.statuses ?? ['failed', 'broken'],
    environment: r?.match.environment ?? '',
    fingerprint: r?.match.fingerprint ?? '',
    testCaseId: r?.match.testCaseId ?? '',
    defectTypeId: r?.action.defectTypeId ?? null,
    comment: r?.action.comment ?? '',
    issueUrl: r?.action.issue?.url ?? '',
    issueKey: r?.action.issue?.key ?? '',
    muteDays: r?.action.mute?.days ?? '',
    accept: r?.action.accept ?? false,
  };
}

function ruleOf(d: Draft, order: number): RuleInput {
  const t = (s: string) => s.trim() || undefined;
  return {
    name: d.name.trim(),
    enabled: d.enabled,
    order,
    match: {
      messageRegex: t(d.messageRegex),
      traceRegex: t(d.traceRegex),
      statuses: d.statuses.length ? (d.statuses as ResultStatus[]) : undefined,
      environment: t(d.environment),
      fingerprint: t(d.fingerprint),
      testCaseId: t(d.testCaseId),
    },
    action: {
      defectTypeId: d.defectTypeId ?? undefined,
      comment: t(d.comment),
      issue: t(d.issueUrl) ? { url: d.issueUrl.trim(), key: t(d.issueKey) } : undefined,
      mute: d.muteDays !== '' && d.muteDays > 0 ? { days: d.muteDays } : undefined,
      accept: d.accept || undefined,
    },
  };
}

function DryRun({ project, draft }: { project: string; draft: Draft }) {
  const { launches } = useLaunchList(project);
  const firstFinished = launches.find((l) => l.status !== 'running')?.id ?? null;
  const [picked, setPicked] = useState<string | null>(null);
  const launchId = picked ?? firstFinished;
  const test = useTestRule(project);
  const results = useResults(project, launchId ?? '', {});
  const names = useMemo(() => new Map(flatItems(results.data).map((r) => [r.id, r] as const)), [results.data]);
  const matches = test.data?.matches ?? [];
  return (
    <Stack gap="xs" data-testid="rule-dry-run">
      <Text size="sm" fw={600}>
        Dry run
      </Text>
      <Text size="xs" c="dimmed">
        Shows which results of a launch the rule would match. Nothing is changed.
      </Text>
      <Group gap="xs" align="flex-end" wrap="wrap">
        <LaunchSelect project={project} value={launchId} onChange={(v) => { setPicked(v); test.reset(); }} label="Launch" style={{ flex: '1 1 240px' }} data-testid="dry-run-launch" />
        <Button variant="light" leftSection={<IconFlask size={14} />} disabled={!launchId || !!regexError(draft.messageRegex) || !!regexError(draft.traceRegex)} loading={test.isPending} onClick={() => launchId && test.mutate({ rule: ruleOf(draft, 0), launchId })} data-testid="dry-run">
          Test
        </Button>
      </Group>
      {test.isError && (
        <Alert color="red" variant="light">
          {errorMessage(test.error)}
        </Alert>
      )}
      {test.isSuccess && (
        <Stack gap={4} data-testid="dry-run-result">
          <Text size="sm" fw={600}>
            {matches.length} result{matches.length === 1 ? '' : 's'} would match
          </Text>
          {matches.slice(0, 50).map((id) => {
            const r = names.get(id);
            return (
              <Anchor key={id} component={Link} to={`/p/${project}/results/${id}`} size="xs" c="inherit" target="_blank" data-testid="dry-run-match">
                <Group gap={6} wrap="nowrap">
                  {r && <StatusDot status={r.status} size={8} />}
                  <span className="fh-truncate">{r?.fullName ?? id}</span>
                </Group>
              </Anchor>
            );
          })}
          {matches.length > 50 && (
            <Text size="xs" c="dimmed">
              and {matches.length - 50} more
            </Text>
          )}
        </Stack>
      )}
    </Stack>
  );
}

function RuleEditor({ project, rule, nextOrder, onClose }: { project: string; rule: Rule | null; nextOrder: number; onClose: () => void }) {
  const [d, setD] = useState<Draft>(() => draftOf(rule ?? undefined));
  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setD((p) => ({ ...p, [k]: v }));
  const types = useDefectTypes(project);
  const save = useSaveRule(project);
  const grouped = useMemo(
    () => DEFECT_GROUPS.map((g) => ({ group: GROUP_LABEL[g], items: (types.data ?? []).filter((t) => t.group === g).map((t) => ({ value: t.id, label: t.name })) })).filter((g) => g.items.length),
    [types.data],
  );
  const msgErr = regexError(d.messageRegex);
  const traceErr = regexError(d.traceRegex);
  const issueErr = d.issueUrl.trim() && !/^https?:\/\//i.test(d.issueUrl.trim()) ? 'Must be an http(s) link' : null;
  const hasMatch = !!(d.messageRegex.trim() || d.traceRegex.trim() || d.fingerprint.trim() || d.testCaseId.trim() || d.environment.trim());
  const hasAction = !!(d.defectTypeId || d.comment.trim() || d.issueUrl.trim() || d.muteDays || d.accept);
  const valid = !!d.name.trim() && !msgErr && !traceErr && !issueErr && hasMatch && hasAction;
  const submit = () =>
    save.mutate(
      { id: rule?.id, body: ruleOf(d, rule?.order ?? nextOrder) },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message: rule ? 'Rule saved' : 'Rule created' });
          onClose();
        },
      },
    );
  return (
    <Stack gap="md" data-testid="rule-editor">
      <TextInput label="Name" required maxLength={120} value={d.name} onChange={(e) => set('name', e.currentTarget.value)} data-testid="rule-name" />
      <Switch label="Enabled: apply to new results on ingest" checked={d.enabled} onChange={(e) => set('enabled', e.currentTarget.checked)} />
      <Divider label="Match" labelPosition="left" />
      <Textarea label="Error message regex" description="Rust regex syntax; size and time limited on the server" autosize minRows={1} maxRows={4} value={d.messageRegex} onChange={(e) => set('messageRegex', e.currentTarget.value)} error={msgErr} classNames={{ input: 'fp-mono' }} data-testid="rule-message-regex" />
      <Textarea label="Stack trace regex" autosize minRows={1} maxRows={4} value={d.traceRegex} onChange={(e) => set('traceRegex', e.currentTarget.value)} error={traceErr} classNames={{ input: 'fp-mono' }} />
      <MultiSelect label="Statuses" data={RESULT_STATUSES.map((s) => ({ value: s, label: STATUS_LABEL[s] }))} value={d.statuses} onChange={(v) => set('statuses', v)} comboboxProps={{ withinPortal: true }} />
      <Group grow gap="xs" wrap="wrap">
        <TextInput label="Environment" placeholder="any" maxLength={128} value={d.environment} onChange={(e) => set('environment', e.currentTarget.value)} miw={160} />
        <TextInput label="Fingerprint" placeholder="any" maxLength={64} value={d.fingerprint} onChange={(e) => set('fingerprint', e.currentTarget.value)} classNames={{ input: 'fp-mono' }} miw={160} />
      </Group>
      <TextInput label="Test case id" placeholder="any" maxLength={64} value={d.testCaseId} onChange={(e) => set('testCaseId', e.currentTarget.value)} classNames={{ input: 'fp-mono' }} />
      {!hasMatch && (
        <Text size="xs" c="var(--fh-semantic-warning-fg)">
          Give at least one condition besides statuses.
        </Text>
      )}
      <Divider label="Action" labelPosition="left" />
      <Select label="Set defect type" placeholder="Keep as is" data={grouped} value={d.defectTypeId} onChange={(v) => set('defectTypeId', v)} clearable searchable comboboxProps={{ withinPortal: true }} data-testid="rule-defect-type" />
      <Textarea label="Comment" autosize minRows={1} maxRows={4} maxLength={2000} value={d.comment} onChange={(e) => set('comment', e.currentTarget.value)} />
      <Group grow gap="xs" wrap="wrap">
        <TextInput label="Issue link" placeholder="https://…" maxLength={500} value={d.issueUrl} onChange={(e) => set('issueUrl', e.currentTarget.value)} error={issueErr} miw={200} />
        <TextInput label="Issue key" placeholder="SHOP-123" maxLength={64} value={d.issueKey} onChange={(e) => set('issueKey', e.currentTarget.value)} miw={120} />
      </Group>
      <Group gap="md" align="flex-end" wrap="wrap">
        <NumberInput label="Mute for days" placeholder="no mute" min={1} max={365} value={d.muteDays} onChange={(v) => set('muteDays', typeof v === 'number' ? v : '')} w={160} />
        <Switch label="Accept (counts as passed for gates)" checked={d.accept} onChange={(e) => set('accept', e.currentTarget.checked)} mb={6} />
      </Group>
      {!hasAction && (
        <Text size="xs" c="var(--fh-semantic-warning-fg)">
          Choose at least one action.
        </Text>
      )}
      <Divider />
      <DryRun project={project} draft={d} />
      {save.isError && (
        <Alert color="red" variant="light" title="Not saved">
          {errorMessage(save.error)}
        </Alert>
      )}
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={submit} disabled={!valid} loading={save.isPending} data-testid="rule-save">
          {rule ? 'Save' : 'Create'}
        </Button>
      </Group>
    </Stack>
  );
}

function ApplyModal({ project, rule, onClose }: { project: string; rule: Rule; onClose: () => void }) {
  const [launchId, setLaunchId] = useState<string | null>(null);
  const apply = useApplyRule(project);
  return (
    <Modal opened onClose={onClose} title={`Apply "${rule.name}" to a launch`} styles={{ title: { fontWeight: 700 } }}>
      <Stack gap="md" data-testid="rule-apply">
        <Text size="sm" c="dimmed">
          Applies the rule to results that were already ingested. Results triaged by hand keep their triage only if the rule does not match them.
        </Text>
        <LaunchSelect project={project} value={launchId} onChange={setLaunchId} label="Launch" data-testid="apply-launch" />
        {apply.isError && (
          <Alert color="red" variant="light">
            {errorMessage(apply.error)}
          </Alert>
        )}
        {apply.isSuccess && (
          <Alert color="green" variant="light" data-testid="rule-apply-result">
            {apply.data?.updated ?? apply.data?.matched ?? 0} results updated.
          </Alert>
        )}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
          <Button disabled={!launchId} loading={apply.isPending} onClick={() => launchId && apply.mutate({ id: rule.id, launchId })} data-testid="rule-apply-run">
            Apply
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

function MatchSummary({ r }: { r: Rule }) {
  const m = r.match;
  const parts: string[] = [];
  if (m.messageRegex) parts.push(`message ~ /${m.messageRegex}/`);
  if (m.traceRegex) parts.push(`trace ~ /${m.traceRegex}/`);
  if (m.environment) parts.push(`env = ${m.environment}`);
  if (m.fingerprint) parts.push(`fingerprint = ${m.fingerprint}`);
  if (m.testCaseId) parts.push(`test case = ${m.testCaseId.slice(0, 12)}`);
  return (
    <Group gap={6} wrap="wrap">
      {(m.statuses ?? []).map((s) => (
        <Badge key={s} size="xs" variant="outline" color="gray" leftSection={<StatusDot status={s} size={6} />} style={{ textTransform: 'none' }}>
          {s}
        </Badge>
      ))}
      {parts.map((p) => (
        <Text key={p} size="xs" className="fp-mono" style={{ wordBreak: 'break-all' }}>
          {p}
        </Text>
      ))}
    </Group>
  );
}

function ActionSummary({ r, types }: { r: Rule; types: Map<string, DefectType> }) {
  const a = r.action;
  const t = a.defectTypeId ? types.get(a.defectTypeId) : undefined;
  return (
    <Group gap={6} wrap="wrap">
      {a.defectTypeId && <DefectChip defect={{ typeId: a.defectTypeId, group: t?.group, name: t?.name ?? a.defectTypeId }} types={types} size="xs" />}
      {a.mute && (
        <Badge size="xs" variant="outline" color="gray" style={{ textTransform: 'none' }}>
          mute {a.mute.days ? `${String(a.mute.days)} d` : ''}
        </Badge>
      )}
      {a.accept && (
        <Badge size="xs" variant="outline" color="green" style={{ textTransform: 'none' }}>
          accept
        </Badge>
      )}
      {a.issue && (
        <Text size="xs" c="dimmed">
          issue {a.issue.key ?? a.issue.url}
        </Text>
      )}
      {a.comment && (
        <Text size="xs" c="dimmed" lineClamp={1}>
          “{a.comment}”
        </Text>
      )}
    </Group>
  );
}

export default function RulesPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const q = useRules(key);
  const types = useDefectTypeMap(key);
  const canWrite = useCan('rules:write', key);
  const save = useSaveRule(key);
  const del = useDeleteRule(key);
  const phone = useMediaQuery('(max-width: 48em)');
  const [editing, setEditing] = useState<Rule | 'new' | null>(null);
  const [applying, setApplying] = useState<Rule | null>(null);
  const [deleting, setDeleting] = useState<Rule | null>(null);
  const list = q.data ?? [];

  const move = (i: number, dir: -1 | 1) => {
    const a = list[i];
    const b = list[i + dir];
    if (!a || !b) return;
    save.mutate({ id: a.id, body: { order: b.order } });
    save.mutate({ id: b.id, body: { order: a.order } });
  };

  return (
    <>
      <PageHeader
        crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Rules' }]}
        title="Rules"
        subtitle="Rules triage known failures automatically when results are ingested, in the order shown. A matching rule sets the defect type, comment, issue or mute."
        actions={
          canWrite ? (
            <Button leftSection={<IconPlus size={16} />} onClick={() => setEditing('new')} data-testid="rule-new">
              New rule
            </Button>
          ) : undefined
        }
      />
      <QueryState query={q}>
        {() =>
          list.length === 0 ? (
            <EmptyState title="No rules yet" description={canWrite ? 'Create a rule from a recurring error message, then test it on a launch.' : 'Nobody has created rules in this project.'} />
          ) : (
            <Stack gap="sm" data-testid="rules">
              {list.map((r, i) => (
                <Card key={r.id} withBorder radius="md" padding="sm" data-testid="rule-row" opacity={r.enabled ? 1 : 0.7}>
                  <Group justify="space-between" align="flex-start" gap="sm" wrap="wrap">
                    <Stack gap={6} style={{ minWidth: 0, flex: '1 1 320px' }}>
                      <Group gap="xs" wrap="wrap">
                        <Badge size="sm" variant="default" radius="sm">
                          {i + 1}
                        </Badge>
                        <Text fw={700}>{r.name}</Text>
                        {!r.enabled && (
                          <Badge size="xs" color="gray" variant="light">
                            disabled
                          </Badge>
                        )}
                      </Group>
                      <MatchSummary r={r} />
                      <ActionSummary r={r} types={types} />
                      <Text size="xs" c="dimmed">
                        {r.hits.toLocaleString()} hits · updated {formatRelative(r.updatedAt)}
                      </Text>
                    </Stack>
                    {canWrite && (
                      <Group gap={4} wrap="nowrap">
                        <Switch size="sm" checked={r.enabled} onChange={(e) => save.mutate({ id: r.id, body: { enabled: e.currentTarget.checked } })} aria-label={r.enabled ? `Disable ${r.name}` : `Enable ${r.name}`} mr={6} />
                        <Tooltip label="Move up">
                          <ActionIcon variant="subtle" disabled={i === 0} onClick={() => move(i, -1)} aria-label="Move up">
                            <IconArrowUp size={16} />
                          </ActionIcon>
                        </Tooltip>
                        <Tooltip label="Move down">
                          <ActionIcon variant="subtle" disabled={i === list.length - 1} onClick={() => move(i, 1)} aria-label="Move down">
                            <IconArrowDown size={16} />
                          </ActionIcon>
                        </Tooltip>
                        <Tooltip label="Apply to a launch">
                          <ActionIcon variant="subtle" onClick={() => setApplying(r)} aria-label={`Apply ${r.name}`} data-testid="rule-apply-open">
                            <IconPlayerPlay size={16} />
                          </ActionIcon>
                        </Tooltip>
                        <Tooltip label="Edit">
                          <ActionIcon variant="subtle" onClick={() => setEditing(r)} aria-label={`Edit ${r.name}`} data-testid="rule-edit">
                            <IconPencil size={16} />
                          </ActionIcon>
                        </Tooltip>
                        <Tooltip label="Delete">
                          <ActionIcon variant="subtle" color="red" onClick={() => setDeleting(r)} aria-label={`Delete ${r.name}`}>
                            <IconTrash size={16} />
                          </ActionIcon>
                        </Tooltip>
                      </Group>
                    )}
                  </Group>
                </Card>
              ))}
            </Stack>
          )
        }
      </QueryState>
      <Drawer opened={editing !== null} onClose={() => setEditing(null)} position="right" size={phone ? '100%' : 'lg'} title={editing === 'new' ? 'New rule' : 'Edit rule'} styles={{ title: { fontWeight: 700 } }}>
        {editing !== null && <RuleEditor key={editing === 'new' ? 'new' : editing.id} project={key} rule={editing === 'new' ? null : editing} nextOrder={(list[list.length - 1]?.order ?? 0) + 1} onClose={() => setEditing(null)} />}
      </Drawer>
      {applying && <ApplyModal project={key} rule={applying} onClose={() => setApplying(null)} />}
      <Modal opened={!!deleting} onClose={() => setDeleting(null)} title="Delete the rule?" styles={{ title: { fontWeight: 700 } }}>
        <Stack gap="md">
          <Text size="sm">Results it already triaged keep their triage.</Text>
          {del.isError && (
            <Alert color="red" variant="light">
              {errorMessage(del.error)}
            </Alert>
          )}
          <Group justify="flex-end" gap="xs">
            <Button variant="default" onClick={() => setDeleting(null)}>
              Cancel
            </Button>
            <Button color="red" loading={del.isPending} onClick={() => deleting && del.mutate(deleting.id, { onSuccess: () => setDeleting(null) })}>
              Delete
            </Button>
          </Group>
        </Stack>
      </Modal>
    </>
  );
}
