import { Alert, Badge, Button, Group, Stack, Table, Text, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { useCan } from '@franshub/ui-shell';
import { IconArrowBackUp } from '@tabler/icons-react';
import { useState } from 'react';
import { useTriageHistory, useUndoTriage, type TriageDecision, type TriageState, type TriageUndoResult } from '../../api/analysis';
import { errorMessage } from '../../api/client';
import type { ResultDetail } from '../../api/types';
import { QueryState } from '../../components/QueryState';
import { formatDate } from '../../lib/format';

const VIA: Record<string, string> = {
  api: 'Triage',
  mcp: 'AI agent (MCP)',
  rp: 'ReportPortal',
  rule: 'Rule at ingest',
  retro: 'Rule applied later',
  suggestion: 'Accepted suggestion',
  auto: 'Auto-apply',
  undo: 'Undo',
};

const SKIP: Record<string, string> = {
  changed_since: 'changed since',
  result_gone: 'result gone',
  launch_merged: 'launch merged',
};

function stateText(s: TriageState): string {
  const d = s.defect ? s.defect.name || s.defect.group : 'none';
  return s.muted ? `${d}, muted${s.muteUntil ? ` until ${formatDate(s.muteUntil)}` : ''}` : d;
}

function SourceBadge({ d }: { d: TriageDecision }) {
  const color = d.source === 'user' ? 'blue' : d.source === 'ai' ? 'grape' : d.source === 'auto' ? 'teal' : 'gray';
  return (
    <Badge size="xs" variant="light" color={color} style={{ textTransform: 'none' }}>
      {d.source}
    </Badge>
  );
}

/** The triage journal of a result, newest first, with "Undo" per batch. */
export function TriageHistory({ project, r }: { project: string; r: ResultDetail }) {
  const q = useTriageHistory(project, { resultId: r.id });
  const undo = useUndoTriage(project);
  const canTriage = useCan('results:triage', project);
  const [last, setLast] = useState<TriageUndoResult | null>(null);
  return (
    <QueryState query={q}>
      {() => {
        const items = q.data?.items ?? [];
        return (
          <Stack gap="xs" data-testid="triage-history">
            {last && (
              <Alert color={last.skipped.length ? 'yellow' : 'green'} variant="light" withCloseButton onClose={() => setLast(null)} data-testid="undo-result">
                <Text size="sm">
                  Restored {last.restored} result{last.restored === 1 ? '' : 's'}.
                  {last.skipped.length ? ` Skipped ${String(last.skipped.length)}:` : ''}
                </Text>
                {last.skipped.length > 0 && (
                  <Stack gap={0} mt={4} data-testid="undo-skipped">
                    {last.skipped.slice(0, 20).map((s) => (
                      <Text key={s.resultId} size="xs" className="fp-mono">
                        {s.resultId} · {SKIP[s.code] ?? s.code}
                      </Text>
                    ))}
                  </Stack>
                )}
              </Alert>
            )}
            {!items.length ? (
              <Text size="sm" c="dimmed">
                No triage decisions yet. Every change of the defect type or mute of this result shows here and can be undone.
              </Text>
            ) : (
              <Table.ScrollContainer minWidth={560}>
                <Table fz="sm" verticalSpacing={6} data-testid="triage-history-table">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>When</Table.Th>
                      <Table.Th>How</Table.Th>
                      <Table.Th>Who</Table.Th>
                      <Table.Th>Change</Table.Th>
                      <Table.Th />
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {items.map((d) => (
                      <Table.Tr key={d.id} data-testid="triage-history-row" data-via={d.via}>
                        <Table.Td style={{ whiteSpace: 'nowrap' }}>{formatDate(d.at)}</Table.Td>
                        <Table.Td>
                          <Group gap={4} wrap="nowrap">
                            <Text size="sm">{VIA[d.via] ?? d.via}</Text>
                            <SourceBadge d={d} />
                          </Group>
                        </Table.Td>
                        <Table.Td style={{ wordBreak: 'break-word' }}>{d.by}</Table.Td>
                        <Table.Td>
                          <Text size="sm">
                            {stateText(d.before)} → {stateText(d.after)}
                          </Text>
                        </Table.Td>
                        <Table.Td>
                          {canTriage && d.undoable && (
                            <Tooltip label="Put back the state before this batch for every result it changed that nobody changed since">
                              <Button
                                size="compact-xs"
                                variant="light"
                                leftSection={<IconArrowBackUp size={12} />}
                                loading={undo.isPending && undo.variables === d.batchId}
                                onClick={() =>
                                  undo.mutate(d.batchId, {
                                    onSuccess: (out) => setLast(out),
                                    onError: (e) => notifications.show({ color: 'red', title: 'Not undone', message: errorMessage(e) }),
                                  })
                                }
                                data-testid="triage-undo"
                              >
                                Undo
                              </Button>
                            </Tooltip>
                          )}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </Table.ScrollContainer>
            )}
          </Stack>
        );
      }}
    </QueryState>
  );
}
