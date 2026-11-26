import { Alert, Anchor, Badge, Button, Card, Drawer, Group, Stack, Table, Text, Title } from '@mantine/core';
import { useMediaQuery } from '@mantine/hooks';
import { notifications } from '@mantine/notifications';
import { IconBellOff, IconBug, IconFilePlus, IconInfoCircle } from '@tabler/icons-react';
import { useCan } from '@franshub/ui-shell';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { CONFIDENCE_COLOR, examplesOf, HINT_KIND_HELP, HINT_KIND_LABEL, latestResult, useFlakyHints, type FlakyHint, type FlakyHints, type HintExample, type SignalSource } from '../../api/flakyHints';
import { useDefectTypes, useSaveRule } from '../../api/stageC';
import type { FlakyTest, RuleInput } from '../../api/types-next';
import { CreateIssueModal } from '../../components/CreateIssueModal';
import { QueryState } from '../../components/QueryState';
import { formatDateTime, formatRelative } from '../../lib/format';
import type { TriageTarget } from '../../lib/triage';
import { MuteDialog } from './MuteDialog';

/** Readable names of the evidence fields. */
const FIELD_LABEL: Record<string, string> = {
  failures: 'Failures',
  timeoutFailures: 'Timeouts',
  ceilingMs: 'Ceiling (ms)',
  atCeiling: 'Failures at the ceiling',
  passP95Ms: 'p95 of passes (ms)',
  passP95OfCeiling: 'p95 of passes / ceiling',
  dimension: 'Dimension',
  value: 'Value',
  runs: 'Runs with it',
  failRate: 'Failure rate with it',
  otherRuns: 'Runs elsewhere',
  otherFailures: 'Failures elsewhere',
  otherFailRate: 'Failure rate elsewhere',
  ratio: 'Ratio',
  p: 'p (Fisher, two-sided)',
  via: 'Evidence',
  name: 'Other test',
  test: 'Other test id',
  jaccard: 'Jaccard of failing launches',
  shared: 'Shared failing launches',
  failuresWith: 'Failures after it',
  passes: 'Clean passes',
  passesWith: 'Clean passes after it',
  retriedRuns: 'Runs passing on retry',
  share: 'Share of failures',
  distinctErrors: 'Different errors',
  fingerprints: 'Errors (fingerprints)',
  matches: 'Failures with infrastructure evidence',
  bursts: 'In a burst of infrastructure errors',
  infraErrors: 'With an infrastructure error',
  signals: 'Signals',
};

const VIA_LABEL: Record<string, string> = { coFailure: 'fails in the same launches', worker: 'ran earlier on the same worker' };

const STATUS_LABEL: Record<HintExample['status'], string> = { failed: 'failed', passedOnRetry: 'passed on retry', passed: 'passed', other: 'other' };

function show(k: string, v: unknown): string {
  if (k === 'via' && typeof v === 'string') return VIA_LABEL[v] ?? v;
  if (Array.isArray(v)) return v.map((x) => String(x)).join(', ');
  if (v && typeof v === 'object') {
    return Object.entries(v as Record<string, unknown>)
      .map(([a, b]) => `${a.replace(':', ' ')} ×${String(b)}`)
      .join(', ');
  }
  if (typeof v === 'number' && (k.endsWith('Rate') || k === 'share' || k === 'passP95OfCeiling')) return `${String(Math.round(v * 1000) / 10)}%`;
  return String(v);
}

function Evidence({ hint }: { hint: FlakyHint }) {
  const rows = Object.entries(hint.evidence).filter(([k, v]) => k !== 'examples' && k !== 'test' && v !== null && v !== undefined);
  return (
    <Table fz="xs" verticalSpacing={2} withRowBorders={false} data-testid="hint-evidence">
      <Table.Tbody>
        {rows.map(([k, v]) => (
          <Table.Tr key={k}>
            <Table.Td c="dimmed" w="50%" style={{ verticalAlign: 'top' }}>
              {FIELD_LABEL[k] ?? k}
            </Table.Td>
            <Table.Td style={{ wordBreak: 'break-word' }}>{show(k, v)}</Table.Td>
          </Table.Tr>
        ))}
      </Table.Tbody>
    </Table>
  );
}

