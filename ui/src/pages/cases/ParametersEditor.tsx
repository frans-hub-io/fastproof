import { ActionIcon, Alert, Badge, Button, Checkbox, Group, Modal, SegmentedControl, Stack, Table, Text, Textarea, TextInput, Tooltip } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconClipboardText, IconPlus, IconSparkles, IconTrash } from '@tabler/icons-react';
import { useState } from 'react';
import { errorMessage } from '../../api/client';
import { useGenerateParameters, type CaseParameters, type ParameterMode, type ParameterRow } from '../../api/parameters';
import { COLUMN_RE, generatorValues, MAX_COLUMNS, MAX_ROWS, newRowId, parseTsv, placeholders, tableProblems } from '../../lib/tm/parameters';

/** Rows rendered at once; the rest behind "Show all". */
const SHOWN_ROWS = 100;

function nextColumnName(columns: readonly string[]): string {
  for (let i = 1; ; i++) {
    const n = `param${String(i)}`;
    if (!columns.includes(n)) return n;
  }
}

function renameColumn(p: CaseParameters, i: number, name: string): CaseParameters {
  const old = p.columns[i];
  if (old === undefined) return p;
  const columns = p.columns.map((c, j) => (j === i ? name : c));
  const rows = p.rows.map((r) => ({
    ...r,
    values: Object.fromEntries(Object.entries(r.values).map(([k, v]) => [k === old ? name : k, v] as const)),
  }));
  return { ...p, columns, rows };
}

function GenerateDialog({ project, caseNumber, value, opened, onClose, onRows }: { project: string; caseNumber: number; value: CaseParameters; opened: boolean; onClose: () => void; onRows: (p: CaseParameters) => void }) {
  const gen = useGenerateParameters(project, caseNumber);
  const start = () => {
    const src = value.generator?.values ?? {};
    const cols = value.columns.length ? value.columns : Object.keys(src);
    const text: Record<string, string> = {};
    for (const c of cols) text[c] = (src[c] ?? [...new Set(value.rows.map((r) => r.values[c] ?? '').filter(Boolean))]).join('\n');
    return text;
  };
  const [mode, setMode] = useState<ParameterMode>(value.generator?.mode ?? 'pairwise');
  const [text, setText] = useState<Record<string, string>>(start);
  const [newCol, setNewCol] = useState('');
  const values = generatorValues(text);
  const sizes = Object.values(values).map((v) => v.length);
  const all = sizes.reduce((a, b) => a * b, sizes.length ? 1 : 0);
  const run = () =>
    gen.mutate(
      { mode, values },
      {
        onSuccess: (r) => {
          const columns = [...value.columns, ...Object.keys(values).filter((c) => !value.columns.includes(c))];
          onRows({ columns, rows: r.rows, generator: { mode, values } });
          notifications.show({ color: 'green', title: 'Rows generated', message: `${String(r.rows.length)} rows replace the table. Save the case to keep them.` });
          onClose();
        },
        onError: (e) => notifications.show({ color: 'red', title: 'Not generated', message: errorMessage(e) }),
      },
    );
  return (
    <Modal opened={opened} onClose={onClose} title="Generate rows" size="lg" attributes={{ content: { 'data-testid': 'parameters-generate' } }}>
      <Stack gap="sm">
        <SegmentedControl
          value={mode}
          onChange={(v) => setMode(v === 'all' ? 'all' : 'pairwise')}
          data={[
            { value: 'pairwise', label: 'Pairwise' },
            { value: 'all', label: 'All combinations' },
          ]}
          aria-label="How rows are generated"
        />
        <Text size="xs" c="dimmed">
          {mode === 'pairwise' ? 'Every pair of values of two parameters appears in at least one row: far fewer rows than all combinations, and the same input always gives the same rows.' : 'One row for every combination of values.'} One value per line.
        </Text>
        {Object.keys(text).map((c) => (
          <Textarea key={c} label={c} value={text[c] ?? ''} onChange={(e) => setText({ ...text, [c]: e.currentTarget.value })} autosize minRows={2} maxRows={8} data-testid={`generate-values-${c}`} />
        ))}
        <Group gap="xs" align="flex-end">
          <TextInput label="Add a parameter" placeholder="browser" value={newCol} onChange={(e) => setNewCol(e.currentTarget.value)} error={newCol && !COLUMN_RE.test(newCol) ? 'A letter or _, then letters, digits, _, . or -' : undefined} w={220} />
          <Button
            variant="default"
            disabled={!COLUMN_RE.test(newCol) || newCol in text}
            onClick={() => {
              setText({ ...text, [newCol]: '' });
              setNewCol('');
            }}
          >
            Add
          </Button>
        </Group>
        <Group justify="space-between" wrap="wrap" pos="sticky" bottom={0} py="xs" bg="var(--mantine-color-body)" style={{ zIndex: 1 }} data-testid="generate-footer">
          <Text size="sm" data-testid="generate-estimate">
            {mode === 'all' ? `${String(all)} rows` : `${String(all)} combinations; pairwise needs far fewer`}
          </Text>
          <Group gap="xs">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button onClick={run} loading={gen.isPending} disabled={!sizes.length || (mode === 'all' && all > MAX_ROWS)} data-testid="generate-run">
              Replace rows
            </Button>
          </Group>
        </Group>
      </Stack>
    </Modal>
  );
}

