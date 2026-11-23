import { createHash, randomUUID } from 'node:crypto';
import { type IncomingMessage, type Server, type ServerResponse, createServer } from 'node:http';
import type { AddressInfo } from 'node:net';

/** A small in-memory FastProof API for the live path: launches, results (with `running`), logs, attachments. */
export interface FakeOptions {
  token?: string;
  logs?: boolean;
  running?: boolean;
  port?: number;
  /** Offer direct uploads; `storeUrl` overrides where the pre-signed URLs point (an unreachable store). */
  direct?: boolean;
  storeUrl?: string;
}

export interface StoredResult {
  id: string;
  status: string;
  history: string[];
  body: Record<string, unknown>;
}

export class FakeServer {
  server: Server;
  url = '';
  launches = new Map<string, { id: string; number: number; externalId: string; finished?: string }>();
  results = new Map<string, StoredResult>();
  order: string[] = [];
  logs: Record<string, unknown>[] = [];
  attachments = new Map<string, { id: string; name: string; contentType: string; body: Buffer }>();
  uploads = 0;
  /** Direct uploads issued: upload id -> declaration and what was PUT. */
  direct = new Map<string, { sha256: string; size: number; name: string; contentType: string; body?: Buffer; auth?: string }>();
  directPuts = 0;
  requests: string[] = [];
  failNext = 0;

  constructor(readonly o: FakeOptions = {}) {
    this.server = createServer((req, res) => void this.handle(req, res));
  }

  async start(): Promise<this> {
    await new Promise<void>((r) => this.server.listen(this.o.port ?? 0, '127.0.0.1', r));
    this.url = `http://127.0.0.1:${(this.server.address() as AddressInfo).port}`;
    return this;
  }

  async stop(): Promise<void> {
    this.server.closeAllConnections();
    await new Promise<void>((r) => this.server.close(() => r()));
  }

  private send(res: ServerResponse, status: number, body: unknown): void {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  }

  private async body(req: IncomingMessage): Promise<Buffer> {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    return Buffer.concat(chunks);
  }

  private async handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = req.url ?? '';
    this.requests.push(`${req.method} ${url}`);
    const body = await this.body(req);
    if (this.failNext > 0) {
      this.failNext--;
      return this.send(res, 503, { code: 'unavailable', message: 'try later' });
    }
    const store = /^\/store\/([^/?]+)\?sig=ok$/.exec(url);
    if (store && req.method === 'PUT') {
      const d = this.direct.get(store[1]!);
      if (!d || req.headers['content-type'] !== 'application/octet-stream') return this.send(res, 403, {});
      d.body = body;
      d.auth = req.headers.authorization;
      this.directPuts++;
      res.writeHead(200);
      res.end();
      return;
    }
    if (this.o.token && req.headers.authorization !== `Bearer ${this.o.token}`)
      return this.send(res, 401, { code: 'unauthorized', message: 'no' });
    const dm = /^\/api\/v1\/projects\/[^/]+\/launches\/[^/]+\/attachments\/direct(?:\/([^/]+)\/complete)?$/.exec(url);
    if (dm && req.method === 'POST') {
      if (!this.o.direct) return this.send(res, 404, { code: 'not_found', message: 'resource not found' });
      if (!dm[1]) {
        const items = (JSON.parse(body.toString()) as { items: { sha256: string; size: number; name: string; contentType: string }[] }).items;
        return this.send(res, 200, {
          items: items.map((it) => {
            const known = this.attachments.get(it.sha256);
            if (known) return { sha256: it.sha256, status: 'exists', id: known.id };
            const uploadId = randomUUID();
            this.direct.set(uploadId, { ...it });
            return {
              sha256: it.sha256,
              status: 'upload',
              uploadId,
              url: `${this.o.storeUrl ?? this.url}/store/${uploadId}?sig=ok`,
              method: 'PUT',
              headers: { 'content-type': 'application/octet-stream' },
            };
          }),
        });
      }
      const d = this.direct.get(dm[1]);
      if (!d) return this.send(res, 404, { code: 'not_found', message: 'direct upload' });
      if (!d.body) return this.send(res, 409, { code: 'not_uploaded', message: 'PUT first' });
      const sha256 = createHash('sha256').update(d.body).digest('hex');
      if (sha256 !== d.sha256 || d.body.length !== d.size)
        return this.send(res, 422, { code: 'verification_failed', message: 'mismatch' });
      let a = this.attachments.get(sha256);
      if (!a) {
        a = { id: randomUUID(), name: d.name, contentType: d.contentType, body: d.body };
        this.attachments.set(sha256, a);
      }
      return this.send(res, 201, { id: a.id, sha256, size: d.size });
    }
    const m = /^\/api\/v1\/projects\/([^/]+)\/launches(?:\/([^/]+)\/(results|logs|attachments|finish))?$/.exec(url);
    if (!m || req.method !== 'POST') return this.send(res, 404, { code: 'not_found', message: 'resource not found' });
    const [, , launchId, what] = m;
    if (!what) {
      const start = JSON.parse(body.toString()) as { externalId: string };
      for (const l of this.launches.values())
        if (l.externalId === start.externalId) return this.send(res, 200, { id: l.id, number: l.number });
      const l = { id: randomUUID(), number: this.launches.size + 1, externalId: start.externalId };
      this.launches.set(l.id, l);
      return this.send(res, 201, { id: l.id, number: l.number });
    }
    const launch = this.launches.get(launchId!);
    if (!launch) return this.send(res, 404, { code: 'not_found', message: 'launch' });
    if (what === 'finish') {
      launch.finished = (JSON.parse(body.toString() || '{}') as { status?: string }).status ?? 'finished';
      return this.send(res, 200, { id: launch.id });
    }
    if (what === 'attachments') {
      this.uploads++;
      const sha256 = createHash('sha256').update(body).digest('hex');
      let a = this.attachments.get(sha256);
      if (!a) {
        a = {
          id: randomUUID(),
          name: decodeURIComponent(String(req.headers['x-fastproof-attachment-name'] ?? '')),
          contentType: String(req.headers['content-type']),
          body,
        };
        this.attachments.set(sha256, a);
      }
      return this.send(res, 201, { id: a.id, sha256, size: body.length });
    }
    const lines = body
      .toString()
      .split('\n')
      .filter((l) => l.trim())
      .map((l) => JSON.parse(l) as Record<string, unknown>);
    if (what === 'logs') {
      if (!this.o.logs) return this.send(res, 405, { code: 'method_not_allowed', message: 'no' });
      this.logs.push(...lines);
      return this.send(res, 202, { accepted: lines.length });
    }
    const summary = { accepted: 0, duplicates: 0, rejected: [] as { index: number; message: string }[] };
    lines.forEach((r, index) => {
      const id = String(r.id);
      const status = String(r.status);
      if (status === 'running' && !this.o.running) {
        summary.rejected.push({ index, message: 'invalid result: unknown variant `running`' });
        return;
      }
      const known = this.results.get(id);
      const ids = new Set(Array.from(this.attachments.values(), (a) => a.id));
      for (const a of (r.attachments as { id: string }[] | undefined) ?? [])
        if (!ids.has(a.id)) {
          summary.rejected.push({ index, message: `unknown attachment id ${a.id}` });
          return;
        }
      if (known && known.status !== 'running') {
        summary.duplicates++;
        return;
      }
      this.order.push(`${id}:${status}`);
      this.results.set(id, { id, status, history: [...(known?.history ?? []), status], body: r });
      summary.accepted++;
    });
    return this.send(res, 202, summary);
  }
}
