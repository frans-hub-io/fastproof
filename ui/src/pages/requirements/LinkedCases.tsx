import { ActionIcon, Alert, Anchor, Button, Group, Stack, Table, Text, TextInput, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconLink, IconUnlink } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { parseCaseNumbers, requirementError, useChangeLinks, useRequirementCases } from '../../api/requirements';
import { CaseStatusBadge } from '../cases/common';
import { caseLabel } from '../../lib/tm/filters';
import { LastResultChip, OriginBadge, ProposedBadge } from './common';

export function LinkCasesForm({ onLink, busy, error }: { onLink: (numbers: number[]) => void; busy: boolean; error: string | null }) {
  const [text, setText] = useState('');
  const parsed = parseCaseNumbers(text);
  const problem = parsed.invalid.length > 0 ? `Not a case number: ${parsed.invalid.join(', ')}` : null;
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (parsed.numbers.length === 0 || problem) return;
        onLink(parsed.numbers);
        setText('');
      }}
    >
      <Group gap="xs" align="flex-start" wrap="wrap">
        <TextInput aria-label="Case numbers to link" placeholder="Case numbers, such as 12, 15" value={text} onChange={(e) => setText(e.currentTarget.value)} error={problem ?? error} style={{ flex: '1 1 220px' }} data-testid="link-cases-input" />
        <Button type="submit" size="sm" variant="default" leftSection={<IconLink size={14} />} disabled={parsed.numbers.length === 0 || !!problem} loading={busy} data-testid="link-cases-submit">
          Link cases
        </Button>
      </Group>
    </form>
  );
}

export function LinkedCases({ project, number, canWrite }: { project: string; number: number; canWrite: boolean }) {
  const cases = useRequirementCases(project, number);
  const change = useChangeLinks(project);
  const [error, setError] = useState<string | null>(null);
  const items = cases.data?.pages.flatMap((p) => p.items) ?? [];
  const mutate = (change_: { add?: number[]; remove?: number[]; confirm?: number[] }, done: string) => {
    setError(null);
    change.mutate(
      { requirement: number, change: change_ },
      {
        onSuccess: () => notifications.show({ color: 'green', message: done }),
        onError: (e) => {
          const m = requirementError(e);
          setError(m);
          notifications.show({ color: 'red', title: 'Links not changed', message: m });
        },
      },
    );
  };

  return (
    <Stack gap="xs" data-testid="linked-cases">
      {canWrite && <LinkCasesForm busy={change.isPending} error={error} onLink={(numbers) => mutate({ add: numbers }, `Linked ${String(numbers.length)} case${numbers.length === 1 ? '' : 's'}.`)} />}
      {cases.isError && <Alert color="red">{errorMessage(cases.error)}</Alert>}
      {cases.isPending ? (
        <Text size="sm" c="dimmed">
          Loading…
        </Text>
      ) : items.length === 0 ? (
        <Text size="sm" c="dimmed" data-testid="linked-cases-empty">
          No case is linked yet. Link cases to see how well this requirement is covered.
        </Text>
      ) : (
        <Table.ScrollContainer minWidth={560}>
          <Table verticalSpacing={6} fz="sm" data-testid="linked-cases-table">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Case</Table.Th>
                <Table.Th>Link</Table.Th>
                <Table.Th>Last result</Table.Th>
                <Table.Th w={canWrite ? 150 : 0}>
                  <Text span className="fp-sr-only">
                    Actions
                  </Text>
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {items.map((l) => (
                <Table.Tr key={l.caseNumber} data-testid="linked-case-row" data-case={l.caseNumber}>
                  <Table.Td style={{ overflowWrap: 'anywhere' }}>
                    <Anchor component={Link} to={`/p/${project}/cases/${String(l.caseNumber)}`} size="sm">
                      <span className="fp-mono">{caseLabel(l.caseNumber)}</span> {l.name}
                    </Anchor>{' '}
                    <CaseStatusBadge status={l.status} size="xs" />
                  </Table.Td>
                  <Table.Td>
                    <Group gap={4} wrap="wrap">
                      <OriginBadge origin={l.origin} />
                      {l.linkStatus === 'proposed' && <ProposedBadge />}
                    </Group>
                  </Table.Td>
                  <Table.Td>
                    <LastResultChip result={l.lastResult} />
                  </Table.Td>
                  <Table.Td ta="right">
                    {canWrite && (
                      <Group gap={4} justify="flex-end" wrap="nowrap">
                        {l.linkStatus === 'proposed' && (
                          <Button size="compact-xs" variant="light" onClick={() => mutate({ confirm: [l.caseNumber] }, `${caseLabel(l.caseNumber)} is linked.`)} data-testid="confirm-link">
                            Confirm
                          </Button>
                        )}
                        <Tooltip label={l.linkStatus === 'proposed' ? 'Dismiss the proposal' : 'Remove the link'}>
                          <ActionIcon variant="subtle" color="red" aria-label={`${l.linkStatus === 'proposed' ? 'Dismiss' : 'Remove'} ${caseLabel(l.caseNumber)}`} onClick={() => mutate({ remove: [l.caseNumber] }, `${caseLabel(l.caseNumber)} is unlinked.`)} data-testid="remove-link">
                            <IconUnlink size={16} />
                          </ActionIcon>
                        </Tooltip>
                      </Group>
                    )}
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
      {cases.hasNextPage && (
        <Button variant="subtle" size="xs" onClick={() => void cases.fetchNextPage()} loading={cases.isFetchingNextPage}>
          Load more
        </Button>
      )}
    </Stack>
  );
}
