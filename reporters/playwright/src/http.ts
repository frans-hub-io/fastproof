import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import { Readable } from 'node:stream';

/** Outcome of one HTTP call; `status` is 0 when no response arrived. */
export interface HttpResult {
  status: number;
  body: unknown;
  retryAfterMs?: number;
  error?: string;
}

export function retryable(r: HttpResult): boolean {
  return r.status === 0 || r.status === 408 || r.status === 429 || r.status >= 500;
}

export function ok(r: HttpResult): boolean {
  return r.status >= 200 && r.status < 300;
}

function retryAfter(v: string | null): number | undefined {
  if (!v) return undefined;
  const n = Number(v);
  if (Number.isFinite(n)) return Math.min(Math.max(n, 0), 60) * 1000;
  const d = Date.parse(v);
  return Number.isFinite(d) ? Math.min(Math.max(d - Date.now(), 0), 60_000) : undefined;
}

export interface HttpOptions {
  url: string;
  token: string;
  timeoutMs: number;
  userAgent: string;
}

/** A small fetch wrapper: bearer token, timeout, JSON or raw bodies, never throws. */
export class Http {
  private readonly base: string;

  constructor(private readonly o: HttpOptions) {
    this.base = o.url.replace(/\/+$/, '');
  }

  async request(
    method: string,
    path: string,
    body?: { json?: unknown; ndjson?: unknown[]; raw?: Buffer; file?: string; contentType?: string },
    headers: Record<string, string> = {},
    timeoutMs = this.o.timeoutMs,
  ): Promise<HttpResult> {
    const h: Record<string, string> = {
      accept: 'application/json',
      'user-agent': this.o.userAgent,
      ...headers,
    };
    if (this.o.token) h.authorization = `Bearer ${this.o.token}`;
    let payload: RequestInit['body'] | undefined;
    let duplex = false;
    if (body?.json !== undefined) {
      h['content-type'] = 'application/json';
      payload = JSON.stringify(body.json);
    } else if (body?.ndjson) {
      h['content-type'] = 'application/x-ndjson';
      payload = body.ndjson.map((l) => JSON.stringify(l)).join('\n') + '\n';
    } else if (body?.raw) {
      h['content-type'] = body.contentType ?? 'application/octet-stream';
      payload = new Uint8Array(body.raw.buffer, body.raw.byteOffset, body.raw.byteLength);
    } else if (body?.file) {
      h['content-type'] = body.contentType ?? 'application/octet-stream';
      try {
        h['content-length'] = String((await stat(body.file)).size);
      } catch (e) {
        return { status: -1, body: undefined, error: `cannot read ${body.file}: ${String(e)}` };
      }
      payload = Readable.toWeb(createReadStream(body.file)) as unknown as RequestInit['body'];
      duplex = true;
    }
    try {
      const init: RequestInit & { duplex?: 'half' } = {
        method,
        headers: h,
        body: payload,
        signal: AbortSignal.timeout(timeoutMs),
      };
      if (duplex) init.duplex = 'half';
      const res = await fetch(this.base + path, init);
      const text = await res.text();
      let parsed: unknown = undefined;
      if (text) {
        try {
          parsed = JSON.parse(text);
        } catch {
          parsed = text;
        }
      }
      const ra = retryAfter(res.headers.get('retry-after'));
      return ra === undefined ? { status: res.status, body: parsed } : { status: res.status, body: parsed, retryAfterMs: ra };
    } catch (e) {
      const err = e as { name?: string; message?: string; cause?: { code?: string; message?: string } };
      const why = err.cause?.code ?? err.cause?.message ?? err.name ?? err.message ?? 'network error';
      return { status: 0, body: undefined, error: String(why) };
    }
  }
}

/** Exponential backoff with full jitter, capped. */
export function backoffMs(attempt: number, baseMs = 250, capMs = 8000): number {
  const exp = Math.min(capMs, baseMs * 2 ** Math.min(attempt, 16));
  return Math.floor(exp / 2 + Math.random() * (exp / 2));
}

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
