import { Alert, Badge, Button, Group, Stack, Table, Text } from '@mantine/core';
import { useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconDeviceFloppy, IconPencil, IconTable, IconTrash, IconX } from '@tabler/icons-react';
import { useState } from 'react';
import { ApiError, errorMessage } from '../../api/client';
import type { CaseParameters } from '../../api/parameters';
import { isConflict, useCase, useUpdateCase } from '../../api/tm';
import { EMPTY_PARAMETERS, pruneValues, tableProblems, unknownPlaceholders } from '../../lib/tm/parameters';
import { toWrite } from '../../lib/tm/versionDiff';
import { ParametersEditor } from './ParametersEditor';

function ReadOnlyTable({ p }: { p: CaseParameters }) {
  return (
    <Table.ScrollContainer minWidth={Math.max(320, 80 + p.columns.length * 140)}>
      <Table withTableBorder withColumnBorders striped verticalSpacing={4} data-testid="parameters-view">
        <Table.Thead>
          <Table.Tr>
            <Table.Th w={48}>#</Table.Th>
            {p.columns.map((c) => (
              <Table.Th key={c} className="fp-mono">
                {c}
              </Table.Th>
            ))}
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {p.rows.slice(0, 200).map((r, i) => (
            <Table.Tr key={r.id} data-enabled={r.enabled}>
              <Table.Td>
                <Text size="xs" c="dimmed">
                  {i + 1}
                  {r.enabled ? '' : ' · off'}
                </Text>
              </Table.Td>
              {p.columns.map((c) => (
                <Table.Td key={c}>
                  <Text size="sm" c={r.enabled ? undefined : 'dimmed'} td={r.enabled ? undefined : 'line-through'} style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                    {r.values[c] ?? ''}
                  </Text>
                </Table.Td>
              ))}
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

/**
 * The parameters of a data-driven case: a table of rows, each run as one iteration of the case. `${name}` in the
 * preconditions and steps takes the row's value. Saving writes a new version of the case.
 */
export function ParametersTab({ projectKey, caseNumber }: { projectKey: string; caseNumber: number }) {
  const q = useCase(projectKey, caseNumber);
  const update = useUpdateCase(projectKey, caseNumber);
  const canWrite = useCan('cases:write', projectKey);
  const [draft, setDraft] = useState<CaseParameters | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const v = q.data;
  if (!v) return null;
  const c = v.value;
  const saved = c.body.parameters ?? null;
  const p = draft ?? saved;
  const unknown = p ? unknownPlaceholders(c.body.preconditions, c.body.steps, p.columns) : [];
  const enabled = p ? p.rows.filter((r) => r.enabled).length : 0;

  const save = (next: CaseParameters | null) => {
    const w = toWrite(c);
    const body = next ? pruneValues(next) : null;
    if (body?.generator) {
      const values = Object.fromEntries(Object.entries(body.generator.values).filter(([k]) => body.columns.includes(k)));
      if (Object.keys(values).length) body.generator = { mode: body.generator.mode, values };
      else delete body.generator;
    }
    w.body = { ...w.body, parameters: body };
    update.mutate(
      { body: w, etag: v.etag },
      {
        onSuccess: (r) => {
          setDraft(null);
          setErrors([]);
          notifications.show({ color: 'green', title: 'Parameters saved', message: `The case is now version ${String(r.value.version)}` });
        },
        onError: (e) => {
          if (e instanceof ApiError && e.code === 'invalid_case' && Array.isArray(e.details)) setErrors((e.details as { path: string; message: string }[]).map((d) => `${d.path}: ${d.message}`));
          notifications.show({ color: 'red', title: 'Not saved', message: isConflict(e) ? 'The case changed meanwhile; reload the page and try again.' : errorMessage(e) });
        },
      },
    );
  };

  if (!p) {
    return (
      <Stack gap="sm" data-testid="parameters-tab">
        <Text size="sm">
          A data-driven case runs once for every row of a parameter table. Write <code>{'${name}'}</code> in the preconditions or steps where a value goes, and each run shows one iteration per row, with its own result.
        </Text>
        {canWrite && !c.archivedAt && (
          <Group>
            <Button size="xs" leftSection={<IconTable size={14} />} onClick={() => setDraft(EMPTY_PARAMETERS)} data-testid="parameters-add">
              Add parameters
            </Button>
          </Group>
        )}
      </Stack>
    );
  }

  return (
    <Stack gap="sm" data-testid="parameters-tab">
      <Group justify="space-between" wrap="wrap" gap="xs">
        <Group gap="xs">
          <Badge variant="light" radius="sm">
            {p.columns.length} column{p.columns.length === 1 ? '' : 's'}
          </Badge>
          <Badge variant="light" radius="sm" color="teal" data-testid="parameters-iterations">
            {enabled} iteration{enabled === 1 ? '' : 's'} per run
          </Badge>
        </Group>
        {canWrite && !c.archivedAt && (
          <Group gap="xs">
            {draft ? (
              <>
                <Button size="xs" leftSection={<IconDeviceFloppy size={14} />} onClick={() => save(draft)} loading={update.isPending} disabled={tableProblems(draft).length > 0} data-testid="parameters-save">
                  Save
                </Button>
                <Button
                  size="xs"
                  variant="default"
                  leftSection={<IconX size={14} />}
                  onClick={() => {
                    setDraft(null);
                    setErrors([]);
                  }}
                >
                  Cancel
                </Button>
              </>
            ) : (
              <>
                <Button size="xs" leftSection={<IconPencil size={14} />} onClick={() => setDraft(structuredClone(saved ?? EMPTY_PARAMETERS))} data-testid="parameters-edit">
                  Edit parameters
                </Button>
                <Button size="xs" variant="subtle" color="red" leftSection={<IconTrash size={14} />} onClick={() => save(null)} loading={update.isPending} data-testid="parameters-remove">
                  Remove
                </Button>
              </>
            )}
          </Group>
        )}
      </Group>
      {unknown.length > 0 && (
        <Alert color="orange" variant="light" title="Placeholders without a column" data-testid="parameters-unknown">
          <Text size="sm">
            {unknown
              .slice(0, 8)
              .map((u) => `\${${u.name}} in ${u.path}`)
              .join(', ')}
            . Add these columns or change the text; the case cannot be saved with them.
          </Text>
        </Alert>
      )}
      {draft ? <ParametersEditor project={projectKey} caseNumber={caseNumber} value={draft} onChange={setDraft} errors={errors} /> : <ReadOnlyTable p={p} />}
    </Stack>
  );
}
