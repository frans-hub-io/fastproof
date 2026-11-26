import { Badge, Card, Code, Group, Stack, Text, Tooltip } from '@mantine/core';
import { useAnalysisCapabilities } from '../../api/analysis';
import { byAttempt, confidenceLabel, detailLines, signalKindLabel, signalSourceColor, signalSourceLabel, useResultSignals, type AttemptSignal } from '../../api/signals';
import type { ResultDetail } from '../../api/types';
import { formatDateTime } from '../../lib/format';

function SignalCard({ s }: { s: AttemptSignal }) {
  const lines = detailLines(s.detail);
  return (
    <Card withBorder radius="md" padding="sm" data-testid="result-signal">
      <Stack gap={6}>
        <Group gap="xs" wrap="wrap">
          <Badge variant="light" color={signalSourceColor(s.source)} style={{ textTransform: 'none' }} data-testid="result-signal-source">
            {signalSourceLabel(s.source)}
          </Badge>
          <Text size="sm" fw={500} data-testid="result-signal-kind">
            {signalKindLabel(s.kind)}
          </Text>
          <Tooltip
            multiline
            w={260}
            label={s.confidence === 'exact' ? 'The sender named this attempt (trace id or client id).' : 'Only the time of the attempt matched, so it may belong to another test running at the same time.'}
          >
            <Badge size="xs" variant={s.confidence === 'exact' ? 'light' : 'outline'} color="gray" style={{ textTransform: 'none' }} data-testid="result-signal-confidence">
              {confidenceLabel(s.confidence)}
            </Badge>
          </Tooltip>
        </Group>
        <Text size="xs" c="dimmed">
          {formatDateTime(s.at)} · <span className="fp-mono">{s.kind}</span>
        </Text>
        {lines.length > 0 && (
          <Code block style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }} data-testid="result-signal-detail">
            {lines.join('\n')}
          </Code>
        )}
      </Stack>
    </Card>
  );
}

/** Signals of grids, CI, Kubernetes and FastMock about a result's attempts. */
export function SignalsBlock({ project, r }: { project: string; r: ResultDetail }) {
  const caps = useAnalysisCapabilities(project);
  const on = caps.data?.features.signals === true;
  const q = useResultSignals(project, r.id, on);
  const items = q.data?.items ?? [];
  if (!on || items.length === 0) return null;
  const groups = byAttempt(items);
  const several = r.attempts.length > 1 || groups.length > 1;
  return (
    <Stack gap="xs" data-testid="result-signals">
      <Text size="sm" fw={600}>
        Signals
      </Text>
      <Text size="xs" c="dimmed">
        What grids, CI, Kubernetes and FastMock reported about the attempts. Signals never change the defect: they add weight to suggestions and flaky-cause hints and can be used in rule conditions. Details come from the sender and are shown as text.
      </Text>
      {groups.map((g) => (
        <Stack key={g.attempt ?? 'none'} gap={6} data-testid="result-signals-attempt">
          {several && (
            <Text size="xs" fw={600} c="dimmed">
              {g.attempt === null ? 'Attempt unknown' : `Attempt ${String(g.attempt)}`}
            </Text>
          )}
          {g.items.map((s) => (
            <SignalCard key={s.id} s={s} />
          ))}
        </Stack>
      ))}
    </Stack>
  );
}
