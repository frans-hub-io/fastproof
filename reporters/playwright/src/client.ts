import { createHash, randomBytes } from 'node:crypto';
import { createReadStream, existsSync, readFileSync } from 'node:fs';
import { readFile, stat } from 'node:fs/promises';
import { hostname } from 'node:os';

import { Api, ApiError, USER_AGENT } from './api';
import { ok, putPresigned } from './http';
import type { Config } from './config';
import { Masker } from './mask';
import { BLOB_PREFIX, SPOOL_VERSION, SpoolSession, flushSpool, type SpoolBlob } from './spool';
import type { AttachmentFile, AttachmentRef, LogLevel, LogLine, LogStream, ReportResult } from './types';

/** Longest log message sent in one line; longer ones are cut. */
export const MAX_LOG_MESSAGE = 64 * 1024;
/** Log lines kept in memory while the server is slow; the oldest are dropped first. */
export const MAX_QUEUED_LOGS = 20_000;
/** Results per request and bytes per request. */
export const RESULT_BATCH = 500;
export const RESULT_BATCH_BYTES = 4 * 1024 * 1024;
/** Consecutive failed requests after which the client stops calling the server and spools. */
export const BREAKER_FAILURES = 5;
/** Text attachments up to this size are masked before upload. */
export const MASK_TEXT_ATTACHMENT_BYTES = 4 * 1024 * 1024;

interface QueuedResult {
  running: boolean;
  result: ReportResult;
  bytes: number;
}

interface Inflight {
  result: ReportResult;
  files: AttachmentFile[];
  passed: boolean;
}

export interface ClientSummary {
  launchId?: string;
  launchNumber?: number;
  sent: number;
  spooled: number;
  lost: number;
  attachments: number;
  offline: boolean;
  offlineReason?: string;
  spoolDir?: string;
  logs: 'on' | 'off' | 'unknown';
  running: boolean;
}

class Semaphore {
  private waiting: (() => void)[] = [];
  constructor(private free: number) {}
  async run<T>(fn: () => Promise<T>): Promise<T> {
    if (this.free > 0) this.free--;
    else await new Promise<void>((r) => this.waiting.push(r));
    try {
      return await fn();
    } finally {
      const next = this.waiting.shift();
      if (next) next();
      else this.free++;
    }
  }
}

async function sha256File(path: string): Promise<string> {
  const h = createHash('sha256');
  for await (const chunk of createReadStream(path)) h.update(chunk as Buffer);
  return h.digest('hex');
}

function isText(ct: string): boolean {
  const base = ct.split(';')[0]!.trim().toLowerCase();
  return base.startsWith('text/') || base === 'application/json' || base.endsWith('+json') || base === 'application/x-ndjson';
}

export function cut(s: string, max: number): string {
  return s.length > max ? s.slice(0, max) + '\n…[truncated]' : s;
}

/**
 * The transport core: a launch, a background sender with batching, live logs with feature detection,
 * attachment uploads with sha256 dedup, a circuit breaker and the offline spool. Hooks call the
 * synchronous methods; nothing here throws into the test run.
 */
export class Client {
  readonly api: Api;
  readonly masker: Masker;
  readonly sessionId: string;
  private launchId: string | undefined;
  private launchNumber: number | undefined;
  private launching: Promise<void> | undefined;
  private offline: boolean;
  private offlineReason: string | undefined;
  private failures = 0;
  private running: boolean;
  private logsState: 'unknown' | 'on' | 'off';
  private queue: QueuedResult[] = [];
  private logs: LogLine[] = [];
  private seq = new Map<string, number>();
  private inflight = new Map<string, Inflight>();
  private pipelines = new Set<Promise<void>>();
  private uploads = new Map<string, Promise<string>>();
  private sem = new Semaphore(4);
  private timer: NodeJS.Timeout | undefined;
  private pumping: Promise<void> | undefined;
  private oldSpool: Promise<void> | undefined;
  private warned = new Set<string>();
  /** Why direct uploads stopped for this run, once they did. */
  private directOff: string | undefined;
  readonly spool: SpoolSession;
  private stats = { sent: 0, lost: 0, attachments: 0, droppedLogs: 0 };

