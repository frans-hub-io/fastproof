import { apiErrorFrom, apiFetch } from '@franshub/ui-shell';
import { API, request, withQuery } from '../api/client';
import type { Launch, LaunchExport } from '../api/types';
import { usesToken } from './authToken';

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

/** The server URL of the HTML report shown in the browser (sent with `Content-Security-Policy: sandbox`). */
export function reportUrl(project: string, launch: Launch, filters: Record<string, string>): string {
  return withQuery(`${base(project, launch.id)}/export`, { format: 'html', inline: 'true', ...filters });
}

/**
 * Opens the report for printing. With a cookie session the server URL opens in a new tab without
 * `opener`, where the sandbox header applies (print it with Ctrl/Cmd+P, Save as PDF). The report is
 * never put in a `blob:` document of the application's origin. Signed in with a token (a URL cannot
 * carry it), or for a launch that needs a job, the file is saved instead and opened from disk.
 * Returns what the caller should tell the user.
 */
export async function openForPrint(project: string, launch: Launch, filters: Record<string, string>, onProgress?: (job: LaunchExport) => void): Promise<string> {
  if (!usesToken() && launch.counts.total <= EXPORT_SYNC_LIMIT) {
    window.open(reportUrl(project, launch, filters), '_blank', 'noopener,noreferrer');
    return 'Press Ctrl/Cmd+P in the new tab and choose Save as PDF (allow pop-ups if no tab opened).';
  }
  saveBlob(await buildExport(project, launch, 'pdf', filters, onProgress), exportFileName(project, launch, 'pdf'));
  return 'Open the saved HTML file in a browser and print it (Save as PDF).';
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
