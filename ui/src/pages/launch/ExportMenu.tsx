import { Button, Menu } from '@mantine/core';
import { notifications } from '@mantine/notifications';
import { IconDownload, IconFileSpreadsheet, IconFileTypePdf, IconHtml } from '@tabler/icons-react';
import { useState } from 'react';
import { useSearchParams } from 'react-router';
import { errorMessage } from '../../api/client';
import type { Launch } from '../../api/types';
import { buildExport, exportFileName, exportFilters, printBlob, saveBlob, type ExportFormat } from '../../lib/launchExport';

/** Export of the launch: HTML report, PDF (the browser prints the report) and Excel; the filters of the page apply. */
export function ExportMenu({ project, launch }: { project: string; launch: Launch }) {
  const [sp] = useSearchParams();
  const [busy, setBusy] = useState(false);
  const filtered = Object.keys(exportFilters(sp)).length > 0;

  const run = async (format: ExportFormat) => {
    setBusy(true);
    const id = `export-${launch.id}`;
    const big = launch.counts.total > 10_000;
    if (big) notifications.show({ id, loading: true, autoClose: false, withCloseButton: false, title: 'Exporting', message: 'Queued' });
    try {
      const blob = await buildExport(project, launch, format, exportFilters(sp), (job) => {
        if (big) notifications.update({ id, loading: true, autoClose: false, title: 'Exporting', message: `${String(job.done)} of ${String(job.total)} results` });
      });
      if (big) notifications.hide(id);
      if (format === 'pdf') printBlob(blob);
      else saveBlob(blob, exportFileName(project, launch, format));
    } catch (e) {
      notifications.hide(id);
      notifications.show({ color: 'red', title: 'Export failed', message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Menu position="bottom-end" withinPortal>
      <Menu.Target>
        <Button variant="default" size="xs" leftSection={<IconDownload size={14} />} loading={busy} data-testid="export-menu">
          Export
        </Button>
      </Menu.Target>
      <Menu.Dropdown>
        <Menu.Label>{filtered ? 'Results with the current filters' : 'All results'}</Menu.Label>
        <Menu.Item leftSection={<IconHtml size={14} />} onClick={() => void run('html')} data-testid="export-html">
          HTML report
        </Menu.Item>
        <Menu.Item leftSection={<IconFileTypePdf size={14} />} onClick={() => void run('pdf')} data-testid="export-pdf">
          PDF (print the report)
        </Menu.Item>
        <Menu.Item leftSection={<IconFileSpreadsheet size={14} />} onClick={() => void run('xlsx')} data-testid="export-xlsx">
          Excel (XLSX)
        </Menu.Item>
      </Menu.Dropdown>
    </Menu>
  );
}
