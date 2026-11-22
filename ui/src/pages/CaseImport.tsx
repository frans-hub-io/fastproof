import { Alert, Anchor, Badge, Button, Card, Code, Group, NumberInput, PasswordInput, Progress, SegmentedControl, Select, SimpleGrid, Stack, Stepper, Table, Text, TextInput } from '@mantine/core';
import { EmptyState, useCan } from '@franshub/ui-shell';
import { notifications } from '@mantine/notifications';
import { IconAlertTriangle, IconCheck, IconFileUpload, IconUpload } from '@tabler/icons-react';
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState, type DragEvent } from 'react';
import { Link, useParams } from 'react-router';
import { errorMessage } from '../api/client';
import { useProject } from '../api/hooks';
import { applyImport, cancelImport, createImport, createTestOpsImport, IMPORT_TARGETS, isCsvFormat, previewImport, tmk, useCaseFields, useCaseFolders, useImportJob, useTmEvents, type ImportFormat, type ImportOptions, type ImportPreview } from '../api/tm';
import { PageHeader } from '../components/PageHeader';
import { formatBytes } from '../lib/format';
import { folderPathLabel } from '../lib/tm/ui';

const FORMATS: { value: ImportFormat; label: string }[] = [
  { value: 'generic-csv', label: 'CSV with my own columns' },
  { value: 'testrail-csv', label: 'TestRail CSV' },
  { value: 'qase-csv', label: 'Qase CSV' },
  { value: 'qase-json', label: 'Qase JSON' },
  { value: 'testops-csv', label: 'Allure TestOps CSV' },
  { value: 'fastproof-ndjson', label: 'FastProof NDJSON export' },
];

const MAX_FILE = 104_857_600;
const MAX_ZIP = 2_147_483_648;

