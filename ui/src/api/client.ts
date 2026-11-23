import { ApiError, apiErrorFrom, apiFetch, loginUrl } from '@franshub/ui-shell';
import type { AttachmentKind } from './types';

export { ApiError };

export const API = '/api/v1';

type Redirect = (url: string) => void;

let redirectToLogin: Redirect = (url) => window.location.assign(url);

export function setLoginRedirect(fn: Redirect): void {
  redirectToLogin = fn;
}

function currentPath(): string {
  const { pathname, search, hash } = window.location;
  return `${pathname}${search}${hash}`;
}

const retryAfter = new WeakMap<object, number>();

/** The `Retry-After` of a failed response in milliseconds, when the server sent whole seconds. */
export function retryAfterMs(err: unknown): number | undefined {
  return typeof err === 'object' && err !== null ? retryAfter.get(err) : undefined;
}

function noteRetryAfter(err: ApiError, header: string | null): void {
  const n = header !== null && /^\d{1,4}$/.test(header.trim()) ? Number(header) : NaN;
  if (Number.isFinite(n)) retryAfter.set(err, n * 1000);
}

export type Query = Record<string, string | number | boolean | null | undefined>;

export function withQuery(path: string, query?: Query): string {
  if (!query) return path;
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    if (v === undefined || v === null || v === '') continue;
    params.set(k, String(v));
  }
  const s = params.toString();
  return s ? `${path}?${s}` : path;
}

export async function request<T>(path: string, init?: RequestInit & { json?: unknown }): Promise<T> {
  const headers = new Headers(init?.headers);
  headers.set('Accept', 'application/json');
  let body = init?.body;
  if (init?.json !== undefined) {
    headers.set('Content-Type', 'application/json');
    body = JSON.stringify(init.json);
  }
  const res = await apiFetch(path, { ...init, headers, body });
  if (res.status === 401) {
    redirectToLogin(loginUrl(currentPath()));
    throw new ApiError(401, { code: 'unauthorized', message: 'Authentication required' });
  }
  if (!res.ok) {
    const err = await apiErrorFrom(res);
    noteRetryAfter(err, res.headers.get('Retry-After'));
    throw err;
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

/** Like `request`, but also returns the response headers (for `ETag`) and sends raw bodies as given. */
export async function requestRaw(path: string, init?: RequestInit & { json?: unknown }): Promise<{ data: unknown; headers: Headers; status: number }> {
  const headers = new Headers(init?.headers);
  if (!headers.has('Accept')) headers.set('Accept', 'application/json');
  let body = init?.body;
  if (init?.json !== undefined) {
    headers.set('Content-Type', 'application/json');
    body = JSON.stringify(init.json);
  }
  const res = await apiFetch(path, { ...init, headers, body });
  if (res.status === 401) {
    redirectToLogin(loginUrl(currentPath()));
    throw new ApiError(401, { code: 'unauthorized', message: 'Authentication required' });
  }
  if (!res.ok) throw await apiErrorFrom(res);
  const text = res.status === 204 ? '' : await res.text();
  return { data: text ? (JSON.parse(text) as unknown) : undefined, headers: res.headers, status: res.status };
}

/** Plain-language messages for error codes whose server text assumes API knowledge. */
const CODE_MESSAGES: Record<string, string> = {
  ai_self_review: 'An AI draft or suggestion needs a second person: whoever created it cannot review it, make it active or accept it. Ask a colleague to do it.',
  export_busy: 'Too many zip exports are being built (at most 2 per person and 4 per project). Wait for one to finish or download it first.',
  launch_finished: 'The launch is finished: only the one who created it, or a caller allowed to sync cases, can add results to it.',
  launch_merged: 'One of the launches was merged into another launch already. Open that launch instead, or undo the earlier merge.',
  launch_running: 'Finish the launches first: only the launch you merge into may still be running.',
  launch_busy: 'Uploads of these launches are still being processed. Merge when they are done.',
  launch_manual: 'Launches with a manual run cannot be merged.',
  launch_rolled_up: 'One of the launches is older than the history rollup and can no longer be merged.',
  merge_mixed: 'Only launches of the same branch, and either all debug or all regular, can be merged.',
  merge_too_large: 'These launches hold more attempts or log lines than one merge may take on this server.',
  merge_expired: 'The undo window of this merge has ended.',
  merge_busy: 'Another merge or undo of this project is running. Try again in a moment.',
  merge_target_triaged: 'People triaged results of the merged launch after the merge. Undoing it deletes that work, so it needs the triage permission.',
  merge_source_gone: 'One of the merged launches was deleted, so this merge cannot be undone.',
  launch_plan: 'Launches started from a test plan cannot be merged.',
  merge_target_changed: 'Results were added to the merged launch after the merge, so it cannot be undone. Delete the launch instead.',
};

export function errorMessage(e: unknown): string {
  if (e instanceof ApiError && e.code in CODE_MESSAGES) return CODE_MESSAGES[e.code] ?? e.message;
  if (e instanceof ApiError) {
    const details = describeDetails(e.details);
    return details ? `${e.message} (${details})` : e.message;
  }
  if (e instanceof Error) return e.message;
  return String(e);
}

function describeDetails(details: unknown): string {
  if (!details || typeof details !== 'object') return '';
  const d = details as Record<string, unknown>;
  if (typeof d.required === 'string') return `requires ${d.required}`;
  if (typeof d.field === 'string') return `field ${d.field}`;
  return '';
}

export function projectPath(key: string): string {
  return `${API}/projects/${encodeURIComponent(key)}`;
}

export function attachmentUrl(project: string, id: string, download = false): string {
  return `${projectPath(project)}/attachments/${encodeURIComponent(id)}/content${download ? '?download=1' : ''}`;
}

export function traceViewerUrl(project: string, id: string): string {
  const content = new URL(attachmentUrl(project, id), window.location.origin).href;
  return `/trace/index.html?trace=${encodeURIComponent(content)}`;
}

const KIND_ALIASES: Record<string, AttachmentKind> = {
  image_diff: 'imageDiff',
  http_exchange: 'httpExchange',
};

export function normalizeKind(kind: string): AttachmentKind {
  const k = KIND_ALIASES[kind] ?? kind;
  switch (k) {
    case 'image':
    case 'video':
    case 'trace':
    case 'imageDiff':
    case 'httpExchange':
    case 'text':
    case 'log':
    case 'json':
      return k;
    default:
      return 'other';
  }
}

export async function fetchAttachmentText(project: string, id: string, maxBytes: number): Promise<{ text: string; truncated: boolean }> {
  const res = await apiFetch(attachmentUrl(project, id));
  if (res.status === 401) {
    redirectToLogin(loginUrl(currentPath()));
    throw new ApiError(401, { code: 'unauthorized', message: 'Authentication required' });
  }
  if (!res.ok) throw await apiErrorFrom(res);
  const buf = await res.arrayBuffer();
  const truncated = buf.byteLength > maxBytes;
  const text = new TextDecoder('utf-8', { fatal: false }).decode(truncated ? buf.slice(0, maxBytes) : buf);
  return { text, truncated };
}
