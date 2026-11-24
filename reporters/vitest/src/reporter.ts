import { relative, resolve, sep } from 'node:path';

import {
  FULL_NAME_SEPARATOR,
  ReporterSession,
  inTestPlan,
  type AttachmentFile,
  type FastProofOptions,
  type Label,
  type Link,
  type Parameter,
  type ReportError,
  type ReportResult,
  type Status,
} from '@franshub/fastproof-playwright';

interface Located {
  line: number;
  column: number;
}

/** The parts of Vitest's `TestCase` the reporter reads (stable from Vitest 3; `attempts()` and `artifacts()` from 5). */
interface TestCase {
  readonly id: string;
  readonly name: string;
  readonly tags?: readonly string[];
  readonly location?: Located;
  readonly options: object;
  readonly project: { name: string };
  readonly module: { moduleId: string };
  readonly parent: { type: string; name?: string; parent?: TestCase['parent'] };
  meta(): object;
  result(): { state: 'pending' | 'passed' | 'failed' | 'skipped'; errors?: readonly Err[] };
  diagnostic(): { duration: number; startTime: number; retryCount?: number } | undefined;
  annotations(): readonly { message: string; type: string; attachment?: object }[];
}

interface Vitest {
  config: { root: string; watch?: boolean };
}

const MAX_OUTPUT = 4 * 1024 * 1024;
const MAX_META_LABELS = 100;
const URL_RE = /^https?:\/\//;

type Err = { message?: string; stack?: string; name?: string; actual?: unknown; expected?: unknown };

interface AttemptView {
  state: 'passed' | 'failed' | 'skipped';
  errors: readonly Err[] | undefined;
  duration: number;
  startTime: number;
}

interface LogRecord {
  stream: 'stdout' | 'stderr';
  content: string;
  time: number;
}

interface TestMeta {
  fastproofId?: unknown;
  fastproofLabels?: unknown;
  fastproofLinks?: unknown;
}

export interface VitestFastProofOptions extends FastProofOptions {
  /** Test plan file (Allure format); default `FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`. */
  testPlan?: string;
}

function isAssertion(e: Err | undefined): boolean {
  if (!e) return false;
  return e.name === 'AssertionError' || (e.name ?? '').endsWith('AssertionError') || 'actual' in e || 'expected' in e;
}

/**
 * FastProof reporter for Vitest. Vitest delivers task events to the main process in batches, so a
 * result is `running` shortly after its test starts and final when the test (with all its retries) ends.
 * Every retry and repeat is its own attempt. Reporting problems never change the run's outcome.
 */
export class FastProofReporter {
  readonly session: ReporterSession;
  private rootDir = process.cwd();
  private readonly tests = new Map<string, TestCase>();
  private readonly pending = new Map<string, LogRecord[]>();
  private watch = false;

  constructor(options: VitestFastProofOptions = {}, out?: (m: string) => void) {
    this.session = new ReporterSession(options, 'vitest', 'vt', out);
  }

  onInit(vitest: Vitest): void {
    this.rootDir = vitest.config.root;
    this.watch = Boolean(vitest.config.watch);
  }

  onTestRunStart(): void {
    this.session.start();
  }

  private selected(tc: TestCase): boolean {
    const plan = this.session.plan;
    if (!plan) return true;
    const loc = tc.location;
    return inTestPlan(plan, { titlePath: this.titlePath(tc), line: loc?.line, column: loc?.column, annotations: this.annotationsOf(tc) });
  }

  private annotationsOf(tc: TestCase): { type: string; description?: string }[] {
    const id = (tc.meta() as TestMeta).fastproofId;
    return typeof id === 'string' && id ? [{ type: 'fastproof.id', description: id }] : [];
  }

  private file(tc: TestCase): string {
    return relative(this.rootDir, tc.module.moduleId).split(sep).join('/');
  }

