import { ActionIcon, Alert, Anchor, Button, Group, Stack, Table, Text, TextInput, Tooltip } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconLink, IconUnlink } from '@tabler/icons-react';
import { useState } from 'react';
import { Link } from 'react-router';
import { errorMessage } from '../../api/client';
import { parseRequirementNumber, requirementError, requirementKey, useCaseRequirements, useChangeLinks } from '../../api/requirements';
import { OriginBadge, ProposedBadge, RequirementStatusBadge } from '../requirements/common';

export function RequirementsTab({ projectKey, caseNumber }: { projectKey: string; caseNumber: number }) {
  const canWrite = useCan('cases:write', projectKey);
  const list = useCaseRequirements(projectKey, caseNumber);
  const change = useChangeLinks(projectKey);
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);
  const number = parseRequirementNumber(text);

  const run = (requirement: number, body: { add?: number[]; remove?: number[]; confirm?: number[] }, done: string, after?: () => void) => {
    setError(null);
    change.mutate(
      { requirement, change: body },
      {
        onSuccess: () => {
          notifications.show({ color: 'green', message: done });
          after?.();
        },
        onError: (e) => {
          const m = requirementError(e);
          setError(m);
          notifications.show({ color: 'red', title: 'Links not changed', message: m });
        },
      },
    );
  };

  return (
    <Stack gap="sm" data-testid="requirements-tab">
      {canWrite && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (number !== null) run(number, { add: [caseNumber] }, `Linked to ${requirementKey(number)}.`, () => setText(''));
          }}
        >
          <Group gap="xs" align="flex-start" wrap="wrap">
            <TextInput aria-label="Requirement number to link" placeholder="Requirement number, such as REQ-12" value={text} onChange={(e) => setText(e.currentTarget.value)} error={text && number === null ? 'Enter a requirement number' : error} style={{ flex: '1 1 220px' }} data-testid="link-requirement-input" />
            <Button type="submit" size="sm" variant="default" leftSection={<IconLink size={14} />} disabled={number === null} loading={change.isPending} data-testid="link-requirement-submit">
              Link requirement
            </Button>
          </Group>
        </form>
      )}
      {list.isError && <Alert color="red">{errorMessage(list.error)}</Alert>}
      {list.isPending ? (
        <Text size="sm" c="dimmed">
          Loading…
        </Text>
      ) : (list.data ?? []).length === 0 ? (
        <Text size="sm" c="dimmed" data-testid="requirements-tab-empty">
          This case covers no requirement yet.
        </Text>
      ) : (
        <Table.ScrollContainer minWidth={520}>
          <Table verticalSpacing={6} fz="sm" data-testid="case-requirements-table">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Requirement</Table.Th>
                <Table.Th>Status</Table.Th>
                <Table.Th>Link</Table.Th>
                <Table.Th w={canWrite ? 150 : 0}>
                  <Text span className="fp-sr-only">
                    Actions
                  </Text>
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {(list.data ?? []).map((r) => (
                <Table.Tr key={r.number} data-testid="case-requirement-row" data-number={r.number}>
                  <Table.Td style={{ overflowWrap: 'anywhere' }}>
                    <Anchor component={Link} to={`/p/${projectKey}/requirements/${String(r.number)}`} size="sm">
                      <span className="fp-mono">{requirementKey(r.number)}</span> {r.title}
                    </Anchor>
                    <Text size="xs" c="dimmed">
                      {r.type}
                    </Text>
                  </Table.Td>
                  <Table.Td>
                    <RequirementStatusBadge status={r.status} />
                  </Table.Td>
                  <Table.Td>
                    <Group gap={4} wrap="wrap">
                      <OriginBadge origin={r.origin} />
                      {r.linkStatus === 'proposed' && <ProposedBadge />}
                    </Group>
                  </Table.Td>
                  <Table.Td ta="right">
                    {canWrite && (
                      <Group gap={4} justify="flex-end" wrap="nowrap">
                        {r.linkStatus === 'proposed' && (
                          <Button size="compact-xs" variant="light" onClick={() => run(r.number, { confirm: [caseNumber] }, `Linked to ${requirementKey(r.number)}.`)} data-testid="confirm-link">
                            Confirm
                          </Button>
                        )}
                        <Tooltip label={r.linkStatus === 'proposed' ? 'Dismiss the proposal' : 'Remove the link'}>
                          <ActionIcon variant="subtle" color="red" aria-label={`${r.linkStatus === 'proposed' ? 'Dismiss' : 'Remove'} ${requirementKey(r.number)}`} onClick={() => run(r.number, { remove: [caseNumber] }, `Unlinked from ${requirementKey(r.number)}.`)} data-testid="remove-link">
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
    </Stack>
  );
}