  constructor(
    readonly cfg: Config,
    readonly reporter = 'playwright',
    private readonly out: (msg: string) => void = (m) => process.stderr.write(m + '\n'),
  ) {
    this.api = new Api(cfg.url, cfg.token, cfg.project, cfg.requestTimeoutMs, 4, USER_AGENT);
    this.masker = new Masker(cfg.secrets, cfg.env);
    this.offline = cfg.offline;
    if (cfg.offline) this.offlineReason = 'offline mode';
    this.running = cfg.runningResults;
    this.logsState = cfg.liveLogs ? 'unknown' : 'off';
    this.sessionId = `${new Date().toISOString().replace(/[-:.]/g, '').slice(0, 15)}-${process.pid}-${randomBytes(4).toString('hex')}`;
    this.spool = new SpoolSession(
      cfg.spoolDir,
      this.sessionId,
      {
        v: SPOOL_VERSION,
        reporter: `${reporter}/${USER_AGENT.split('/')[1]}`,
        project: cfg.project,
        launch: cfg.launch,
        createdAt: new Date().toISOString(),
        pid: process.pid,
        host: hostname(),
      },
      cfg.spoolMaxBytes,
    );
  }

  info(msg: string): void {
    if (!this.cfg.quiet) this.out(`[fastproof] ${msg}`);
  }

  warnOnce(key: string, msg: string): void {
    if (this.warned.has(key)) return;
    this.warned.add(key);
    this.out(`[fastproof] ${this.masker.mask(msg)}`);
  }

  /** Starts the launch and the background sender; also sends what earlier runs left in the spool. */
  start(): void {
    if (this.offline) return;
    this.launching = this.ensureLaunch(2);
    this.timer = setInterval(() => void this.pump(), this.cfg.batchIntervalMs);
    this.timer.unref();
    this.oldSpool = (async () => {
      await this.launching;
      if (this.offline) return;
      const r = await flushSpool(this.api, this.cfg.spoolDir, {
        exclude: this.sessionId,
        project: this.cfg.project,
      }).catch(() => undefined);
      if (r && r.sessions) this.info(`sent ${r.results} results left by earlier runs from ${this.cfg.spoolDir}`);
    })();
  }

  get launch(): string | undefined {
    return this.launchId;
  }

  get logsLive(): 'on' | 'off' | 'unknown' {
    return this.logsState;
  }

  private async ensureLaunch(retries = 0): Promise<void> {
    if (this.launchId || this.offline) return;
    try {
      const l = await this.api.startLaunch(this.cfg.launch, retries);
      this.launchId = l.id;
      this.launchNumber = l.number;
      this.failures = 0;
    } catch (e) {
      this.failed(e);
    }
  }

  private failed(e: unknown): void {
    const err = e instanceof ApiError ? e : new ApiError(String(e), 0, false);
    if (err.fatal) {
      if (err.status === 401 || err.status === 403 || err.status === 404) this.goOffline(err.message);
      else this.warnOnce(`fatal:${err.status}`, `the server refused a request: ${err.message}`);
      return;
    }
    this.failures++;
    if (this.failures >= BREAKER_FAILURES) this.goOffline(err.message);
  }

  private goOffline(reason: string): void {
    if (this.offline) return;
    this.offline = true;
    this.offlineReason = reason;
    this.warnOnce('offline', `reporting continues offline (${reason}); results go to ${this.cfg.spoolDir}`);
    for (const q of this.queue.splice(0)) if (!q.running) this.spoolResult(q.result, []);
    this.logs = [];
  }

  /** Opens a result as `running` (when the server supports it). */
  openRunning(r: ReportResult): void {
    if (!this.running || this.offline) return;
    this.enqueue({ ...r, status: 'running' }, true);
  }

  /** Queues one live log line; the sequence number is per result id and attempt. */
  log(
    resultId: string,
    attempt: number,
    level: LogLevel,
    stream: LogStream,
    message: string,
    step?: string,
    ts: number = Date.now(),
  ): void {
    if (this.logsState === 'off' || this.offline) return;
    const key = `${resultId}\u0000${attempt}`;
    const seq = this.seq.get(key) ?? 0;
    this.seq.set(key, seq + 1);
    const line: LogLine = {
      resultId,
      attempt,
      seq,
      ts,
      level,
      stream,
      message: cut(this.masker.mask(message), MAX_LOG_MESSAGE),
    };
    if (step) line.step = this.masker.mask(step);
    this.logs.push(line);
    if (this.logs.length > MAX_QUEUED_LOGS) {
      this.logs.splice(0, this.logs.length - MAX_QUEUED_LOGS);
      this.stats.droppedLogs++;
    }
  }

  private enqueue(result: ReportResult, running: boolean): void {
    this.queue.push({ running, result, bytes: JSON.stringify(result).length });
  }

