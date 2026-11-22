import { Card, Group, Stack, Text } from '@mantine/core';
import { useRunItemExecutions } from '../../api/tm';
import { formatDateTime } from '../../lib/format';
import { defectInfo } from '../../lib/tm/ui';
import { RunStatusBadge } from '../cases/common';

/** Earlier executions of a run item (kept on rerun): status, executor, times, comment and the steps that did not pass. */
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
            </Stack>
          );
        })}
      </Stack>
    </Card>
  );
}
