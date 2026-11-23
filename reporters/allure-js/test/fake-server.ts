import { createHash, randomUUID } from 'node:crypto';
import { type IncomingMessage, type Server, type ServerResponse, createServer } from 'node:http';
import type { AddressInfo } from 'node:net';

/** A small in-memory FastProof API for the live path: launches, results (with `running`), logs, attachments. */
export interface FakeOptions {
  token?: string;
  logs?: boolean;
  running?: boolean;
  port?: number;
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
    if (this.o.token && req.headers.authorization !== `Bearer ${this.o.token}`)
      return this.send(res, 401, { code: 'unauthorized', message: 'no' });
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