  /** Uploads the attempt's attachments in the background, then queues the final result. */
  finish(result: ReportResult, files: AttachmentFile[], passed: boolean): void {
    this.inflight.set(result.id, { result, files, passed });
    const p = this.pipeline(result, files, passed).finally(() => {
      this.inflight.delete(result.id);
      this.pipelines.delete(p);
    });
    this.pipelines.add(p);
  }

  private async prepare(f: AttachmentFile): Promise<{ file: AttachmentFile; sha256: string; size: number } | undefined> {
    try {
      let size = f.body ? f.body.length : (await stat(f.path!)).size;
      if (size > this.cfg.maxAttachmentBytes) {
        this.warnOnce(`big:${f.name}`, `attachment ${f.name} is larger than ${this.cfg.maxAttachmentBytes} bytes; skipped`);
        return undefined;
      }
      let file = f;
      if (this.masker.active && isText(f.contentType) && size <= MASK_TEXT_ATTACHMENT_BYTES) {
        const text = (f.body ?? (await readFile(f.path!))).toString('utf8');
        const masked = this.masker.mask(text);
        if (masked !== text || f.body) {
          const body = Buffer.from(masked, 'utf8');
          file = { name: f.name, contentType: f.contentType, body };
          size = body.length;
        }
      }
      const sha256 = file.body
        ? createHash('sha256').update(file.body).digest('hex')
        : await sha256File(file.path!);
      return { file, sha256, size };
    } catch (e) {
      this.warnOnce(`read:${f.name}`, `cannot read attachment ${f.name}: ${String(e)}`);
      return undefined;
    }
  }

  private async pipeline(result: ReportResult, files: AttachmentFile[], passed: boolean): Promise<void> {
    const prepared = (await Promise.all(files.map((f) => this.prepare(f)))).filter(
      (p): p is NonNullable<typeof p> => p !== undefined,
    );
    if (!this.offline && this.launching) await this.launching;
    if (!this.launchId && !this.offline) await this.ensureLaunch(1);
    const refs: AttachmentRef[] = [];
    const pending: typeof prepared = [];
    for (const p of prepared) {
      if (this.offline || !this.launchId) {
        pending.push(p);
        continue;
      }
      const id = await this.upload(p.file, p.sha256, p.size);
      if (id === 'retry') pending.push(p);
      else if (id !== 'skip') refs.push({ id, name: p.file.name, contentType: p.file.contentType });
    }
    result.attachments = [...(result.attachments ?? []), ...refs];
    if (this.offline || pending.length) {
      this.spoolResult(result, pending, passed);
      return;
    }
    this.enqueue(result, false);
    this.kick();
  }

  /**
   * Sends an attachment straight to the S3 store (see PROTOCOL.md, "Direct uploads"); `undefined` means "use the
   * server path": no direct uploads on the server, the store not reachable from here, or the file refused or
   * rejected. Server errors other than those are thrown like those of the server path.
   */
  private async direct(launchId: string, file: AttachmentFile, sha256: string, size: number): Promise<string | undefined> {
    if (this.directOff || this.cfg.directUploads === 'off' || size < this.cfg.directUploadMinBytes) return undefined;
    let t;
    try {
      t = await this.api.directUpload(launchId, { sha256, size, name: file.name, contentType: file.contentType }, 2);
    } catch (e) {
      if (!(e instanceof ApiError) || !e.fatal) throw e;
      if ([404, 405, 501].includes(e.status)) this.directOff = 'the server does not offer direct uploads';
      else this.warnOnce(`direct:a${e.status}`, `direct upload of ${file.name} refused (${e.message}); sent through the server`);
      return undefined;
    }
    if (t.status === 'exists' && t.id) return t.id;
    if (t.status !== 'upload' || !t.url || !t.uploadId) return undefined;
    const put = await putPresigned(
      t.url,
      t.headers ?? {},
      file.body ? { raw: file.body } : { file: file.path! },
      size,
      120_000 + Math.floor(size / 512),
    );
    if (put.status === 0) {
      this.directOff = `the S3 store is not reachable (${put.error ?? 'network error'})`;
      this.warnOnce('direct:unreachable', `direct uploads off: ${this.directOff}; attachments go through the server`);
      return undefined;
    }
    if (!ok(put)) {
      this.warnOnce(`direct:${put.status}`, `the S3 store answered ${put.status} to a direct upload; sent through the server`);
      return undefined;
    }
    try {
      return await this.api.completeDirect(launchId, t.uploadId, 2);
    } catch (e) {
      if (e instanceof ApiError && e.fatal) {
        this.warnOnce(`direct:c${e.status}`, `direct upload of ${file.name} not accepted (${e.message}); sent through the server`);
        return undefined;
      }
      throw e;
    }
  }

