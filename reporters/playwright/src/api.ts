import { Http, type HttpResult, backoffMs, ok, retryable, sleep } from './http';
import type { IngestSummary, Launch, LaunchStart, LogLine, ReportResult } from './types';

export const VERSION = '0.1.0';
export const USER_AGENT = `fastproof-playwright/${VERSION}`;

/** A failed call; `fatal` means retrying the same request cannot help (4xx other than 408/429). */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly fatal: boolean,
  ) {
    super(message);
  }
}

function describe(r: HttpResult): string {
  if (r.status === 0) return `server unreachable (${r.error ?? 'network error'})`;
  if (r.status < 0) return r.error ?? 'local error';
  const b = r.body as { code?: string; message?: string } | undefined;
  return `HTTP ${r.status}${b?.message ? `: ${b.message}` : ''}`;
}

/** Typed calls of the FastProof API with bounded retries. */
export class Api {
  readonly http: Http;

  constructor(
    url: string,
    token: string,
    readonly project: string,
    timeoutMs = 15_000,
    readonly retries = 4,
    userAgent = USER_AGENT,
  ) {
    this.http = new Http({ url, token, timeoutMs, userAgent });
  }

  private base(launchId?: string): string {
    const p = `/api/v1/projects/${encodeURIComponent(this.project)}`;
    return launchId ? `${p}/launches/${encodeURIComponent(launchId)}` : p;
  }

  /** Retries network errors, 408, 429 and 5xx with backoff; other failures throw at once. */
  async call(fn: () => Promise<HttpResult>, retries = this.retries): Promise<HttpResult> {
    let last: HttpResult = { status: 0, body: undefined };
    for (let i = 0; i <= retries; i++) {
      last = await fn();
      if (ok(last)) return last;
      if (!retryable(last)) throw new ApiError(describe(last), last.status, true);
      if (i < retries) await sleep(last.retryAfterMs ?? backoffMs(i));
    }
    throw new ApiError(describe(last), last.status, false);
  }

  async startLaunch(start: LaunchStart, retries?: number): Promise<Launch> {
    const r = await this.call(() => this.http.request('POST', `${this.base()}/launches`, { json: start }), retries);
    const l = r.body as Launch;
    if (!l || typeof l.id !== 'string') throw new ApiError('unexpected launch response', r.status, true);
    return l;
  }

  async postResults(launchId: string, results: ReportResult[], retries?: number): Promise<IngestSummary> {
    const r = await this.call(
      () => this.http.request('POST', `${this.base(launchId)}/results`, { ndjson: results }),
      retries,
    );
    const s = r.body as IngestSummary;
    return { accepted: s?.accepted ?? 0, duplicates: s?.duplicates ?? 0, rejected: s?.rejected ?? [] };
  }

  /** `undefined` when the server has no logs endpoint (404/405). */
  async postLogs(launchId: string, lines: LogLine[], retries?: number): Promise<'ok' | 'unsupported'> {
    try {
      await this.call(() => this.http.request('POST', `${this.base(launchId)}/logs`, { ndjson: lines }), retries);
      return 'ok';
    } catch (e) {
      if (e instanceof ApiError && (e.status === 404 || e.status === 405 || e.status === 501)) return 'unsupported';
      throw e;
    }
  }

  async uploadAttachment(
    launchId: string,
    a: { path?: string; body?: Buffer; name: string; contentType: string },
    retries?: number,
  ): Promise<{ id: string; sha256: string; size: number }> {
    const headers = { 'x-fastproof-attachment-name': encodeHeader(a.name) };
    const r = await this.call(
      () =>
        this.http.request(
          'POST',
          `${this.base(launchId)}/attachments`,
          a.path ? { file: a.path, contentType: a.contentType } : { raw: a.body ?? Buffer.alloc(0), contentType: a.contentType },
          headers,
          120_000,
        ),
      retries,
    );
    const b = r.body as { id?: string; sha256?: string; size?: number };
    if (!b || typeof b.id !== 'string') throw new ApiError('unexpected attachment response', r.status, true);
    return { id: b.id, sha256: b.sha256 ?? '', size: b.size ?? 0 };
  }

  async finishLaunch(launchId: string, status: 'finished' | 'interrupted', retries?: number): Promise<void> {
    await this.call(
      () => this.http.request('POST', `${this.base(launchId)}/finish`, { json: { status } }),
      retries,
    );
  }
}

/** Header values must be visible ASCII; other characters are percent-encoded. */
export function encodeHeader(v: string): string {
  const s = v.replace(/[\r\n]/g, ' ').slice(0, 255);
  return /^[\x20-\x7e]*$/.test(s) ? s : encodeURIComponent(s);
}
