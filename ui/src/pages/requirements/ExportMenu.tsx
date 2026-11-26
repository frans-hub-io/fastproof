import { Alert, Anchor, Button, Group, Loader, Menu, Text } from '@mantine/core';
import { IconDownload } from '@tabler/icons-react';
import { useState } from 'react';
import { requirementError, startCoverageExport, type CoverageScope } from '../../api/requirements';
import { useTmJob, type TmJob } from '../../api/tm';

/** Starts a coverage export, follows its job and offers the one-time download link. */
export function ExportMenu({ project, scope, disabled }: { project: string; scope: CoverageScope; disabled?: boolean }) {
  const [started, setStarted] = useState<{ id: string; url: string | null; format: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const job = useTmJob(project, started?.id ?? null);
  const j: TmJob | undefined = job.data;

  const start = (format: 'csv' | 'xlsx') => {
    setError(null);
    setBusy(true);
    startCoverageExport(project, format, scope)
      .then((r) => setStarted({ id: r.id, url: r.downloadUrl ?? null, format }))
      .catch((e: unknown) => setError(requirementError(e)))
      .finally(() => setBusy(false));
  };
  const pending = !!started && (!j || ['queued', 'dry_run', 'running'].includes(j.status));

  return (
    <Group gap="xs" wrap="wrap" data-testid="coverage-export">
      <Menu position="bottom-end">
        <Menu.Target>
          <Button size="xs" variant="default" leftSection={<IconDownload size={14} />} loading={busy} disabled={disabled || pending} data-testid="export-menu">
            Export
          </Button>
        </Menu.Target>
        <Menu.Dropdown>
          <Menu.Item onClick={() => start('csv')} data-testid="export-csv">
            CSV
          </Menu.Item>
          <Menu.Item onClick={() => start('xlsx')} data-testid="export-xlsx">
            XLSX
          </Menu.Item>
        </Menu.Dropdown>
      </Menu>
      {pending && (
        <Group gap={6} data-testid="export-running">
          <Loader size="xs" />
          <Text size="xs">Preparing the {started.format.toUpperCase()} file…</Text>
        </Group>
      )}
      {started && j?.status === 'done' && (
        <Text size="sm" data-testid="export-ready">
          {started.url ? (
            <Anchor href={started.url} download data-testid="export-download">
              Download the {started.format.toUpperCase()} file
            </Anchor>
          ) : (
            'The file is ready. Start the export again to get its link.'
          )}
        </Text>
      )}
      {(j?.status === 'failed' || j?.status === 'cancelled') && (
        <Alert color="red" p="xs" data-testid="export-failed">
          {j.report?.errors[0]?.message ?? 'The export failed.'}
        </Alert>
      )}
      {error && (
        <Text size="xs" c="red" data-testid="export-error">
          {error}
        </Text>
      )}
    </Group>
  );
}
