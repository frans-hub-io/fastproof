import { http, HttpResponse } from 'msw';
import { attachmentContent } from './fixtures';
import sampleUrl from '../../e2e/fixtures/sample.webm?url';

const API = '/api/v1';
const MEDIA_TTL_MS = 15 * 60_000;

interface Issued {
  attachmentId: string;
  expired: boolean;
}

const issued = new Map<string, Issued>();
let counter = 0;
let sample: Promise<Uint8Array | null> | null = null;

function loadSample(): Promise<Uint8Array | null> {
  sample ??= fetch(sampleUrl)
    .then((r) => (r.ok ? r.arrayBuffer() : null))
    .then((b) => (b && b.byteLength > 1000 ? new Uint8Array(b) : null))
    .catch(() => null);
  return sample;
}

/** Marks every address handed out so far as expired: the next request to it answers 404. */
export function expireMediaUrls(): number {
  let n = 0;
  for (const t of issued.values()) {
    if (!t.expired) n++;
    t.expired = true;
  }
  return n;
}

export function issuedMediaTokens(): number {
  return issued.size;
}

if (typeof window !== 'undefined') {
  (window as unknown as { __fpExpireMediaUrls?: () => number }).__fpExpireMediaUrls = expireMediaUrls;
}

export interface ByteRange {
  start: number;
  end: number;
}

/** `bytes=a-b`, `bytes=a-` and `bytes=-n` against a body of `size` bytes; `null` when not satisfiable. */
export function parseRange(header: string | null, size: number): ByteRange | 'full' | null {
  if (!header) return 'full';
  const m = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
  if (!m) return 'full';
  const [, a = '', b = ''] = m;
  if (a === '' && b === '') return 'full';
  if (a === '') {
    const n = Number(b);
    if (n === 0) return null;
    return { start: Math.max(0, size - n), end: size - 1 };
  }
  const start = Number(a);
  const end = b === '' ? size - 1 : Math.min(Number(b), size - 1);
  if (start >= size || start > end) return null;
  return { start, end };
}

type ErrorFn = (status: number, code: string, message: string) => Response;

export function mediaHandlers(error: ErrorFn) {
  return [
    http.post(`${API}/projects/:key/attachments/:id/media-url`, ({ params }) => {
      const id = String(params.id);
      const a = attachmentContent(id);
      if (!a || a.meta.kind !== 'video') return error(404, 'not_found', 'attachment not found');
      counter += 1;
      const token = `m${String(counter)}-${id.slice(0, 8)}`;
      issued.set(token, { attachmentId: id, expired: false });
      return HttpResponse.json({ url: `${API}/media/${token}`, expiresAt: new Date(Date.now() + MEDIA_TTL_MS).toISOString() });
    }),

    http.get(`${API}/media/:token`, async ({ params, request }) => {
      const t = issued.get(String(params.token));
      if (!t || t.expired) return error(404, 'not_found', 'the media address expired');
      const a = attachmentContent(t.attachmentId);
      if (!a) return error(404, 'not_found', 'attachment not found');
      const fallback = a.body();
      const bytes = (await loadSample()) ?? (typeof fallback === 'string' ? new TextEncoder().encode(fallback) : fallback);
      const size = bytes.length;
      const range = parseRange(request.headers.get('Range'), size);
      const headers: Record<string, string> = {
        'Content-Type': a.meta.contentType,
        'Accept-Ranges': 'bytes',
        'X-Content-Type-Options': 'nosniff',
        'Cache-Control': 'private, no-store',
        ETag: `"mock-${String(size)}"`,
      };
      if (range === null) return new HttpResponse(null, { status: 416, headers: { ...headers, 'Content-Range': `bytes */${String(size)}` } });
      if (range === 'full') return new HttpResponse(bytes.slice().buffer, { status: 200, headers: { ...headers, 'Content-Length': String(size) } });
      const part = bytes.slice(range.start, range.end + 1);
      return new HttpResponse(part.buffer, {
        status: 206,
        headers: { ...headers, 'Content-Range': `bytes ${String(range.start)}-${String(range.end)}/${String(size)}`, 'Content-Length': String(part.length) },
      });
    }),
  ];
}
