import { Card, Group, Stack, Text } from '@mantine/core';
import { useRunItemExecutions, type RunItemExecution } from '../../api/tm';
import { formatDateTime } from '../../lib/format';
import { valuesLabel } from '../../lib/tm/parameters';
import { defectInfo } from '../../lib/tm/ui';
import { RunStatusBadge } from '../cases/common';

/** The iterations of an earlier execution of a parameterized item: values, status and step results of each. */
export function ExecutionIterations({ x }: { x: RunItemExecution }) {
  const list = x.iterations ?? [];
  const count = x.iterationCount ?? 0;
  if (count === 0) return null;
  return (
    <Stack gap={2} ml="sm" miw={0} data-testid="execution-iterations" aria-label={`Iterations of run ${String(x.executionNo)}`}>
      {list.map((it) => {
        const defect = defectInfo(it.defect);
        const open = it.status === 'failed' || it.status === 'blocked';
        return (
          <details key={it.rowId} style={{ minWidth: 0 }} open={open} data-testid="execution-iteration" data-status={it.status} data-row={it.rowId}>
            <summary style={{ cursor: 'pointer', overflowWrap: 'anywhere' }}>
              <Text span size="xs" className="fp-mono">
                #{it.position}
              </Text>{' '}
              <Text span size="xs">
                {valuesLabel(x.parameterColumns ?? [], it.values)}
              </Text>{' '}
              <RunStatusBadge status={it.status} size="xs" />
            </summary>
            <Stack gap={2} ml="md" mt={2}>
              {it.comment && (
                <Text size="xs" style={{ whiteSpace: 'pre-wrap' }}>
                  {it.comment}
                </Text>
              )}
              {it.stepResults.map((r) => (
                <Text key={r.path} size="xs" style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }} data-testid="execution-iteration-step" data-status={r.status}>
                  Step {r.path} {r.status}
                  {r.actual ? `: ${r.actual}` : ''}
                </Text>
              ))}
              {it.stepResults.length === 0 && it.stepResultsOmitted && (
                <Text size="xs" c="dimmed">
                  Step results are not listed here.
                </Text>
              )}
              {defect && (
                <Text size="xs">
                  Defect: {defect.label}
                  {defect.issue ? ` · ${defect.issue}` : ''}
                </Text>
              )}
            </Stack>
          </details>
        );
      })}
      {list.length < count && (
        <Text size="xs" c="dimmed" data-testid="execution-iterations-more">
          Showing {list.length} of {count} iterations.
        </Text>
      )}
    </Stack>
  );
}

/** Earlier executions of a run item (kept on rerun): status, executor, times, comment, the steps that did not pass and the iterations. */
export function Executions({ project, itemId, executionNo }: { project: string; itemId: string; executionNo: number }) {
  const q = useRunItemExecutions(project, itemId, executionNo > 1);
  const list = q.data ?? [];
  if (executionNo <= 1 || list.length === 0) return null;
  return (
    <Card withBorder radius="lg" padding="sm" data-testid="run-executions" component="section" aria-labelledby="run-executions-title">
      <Stack gap="xs">
        <Text size="sm" fw={600} component="h3" m={0} id="run-executions-title">
          Earlier runs of this case
        </Text>
        {list.map((x) => {
          const notPassed = x.stepResults.filter((r) => r.status === 'failed' || r.status === 'blocked');
          const defect = defectInfo(x.defect);
          return (
            <Stack key={x.executionNo} gap={2} data-testid="run-execution" data-execution={x.executionNo}>
              <Group gap="xs" wrap="wrap">
                <Text size="sm" fw={500}>
                  Run {x.executionNo}
                </Text>
                <RunStatusBadge status={x.status} size="xs" />
                <Text size="xs" c="dimmed">
                  {x.executedBy ? `by ${x.executedBy}` : 'not executed'}
                  {x.finishedAt ? ` · ${formatDateTime(x.finishedAt)}` : ''} · run again by {x.rerunBy} {formatDateTime(x.rerunAt)}
                </Text>
              </Group>
              {x.comment && (
                <Text size="xs" style={{ whiteSpace: 'pre-wrap' }}>
                  {x.comment}
                </Text>
              )}
              {notPassed.map((r) => (
                <Text key={r.path} size="xs" style={{ whiteSpace: 'pre-wrap' }}>
                  Step {r.path} {r.status}
                  {r.actual ? `: ${r.actual}` : ''}
                </Text>
              ))}
              {defect && (
                <Text size="xs">
                  Defect: {defect.label}
                  {defect.issue ? ` · ${defect.issue}` : ''}
                </Text>
              )}
              <ExecutionIterations x={x} />
            </Stack>
          );
        })}
      </Stack>
    </Card>
  );
}