  private upload(file: AttachmentFile, sha256: string, size: number): Promise<string> {
    const known = this.uploads.get(sha256);
    if (known) return known;
    const launchId = this.launchId!;
    const p = this.sem.run(async () => {
      if (this.offline) return 'retry';
      try {
        const direct = await this.direct(launchId, file, sha256, size);
        if (direct) {
          this.failures = 0;
          this.stats.attachments++;
          return direct;
        }
        const r = await this.api.uploadAttachment(launchId, file, 2);
        this.failures = 0;
        this.stats.attachments++;
        return r.id;
      } catch (e) {
        if (e instanceof ApiError && e.fatal && (e.status < 0 || e.status === 413 || e.status === 400 || e.status === 415)) {
          this.warnOnce(`att:${e.status}`, `attachment ${file.name} rejected: ${e.message}`);
          return 'skip';
        }
        this.failed(e);
        return 'retry';
      }
    });
    this.uploads.set(sha256, p);
    void p.then((id) => {
      if (id === 'retry' || id === 'skip') this.uploads.delete(sha256);
    });
    return p;
  }

  private spoolResult(
    result: ReportResult,
    blobs: { file: AttachmentFile; sha256: string; size: number }[],
    passed = true,
  ): void {
    const kept: SpoolBlob[] = [];
    const refs: AttachmentRef[] = [...(result.attachments ?? [])];
    for (const b of blobs) {
      const src: { path?: string; body?: Buffer } = b.file.body ? { body: b.file.body } : { path: b.file.path! };
      if (this.spool.addBlob(b.sha256, src, b.size, passed)) {
        kept.push({ sha256: b.sha256, name: b.file.name, contentType: b.file.contentType, size: b.size });
        refs.push({ id: BLOB_PREFIX + b.sha256, name: b.file.name, contentType: b.file.contentType });
      }
    }
    const r = { ...result, attachments: refs };
    if (!this.spool.append({ v: SPOOL_VERSION, result: r, blobs: kept })) this.stats.lost++;
  }

  private kick(): void {
    if (this.queue.length >= RESULT_BATCH || this.logs.length >= 500) void this.pump();
  }

  /** One round of the background sender; concurrent calls share the round in progress. */
  pump(): Promise<void> {
    if (this.pumping) return this.pumping;
    this.pumping = (async () => {
      try {
        if (this.offline) return;
        if (!this.launchId) await this.ensureLaunch(0);
        if (!this.launchId) return;
        await this.sendResults();
        await this.sendLogs();
      } finally {
        this.pumping = undefined;
      }
    })();
    return this.pumping;
  }

  private takeBatch(): QueuedResult[] {
    const batch: QueuedResult[] = [];
    let bytes = 0;
    while (this.queue.length && batch.length < RESULT_BATCH) {
      const next = this.queue[0]!;
      if (batch.length && bytes + next.bytes > RESULT_BATCH_BYTES) break;
      batch.push(this.queue.shift()!);
      bytes += next.bytes;
    }
    return batch;
  }

  private async sendResults(): Promise<void> {
    while (this.queue.length && !this.offline) {
      const batch = this.takeBatch();
      try {
        const sum = await this.api.postResults(this.launchId!, batch.map((b) => b.result), 0);
        this.failures = 0;
        const rejected = new Set<number>();
        for (const r of sum.rejected) {
          rejected.add(r.index);
          const item = batch[r.index];
          if (item?.running) {
            if (this.running) this.running = false;
          } else {
            this.stats.lost++;
            this.warnOnce(`rej:${r.message.slice(0, 40)}`, `the server rejected a result: ${r.message}`);
          }
        }
        this.stats.sent += batch.filter((b, i) => !b.running && !rejected.has(i)).length;
      } catch (e) {
        if (e instanceof ApiError && e.status === 413 && batch.length > 1) {
          for (const b of batch) b.bytes = RESULT_BATCH_BYTES;
          this.queue.unshift(...batch);
          continue;
        }
        if (e instanceof ApiError && e.fatal && e.status === 413) {
          this.stats.lost += batch.filter((b) => !b.running).length;
          this.warnOnce('413', `a result is too large for the server: ${e.message}`);
          continue;
        }
        this.queue.unshift(...batch.filter((b) => !b.running));
        this.failed(e);
        return;
      }
    }
  }

