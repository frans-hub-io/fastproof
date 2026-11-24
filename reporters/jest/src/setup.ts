import { appendFileSync, mkdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { format } from 'node:util';
import { randomBytes } from 'node:crypto';

import {
  LAUNCH_ENV,
  SEED_ENV,
  formatTraceparent,
  randomSpanId,
  traceFor,
  type TraceContext,
} from '@franshub/fastproof-playwright';

import { STAGE_ENV, attemptFile, testKey, type StageLine } from './stage';

export interface FastProofTrace extends TraceContext {
  /** `traceparent` and `baggage`; `newSpan` makes a fresh span id under the same trace (one per request). */
  headers(opts?: { newSpan?: boolean }): Record<string, string>;
  /** A copy of the environment with `TRACEPARENT` and `BAGGAGE`, for child processes. */
  env(from?: NodeJS.ProcessEnv): NodeJS.ProcessEnv;
}

interface Current {
  key: string;
  attempt: number;
  file: string;
  steps: string[];
}

interface JestGlobals {
  expect?: { getState(): { testPath?: string; currentTestName?: string } };
  beforeEach?: (fn: () => void) => void;
  afterEach?: (fn: () => void) => void;
  afterAll?: (fn: () => void) => void;
}

const MAX_LINE = 64 * 1024;
const MAX_ATTEMPT_BYTES = 16 * 1024 * 1024;

let current: Current | undefined;
const counts = new Map<string, number>();
const sizes = new Map<string, number>();
let pending: { file: string; text: string }[] = [];
let timer: NodeJS.Timeout | undefined;

function stageDir(): string | undefined {
  const d = process.env[STAGE_ENV];
  return d ? d : undefined;
}

function flush(): void {
  if (timer) {
    clearTimeout(timer);
    timer = undefined;
  }
  const batch = pending;
  pending = [];
  for (const p of batch) {
    try {
      appendFileSync(p.file, p.text, { mode: 0o600 });
    } catch {
      continue;
    }
  }
}

function write(file: string, line: StageLine): void {
  const text = JSON.stringify(line) + '\n';
  const used = (sizes.get(file) ?? 0) + text.length;
  if (used > MAX_ATTEMPT_BYTES) return;
  sizes.set(file, used);
  pending.push({ file, text });
  if (!timer) {
    timer = setTimeout(flush, 100);
    timer.unref();
  }
}

function emit(stream: 'stdout' | 'stderr', level: 'debug' | 'info' | 'warn' | 'error', args: unknown[]): void {
  const c = current;
  if (!c) return;
  let m = format(...args);
  if (m.length > MAX_LINE) m = m.slice(0, MAX_LINE);
  write(c.file, { k: 'log', t: Date.now(), s: stream, l: level, m });
}

/** Attaches a file path or bytes to the running attempt; the reporter uploads it when the attempt ends. */
export function attach(name: string, content: string | Buffer | Uint8Array, contentType = 'application/octet-stream'): void {
  const dir = stageDir();
  const c = current;
  if (!dir || !c) return;
  const path = join(dir, 'blobs', randomBytes(8).toString('hex'));
  mkdirSync(join(dir, 'blobs'), { recursive: true, mode: 0o700 });
  writeFileSync(path, content, { mode: 0o600 });
  write(c.file, { k: 'att', n: name, c: contentType, p: path });
}

/** Attaches an existing file (screenshot, report) to the running attempt, streamed from disk. */
export function attachFile(path: string, name = basename(path), contentType = 'application/octet-stream'): void {
  const c = current;
  if (!stageDir() || !c) return;
  const abs = resolve(path);
  try {
    if (!statSync(abs).isFile()) return;
  } catch {
    return;
  }
  write(c.file, { k: 'att', n: name, c: contentType, p: abs });
}

/** A live step: `▶`/`✔`/`✘` lines in the attempt's log with the step path. */
export async function step<T>(name: string, fn: () => T | Promise<T>): Promise<T> {
  const c = current;
  if (!c) return fn();
  c.steps.push(name);
  const path = c.steps.join(' › ');
  const started = Date.now();
  write(c.file, { k: 'log', t: started, s: 'step', l: 'info', m: `▶ ${name}`, step: path });
  try {
    const r = await fn();
    write(c.file, { k: 'log', t: Date.now(), s: 'step', l: 'info', m: `✔ ${name} (${Date.now() - started} ms)`, step: path });
    return r;
  } catch (e) {
    const msg = String((e as Error)?.message ?? e).split('\n')[0];
    write(c.file, { k: 'log', t: Date.now(), s: 'step', l: 'error', m: `✘ ${name}: ${msg}`, step: path });
    throw e;
  } finally {
    c.steps.pop();
  }
}

/**
 * The trace context of the running attempt, the one the reporter sends with the result. Needs this setup
 * file in `setupFilesAfterEnv`.
 */
export function fastproofTrace(): FastProofTrace {
  const c = current;
  const t = traceFor(process.env[SEED_ENV], process.env[LAUNCH_ENV] ?? '', c?.key ?? '', c?.attempt ?? 0);
  return {
    ...t,
    headers(opts) {
      const span = opts?.newSpan ? randomSpanId() : t.spanId;
      return { traceparent: formatTraceparent(t.traceId, span), baggage: t.baggage };
    },
    env(from = process.env) {
      return { ...from, TRACEPARENT: t.traceparent, BAGGAGE: t.baggage };
    },
  };
}

function install(g: JestGlobals): void {
  const dir = stageDir();
  if (!dir || !g.expect || !g.beforeEach || !g.afterEach) return;
  const expectRef = g.expect;
  g.beforeEach(() => {
    const s = expectRef.getState();
    if (!s.testPath || !s.currentTestName) {
      current = undefined;
      return;
    }
    const key = testKey(s.testPath, s.currentTestName);
    const attempt = counts.get(key) ?? 0;
    counts.set(key, attempt + 1);
    current = { key, attempt, file: attemptFile(dir, key, attempt), steps: [] };
  });
  g.afterEach(() => {
    flush();
  });
  g.afterAll?.(flush);
  const levels = { log: 'info', info: 'info', debug: 'debug', warn: 'warn', error: 'error' } as const;
  for (const name of Object.keys(levels) as (keyof typeof levels)[]) {
    const original = console[name].bind(console);
    console[name] = (...args: unknown[]) => {
      original(...args);
      emit(name === 'warn' || name === 'error' ? 'stderr' : 'stdout', levels[name], args);
    };
  }
}

install(globalThis as unknown as JestGlobals);
