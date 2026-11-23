import { apiErrorFrom, apiFetch } from '@franshub/ui-shell';
import { API, request, withQuery } from '../api/client';
import type { Launch, LaunchExport } from '../api/types';

export type ExportFormat = 'html' | 'xlsx' | 'pdf';

/** Results above this are exported by a background job (the server answers 409 above it). */
export const EXPORT_SYNC_LIMIT = 10_000;

const FILTER_KEYS = [
  'status', 'q', 'suite', 'label', 'flaky', 'fingerprint', 'source', 'message', 'retried', 'muted', 'newFailures',
  'severity', 'owner', 'tag', 'minDurationMs', 'maxDurationMs', 'defect', 'attachment', 'env',
] as const;

/** The filters of the launch page that the export applies, as the result list takes them. */
export function exportFilters(sp: URLSearchParams): Record<string, string> {
  const out: Record<string, string> = {};
  for (const k of FILTER_KEYS) {
    const v = sp.get(k);
    if (v) out[k] = v;
  }
  return out;
}

function base(project: string, launch: string): string {
  return `${API}/projects/${encodeURIComponent(project)}/launches/${encodeURIComponent(launch)}`;
}

async function fetchBlob(path: string): Promise<Blob> {
  const res = await apiFetch(path);
  if (!res.ok) throw await apiErrorFrom(res);
  return res.blob();
}

export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** Opens the print-optimised report in a new tab and starts the browser's print dialog (Save as PDF). */
export function printBlob(blob: Blob): void {
  const url = URL.createObjectURL(new Blob([blob], { type: 'text/html' }));
  const w = window.open(url, '_blank');
  if (!w) {
    URL.revokeObjectURL(url);
    throw new Error('The browser blocked the new tab; allow pop-ups for this site and try again.');
  }
  w.addEventListener('load', () => {
    w.focus();
    w.print();
  });
  setTimeout(() => URL.revokeObjectURL(url), 5 * 60_000);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Builds the export (inline for a small launch, by a background job above the limit) and returns the file. */
export async function buildExport(
  project: string,
  launch: Launch,
  format: ExportFormat,
  filters: Record<string, string>,
  onProgress?: (job: LaunchExport) => void,
  pollMs = 2000,
): Promise<Blob> {
  const serverFormat = format === 'pdf' ? 'html' : format;
  const b = base(project, launch.id);
  if (launch.counts.total <= EXPORT_SYNC_LIMIT) {
    return fetchBlob(withQuery(`${b}/export`, { format: serverFormat, ...filters }));
  }
  let job = await request<LaunchExport>(withQuery(`${b}/exports`, { format: serverFormat, ...filters }), { method: 'POST' });
  onProgress?.(job);
  while (job.status === 'queued' || job.status === 'running') {
    await sleep(pollMs);
    job = await request<LaunchExport>(`${b}/exports/${job.id}`);
    onProgress?.(job);
  }
  if (job.status !== 'done') throw new Error(job.error ?? 'The export failed');
  return fetchBlob(`${b}/exports/${job.id}/download`);
}

export function exportFileName(project: string, launch: Launch, format: ExportFormat): string {
  return `launch-${project}-${String(launch.number)}.${format === 'pdf' ? 'html' : format}`;
}
