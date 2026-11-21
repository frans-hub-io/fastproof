import {
  closeSync,
  copyFileSync,
  existsSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
  writeSync,
} from 'node:fs';
import { hostname } from 'node:os';
import { join } from 'node:path';

import { Api, ApiError } from './api';
import type { LaunchStart, ReportResult } from './types';

/**
 * The offline spool (format in reporters/PROTOCOL.md):
 * `<root>/<session>/meta.json`, `results.ndjson`, `progress.json`, `blobs/<sha256>`.
 */
export const SPOOL_VERSION = 1;
export const BLOB_PREFIX = 'blob:';

export interface SpoolMeta {
  v: number;
  reporter: string;
  project: string;
  launch: LaunchStart;
  createdAt: string;
  pid: number;
  host: string;
  /** Set when the run ended; `finish` tells whether the launch should be finished after the flush. */
  ended?: boolean;
  finish?: 'finished' | 'interrupted';
}

export interface SpoolBlob {
  sha256: string;
  name: string;
  contentType: string;
  size: number;
}

export interface SpoolEntry {
  v: number;
  result: ReportResult;
  blobs: SpoolBlob[];
}

function dirSize(p: string): number {
  let total = 0;
  let entries: string[];
  try {
    entries = readdirSync(p);
  } catch {
    return 0;
  }
  for (const e of entries) {
    const f = join(p, e);
    try {
      const s = statSync(f);
      total += s.isDirectory() ? dirSize(f) : s.size;
    } catch {
      continue;
    }
  }
  return total;
}

function writeAtomic(path: string, data: string): void {
  const tmp = `${path}.${process.pid}.tmp`;
  writeFileSync(tmp, data);
  renameSync(tmp, path);
}

/** One reporter process's spool session; all writes respect a byte budget shared by the spool root. */
export class SpoolSession {
  readonly dir: string;
  private used: number;
  private opened = false;
  dropped = { results: 0, blobs: 0 };
  written = { results: 0, blobs: 0 };

  constructor(
    readonly root: string,
    readonly id: string,
    private readonly meta: SpoolMeta,
    private readonly maxBytes: number,
  ) {
    this.dir = join(root, id);
    this.used = dirSize(root);
  }

  private open(): boolean {
    if (this.opened) return true;
    try {
      mkdirSync(join(this.dir, 'blobs'), { recursive: true });
      writeAtomic(join(this.dir, 'meta.json'), JSON.stringify(this.meta));
      this.opened = true;
    } catch {
      return false;
    }
    return true;
  }

  get exists(): boolean {
    return this.opened;
  }

  /**
   * Copies an attachment into the spool. `low` priority (attachments of passed tests) may use only half
   * of the budget, so failures keep room for their screenshots and traces.
   */
  addBlob(sha256: string, src: { path?: string; body?: Buffer }, size: number, low: boolean): boolean {
    if (!this.open()) return false;
    const target = join(this.dir, 'blobs', sha256);
    if (existsSync(target)) return true;
    const limit = low ? this.maxBytes / 2 : this.maxBytes;
    if (this.used + size > limit) {
      this.dropped.blobs++;
      return false;
    }
    try {
      const tmp = `${target}.${process.pid}.tmp`;
      if (src.path) copyFileSync(src.path, tmp);
      else writeFileSync(tmp, src.body ?? Buffer.alloc(0));
      renameSync(tmp, target);
      this.used += size;
      this.written.blobs++;
      return true;
    } catch {
      this.dropped.blobs++;
      return false;
    }
  }

  append(entry: SpoolEntry): boolean {
    if (!this.open()) {
      this.dropped.results++;
      return false;
    }
    const line = JSON.stringify(entry) + '\n';
    const size = Buffer.byteLength(line);
    if (this.used + size > this.maxBytes) {
      this.dropped.results++;
      return false;
    }
    try {
      const fd = openSync(join(this.dir, 'results.ndjson'), 'a');
      try {
        writeSync(fd, line);
        fsyncSync(fd);
      } finally {
        closeSync(fd);
      }
      this.used += size;
      this.written.results++;
      return true;
    } catch {
      this.dropped.results++;
      return false;
    }
  }

  end(finish: 'finished' | 'interrupted' | undefined): void {
    if (!this.opened) return;
    this.meta.ended = true;
    if (finish) this.meta.finish = finish;
    try {
      writeAtomic(join(this.dir, 'meta.json'), JSON.stringify(this.meta));
    } catch {
      return;
    }
  }
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === 'EPERM';
  }
}

export interface SessionInfo {
  id: string;
  dir: string;
  meta: SpoolMeta;
}

