import { Anchor, Badge, Group, Popover, Stack, Table, Text, UnstyledButton } from '@mantine/core';
import { IconCheck, IconClockHour4, IconShieldCheck, IconShieldX, IconX } from '@tabler/icons-react';
import { Link } from 'react-router';
import { useGate } from '../api/stageC';
import type { Launch } from '../api/types';

const LOOK = {
  passed: { color: 'green', label: 'Gate passed', icon: <IconShieldCheck size={12} /> },
  failed: { color: 'red', label: 'Gate failed', icon: <IconShieldX size={12} /> },
  pending: { color: 'yellow', label: 'Gate pending', icon: <IconClockHour4 size={12} /> },
} as const;

/** Quality gate status of a launch; nothing when the server has no gate for it. */
export function GateBadge({ project, launch }: { project: string; launch: Launch }) {
  const q = useGate(project, launch);
  if (!q.data) return null;
  const g = q.data;
  if (g.status !== 'pending' && g.results.length === 0) return null;
  const look = LOOK[g.status];
  return (
    <Popover width={360} position="bottom-start" withArrow shadow="md">
      <Popover.Target>
        <UnstyledButton aria-label={`${look.label}: show the rules`} data-testid="gate-badge" data-gate={g.status}>
          <Badge size="sm" variant="light" color={look.color} radius="sm" leftSection={look.icon} style={{ textTransform: 'none', cursor: 'pointer' }}>
            {look.label}
          </Badge>
        </UnstyledButton>
      </Popover.Target>
      <Popover.Dropdown>
        <Stack gap="xs" data-testid="gate-details">
          {g.status === 'pending' && (
            <Text size="sm" c="dimmed">
              The gate is evaluated when the launch finishes.
            </Text>
          )}
          {g.results.length > 0 && (
            <div className="fp-table-wrap">
              <Table fz="xs" verticalSpacing={4} horizontalSpacing={6}>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th />
                    <Table.Th>Rule</Table.Th>
                    <Table.Th ta="right">Actual</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {g.results.map((r, i) => (
                    <Table.Tr key={`${r.gate}:${r.rule}:${String(i)}`}>
                      <Table.Td>{r.ok ? <IconCheck size={14} color="var(--fp-status-passed)" /> : <IconX size={14} color="var(--fp-status-failed)" />}</Table.Td>
                      <Table.Td>
                        <Text size="xs" fw={600}>
                          {r.gate}
                        </Text>
                        <Text size="xs" c="dimmed" className="fp-mono">
                          {r.rule}
                        </Text>
                      </Table.Td>
                      <Table.Td ta="right" className="fp-mono">
                        {r.actual ?? '—'}
                      </Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </div>
          )}
          <Group justify="flex-end">
            <Anchor component={Link} to={`/p/${project}/gates`} size="xs">
              Edit gates
            </Anchor>
          </Group>
        </Stack>
      </Popover.Dropdown>
    </Popover>
  );
}
