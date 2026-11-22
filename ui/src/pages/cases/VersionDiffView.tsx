import { Badge, Group, SimpleGrid, Stack, Table, Text } from '@mantine/core';
import type { VersionDiff, StepChange } from '../../lib/tm/versionDiff';

const KIND_LABEL: Record<StepChange['kind'], string> = { added: 'added', removed: 'removed', changed: 'changed', moved: 'moved', same: 'same' };
const KIND_COLOR: Record<StepChange['kind'], string> = { added: 'teal', removed: 'red', changed: 'yellow', moved: 'blue', same: 'gray' };

function Side({ text, op }: { text: string; op: 'remove' | 'add' }) {
  return (
    <pre className="fp-tm-diff-cell" data-op={op}>
      {text || '—'}
    </pre>
  );
}

/** Two versions side by side: changed fields, then steps matched by their id. */
export function VersionDiffView({ diff, beforeLabel, afterLabel, showSame = false }: { diff: VersionDiff; beforeLabel: string; afterLabel: string; showSame?: boolean }) {
  if (diff.same) {
    return (
      <Text size="sm" c="dimmed" data-testid="diff-same">
        No differences.
      </Text>
    );
  }
  const steps = diff.steps.filter((s) => showSame || s.kind !== 'same');
  return (
    <Stack gap="md" data-testid="version-diff">
      {diff.fields.length > 0 && (
        <div className="fp-table-wrap">
          <Table withTableBorder withColumnBorders layout="fixed" verticalSpacing={4} miw={480}>
            <Table.Thead>
              <Table.Tr>
                <Table.Th w="22%">Field</Table.Th>
                <Table.Th>{beforeLabel}</Table.Th>
                <Table.Th>{afterLabel}</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {diff.fields.map((f) => (
                <Table.Tr key={f.key} data-testid="diff-field" data-key={f.key}>
                  <Table.Td>
                    <Text size="sm" fw={500}>
                      {f.label}
                    </Text>
                  </Table.Td>
                  <Table.Td>
                    <Side text={f.before} op="remove" />
                  </Table.Td>
                  <Table.Td>
                    <Side text={f.after} op="add" />
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </div>
      )}
      {steps.length > 0 && (
        <Stack gap="xs">
          <Text size="sm" fw={600}>
            Steps
          </Text>
          {steps.map((s) => (
            <Stack key={s.id} gap={4} className="fp-tm-diff-step" data-testid="diff-step" data-kind={s.kind}>
              <Group gap={6} wrap="wrap">
                <Badge size="xs" color={KIND_COLOR[s.kind]} variant="light" radius="sm">
                  {KIND_LABEL[s.kind]}
                </Badge>
                <Text size="xs" c="dimmed" className="fp-mono">
                  {s.beforePath && s.afterPath && s.beforePath !== s.afterPath ? `${s.beforePath} → ${s.afterPath}` : (s.afterPath ?? s.beforePath)}
                </Text>
                <Text size="sm" className="fh-truncate" style={{ minWidth: 0, flex: 1 }}>
                  {s.action}
                </Text>
              </Group>
              {s.parts.map((p) => (
                <SimpleGrid key={p.part} cols={{ base: 1, sm: 2 }} spacing={4}>
                  <Stack gap={0}>
                    <Text size="xs" c="dimmed">
                      {p.part} · {beforeLabel}
                    </Text>
                    <Side text={p.before} op="remove" />
                  </Stack>
                  <Stack gap={0}>
                    <Text size="xs" c="dimmed">
                      {p.part} · {afterLabel}
                    </Text>
                    <Side text={p.after} op="add" />
                  </Stack>
                </SimpleGrid>
              ))}
            </Stack>
          ))}
        </Stack>
      )}
    </Stack>
  );
}
