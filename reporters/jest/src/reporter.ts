import { mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, sep } from 'node:path';

import {
  FULL_NAME_SEPARATOR,
  ReporterSession,
  inTestPlan,
  type AttachmentFile,
  type FastProofOptions,
  type Label,
  type ReportError,
  type ReportResult,
  type Status,
} from '@franshub/fastproof-playwright';

import { STAGE_ENV, Tail, attemptFile, testKey, type StageLine } from './stage';

const MAX_OUTPUT = 4 * 1024 * 1024;
const POLL_MS = 200;
const MAX_OPEN_TAILS = 256;
const ID_MARKER = /\[(?:fastproof-id|allure-id):([^\]\s]{1,200})\]|@allure\.id[=:]([^\s\]]{1,200})/;

export interface JestFastProofOptions extends FastProofOptions {
  /** Test plan file (Allure format); default `FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`. */
  testPlan?: string;
}

interface TestRef {
  path: string;
}

interface CaseInfo {
  ancestorTitles: string[];
  fullName: string;
  title: string;
  startedAt?: number | null;
}

interface CaseResult extends CaseInfo {
  status: string;
  duration?: number | null;
  invocations?: number;
  failureMessages?: string[];
  failureDetails?: unknown[];
  location?: { line: number; column: number } | null;
}

interface FileResult {
  testFilePath: string;
  testResults: CaseResult[];
  testExecError?: { message?: string; stack?: string; type?: string } | null;
  failureMessage?: string | null;
  perfStats?: { start: number; end: number };
}

interface GlobalConfig {
  rootDir: string;
  watch?: boolean;
  watchAll?: boolean;
}

interface Open {
  id: string;
  tail: Tail;
  attempt: number;
  key: string;
  start: number;
  info: CaseInfo;
  path: string;
  atts: StageLine[];
}

/**
 * FastProof reporter for Jest. Jest reports every attempt of a retried test (`jest.retryTimes`) as its own
 * case, so each becomes its own result. Console output, steps and attachments reach the reporter through a
 * directory shared with the test processes (see `@franshub/fastproof-jest/setup`); without that setup file the
 * results carry no output.
 */
export class FastProofReporter {
  readonly session: ReporterSession;
  private globalConfig: GlobalConfig | undefined;
  private readonly open = new Map<string, Open>();
  private readonly seen = new Set<string>();
  private stage: string | undefined;
  private poller: NodeJS.Timeout | undefined;

  constructor(globalConfig?: GlobalConfig, options: JestFastProofOptions = {}, _context?: unknown, out?: (m: string) => void) {
    this.globalConfig = globalConfig;
    this.session = new ReporterSession(options ?? {}, 'jest', 'jt', out);
    if (this.session.cfg.enabled) {
      try {
        const base = join(tmpdir(), 'fastproof-jest-');
        this.stage = mkdtempSync(base);
        mkdirSync(join(this.stage, 'blobs'), { recursive: true, mode: 0o700 });
        process.env[STAGE_ENV] = this.stage;
      } catch {
        this.stage = undefined;
      }
    }
  }

  getLastError(): Error | undefined {
    return undefined;
  }

  private rootDir(): string {
    return this.globalConfig?.rootDir ?? process.cwd();
  }

  private file(path: string): string {
    return relative(this.rootDir(), path).split(sep).join('/');
  }

  private watching(): boolean {
    return Boolean(this.globalConfig?.watch || this.globalConfig?.watchAll);
  }

  onRunStart(): void {
    this.session.start();
    if (this.session.client && this.stage) {
      this.poller = setInterval(() => this.poll(), POLL_MS);
      this.poller.unref();
    }
  }

  private idOf(info: CaseInfo, path: string): { key: string; marker: string | undefined } {
    return { key: testKey(path, info.fullName), marker: ID_MARKER.exec(info.title)?.slice(1).find(Boolean) };
  }

  private selected(info: CaseInfo, path: string, location?: { line: number; column: number } | null): boolean {
    const plan = this.session.plan;
    if (!plan) return true;
    const marker = ID_MARKER.exec(info.title)?.slice(1).find(Boolean);
    return inTestPlan(plan, {
      titlePath: [this.file(path), ...info.ancestorTitles, info.title],
      line: location?.line,
      column: location?.column,
      annotations: marker ? [{ type: 'fastproof.id', description: marker }] : [],
    });
  }