  private async sendLogs(): Promise<void> {
    while (this.logs.length && this.logsState !== 'off' && !this.offline) {
      const batch = this.logs.splice(0, 1000);
      try {
        const r = await this.api.postLogs(this.launchId!, batch, 0);
        if (r === 'unsupported') {
          this.logsState = 'off';
          this.logs = [];
          return;
        }
        this.logsState = 'on';
        this.failures = 0;
      } catch (e) {
        if (e instanceof ApiError && e.fatal) {
          this.warnOnce('logs', `live logs rejected (${e.message}); they stay in the results`);
          this.logsState = 'off';
          this.logs = [];
          return;
        }
        this.logs.unshift(...batch);
        this.failed(e);
        return;
      }
    }
  }

  private spoolInflight(): void {
    for (const f of this.inflight.values()) {
      const blobs: { file: AttachmentFile; sha256: string; size: number }[] = [];
      for (const file of f.files) {
        try {
          const body = file.body ?? readFileSync(file.path!);
          const masked =
            this.masker.active && isText(file.contentType) && body.length <= MASK_TEXT_ATTACHMENT_BYTES
              ? Buffer.from(this.masker.mask(body.toString('utf8')), 'utf8')
              : body;
          blobs.push({
            file: { name: file.name, contentType: file.contentType, body: masked },
            sha256: createHash('sha256').update(masked).digest('hex'),
            size: masked.length,
          });
        } catch {
          continue;
        }
      }
      this.spoolResult(f.result, blobs, f.passed);
    }
    this.inflight.clear();
  }

  /**
   * Waits for uploads and the queue within the flush timeout, spools what is left, finishes the launch
   * when asked, and returns a summary. Never throws.
   */
  async close(status: 'finished' | 'interrupted'): Promise<ClientSummary> {
    const deadline = Date.now() + this.cfg.flushTimeoutMs;
    const remaining = () => Math.max(0, deadline - Date.now());
    const until = (p: Promise<unknown>) =>
      new Promise<boolean>((resolve) => {
        const t = setTimeout(() => resolve(false), remaining());
        p.then(
          () => {
            clearTimeout(t);
            resolve(true);
          },
          () => {
            clearTimeout(t);
            resolve(true);
          },
        );
      });
    if (this.timer) clearInterval(this.timer);
    try {
      await until(Promise.allSettled([...this.pipelines]));
      while (!this.offline && (this.queue.length || this.logs.length) && remaining() > 0) {
        const before = this.queue.length + this.logs.length;
        await until(this.pump());
        if (this.queue.length + this.logs.length >= before) await until(new Promise((r) => setTimeout(r, 200)));
      }
      if (this.oldSpool) await until(this.oldSpool);
      if (this.inflight.size || this.queue.some((q) => !q.running)) {
        this.goOffline(this.offlineReason ?? 'the flush timeout was reached');
        this.spoolInflight();
        for (const q of this.queue.splice(0)) if (!q.running) this.spoolResult(q.result, []);
      }
      const wantFinish =
        this.cfg.finishLaunch === 'always' || (this.cfg.finishLaunch === 'auto' && this.cfg.ownLaunch);
      if (this.spool.exists) {
        this.spool.end(wantFinish ? status : undefined);
        if (!this.cfg.offline && remaining() > 0) {
          const r = await flushSpool(this.api, this.cfg.spoolDir, {
            deadline,
            project: this.cfg.project,
          }).catch(() => undefined);
          if (r) this.stats.sent += r.results;
          if (r && r.sessions && !this.launchId) {
            const l = await this.api.startLaunch(this.cfg.launch, 0).catch(() => undefined);
            if (l) {
              this.launchId = l.id;
              this.launchNumber = l.number;
            }
          }
        }
      } else if (wantFinish && this.launchId && remaining() > 0) {
        await this.api.finishLaunch(this.launchId, status, 1).catch((e) => this.failed(e));
      }
    } catch (e) {
      this.warnOnce('close', `reporting error: ${String(e)}`);
    }
    const spooledLeft = this.spool.exists && existsSync(this.spool.dir);
    const s: ClientSummary = {
      sent: this.stats.sent,
      spooled: spooledLeft ? this.spool.written.results : 0,
      lost: this.stats.lost + this.spool.dropped.results,
      attachments: this.stats.attachments,
      offline: this.offline,
      logs: this.logsState,
      running: this.running,
    };
    if (this.launchId) s.launchId = this.launchId;
    if (this.launchNumber !== undefined) s.launchNumber = this.launchNumber;
    if (this.offlineReason) s.offlineReason = this.offlineReason;
    if (spooledLeft) s.spoolDir = this.spool.dir;
    return s;
  }
}