function PasteDialog({ value, opened, onClose, onRows }: { value: CaseParameters; opened: boolean; onClose: () => void; onRows: (p: CaseParameters) => void }) {
  const [text, setText] = useState('');
  const [header, setHeader] = useState(value.columns.length === 0);
  const [replace, setReplace] = useState(false);
  const parsed = parseTsv(text, value.columns, header);
  return (
    <Modal opened={opened} onClose={onClose} title="Paste rows" size="lg">
      <Stack gap="sm">
        <Text size="xs" c="dimmed">
          Copy cells from a spreadsheet: one row per line, cells separated by tabs.
        </Text>
        <Textarea value={text} onChange={(e) => setText(e.currentTarget.value)} autosize minRows={5} maxRows={14} aria-label="Rows to paste" className="fp-mono" data-testid="parameters-paste-text" />
        <Checkbox label="The first line names the columns" checked={header} onChange={(e) => setHeader(e.currentTarget.checked)} />
        <Checkbox label="Replace the current rows" checked={replace} onChange={(e) => setReplace(e.currentTarget.checked)} />
        <Group justify="space-between">
          <Text size="sm">{parsed.rows.length} rows</Text>
          <Group gap="xs">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button
              disabled={!parsed.rows.length}
              onClick={() => {
                onRows({ ...value, columns: parsed.columns, rows: [...(replace ? [] : value.rows), ...parsed.rows].slice(0, MAX_ROWS) });
                setText('');
                onClose();
              }}
              data-testid="parameters-paste-apply"
            >
              Add rows
            </Button>
          </Group>
        </Group>
      </Stack>
    </Modal>
  );
}