  private identity(info: CaseInfo, path: string): Partial<ReportResult> {
    const s = this.session;
    const { marker } = this.idOf(info, path);
    const file = this.file(path);
    const labels: Label[] = [{ name: 'framework', value: 'jest' }, { name: 'suite', value: s.mask(file) }];
    if (info.ancestorTitles.length) labels.push({ name: 'subSuite', value: s.mask(info.ancestorTitles.join(FULL_NAME_SEPARATOR)) });
    const out: Partial<ReportResult> = {
      name: s.mask(info.title),
      fullName: s.mask([file, ...info.ancestorTitles, info.title].join(FULL_NAME_SEPARATOR)),
      labels,
      parameters: [],
      source: { file },
    };
    if (marker) out.testCaseId = s.mask(marker);
    return out;
  }

  onTestCaseStart(test: TestRef, info: CaseInfo): void {
    if (!this.selected(info, test.path)) return;
    this.session.guard((c) => {
      const { key } = this.idOf(info, test.path);
      const attempt = this.attemptNumber(key);
      const id = this.session.resultId(key, attempt);
      const start = info.startedAt ?? Date.now();
      if (this.open.size < MAX_OPEN_TAILS && this.stage)
        this.open.set(`${key}\u0000${attempt}`, { id, tail: new Tail(attemptFile(this.stage, key, attempt)), attempt, key, start, info, path: test.path, atts: [] });
      c.openRunning({
        id,
        ...this.identity(info, test.path),
        status: 'running',
        start,
        retry: attempt,
        traceparent: this.session.trace(key, attempt).traceparent,
      } as ReportResult);
    });
  }

  private readonly started = new Map<string, number>();

  private attemptNumber(key: string): number {
    const n = this.started.get(key) ?? 0;
    this.started.set(key, n + 1);
    return n;
  }

  private logBuffer = new Map<string, string[]>();

  private poll(): void {
    this.session.guard(() => {
      for (const o of this.open.values()) this.take(o, o.tail.read());
    });
  }

  private take(o: Open, lines: StageLine[]): void {
    const buf = this.logBuffer.get(o.id) ?? [];
    for (const l of lines) {
      if (l.k === 'att') {
        if (o.atts.length < 100) o.atts.push(l);
        continue;
      }
      if (l.s !== 'step' && buf.length < 50_000) buf.push(`${l.s}\u0000${l.m}`);
      this.session.client?.log(o.id, o.attempt, l.l, l.s, l.m.replace(/\n$/, ''), l.step, l.t);
    }
    this.logBuffer.set(o.id, buf);
  }

  private status(r: CaseResult): Status {
    switch (r.status) {
      case 'passed':
        return 'passed';
      case 'failed': {
        const d = r.failureDetails?.[0] as { matcherResult?: unknown; name?: string; code?: string } | undefined;
        const assertion = !d || d.matcherResult !== undefined || /AssertionError/.test(d.name ?? '') || d.code === 'ERR_ASSERTION';
        return assertion ? 'failed' : 'broken';
      }
      case 'skipped':
      case 'pending':
      case 'todo':
      case 'disabled':
        return 'skipped';
      default:
        return 'unknown';
    }
  }

  private error(r: CaseResult): ReportError | undefined {
    const s = this.session;
    const d = r.failureDetails?.[0] as { message?: unknown; stack?: unknown } | undefined;
    const msg0 = r.failureMessages?.[0];
    const message = typeof d?.message === 'string' && d.message ? d.message : msg0?.split('\n    at ')[0];
    const trace = typeof d?.stack === 'string' && d.stack ? d.stack : msg0;
    const out: ReportError = {};
    const m = s.text(message, 64 * 1024);
    const t = s.text(trace, MAX_OUTPUT);
    if (m) out.message = m;
    if (t) out.trace = t;
    return out.message || out.trace ? out : undefined;
  }

  private join(buf: string[] | undefined, stream: 'stdout' | 'stderr'): string | undefined {
    if (!buf?.length) return undefined;
    const text = buf
      .filter((l) => l.startsWith(`${stream}\u0000`))
      .map((l) => l.slice(stream.length + 1))
      .join('\n');
    return this.session.text(text, MAX_OUTPUT);
  }

  private files(lines: StageLine[]): AttachmentFile[] {
    const out: AttachmentFile[] = [];
    for (const l of lines) {
      if (l.k !== 'att' || out.length >= 100) continue;
      out.push({ name: this.session.mask(l.n) || 'attachment', contentType: l.c, path: l.p });
    }
    return out;
  }