function Examples({ project, hint }: { project: string; hint: FlakyHint }) {
  const ex = examplesOf(hint);
  if (!ex.length) return null;
  return (
    <Stack gap={2} data-testid="hint-examples">
      <Text size="xs" fw={600}>
        Recent runs
      </Text>
      {ex.map((e) => (
        <Group key={`${e.launchId}:${e.resultId ?? ''}`} gap="xs" wrap="nowrap">
          <Anchor component={Link} to={e.resultId ? `/p/${project}/results/${e.resultId}` : `/p/${project}/launches/${e.launchId}`} size="xs">
            {formatDateTime(e.at)}
          </Anchor>
          <Text size="xs" c="dimmed">
            {STATUS_LABEL[e.status]}
          </Text>
        </Group>
      ))}
    </Stack>
  );
}

/** Rule drafts match messages case by case (rule regexes are case-sensitive, and the editor checks them as JavaScript). */
const TIMEOUT_REGEX = '[Tt]imed? ?[Oo]ut|[Tt]imeout|[Dd]eadline exceeded';
const INFRA_REGEX = '[Cc]onnection (refused|reset)|ECONNREFUSED|ECONNRESET|[Bb]ad [Gg]ateway|[Ss]ervice [Uu]navailable|[Oo]ut of memory|[Ss]ession not created|[Bb]rowser (has )?crashed';

/** The built-in defect type of a group, for rule drafts. */
function useBuiltIn(project: string, group: string): string | null {
  const types = useDefectTypes(project);
  return types.data?.find((t) => t.builtIn && t.group === group)?.id ?? null;
}

/** A disabled rule for the cause, for a person to review in Rules. */
function draftFor(hint: FlakyHint, item: FlakyTest, defectTypeId: string | null): RuleInput {
  const name = `Flaky ${HINT_KIND_LABEL[hint.kind].toLowerCase()}: ${item.name}`.slice(0, 200);
  if (hint.kind === 'infra') {
    const sig = hint.evidence.signals;
    const signals = sig && typeof sig === 'object' ? Object.keys(sig) : [];
    const top = signals[0]?.split(':') ?? [];
    return {
      name,
      enabled: false,
      match: top.length === 2 ? { testCaseId: item.testCaseId, signals: [{ source: top[0] as SignalSource, kind: top[1] }] } : { testCaseId: item.testCaseId, messageRegex: INFRA_REGEX },
      action: { defectTypeId: defectTypeId ?? undefined, comment: 'Infrastructure trouble (flaky-cause hint)' },
    };
  }
  return {
    name,
    enabled: false,
    match: { testCaseId: item.testCaseId, messageRegex: TIMEOUT_REGEX, statuses: ['failed', 'broken'] },
    action: { defectTypeId: defectTypeId ?? undefined, comment: 'Timed out (flaky-cause hint)' },
  };
}

function HintCard({ project, item, hint, latest, onMute, onIssue }: { project: string; item: FlakyTest; hint: FlakyHint; latest: HintExample | null; onMute: () => void; onIssue: () => void }) {
  const canTriage = useCan('results:triage', project);
  const canMute = useCan('results:mute', project) && canTriage;
  const canIssue = useCan('issues:write', project);
  const canRule = useCan('rules:write', project);
  const save = useSaveRule(project);
  const [draft, setDraft] = useState<string | null>(null);
  const automation = useBuiltIn(project, 'automationBug');
  const system = useBuiltIn(project, 'systemIssue');
  const createRule = () =>
    save.mutate(
      { body: draftFor(hint, item, hint.kind === 'infra' ? system : automation) },
      {
        onSuccess: (r) => setDraft(r.name),
        onError: (e) => notifications.show({ color: 'red', title: 'Rule draft not created', message: errorMessage(e) }),
      },
    );
  const mute = hint.actions.includes('mute') && canMute && !!latest?.resultId;
  const issue = hint.actions.includes('issue') && canIssue && !!latest?.resultId;
  const rule = hint.actions.includes('rule') && canRule;
  return (
    <Card withBorder radius="md" padding="sm" data-testid="hint-card" data-kind={hint.kind}>
      <Stack gap="xs">
        <Group gap="xs" justify="space-between" wrap="nowrap">
          <Group gap="xs">
            <Badge variant="light" color={CONFIDENCE_COLOR[hint.confidence]} style={{ textTransform: 'none' }}>
              {HINT_KIND_LABEL[hint.kind]}
            </Badge>
            <Text size="xs" c="dimmed">
              {hint.confidence} confidence · {hint.sample} {hint.sample === 1 ? 'run' : 'runs'} of evidence
            </Text>
          </Group>
        </Group>
        <Text size="sm" style={{ wordBreak: 'break-word' }} data-testid="hint-summary">
          {hint.summary}
        </Text>
        <Text size="xs" c="dimmed">
          {HINT_KIND_HELP[hint.kind]}
        </Text>
        <Evidence hint={hint} />
        <Examples project={project} hint={hint} />
        {draft && (
          <Alert color="green" variant="light" p="xs" data-testid="hint-rule-created">
            <Text size="sm">
              Rule draft created: “{draft}”, disabled.{' '}
              <Anchor component={Link} to={`/p/${project}/rules`} size="sm">
                Review it in Rules
              </Anchor>
            </Text>
          </Alert>
        )}
        {(mute || issue || rule) && (
          <Group gap="xs">
            {mute && (
              <Button size="xs" variant="light" leftSection={<IconBellOff size={14} />} onClick={onMute} data-testid="hint-mute">
                Mute with an end
              </Button>
            )}
            {issue && (
              <Button size="xs" variant="light" leftSection={<IconBug size={14} />} onClick={onIssue} data-testid="hint-issue">
                Issue
              </Button>
            )}
            {rule && (
              <Button size="xs" variant="light" leftSection={<IconFilePlus size={14} />} loading={save.isPending} disabled={!!draft} onClick={createRule} data-testid="hint-rule">
                Rule draft
              </Button>
            )}
          </Group>
        )}
      </Stack>
    </Card>
  );
}

