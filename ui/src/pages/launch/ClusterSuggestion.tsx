import { Alert, Anchor, Badge, Button, Collapse, Group, Paper, Stack, Table, Text, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { useCan } from '@franshub/ui-shell';
import { IconBulb, IconCheck, IconChevronDown, IconChevronUp, IconGavel, IconRefresh, IconX } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { useTriage } from '../../api/stageC';
import { bandLabel, suggestionChip, useAcceptSuggestion, useClusterSuggestions, useRecomputeSuggestions, useRejectSuggestion, useSuggestionRule, type ClusterSuggestions, type Suggestion } from '../../api/suggestions';
import type { Launch } from '../../api/types';
import type { Cluster, DefectInfo } from '../../api/types-next';
import { formatDate } from '../../lib/format';

const BAND_COLOR: Record<string, string> = { same: 'teal', high: 'blue', low: 'gray' };

const NO_SUGGESTION: Record<string, string> = {
  newError: 'New error: nothing to compare with yet.',
  noHistory: 'No triage history in this project yet.',
  belowThreshold: 'No earlier decision is close enough.',
};

/** The compact chip of a cluster's top suggestion ("Product bug · high · as in #431, PAY-1234"). */
export function SuggestionChip({ s }: { s: Suggestion }) {
  return (
    <Badge size="sm" variant="light" color={BAND_COLOR[s.band] ?? 'gray'} leftSection={<IconBulb size={12} />} style={{ textTransform: 'none', maxWidth: '100%' }} data-testid="suggestion-chip">
      <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{suggestionChip(s)}</span>
    </Badge>
  );
}

/** The top suggestion of a cluster in its row (nothing without one). */
export function ClusterSuggestionChip({ project, launch, cluster }: { project: string; launch: Launch; cluster: Cluster }) {
  const { cluster: c } = useClusterSuggestions(project, launch.id, cluster.fingerprint);
  const top = c?.suggestions.find((s) => s.state === 'open');
  return top ? <SuggestionChip s={top} /> : null;
}

function signedWeight(w: number): string {
  const v = Math.round(w * 100) / 100;
  return v > 0 ? `+${v.toFixed(2)}` : v.toFixed(2);
}

function Why({ project, s, cluster }: { project: string; s: Suggestion; cluster: ClusterSuggestions | null }) {
  const commits = cluster?.commits;
  return (
    <Stack gap={6} data-testid="suggestion-why">
      <Table withRowBorders={false} verticalSpacing={2} horizontalSpacing="xs" fz="xs">
        <Table.Tbody>
          {s.why.map((w, i) => (
            <Table.Tr key={`${w.signal}-${String(i)}`}>
              <Table.Td className="fp-mono" style={{ width: 56, color: w.weight < 0 ? 'var(--mantine-color-red-text)' : undefined }}>
                {signedWeight(w.weight)}
              </Table.Td>
              <Table.Td>{w.detail}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
      <Text size="xs" c="dimmed">
        Score {s.score.toFixed(2)}: the sum of the rows above (the age row scales it). This is a nearest-decision search over what people decided, not a trained model.
      </Text>
      {s.sample && (
        <Text size="xs" c="dimmed" data-testid="suggestion-sample">
          Decided by {s.sample.decidedBy} on {formatDate(s.sample.decidedAt)}
          {s.sample.launchNumber ? ` in launch #${String(s.sample.launchNumber)}` : ''} · confirmed {s.sample.confirmed} · overridden {s.sample.overridden}
          {s.sample.resultId ? (
            <>
              {' · '}
              <Anchor component={Link} to={`/p/${encodeURIComponent(project)}/results/${encodeURIComponent(s.sample.resultId)}`} size="xs" data-testid="suggestion-sample-link">
                sample result
              </Anchor>
            </>
          ) : null}
        </Text>
      )}
      {commits && (commits.lastGood || commits.first) && (
        <Text size="xs" c="dimmed" data-testid="suggestion-commits">
          Commits between {commits.lastGood?.slice(0, 7) ?? '?'} and {commits.first?.slice(0, 7) ?? '?'}
          {commits.compareUrl ? (
            <>
              {' · '}
              <Anchor href={commits.compareUrl} target="_blank" rel="noopener noreferrer" size="xs">
                compare
              </Anchor>
            </>
          ) : null}
        </Text>
      )}
      {(s.comment || s.issueUrl) && (
        <Text size="xs" c="dimmed">
          {s.comment ? `"${s.comment}"` : ''}
          {s.issueUrl ? (
            <>
              {s.comment ? ' · ' : ''}
              <Anchor href={s.issueUrl} target="_blank" rel="noopener noreferrer" size="xs">
                {s.issueKey ?? 'issue'}
              </Anchor>
            </>
          ) : null}
        </Text>
      )}
    </Stack>
  );
}

/** One suggestion with its actions and its "why". */
export function SuggestionCard({ project, s, cluster, canTriage, canRule, onOther, testId = 'suggestion' }: { project: string; s: Suggestion; cluster: ClusterSuggestions | null; canTriage: boolean; canRule: boolean; onOther?: () => void; testId?: string }) {
  const [why, setWhy] = useState(false);
  const accept = useAcceptSuggestion(project);
  const reject = useRejectSuggestion(project);
  const rule = useSuggestionRule(project);
  const open = s.state === 'open';
  return (
    <Paper withBorder radius="md" p="xs" data-testid={testId} data-band={s.band} data-state={s.state}>
      <Stack gap={6}>
        <Group gap="xs" wrap="wrap" justify="space-between">
          <Group gap={6} wrap="wrap" style={{ minWidth: 0 }}>
            <SuggestionChip s={s} />
            {s.state === 'accepted' && (
              <Badge size="xs" color="teal" variant="outline" style={{ textTransform: 'none' }}>
                Applied
              </Badge>
            )}
            <Tooltip label={s.precision === null ? 'The percentage shows once a backtest had 50 decisions in this band' : 'Precision of this band in the latest backtest'}>
              <Text size="xs" c="dimmed">
                {bandLabel(s)}
              </Text>
            </Tooltip>
          </Group>
          <Button size="compact-xs" variant="subtle" onClick={() => setWhy((v) => !v)} rightSection={why ? <IconChevronUp size={12} /> : <IconChevronDown size={12} />} data-testid="suggestion-why-toggle">
            Why
          </Button>
        </Group>
        {open && (canTriage || canRule) && (
          <Group gap="xs" wrap="wrap">
            {canTriage && (
              <Button
                size="compact-xs"
                leftSection={<IconCheck size={12} />}
                loading={accept.isPending}
                onClick={() =>
                  accept.mutate(
                    { id: s.id },
                    {
                      onSuccess: (r) => notifications.show({ color: 'green', title: 'Suggestion accepted', message: `${String(r.updated)} result${r.updated === 1 ? '' : 's'} triaged${r.skipped ? `, ${String(r.skipped)} decided by people left as they were` : ''}` }),
                      onError: (e) => notifications.show({ color: 'red', title: 'Not accepted', message: errorMessage(e) }),
                    },
                  )
                }
                data-testid="suggestion-accept"
              >
                Accept
              </Button>
            )}
            {canTriage && onOther && (
              <Button size="compact-xs" variant="light" onClick={onOther} data-testid="suggestion-other">
                Other
              </Button>
            )}
            {canTriage && (
              <Button
                size="compact-xs"
                variant="light"
                color="gray"
                leftSection={<IconX size={12} />}
                loading={reject.isPending}
                onClick={() => reject.mutate({ id: s.id }, { onError: (e) => notifications.show({ color: 'red', title: 'Not rejected', message: errorMessage(e) }) })}
                data-testid="suggestion-reject"
              >
                Reject
              </Button>
            )}
            {canRule && s.ruleDraftOffered && (
              <Button
                size="compact-xs"
                variant="light"
                color="grape"
                leftSection={<IconGavel size={12} />}
                loading={rule.isPending}
                onClick={() =>
                  rule.mutate(s.id, {
                    onSuccess: (r) => notifications.show({ color: 'green', title: 'Rule drafted', message: `"${r.name}" was created disabled; review and enable it under Rules.` }),
                    onError: (e) => notifications.show({ color: 'red', title: 'No rule', message: errorMessage(e) }),
                  })
                }
                data-testid="suggestion-rule"
              >
                Make a rule
              </Button>
            )}
          </Group>
        )}
        <Collapse expanded={why}>
          <Why project={project} s={s} cluster={cluster} />
        </Collapse>
      </Stack>
    </Paper>
  );
}

/** A defect an AI agent or auto-apply chose, with "Confirm" (a person's triage with the same type). */
export function ConfirmDecision({ project, launchId, fingerprint, defect, canTriage }: { project: string; launchId: string; fingerprint: string; defect: DefectInfo | null | undefined; canTriage: boolean }) {
  const triage = useTriage(project);
  const source = defect?.source as string | undefined;
  if (!defect || (source !== 'ai' && source !== 'auto')) return null;
  return (
    <Alert variant="light" color="blue" p="xs" data-testid="confirm-decision">
      <Group gap="xs" justify="space-between" wrap="wrap">
        <Text size="xs">
          {source === 'auto' ? 'Applied automatically' : 'Decided by an AI agent'}: {defect.name}. People's decisions teach the suggestions; machine ones do not.
        </Text>
        {canTriage && (
          <Button
            size="compact-xs"
            variant="light"
            loading={triage.isPending}
            onClick={() =>
              triage.mutate(
                { launchId, fingerprint, defectTypeId: defect.typeId, source: 'user' },
                {
                  onSuccess: () => notifications.show({ color: 'green', title: 'Confirmed', message: `${defect.name} is now your decision` }),
                  onError: (e) => notifications.show({ color: 'red', title: 'Not confirmed', message: errorMessage(e) }),
                },
              )
            }
            data-testid="confirm-decision-button"
          >
            Confirm
          </Button>
        )}
      </Group>
    </Alert>
  );
}

/** Triage suggestions of a cluster: up to three, each with Accept, Other, Reject, Make a rule and "Why". */
export function ClusterSuggestion({ project, launch, cluster, onOther }: { project: string; launch: Launch; cluster: Cluster; onOther?: () => void }) {
  const { on, q, cluster: c } = useClusterSuggestions(project, launch.id, cluster.fingerprint);
  const canTriage = useCan('results:triage', project);
  const canRules = useCan('rules:write', project);
  const canPropose = useCan('ai:propose', project);
  const canRule = canRules || canPropose;
  const recompute = useRecomputeSuggestions(project, launch.id);
  if (!on) return null;
  const confirm = <ConfirmDecision project={project} launchId={launch.id} fingerprint={cluster.fingerprint} defect={cluster.defect} canTriage={canTriage} />;
  if (q.isLoading) return null;
  if (q.data?.state === 'pending' && !c?.suggestions.length) {
    return (
      <Text size="xs" c="dimmed" data-testid="suggestions-pending">
        Suggestions are computed when the launch has finished.
      </Text>
    );
  }
  if (!c) return confirm;
  const shown = c.suggestions.filter((s) => s.state === 'open' || s.state === 'accepted');
  if (!shown.length) {
    const why = c.noSuggestion && NO_SUGGESTION[c.noSuggestion];
    return (
      <Stack gap={4}>
        {confirm}
        {why && (
          <Group gap="xs" wrap="wrap">
            <Text size="xs" c="dimmed" data-testid="no-suggestion" data-reason={c.noSuggestion ?? ''}>
              {why}
            </Text>
            {canTriage && (
              <Button size="compact-xs" variant="subtle" leftSection={<IconRefresh size={12} />} loading={recompute.isPending} onClick={() => recompute.mutate(undefined, { onError: (e) => notifications.show({ color: 'red', title: 'Not recomputed', message: errorMessage(e) }) })} data-testid="suggestions-recompute">
                Recompute
              </Button>
            )}
          </Group>
        )}
      </Stack>
    );
  }
  return (
    <Stack gap={6} data-testid="cluster-suggestions">
      {confirm}
      {shown.map((s) => (
        <SuggestionCard key={s.id} project={project} s={s} cluster={c} canTriage={canTriage} canRule={canRule} onOther={onOther} />
      ))}
    </Stack>
  );
}