export default function CaseImportPage() {
  const { key = '' } = useParams();
  const project = useProject(key);
  const canImport = useCan('cases:import', key);
  const fields = useCaseFields(key);
  const folders = useCaseFolders(key);
  const qc = useQueryClient();
  const [step, setStep] = useState(0);
  const [source, setSource] = useState<'file' | 'testops'>('file');
  const [file, setFile] = useState<File | null>(null);
  const [zip, setZip] = useState<File | null>(null);
  const zipInput = useRef<HTMLInputElement>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [mapping, setMapping] = useState<Record<string, string>>({});
  const [format, setFormat] = useState<ImportFormat>('generic-csv');
  const [layout, setLayout] = useState<'rows' | 'cells'>('cells');
  const [mode, setMode] = useState<ImportOptions['mode']>('createOnly');
  const [target, setTarget] = useState<string | null>(null);
  const [unknown, setUnknown] = useState<ImportOptions['unknownFieldValues']>('skip');
  const [busy, setBusy] = useState(false);
  const [jobId, setJobId] = useState<string | null>(null);
  const [testops, setTestops] = useState({ baseUrl: '', token: '', remoteProjectId: 0 });
  const input = useRef<HTMLInputElement>(null);
  const job = useImportJob(key, jobId);
  const j = job.data;
  const finished = j?.status === 'done';
  useEffect(() => {
    if (finished) void qc.invalidateQueries({ queryKey: tmk.all(key) });
  }, [finished, qc, key]);

  useTmEvents(key, {
    tmJobUpdated: (e) => {
      if (e.id === jobId) void qc.invalidateQueries({ queryKey: tmk.job(key, e.id) });
    },
  });

  const generic = format === 'generic-csv';
  const csv = isCsvFormat(format);
  const targets = useMemo(() => {
    const list = [...IMPORT_TARGETS, ...(fields.data ?? []).filter((f) => !f.archived).map((f) => ({ value: `field:${f.key}`, label: `Field: ${f.name}` }))];
    for (const v of Object.values(mapping)) if (v.startsWith('field:') && !list.some((t) => t.value === v)) list.push({ value: v, label: `Field: ${v.slice(6)} (no such field yet)` });
    return list;
  }, [fields.data, mapping]);
  const fieldTargets = useMemo(() => targets.filter((t) => t.value === 'ignore' || t.value.startsWith('field:')), [targets]);
  /** Columns whose target can be chosen: every column of a generic CSV, the custom columns of a tool's CSV. */
  const mappable = (h: string) => csv && format === preview?.format && (generic || h in preview.suggestedMapping);
  const folderData = useMemo(() => (folders.data ?? []).map((f) => ({ value: f.id, label: folderPathLabel(folders.data, f.id) })).sort((a, b) => a.label.localeCompare(b.label)), [folders.data]);

  const pick = async (f: File | null) => {
    if (!f) return;
    if (f.size > MAX_FILE) {
      notifications.show({ color: 'red', title: 'File too large', message: `At most ${formatBytes(MAX_FILE)}` });
      return;
    }
    setFile(f);
    setBusy(true);
    try {
      const p = await previewImport(key, f);
      setPreview(p);
      const defs = (fields.data ?? []).filter((d) => !d.archived);
      const normalized = Object.fromEntries(
        Object.entries(p.suggestedMapping).map(([h, t]) => {
          if (!t.startsWith('field:')) return [h, t];
          const name = t.slice(6).toLowerCase();
          const def = defs.find((d) => d.key === name || d.name.toLowerCase() === name);
          return [h, def ? `field:${def.key}` : t];
        }),
      );
      setMapping(normalized);
      setFormat(p.format);
      setLayout(p.stepsLayout);
      setStep(1);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Cannot read the file', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const drop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    void pick(e.dataTransfer.files[0] ?? null);
  };

  const dryRun = async () => {
    if (!file) return;
    setBusy(true);
    try {
      const sendMapping = csv && format === preview?.format && Object.keys(mapping).length > 0 ? Object.fromEntries(Object.entries(mapping).map(([k, v]) => [k, v || 'ignore'])) : undefined;
      const created = await createImport(key, file, { format, mapping: sendMapping, stepsLayout: csv ? layout : undefined, mode, targetFolderId: target, unknownFieldValues: unknown }, zip);
      setJobId(created.id);
      setStep(2);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Import not started', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const startTestOps = async () => {
    setBusy(true);
    try {
      const created = await createTestOpsImport(key, { source: 'allure-testops', baseUrl: testops.baseUrl.trim(), token: testops.token, remoteProjectId: testops.remoteProjectId, mode, targetFolderId: target });
      setTestops((t) => ({ ...t, token: '' }));
      setJobId(created.id);
      setStep(2);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Import not started', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const apply = async () => {
    if (!jobId) return;
    setBusy(true);
    try {
      await applyImport(key, jobId);
      void qc.invalidateQueries({ queryKey: tmk.job(key, jobId) });
      setStep(3);
    } catch (e) {
      notifications.show({ color: 'red', title: 'Not applied', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (jobId) await cancelImport(key, jobId).catch(() => undefined);
    setJobId(null);
    setStep(preview ? 1 : 0);
  };

  if (!canImport) return <EmptyState title="Access denied" description="Importing cases needs the cases:import permission." />;

  const urlOk = /^https:\/\/[^\s/]+/.test(testops.baseUrl.trim());
  const done = j?.status === 'done';

  return (
    <>
      <PageHeader crumbs={[{ label: 'Projects', to: '/' }, { label: project.data?.name ?? key, to: `/p/${key}` }, { label: 'Test cases', to: `/p/${key}/cases` }, { label: 'Import' }]} title="Import test cases" subtitle="Every import runs as a dry run first; nothing changes until you apply it." />
      <Stepper active={step} onStepClick={(s) => s < step && !jobId && setStep(s)} size="sm" allowNextStepsSelect={false} data-testid="import-stepper">
        <Stepper.Step label="Upload" description="CSV, JSON or TestOps">
          <Stack gap="sm" mt="md">
            <SegmentedControl
              value={source}
              onChange={(v) => setSource(v === 'testops' ? 'testops' : 'file')}
              data={[
                { value: 'file', label: 'A file' },
                { value: 'testops', label: 'Allure TestOps API (beta)' },
              ]}
              w="fit-content"
              aria-label="Import from"
            />
            {source === 'file' ? (
              <div className="fp-tm-drop" onDragOver={(e) => e.preventDefault()} onDrop={drop} data-testid="import-drop">
                <Stack align="center" gap="xs">
                  <IconFileUpload size={32} aria-hidden />
                  <Text size="sm">Drop a TestRail, Qase, TestOps or generic CSV, a Qase JSON or a FastProof NDJSON export here</Text>
                  <input ref={input} type="file" accept=".csv,.json,.ndjson,text/csv,application/json" hidden onChange={(e) => void pick(e.currentTarget.files?.[0] ?? null)} data-testid="import-file" aria-label="Choose a file to import" />
                  <Button leftSection={<IconUpload size={14} />} onClick={() => input.current?.click()} loading={busy}>
                    Choose a file
                  </Button>
                  <Text size="xs" c="dimmed">
                    Up to {formatBytes(MAX_FILE)}; attachments of a CSV import go in a zip next to it (chosen in the next step).
                  </Text>
                </Stack>
              </div>
            ) : (
              <Card withBorder radius="md" padding="md">
                <Stack gap="sm">
                  <Alert color="yellow" variant="light" icon={<IconAlertTriangle size={16} />}>
                    Beta. The token is stored as a project secret at once and is never shown again.
                  </Alert>
                  <TextInput label="TestOps URL" placeholder="https://testops.example.com" value={testops.baseUrl} onChange={(e) => setTestops({ ...testops, baseUrl: e.currentTarget.value })} error={testops.baseUrl && !urlOk ? 'An https URL' : undefined} />
                  <PasswordInput label="API token" value={testops.token} onChange={(e) => setTestops({ ...testops, token: e.currentTarget.value })} autoComplete="off" />
                  <NumberInput label="TestOps project id" value={testops.remoteProjectId || ''} onChange={(v) => setTestops({ ...testops, remoteProjectId: typeof v === 'number' ? v : 0 })} min={1} />
                  <Select label="Into folder" data={folderData} value={target} onChange={setTarget} clearable searchable comboboxProps={{ withinPortal: true }} />
                  <Group justify="flex-end">
                    <Button onClick={() => void startTestOps()} loading={busy} disabled={!urlOk || !testops.token || testops.remoteProjectId < 1}>
                      Start the dry run
                    </Button>
                  </Group>
                </Stack>
              </Card>
            )}
          </Stack>
        </Stepper.Step>
        <Stepper.Step label="Map columns" description="Preview">
          {preview && (
            <Stack gap="sm" mt="md" data-testid="import-mapping">
              <Group gap="sm" wrap="wrap" align="flex-end">
                <Select label="Format" data={FORMATS} value={format} onChange={(v) => v && setFormat(v)} allowDeselect={false} w={240} comboboxProps={{ withinPortal: true }} data-testid="import-format" />
                {csv && <Stack gap={2}>
                  <Text size="sm" fw={500}>
                    Steps are in
                  </Text>
                  <SegmentedControl
                    value={layout}
                    onChange={(v) => setLayout(v === 'rows' ? 'rows' : 'cells')}
                    data={[
                      { value: 'cells', label: 'One cell, numbered' },
                      { value: 'rows', label: 'One row per step' },
                    ]}
                    aria-label="Steps layout"
                  />
                </Stack>}
                <Select label="Existing cases" data={[{ value: 'createOnly', label: 'Skip them' }, { value: 'upsert', label: 'Update them' }]} value={mode} onChange={(v) => setMode(v === 'upsert' ? 'upsert' : 'createOnly')} allowDeselect={false} w={180} comboboxProps={{ withinPortal: true }} />
                <Select label="Unknown field values" data={[{ value: 'skip', label: 'Leave empty' }, { value: 'create', label: 'Add as options' }, { value: 'fail', label: 'Fail the row' }]} value={unknown} onChange={(v) => setUnknown(v === 'create' || v === 'fail' ? v : 'skip')} allowDeselect={false} w={200} comboboxProps={{ withinPortal: true }} />
                <Select label="Into folder" data={folderData} value={target} onChange={setTarget} clearable searchable w={260} comboboxProps={{ withinPortal: true }} />
              </Group>
              <Text size="xs" c="dimmed">
                {file?.name} · {file ? formatBytes(file.size) : ''} · the first {preview.rows.length} {csv ? 'rows' : 'cases'}
                {csv && !generic ? ' · the format reads its own columns; choose only where custom columns go' : ''}
              </Text>
              <Group gap="xs">
                <input ref={zipInput} type="file" accept=".zip,application/zip" hidden data-testid="import-zip" aria-label="Choose a zip of attachments" onChange={(e) => {
                  const z = e.currentTarget.files?.[0] ?? null;
                  if (z && z.size > MAX_ZIP) {
                    notifications.show({ color: 'red', title: 'Zip too large', message: `At most ${formatBytes(MAX_ZIP)}` });
                    return;
                  }
                  setZip(z);
                }} />
                <Button size="xs" variant="default" onClick={() => zipInput.current?.click()}>
                  {zip ? `Attachments: ${zip.name}` : 'Add a zip of attachments'}
                </Button>
                {zip && (
                  <Anchor size="xs" component="button" onClick={() => setZip(null)}>
                    Remove
                  </Anchor>
                )}
              </Group>
              <div className="fp-table-wrap">
                <Table withTableBorder withColumnBorders verticalSpacing={4} miw={Math.max(600, preview.headers.length * 160)}>
                  <Table.Thead>
                    <Table.Tr>
                      {preview.headers.map((h) => (
                        <Table.Th key={h} miw={150}>
                          <Stack gap={4}>
                            <Text size="xs" fw={700}>
                              {h}
                            </Text>
                            {mappable(h) ? (
                              <Select size="xs" aria-label={`Map ${h} to`} data={generic ? targets : fieldTargets} value={mapping[h] || 'ignore'} onChange={(v) => setMapping((m) => ({ ...m, [h]: v ?? 'ignore' }))} comboboxProps={{ withinPortal: true }} allowDeselect={false} data-testid="import-map" />
                            ) : (
                              csv && (
                                <Text size="xs" c="dimmed">
                                  read by the format
                                </Text>
                              )
                            )}
                          </Stack>
                        </Table.Th>
                      ))}
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {preview.rows.map((r, i) => (
                      <Table.Tr key={i}>
                        {preview.headers.map((h, k) => (
                          <Table.Td key={h}>
                            <Text size="xs" lineClamp={3} style={{ whiteSpace: 'pre-wrap' }}>
                              {r[k] ?? ''}
                            </Text>
                          </Table.Td>
                        ))}
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
              {generic && format === preview.format && !Object.values(mapping).includes('name') && <Alert color="red">Map one column to Name.</Alert>}
              <Group justify="space-between">
                <Button variant="default" onClick={() => { setStep(0); setPreview(null); setFile(null); setZip(null); }}>
                  Choose another file
                </Button>
                <Button onClick={() => void dryRun()} loading={busy} disabled={generic && format === preview.format && !Object.values(mapping).includes('name')} data-testid="import-dry-run">
                  Dry run
                </Button>
              </Group>
            </Stack>
          )}
        </Stepper.Step>
        <Stepper.Step label="Check the report" description="Dry run">
          <Stack gap="sm" mt="md" data-testid="import-report">
            {!j || j.status === 'queued' || j.status === 'dry_run' ? (
              <Stack gap={4}>
                <Text size="sm">Checking every row…</Text>
                <Progress value={j?.progress.total ? (j.progress.done / j.progress.total) * 100 : 10} animated aria-label="Dry run progress" />
              </Stack>
            ) : j.status === 'failed' || j.status === 'cancelled' ? (
              <Alert color="red" data-testid="import-failed">
                The import {j.status}.{j.report?.errors[0] ? ` ${j.report.errors[0].code}: ${j.report.errors[0].message}` : ''}
              </Alert>
            ) : j.report ? (
              <>
                <SimpleGrid cols={{ base: 2, sm: 4 }} spacing="xs">
                  <Card withBorder padding="xs" radius="md" data-testid="report-create">
                    <Text size="xs" c="dimmed">
                      New cases
                    </Text>
                    <Text fw={700} size="lg">
                      {j.report.create}
                    </Text>
                  </Card>
                  <Card withBorder padding="xs" radius="md">
                    <Text size="xs" c="dimmed">
                      Updated
                    </Text>
                    <Text fw={700} size="lg">
                      {j.report.update}
                    </Text>
                  </Card>
                  <Card withBorder padding="xs" radius="md">
                    <Text size="xs" c="dimmed">
                      Skipped
                    </Text>
                    <Text fw={700} size="lg">
                      {j.report.skip}
                    </Text>
                  </Card>
                  <Card withBorder padding="xs" radius="md">
                    <Text size="xs" c="dimmed">
                      New folders
                    </Text>
                    <Text fw={700} size="lg">
                      {j.report.folders}
                    </Text>
                  </Card>
                </SimpleGrid>
                {j.report.unknownFields.length > 0 && (
                  <Alert color="yellow" title="Fields that do not exist">
                    {j.report.unknownFields.map((f) => (
                      <Code key={f} mr={4}>
                        {f}
                      </Code>
                    ))}
                  </Alert>
                )}
                {Object.keys(j.report.unknownValues).length > 0 && (
                  <Alert color="yellow" title="Values that are not options" data-testid="report-unknown-values">
                    {Object.entries(j.report.unknownValues).map(([k, vals]) => (
                      <Text key={k} size="sm">
                        {k}: {vals.join(', ')}
                      </Text>
                    ))}
                  </Alert>
                )}
                {j.report.errors.length > 0 && (
                  <div className="fp-table-wrap">
                    <Table withTableBorder verticalSpacing={4} data-testid="report-errors">
                      <Table.Thead>
                        <Table.Tr>
                          <Table.Th>Row</Table.Th>
                          <Table.Th>Problem</Table.Th>
                        </Table.Tr>
                      </Table.Thead>
                      <Table.Tbody>
                        {j.report.errors.map((e) => (
                          <Table.Tr key={`${String(e.row)}:${e.code}`}>
                            <Table.Td>{e.row}</Table.Td>
                            <Table.Td>
                              <Badge size="xs" variant="light" color="red" mr={6}>
                                {e.code}
                              </Badge>
                              {e.message}
                            </Table.Td>
                          </Table.Tr>
                        ))}
                      </Table.Tbody>
                    </Table>
                  </div>
                )}
                <Group justify="space-between">
                  <Button variant="default" onClick={() => void cancel()}>
                    Cancel the import
                  </Button>
                  <Button onClick={() => void apply()} loading={busy} disabled={j.status !== 'ready' || j.report.create + j.report.update === 0} data-testid="import-apply">
                    Apply: {j.report.create} new, {j.report.update} updated
                  </Button>
                </Group>
              </>
            ) : null}
          </Stack>
        </Stepper.Step>
        <Stepper.Step label="Apply" description="Progress">
          <Stack gap="sm" mt="md" data-testid="import-progress">
            <Progress value={j?.progress.total ? (j.progress.done / j.progress.total) * 100 : 0} animated={!done} aria-label="Import progress" />
            <Text size="sm" aria-live="polite">
              {done ? `Done: ${String(j.progress.done)} rows imported.` : `${String(j?.progress.done ?? 0)} of ${String(j?.progress.total ?? '?')} rows…`}
            </Text>
            {done && j.report && (
              <Text size="sm" c="dimmed" data-testid="import-result">
                {j.report.create} created, {j.report.update} updated, {j.report.skip} skipped, {j.report.folders} folders, {j.report.errors.length} errors
              </Text>
            )}
            {(j?.status === 'failed' || j?.status === 'cancelled') && (
              <Alert color="red" data-testid="import-apply-failed">
                The import {j.status}.{j.report?.errors[0] ? ` ${j.report.errors[0].code}: ${j.report.errors[0].message}` : ''}
              </Alert>
            )}
            {done && (
              <Group>
                <Button component={Link} to={`/p/${key}/cases`} leftSection={<IconCheck size={14} />} data-testid="import-open-cases">
                  Open the cases
                </Button>
                <Anchor size="sm" onClick={() => { setStep(0); setJobId(null); setPreview(null); setFile(null); }} component="button">
                  Import another file
                </Anchor>
              </Group>
            )}
          </Stack>
        </Stepper.Step>
      </Stepper>
    </>
  );
}