  private describes(tc: TestCase): string[] {
    const out: string[] = [];
    for (let p: TestCase['parent'] | undefined = tc.parent; p && p.type === 'suite'; p = p.parent) out.unshift(p.name ?? '');
    return out;
  }

  private titlePath(tc: TestCase): string[] {
    return [this.file(tc), ...this.describes(tc), tc.name];
  }

  private identity(tc: TestCase): Partial<ReportResult> {
    const s = this.session;
    const meta = tc.meta() as TestMeta;
    const project = tc.project.name;
    const labels: Label[] = [{ name: 'framework', value: 'vitest' }];
    if (project) labels.push({ name: 'parentSuite', value: s.mask(project) });
    labels.push({ name: 'suite', value: s.mask(this.file(tc)) });
    const describes = this.describes(tc);
    if (describes.length) labels.push({ name: 'subSuite', value: s.mask(describes.join(FULL_NAME_SEPARATOR)) });
    for (const t of tc.tags ?? []) labels.push({ name: 'tag', value: s.mask(String(t)) });
    const extra = meta.fastproofLabels;
    if (extra && typeof extra === 'object' && !Array.isArray(extra)) {
      for (const [name, value] of Object.entries(extra as Record<string, unknown>).slice(0, MAX_META_LABELS))
        if (typeof value === 'string' || typeof value === 'number') labels.push({ name, value: s.mask(String(value)) });
    }
    const links: Link[] = [];
    if (Array.isArray(meta.fastproofLinks)) {
      for (const l of meta.fastproofLinks.slice(0, 50)) {
        const o = typeof l === 'string' ? { url: l } : (l as { url?: unknown; name?: unknown; type?: unknown });
        if (typeof o.url === 'string' && URL_RE.test(o.url)) {
          const link: Link = { url: s.mask(o.url) };
          if (typeof (o as { name?: unknown }).name === 'string') link.name = s.mask((o as { name: string }).name);
          if (typeof (o as { type?: unknown }).type === 'string') link.type = (o as { type: string }).type;
          links.push(link);
        }
      }
    }
    const parameters: Parameter[] = [];
    if (project) parameters.push({ name: 'project', value: s.mask(project) });
    const out: Partial<ReportResult> = {
      name: s.mask(tc.name),
      fullName: s.mask(this.titlePath(tc).join(FULL_NAME_SEPARATOR)),
      labels,
      parameters,
      source: { file: this.file(tc), ...(tc.location ? { line: tc.location.line } : {}) },
    };
    if (links.length) out.links = links;
    if (typeof meta.fastproofId === 'string' && meta.fastproofId) out.testCaseId = s.mask(meta.fastproofId);
    return out;
  }

  private retries(tc: TestCase): number {
    const o = tc.options as { retry?: unknown; repeats?: unknown };
    return Number(o.retry ?? 0) + Number(o.repeats ?? 0);
  }

  onTestCaseReady(tc: TestCase): void {
    if (!this.selected(tc)) return;
    this.session.guard((c) => {
      this.tests.set(tc.id, tc);
      const t = this.session.trace(tc.id, 0);
      c.openRunning({
        id: this.session.resultId(tc.id, 0),
        ...this.identity(tc),
        status: 'running',
        start: Date.now(),
        retry: 0,
        traceparent: t.traceparent,
      } as ReportResult);
      const early = this.pending.get(tc.id);
      if (early && !this.retries(tc)) {
        this.pending.delete(tc.id);
        for (const l of early) this.live(tc.id, 0, l);
      }
    });
  }

  onUserConsoleLog(log: { taskId?: string; type: 'stdout' | 'stderr'; content: string; time: number }): void {
    if (!log.taskId) return;
    this.session.guard(() => {
      const rec: LogRecord = { stream: log.type, content: log.content, time: log.time };
      const tc = this.tests.get(log.taskId!);
      if (tc && !this.retries(tc)) this.live(log.taskId!, 0, rec);
      else {
        const list = this.pending.get(log.taskId!) ?? [];
        list.push(rec);
        if (list.length <= 20_000) this.pending.set(log.taskId!, list);
      }
    });
  }