  private finalize(path: string, r: CaseResult, o: Open | undefined): void {
    this.session.guard((c) => {
      const key = testKey(path, r.fullName);
      const attempt = o?.attempt ?? Math.max(0, (r.invocations ?? 1) - 1);
      const id = o?.id ?? this.session.resultId(key, attempt);
      let atts: StageLine[] = [];
      if (o) {
        this.take(o, o.tail.drain());
        atts = o.atts;
      }
      const status = this.status(r);
      const start = Math.round(o?.start ?? r.startedAt ?? Date.now());
      const duration = Math.max(0, Math.round(r.duration ?? 0));
      const res: ReportResult = {
        id,
        ...this.identity(r, path),
        name: this.session.mask(r.title),
        status,
        start,
        stop: start + duration,
        durationMs: duration,
        retry: attempt,
        traceparent: this.session.trace(key, attempt).traceparent,
      };
      if (r.location) res.source = { ...res.source, line: r.location.line };
      const err = this.error(r);
      if (err) res.error = err;
      else if (status === 'failed' || status === 'broken') res.error = { message: 'the test failed without details' };
      const buf = this.logBuffer.get(id);
      this.logBuffer.delete(id);
      const stdout = this.join(buf, 'stdout');
      const stderr = this.join(buf, 'stderr');
      if (stdout) res.stdout = stdout;
      if (stderr) res.stderr = stderr;
      c.finish(res, this.files(atts), status === 'passed' || status === 'skipped');
      this.open.delete(`${key}\u0000${attempt}`);
    });
  }

  onTestCaseResult(test: TestRef, r: CaseResult): void {
    if (!this.session.client || !this.selected(r, test.path, r.location)) return;
    const key = testKey(test.path, r.fullName);
    this.seen.add(key);
    this.finalize(test.path, r, this.open.get(`${key}\u0000${Math.max(0, (r.invocations ?? 1) - 1)}`));
  }

  onTestResult(test: TestRef, result: FileResult): void {
    if (!this.session.client) return;
    this.session.guard((c) => {
      for (const r of result.testResults) {
        const key = testKey(test.path, r.fullName);
        if (this.seen.has(key) || !this.selected(r, test.path, r.location)) continue;
        if (r.status === 'passed' || r.status === 'failed') {
          this.finalize(test.path, r, undefined);
          continue;
        }
        this.finalize(test.path, { ...r, duration: 0, startedAt: r.startedAt ?? Date.now() }, undefined);
      }
      const e = result.testExecError;
      if (e && (e.message || e.stack)) {
        const file = this.file(test.path);
        const key = testKey(test.path, '');
        const now = Date.now();
        const res: ReportResult = {
          id: this.session.resultId(key, 0),
          name: this.session.mask(`${file} (suite failed to run)`),
          fullName: this.session.mask(`${file}${FULL_NAME_SEPARATOR}suite failed to run`),
          status: 'broken',
          start: result.perfStats?.start ?? now,
          stop: result.perfStats?.end ?? now,
          durationMs: result.perfStats ? Math.max(0, result.perfStats.end - result.perfStats.start) : 0,
          retry: 0,
          labels: [{ name: 'framework', value: 'jest' }, { name: 'suite', value: this.session.mask(file) }],
          error: {
            message: this.session.text(e.message ?? e.stack, 64 * 1024) ?? 'the test file failed to run',
            ...(e.stack ? { trace: this.session.text(e.stack, MAX_OUTPUT) } : {}),
          },
          traceparent: this.session.trace(key, 0).traceparent,
        };
        c.finish(res, [], false);
      }
    });
  }

  async onRunComplete(_contexts: unknown, results: { wasInterrupted?: boolean }): Promise<void> {
    if (this.poller) clearInterval(this.poller);
    if (this.watching()) return;
    this.session.guard((c) => {
      for (const o of this.open.values()) {
        const now = Date.now();
        this.take(o, o.tail.drain());
        c.finish(
          {
            id: o.id,
            ...this.identity(o.info, o.path),
            name: this.session.mask(o.info.title),
            status: 'broken',
            start: o.start,
            stop: now,
            durationMs: Math.max(0, now - o.start),
            retry: o.attempt,
            traceparent: this.session.trace(o.key, o.attempt).traceparent,
            error: { message: 'the test did not finish (interrupted run)' },
          } as ReportResult,
          [],
          false,
        );
      }
      this.open.clear();
    });
    await this.session.end(results?.wasInterrupted ? 'interrupted' : 'finished');
    if (this.stage) {
      try {
        rmSync(this.stage, { recursive: true, force: true });
      } catch {
        /* the temporary directory is cleaned by the OS */
      }
      delete process.env[STAGE_ENV];
    }
  }
}
