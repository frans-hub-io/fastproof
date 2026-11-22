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
  if (!res.ok) throw await apiErrorFrom(res);
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

export function errorMessage(e: unknown): string {
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