/** Editor of a case's parameter table: columns, rows (each can be switched off), paste and generate. */
export function ParametersEditor({ project, caseNumber, value, onChange, errors = [] }: { project: string; caseNumber: number; value: CaseParameters; onChange: (p: CaseParameters) => void; errors?: string[] }) {
  const [generate, setGenerate] = useState(false);
  const [paste, setPaste] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const problems = [...tableProblems(value), ...errors];
  const rows = showAll ? value.rows : value.rows.slice(0, SHOWN_ROWS);
  const setRow = (id: string, patch: Partial<ParameterRow>) => onChange({ ...value, rows: value.rows.map((r) => (r.id === id ? { ...r, ...patch } : r)) });
  const addColumn = () => onChange({ ...value, columns: [...value.columns, nextColumnName(value.columns)] });
  const addRow = () => onChange({ ...value, rows: [...value.rows, { id: newRowId(), values: Object.fromEntries(value.columns.map((c) => [c, ''] as const)), enabled: true }] });
  const enabled = value.rows.filter((r) => r.enabled).length;
  return (
    <Stack gap="sm" data-testid="parameters-editor">
      <Group gap="xs" wrap="wrap">
        <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={addColumn} disabled={value.columns.length >= MAX_COLUMNS} data-testid="parameters-add-column">
          Add column
        </Button>
        <Button size="xs" variant="light" leftSection={<IconPlus size={14} />} onClick={addRow} disabled={!value.columns.length || value.rows.length >= MAX_ROWS} data-testid="parameters-add-row">
          Add row
        </Button>
        <Button size="xs" variant="default" leftSection={<IconClipboardText size={14} />} onClick={() => setPaste(true)} data-testid="parameters-paste">
          Paste rows
        </Button>
        <Button size="xs" variant="default" leftSection={<IconSparkles size={14} />} onClick={() => setGenerate(true)} data-testid="parameters-generate-open">
          Generate…
        </Button>
        <Text size="xs" c="dimmed">
          {value.rows.length} rows, {enabled} enabled: {enabled} iteration{enabled === 1 ? '' : 's'} per run
        </Text>
      </Group>
      {problems.length > 0 && (
        <Alert color="red" variant="light" title="Fix these before saving" data-testid="parameters-problems">
          <Stack gap={2}>
            {problems.slice(0, 10).map((p) => (
              <Text key={p} size="sm">
                {p}
              </Text>
            ))}
          </Stack>
        </Alert>
      )}
      {value.columns.length === 0 ? (
        <Text size="sm" c="dimmed">
          Add a column for each parameter, then use it in the steps as <code>{'${name}'}</code>.
        </Text>
      ) : (
        <Table.ScrollContainer minWidth={Math.max(360, 120 + value.columns.length * 160)}>
          <Table withTableBorder withColumnBorders striped verticalSpacing={4} data-testid="parameters-table">
            <Table.Thead>
              <Table.Tr>
                <Table.Th w={56}>Run</Table.Th>
                {value.columns.map((c, i) => (
                  <Table.Th key={i}>
                    <Group gap={2} wrap="nowrap">
                      <TextInput
                        size="xs"
                        value={c}
                        onChange={(e) => onChange(renameColumn(value, i, e.currentTarget.value))}
                        aria-label={`Name of column ${String(i + 1)}`}
                        error={!COLUMN_RE.test(c)}
                        className="fp-mono"
                        data-testid="parameters-column"
                        style={{ flex: 1 }}
                      />
                      <Tooltip label="Delete the column">
                        <ActionIcon size="sm" variant="subtle" color="red" aria-label={`Delete column ${c}`} onClick={() => onChange({ ...value, columns: value.columns.filter((_, j) => j !== i) })}>
                          <IconTrash size={12} />
                        </ActionIcon>
                      </Tooltip>
                    </Group>
                  </Table.Th>
                ))}
                <Table.Th w={40} />
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {rows.map((r, ri) => (
                <Table.Tr key={r.id} data-testid="parameters-row" data-enabled={r.enabled}>
                  <Table.Td>
                    <Checkbox size="xs" checked={r.enabled} onChange={(e) => setRow(r.id, { enabled: e.currentTarget.checked })} aria-label={`Run row ${String(ri + 1)}`} />
                  </Table.Td>
                  {value.columns.map((c) => (
                    <Table.Td key={c}>
                      <TextInput size="xs" value={r.values[c] ?? ''} onChange={(e) => setRow(r.id, { values: { ...r.values, [c]: e.currentTarget.value } })} aria-label={`${c} in row ${String(ri + 1)}`} />
                    </Table.Td>
                  ))}
                  <Table.Td>
                    <ActionIcon size="sm" variant="subtle" color="red" aria-label={`Delete row ${String(ri + 1)}`} onClick={() => onChange({ ...value, rows: value.rows.filter((x) => x.id !== r.id) })}>
                      <IconTrash size={12} />
                    </ActionIcon>
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
      {!showAll && value.rows.length > SHOWN_ROWS && (
        <Button size="xs" variant="subtle" onClick={() => setShowAll(true)}>
          Show all {value.rows.length} rows
        </Button>
      )}
      {value.generator && (
        <Group gap={4}>
          <Badge size="xs" variant="light" color="gray" radius="sm">
            {value.generator.mode === 'pairwise' ? 'pairwise' : 'all combinations'}
          </Badge>
          <Text size="xs" c="dimmed">
            The rows were generated; editing them keeps the generator input for the next time.
          </Text>
        </Group>
      )}
      {generate && <GenerateDialog project={project} caseNumber={caseNumber} value={value} opened={generate} onClose={() => setGenerate(false)} onRows={onChange} />}
      <PasteDialog value={value} opened={paste} onClose={() => setPaste(false)} onRows={onChange} />
    </Stack>
  );
}

/**
 * The `${name}` placeholders of a step, highlighted: known columns of the saved parameters in blue, others in red
 * (saving fails with them while the case has parameters). Nothing when the step has none.
 */
export function PlaceholderHints({ texts, columns }: { texts: readonly string[]; columns: readonly string[] | undefined }) {
  const names = [...new Set(texts.flatMap((t) => placeholders(t).map((p) => p.name)))];
  if (!names.length) return null;
  return (
    <Group gap={4} mt={4} wrap="wrap" data-testid="step-placeholders">
      {names.map((n) => {
        const known = !columns || columns.includes(n);
        return (
          <Tooltip key={n} label={!columns ? 'Takes a value once the case has parameters' : known ? 'A parameter of this case' : 'No such parameter: add the column on the Parameters tab'}>
            <Badge size="sm" radius="sm" variant="light" color={known ? 'blue' : 'red'} className="fp-mono" data-known={known} tt="none">
              {`\${${n}}`}
            </Badge>
          </Tooltip>
        );
      })}
    </Group>
  );
}