  private live(taskId: string, attempt: number, l: LogRecord): void {
    const key = `${taskId}\u0000${attempt}`;
    const kept = this.sent.get(key) ?? [];
    if (kept.length < 20_000) kept.push(l);
    this.sent.set(key, kept);
    this.session.client!.log(
      this.session.resultId(taskId, attempt),
      attempt,
      l.stream === 'stderr' ? 'warn' : 'info',
      l.stream,
      l.content.replace(/\n$/, ''),
      undefined,
      l.time,
    );
  }

  private attemptViews(tc: TestCase): AttemptView[] {
    const withAttempts = tc as { attempts?: () => readonly AttemptView[] };
    if (typeof withAttempts.attempts === 'function') {
      const a = withAttempts.attempts();
      if (a.length) return [...a];
    }
    const res = tc.result();
    if (res.state === 'pending') return [];
    const d = tc.diagnostic();
    if (res.state === 'skipped' && !d) return [];
    const errors = res.errors ?? [];
    const retried = Math.min(d?.retryCount ?? 0, 100);
    const out: AttemptView[] = [];
    const start = d?.startTime ?? Date.now();
    for (let i = 0; i < retried; i++)
      out.push({ state: 'failed', errors: errors[i] ? [errors[i]!] : undefined, duration: 0, startTime: start });
    const last = errors[retried] ?? (res.state === 'failed' ? errors[errors.length - 1] : undefined);
    out.push({
      state: res.state,
      errors: res.state === 'failed' && last ? [last] : undefined,
      duration: d?.duration ?? 0,
      startTime: start,
    });
    return out;
  }

  private status(a: AttemptView): Status {
    if (a.state === 'passed') return 'passed';
    if (a.state === 'skipped') return 'skipped';
    return a.errors && a.errors.length && !isAssertion(a.errors[0]) ? 'broken' : 'failed';
  }

  private error(e: Err | undefined): ReportError | undefined {
    if (!e) return undefined;
    const s = this.session;
    const out: ReportError = {};
    const msg = s.text(e.message, 64 * 1024);
    if (msg) out.message = msg;
    const trace = s.text(e.stack, MAX_OUTPUT);
    if (trace) out.trace = trace;
    if (e.actual !== undefined && e.expected !== undefined && isAssertion(e)) {
      const a = s.text(String(e.actual), 4096);
      const x = s.text(String(e.expected), 4096);
      if (a !== undefined) out.actual = a;
      if (x !== undefined) out.expected = x;
    }
    return out.message || out.trace ? out : undefined;
  }

  private files(tc: TestCase): AttachmentFile[] {
    const out: AttachmentFile[] = [];
    const seen = new Set<string>();
    type Att = { name?: string; contentType?: string; path?: string; body?: string | Uint8Array; bodyEncoding?: string };
    const add = (name: string, a: Att) => {
      const contentType = a.contentType || 'application/octet-stream';
      const body = a.body;
      let file: AttachmentFile | undefined;
      if (a.path && !URL_RE.test(a.path)) file = { name, contentType, path: resolve(this.rootDir, a.path) };
      else if (typeof body === 'string')
        file = { name, contentType, body: Buffer.from(body, a.bodyEncoding === 'utf-8' ? 'utf8' : 'base64') };
      else if (body) file = { name, contentType, body: Buffer.from(body) };
      if (!file) return;
      const key = `${name}\u0000${file.path ?? file.body!.toString('base64')}`;
      if (seen.has(key)) return;
      seen.add(key);
      out.push(file);
    };
    for (const an of tc.annotations()) if (an.attachment) add(an.message || an.type, an.attachment as Att);
    const withArtifacts = tc as { artifacts?: () => readonly { type: string; attachments?: Att[] }[] };
    if (typeof withArtifacts.artifacts === 'function') {
      for (const art of withArtifacts.artifacts()) {
        if (art.type === 'internal:annotation') continue;
        const base = art.type.replace(/^internal:/, '');
        const list = art.attachments ?? [];
        list.forEach((x, i) => add(x.name ?? (list.length > 1 ? `${base}-${i + 1}` : base), x));
      }
    }
    return out.slice(0, 100).map((f) => ({ ...f, name: this.session.mask(f.name) || 'attachment' }));
  }