/** Sessions that can be sent now: ended, or left by a process that is gone. */
export function listSessions(root: string, exclude?: string): SessionInfo[] {
  let names: string[];
  try {
    names = readdirSync(root);
  } catch {
    return [];
  }
  const out: SessionInfo[] = [];
  for (const id of names.sort()) {
    if (id === exclude) continue;
    const dir = join(root, id);
    try {
      const meta = JSON.parse(readFileSync(join(dir, 'meta.json'), 'utf8')) as SpoolMeta;
      if (meta.v !== SPOOL_VERSION) continue;
      const running = !meta.ended && meta.host === hostname() && meta.pid !== process.pid && pidAlive(meta.pid);
      if (!running) out.push({ id, dir, meta });
    } catch {
      continue;
    }
  }
  return out;
}

export interface FlushOutcome {
  sessions: number;
  results: number;
  failed: number;
  remaining: number;
  errors: string[];
}

function readProgress(dir: string): number {
  try {
    return Number(JSON.parse(readFileSync(join(dir, 'progress.json'), 'utf8')).lines) || 0;
  } catch {
    return 0;
  }
}

function replaceBlobIds(r: ReportResult, ids: Map<string, string>): boolean {
  let complete = true;
  const fix = (list: { id: string }[] | undefined) => {
    if (!list) return list;
    return list.filter((a) => {
      if (!a.id.startsWith(BLOB_PREFIX)) return true;
      const id = ids.get(a.id.slice(BLOB_PREFIX.length));
      if (id) a.id = id;
      else complete = false;
      return Boolean(id);
    });
  };
  const walk = (s: { attachments?: { id: string }[]; steps?: unknown[] }) => {
    s.attachments = fix(s.attachments);
    for (const c of (s.steps ?? []) as { attachments?: { id: string }[]; steps?: unknown[] }[]) walk(c);
  };
  walk(r as unknown as { attachments?: { id: string }[]; steps?: unknown[] });
  return complete;
}

/**
 * Sends spooled sessions: resolves each launch by its external id, uploads blobs once per hash, posts
 * results in batches (idempotent by client id), finishes the launch when the run asked for it, and
 * deletes the session. Stops at `deadline` and resumes from `progress.json` next time.
 */
export async function flushSpool(
  api: Api,
  root: string,
  o: { exclude?: string; deadline?: number; project?: string; batch?: number } = {},
): Promise<FlushOutcome> {
  const out: FlushOutcome = { sessions: 0, results: 0, failed: 0, remaining: 0, errors: [] };
  const batchSize = o.batch ?? 200;
  const late = () => o.deadline !== undefined && Date.now() > o.deadline;
  for (const s of listSessions(root, o.exclude)) {
    if (o.project && s.meta.project !== o.project) continue;
    let lines: string[];
    try {
      lines = readFileSync(join(s.dir, 'results.ndjson'), 'utf8').split('\n').filter(Boolean);
    } catch {
      lines = [];
    }
    let done = readProgress(s.dir);
    if (late()) {
      out.remaining += lines.length - done;
      continue;
    }
    try {
      const launch = await api.startLaunch(s.meta.launch, 2);
      const blobIds = new Map<string, string>();
      while (done < lines.length && !late()) {
        const chunk = lines.slice(done, done + batchSize);
        const results: ReportResult[] = [];
        for (const l of chunk) {
          let e: SpoolEntry;
          try {
            e = JSON.parse(l) as SpoolEntry;
          } catch {
            out.failed++;
            continue;
          }
          for (const b of e.blobs ?? []) {
            if (blobIds.has(b.sha256)) continue;
            const path = join(s.dir, 'blobs', b.sha256);
            if (!existsSync(path)) continue;
            const r = await api.uploadAttachment(launch.id, { path, name: b.name, contentType: b.contentType }, 2);
            blobIds.set(b.sha256, r.id);
          }
          replaceBlobIds(e.result, blobIds);
          results.push(e.result);
        }
        if (results.length) {
          const sum = await api.postResults(launch.id, results, 2);
          out.results += sum.accepted + sum.duplicates;
          out.failed += sum.rejected.length;
          for (const r of sum.rejected.slice(0, 3)) out.errors.push(r.message);
        }
        done += chunk.length;
        writeAtomic(join(s.dir, 'progress.json'), JSON.stringify({ lines: done }));
      }
      if (done >= lines.length) {
        if (s.meta.ended && s.meta.finish) await api.finishLaunch(launch.id, s.meta.finish, 2);
        if (s.meta.ended || s.meta.pid !== process.pid) rmSync(s.dir, { recursive: true, force: true });
        out.sessions++;
      } else {
        out.remaining += lines.length - done;
      }
    } catch (e) {
      out.remaining += lines.length - done;
      out.errors.push(e instanceof ApiError || e instanceof Error ? e.message : String(e));
      if (e instanceof ApiError && !e.fatal) break;
    }
  }
  return out;
}