function Body({ project, item, data }: { project: string; item: FlakyTest; data: FlakyHints }) {
  const [muting, setMuting] = useState<FlakyHint | null>(null);
  const [issueFor, setIssueFor] = useState<TriageTarget | null>(null);
  const latest = latestResult(data.hints);
  const window = data.backend === 'clickhouse' ? 'the whole ClickHouse history' : 'the last 50 runs within raw retention';
  return (
    <Stack gap="sm" data-testid="hint-panel">
      <Text size="sm" data-testid="hint-window">
        {data.windowRuns} runs, {data.failures} failing
        {data.infraShare !== null && data.infraShare !== undefined ? `, ${String(Math.round(data.infraShare * 100))}% of them with infrastructure evidence` : ''} ({window}).
        {data.computedAt ? ` Computed ${formatRelative(data.computedAt)}.` : ''}
      </Text>
      {data.hints.length === 0 ? (
        <Alert variant="light" color="gray" icon={<IconInfoCircle size={16} />} data-testid="hint-empty">
          {data.computedAt ? 'No cause has enough evidence yet. Each kind needs a minimum of runs or failures before it says anything.' : 'Causes are computed after the next launch in which this test changes status, or overnight.'}
        </Alert>
      ) : (
        data.hints.map((h) => (
          <HintCard
            key={h.kind}
            project={project}
            item={item}
            hint={h}
            latest={latest}
            onMute={() => setMuting(h)}
            onIssue={() => latest?.resultId && setIssueFor({ label: item.name, count: 1, resultIds: [latest.resultId] })}
          />
        ))
      )}
      <Text size="xs" c="dimmed">
        A rule-based search over the evidence shown, not a learned model. A hint never mutes, files or changes anything by itself; failures with infrastructure evidence still count in the flakiness score.
      </Text>
      {muting && latest?.resultId && (
        <MuteDialog project={project} resultId={latest.resultId} testName={item.name} reason={`Flaky (${HINT_KIND_LABEL[muting.kind].toLowerCase()}): ${muting.summary}`} onClose={() => setMuting(null)} />
      )}
      <CreateIssueModal project={project} target={issueFor} onClose={() => setIssueFor(null)} />
    </Stack>
  );
}

/** The likely causes of one flaky test with their evidence and actions. */
export function HintPanel({ project, item, onClose }: { project: string; item: FlakyTest; onClose: () => void }) {
  const phone = useMediaQuery('(max-width: 48em)');
  const q = useFlakyHints(project, item.seriesId);
  return (
    <Drawer
      opened
      onClose={onClose}
      position="right"
      size={phone ? '100%' : 'lg'}
      title={
        <Stack gap={0}>
          <Title order={4}>Likely causes</Title>
          <Text size="sm" c="dimmed" style={{ wordBreak: 'break-word' }}>
            {item.name}
          </Text>
        </Stack>
      }
    >
      <QueryState query={q} notFound="This test is no longer in the project">
        {() => q.data && <Body project={project} item={item} data={q.data} />}
      </QueryState>
    </Drawer>
  );
}