  onTestCaseResult(tc: TestCase): void {
    if (!this.session.client || !this.selected(tc)) return;
    this.session.guard((c) => {
      this.tests.delete(tc.id);
      const views = this.attemptViews(tc);
      const logs = this.pending.get(tc.id) ?? [];
      this.pending.delete(tc.id);
      const identity = this.identity(tc);
      if (!views.length) {
        const now = Date.now();
        views.push({ state: 'skipped', errors: undefined, duration: 0, startTime: now });
      }
      const files = this.files(tc);
      const lastIndex = views.length - 1;
      views.forEach((v, i) => {
        const id = this.session.resultId(tc.id, i);
        const mine = this.logsFor(logs, views, i);
        for (const l of mine) this.live(tc.id, i, l);
        const status = this.status(v);
        const t = this.session.trace(tc.id, i);
        const duration = Math.max(0, Math.round(v.duration));
        const r: ReportResult = {
          id,
          ...identity,
          name: identity.name ?? tc.name,
          status,
          start: Math.round(v.startTime),
          stop: Math.round(v.startTime) + duration,
          durationMs: duration,
          retry: i,
          traceparent: t.traceparent,
        };
        const err = this.error(v.errors?.[0]);
        if (err) r.error = err;
        else if (status === 'failed' || status === 'broken') r.error = { message: 'the test failed without details' };
        const out = (stream: 'stdout' | 'stderr') => {
          const joined = this.sentLogs(tc.id, i)
            .filter((l) => l.stream === stream)
            .map((l) => l.content)
            .join('');
          return this.session.text(joined, MAX_OUTPUT);
        };
        const stdout = out('stdout');
        const stderr = out('stderr');
        if (stdout) r.stdout = stdout;
        if (stderr) r.stderr = stderr;
        c.finish(r, i === lastIndex ? files : [], status === 'passed' || status === 'skipped');
        this.sent.delete(`${tc.id}\u0000${i}`);
      });
    });
  }

  private sent = new Map<string, LogRecord[]>();

  private sentLogs(id: string, attempt: number): LogRecord[] {
    return this.sent.get(`${id}\u0000${attempt}`) ?? [];
  }

  private logsFor(all: LogRecord[], views: AttemptView[], index: number): LogRecord[] {
    if (!all.length) return [];
    const starts = views.map((v) => v.startTime);
    return all.filter((l) => {
      let owner = 0;
      for (let i = 0; i < starts.length; i++) if (starts[i]! <= l.time) owner = i;
      return owner === index;
    });
  }

  async onTestRunEnd(
    _modules: unknown,
    _errors: unknown,
    reason: 'passed' | 'interrupted' | 'failed',
  ): Promise<void> {
    if (this.watch) return;
    this.session.guard((c) => {
      for (const [id, tc] of this.tests) {
        const now = Date.now();
        c.finish(
          {
            id: this.session.resultId(id, 0),
            ...this.identity(tc),
            name: this.session.mask(tc.name),
            status: 'broken',
            start: now,
            stop: now,
            durationMs: 0,
            retry: 0,
            traceparent: this.session.trace(id, 0).traceparent,
            error: { message: 'the test did not finish (interrupted run)' },
          } as ReportResult,
          [],
          false,
        );
      }
      this.tests.clear();
    });
    await this.session.end(reason === 'interrupted' ? 'interrupted' : 'finished');
  }
}
