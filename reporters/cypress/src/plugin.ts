import { basename } from 'node:path';

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

const MAX_OUTPUT = 4 * 1024 * 1024;
const ID_MARKER = /\[(?:fastproof-id|allure-id):([^\]\s]{1,200})\]|@allure\.id[=:]([^\s\]]{1,200})/;
const MAX_LOGS = 2000;
const MAX_TASK_TEXT = 64 * 1024;

export interface CypressFastProofOptions extends FastProofOptions {
  /** Test plan file (Allure format); default `FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`. */
  testPlan?: string;
  /** Attach the spec's video: to failed attempts only (default), to every attempt of the spec, or never. */
  video?: 'on-failure' | 'always' | 'off';
}

interface Spec {
  relative?: string;
  name?: string;
  absolute?: string;
}

interface AttemptRaw {
  state?: string;
  error?: { name?: string; message?: string; stack?: string } | null;
  duration?: number;
  startedAt?: string | number;
  wallClockStartedAt?: string | number;
  wallClockDuration?: number;
  screenshots?: { path?: string; name?: string; takenAt?: string }[];
}

interface TestRaw {
  title: string[];
  state?: string;
  displayError?: string | null;
  duration?: number;
  attempts?: AttemptRaw[];
}

interface SpecResults {
  tests?: TestRaw[];
  video?: string | null;
  screenshots?: { path?: string; name?: string; takenAt?: string }[];
  stats?: { startedAt?: string };
  error?: string | null;
}

interface Shot {
  path: string;
  name: string;
  takenAt: number;
}

interface BrowserAttempt {
  id: string;
  start: number;
  stop?: number;
  state?: string;
  err?: { name?: string; message?: string; stack?: string };
}

interface CypressConfig {
  env?: Record<string, unknown>;
  specPattern?: string | string[];
  [k: string]: unknown;
}

type On = (event: string, handler: any) => void;

function time(v: string | number | undefined): number | undefined {
  if (v === undefined || v === null) return undefined;
  const n = typeof v === 'number' ? v : Date.parse(v);
  return Number.isFinite(n) ? n : undefined;
}

function str(v: unknown, max: number): string {
  return typeof v === 'string' ? v.slice(0, max) : '';
}

function titlePathOf(v: unknown): string[] {
  return Array.isArray(v) ? v.slice(0, 50).map((x) => str(x, 1000)) : [];
}

/**
 * Registers the FastProof plugin in `setupNodeEvents`. With `import '@franshub/fastproof-cypress/support'` in the
 * support file, tests are reported `running` when they start and get a `traceparent`; without it the results are
 * built from Cypress's own result at the end of each spec. Either way every retry is its own attempt.
 */
export class CypressFastProof {
  readonly session: ReporterSession;
  private readonly browser = new Map<string, BrowserAttempt>();
  private shots: Shot[] = [];
  private readonly planFiles: string[] | undefined;
  private readonly video: 'on-failure' | 'always' | 'off';
  private runMode = false;

  constructor(options: CypressFastProofOptions = {}, out?: (m: string) => void) {
    this.session = new ReporterSession(options, 'cypress', 'cy', out);
    this.video = options.video ?? 'on-failure';
    this.planFiles = this.filesOfPlan();
  }

  private filesOfPlan(): string[] | undefined {
    const plan = this.session.plan;
    if (!plan || !plan.tests.length) return undefined;
    const files = new Set<string>();
    for (const t of plan.tests) {
      const sel = t.selector?.trim();
      if (!sel) return undefined;
      const file = sel.includes(FULL_NAME_SEPARATOR) ? sel.split(FULL_NAME_SEPARATOR)[0] : sel.split('#')[0]!.replace(/:\d+(:\d+)?$/, '');
      if (!file) return undefined;
      files.add(file);
    }
    return [...files];
  }

  /** Hooks into Cypress; returns the configuration with what the support file needs. */
  register(on: On, config: CypressConfig): CypressConfig {
    if (this.planFiles) config.specPattern = this.planFiles;
    on('before:run', () => {
      this.runMode = true;
      this.session.start();
    });
    on('task', {
      'fastproof:begin': (a: unknown) => this.begin(a),
      'fastproof:end': (a: unknown) => this.end(a),
    });
    on('after:screenshot', (d: { path?: string; name?: string; takenAt?: string; specName?: string }) => {
      this.session.guard(() => {
        if (!d?.path) return;
        if (this.shots.length < 500) this.shots.push({ path: d.path, name: d.name ?? basename(d.path), takenAt: time(d.takenAt) ?? Date.now() });
      });
    });
    on('after:spec', (spec: Spec, results: SpecResults) => {
      this.session.start();
      this.session.guard(() => this.spec(spec, results));
    });
    on('after:run', async (results: { status?: string }) => {
      if (!this.runMode) return;
      await this.session.end(results?.status === 'failed' ? 'interrupted' : 'finished');
    });
    return config;
  }

  private specName(spec: Spec): string {
    return spec.relative ?? spec.name ?? '';
  }

  private selected(spec: string, titlePath: string[]): boolean {
    const plan = this.session.plan;
    if (!plan) return true;
    const marker = ID_MARKER.exec(titlePath[titlePath.length - 1] ?? '')?.slice(1).find(Boolean);
    return inTestPlan(plan, {
      titlePath: [spec, ...titlePath],
      annotations: marker ? [{ type: 'fastproof.id', description: marker }] : [],
    });
  }

  private testKey(spec: string, titlePath: string[]): string {
    return `${spec}\u0000${titlePath.join('\u0000')}`;
  }

  private identity(spec: string, titlePath: string[]): Partial<ReportResult> {
    const s = this.session;
    const title = titlePath[titlePath.length - 1] ?? '';
    const describes = titlePath.slice(0, -1);
    const labels: Label[] = [{ name: 'framework', value: 'cypress' }, { name: 'suite', value: s.mask(spec) }];
    if (describes.length) labels.push({ name: 'subSuite', value: s.mask(describes.join(FULL_NAME_SEPARATOR)) });
    const marker = ID_MARKER.exec(title)?.slice(1).find(Boolean);
    const out: Partial<ReportResult> = {
      name: s.mask(title),
      fullName: s.mask([spec, ...titlePath].join(FULL_NAME_SEPARATOR)),
      labels,
      parameters: [],
      source: { file: spec },
    };
    if (marker) out.testCaseId = s.mask(marker);
    return out;
  }

  private begin(a: unknown): unknown {
    const o = (a ?? {}) as { spec?: unknown; titlePath?: unknown; retry?: unknown; start?: unknown };
    const spec = str(o.spec, 1000);
    const titlePath = titlePathOf(o.titlePath);
    const retry = Math.max(0, Math.min(1000, Number(o.retry) || 0));
    if (!titlePath.length) return null;
    this.session.start();
    if (!this.selected(spec, titlePath)) return { skip: true, traceparent: '', traceId: '', spanId: '', baggage: '' };
    const key = this.testKey(spec, titlePath);
    const id = this.session.resultId(key, retry);
    const t = this.session.trace(key, retry);
    const start = time(o.start as number) ?? Date.now();
    this.session.guard((c) => {
      if (this.browser.size < 20_000) this.browser.set(`${key}\u0000${retry}`, { id, start });
      c.openRunning({ id, ...this.identity(spec, titlePath), status: 'running', start, retry, traceparent: t.traceparent } as ReportResult);
    });
    return { skip: false, traceparent: t.traceparent, traceId: t.traceId, spanId: t.spanId, baggage: t.baggage };
  }

  private end(a: unknown): unknown {
    const o = (a ?? {}) as {
      spec?: unknown;
      titlePath?: unknown;
      retry?: unknown;
      state?: unknown;
      err?: { name?: unknown; message?: unknown; stack?: unknown };
      logs?: unknown;
      stop?: unknown;
    };
    const spec = str(o.spec, 1000);
    const titlePath = titlePathOf(o.titlePath);
    const retry = Math.max(0, Math.min(1000, Number(o.retry) || 0));
    if (!titlePath.length) return null;
    this.session.guard((c) => {
      const key = this.testKey(spec, titlePath);
      const b = this.browser.get(`${key}\u0000${retry}`);
      if (!b) return;
      b.stop = time(o.stop as number) ?? Date.now();
      b.state = str(o.state, 20);
      if (o.err) b.err = { name: str(o.err.name, 200), message: str(o.err.message, MAX_TASK_TEXT), stack: str(o.err.stack, MAX_OUTPUT) };
      const logs = Array.isArray(o.logs) ? o.logs.slice(0, MAX_LOGS) : [];
      for (const l of logs as { t?: unknown; m?: unknown; l?: unknown }[]) {
        const m = str(l?.m, 4000);
        if (m) c.log(b.id, retry, l.l === 'error' ? 'error' : 'info', 'step', m, undefined, time(l.t as number) ?? Date.now());
      }
    });
    return null;
  }

  private status(a: AttemptRaw | undefined, b: BrowserAttempt | undefined): Status {
    const state = a?.state ?? b?.state ?? 'unknown';
    if (state === 'passed') return 'passed';
    if (state === 'pending' || state === 'skipped') return 'skipped';
    if (state === 'failed') {
      const e = a?.error ?? b?.err;
      const name = e?.name ?? '';
      return name && name !== 'AssertionError' && !/Assertion/.test(name) ? 'broken' : 'failed';
    }
    return 'unknown';
  }

  private error(a: AttemptRaw | undefined, b: BrowserAttempt | undefined, display: string | null | undefined): ReportError | undefined {
    const s = this.session;
    const e = a?.error ?? b?.err;
    const out: ReportError = {};
    const m = s.text(e?.message ?? display ?? undefined, MAX_TASK_TEXT);
    const t = s.text(e?.stack, MAX_OUTPUT);
    if (m) out.message = m;
    if (t) out.trace = t;
    return out.message || out.trace ? out : undefined;
  }

  private spec(spec: Spec, results: SpecResults): void {
    const specName = this.specName(spec);
    const c = this.session.client;
    if (!c) return;
    const tests = Array.isArray(results?.tests) ? results.tests : [];
    const shots: Shot[] = this.shots;
    this.shots = [];
    for (const s of results?.screenshots ?? [])
      if (s.path && !shots.some((x) => x.path === s.path)) shots.push({ path: s.path, name: s.name ?? basename(s.path), takenAt: time(s.takenAt) ?? 0 });
    const specStart = time(results?.stats?.startedAt) ?? Date.now();
    let cursor = specStart;
    const prepared: { key: string; titlePath: string[]; attempts: { start: number; stop: number; raw: AttemptRaw | undefined; b: BrowserAttempt | undefined }[]; t: TestRaw }[] = [];
    for (const t of tests) {
      const titlePath = titlePathOf(t.title);
      if (!titlePath.length || !this.selected(specName, titlePath)) continue;
      const key = this.testKey(specName, titlePath);
      const raws = t.attempts && t.attempts.length ? t.attempts : [{ state: t.state, duration: t.duration } as AttemptRaw];
      const attempts = raws.slice(0, 1000).map((raw, i) => {
        const b = this.browser.get(`${key}\u0000${i}`);
        const duration = Math.max(0, Math.round(raw.wallClockDuration ?? raw.duration ?? (b?.stop && b.start ? b.stop - b.start : 0)));
        const start = time(raw.wallClockStartedAt ?? raw.startedAt) ?? b?.start ?? cursor;
        cursor = start + duration;
        return { start, stop: start + duration, raw, b };
      });
      prepared.push({ key, titlePath, attempts, t });
    }
    const all = prepared.flatMap((p) => p.attempts.map((a) => a.start)).sort((x, y) => x - y);
    const owner = (shot: Shot): number | undefined => {
      let best: number | undefined;
      for (const s of all) if (s <= shot.takenAt) best = s;
      return best;
    };
    for (const p of prepared) {
      const last = p.attempts.length - 1;
      p.attempts.forEach((a, i) => {
        const id = a.b?.id ?? this.session.resultId(p.key, i);
        const status = this.status(a.raw, a.b);
        const t = this.session.trace(p.key, i);
        const r: ReportResult = {
          id,
          ...this.identity(specName, p.titlePath),
          name: this.session.mask(p.titlePath[p.titlePath.length - 1] ?? ''),
          status,
          start: a.start,
          stop: a.stop,
          durationMs: a.stop - a.start,
          retry: i,
          traceparent: t.traceparent,
        };
        const err = this.error(a.raw, a.b, i === last ? p.t.displayError : undefined);
        if (err) r.error = err;
        else if (status === 'failed' || status === 'broken') r.error = { message: 'the test failed without details' };
        const files: AttachmentFile[] = [];
        for (const sh of shots) if (owner(sh) === a.start) files.push({ name: this.session.mask(sh.name), contentType: 'image/png', path: sh.path });
        for (const sh of a.raw?.screenshots ?? [])
          if (sh.path && !files.some((f) => f.path === sh.path)) files.push({ name: this.session.mask(sh.name ?? basename(sh.path)), contentType: 'image/png', path: sh.path });
        const video = results?.video;
        if (video && this.video !== 'off' && (this.video === 'always' || status === 'failed' || status === 'broken'))
          files.push({ name: 'video.mp4', contentType: 'video/mp4', path: video });
        c.finish(r, files.slice(0, 100), status === 'passed' || status === 'skipped');
        this.browser.delete(`${p.key}\u0000${i}`);
      });
    }
    if (results?.error && !tests.length) this.specError(specName, results.error);
  }

  private specError(specName: string, message: string): void {
    const c = this.session.client;
    if (!c) return;
    const now = Date.now();
    const key = this.testKey(specName, ['suite failed to run']);
    c.finish(
      {
        id: this.session.resultId(key, 0),
        name: this.session.mask(`${specName} (suite failed to run)`),
        fullName: this.session.mask(`${specName}${FULL_NAME_SEPARATOR}suite failed to run`),
        status: 'broken',
        start: now,
        stop: now,
        durationMs: 0,
        retry: 0,
        labels: [{ name: 'framework', value: 'cypress' }, { name: 'suite', value: this.session.mask(specName) }],
        error: { message: this.session.text(message, MAX_TASK_TEXT) ?? 'the spec failed to run' },
        traceparent: this.session.trace(key, 0).traceparent,
      },
      [],
      false,
    );
  }
}

/** `setupNodeEvents(on, config) { return fastproof(on, config, { ...options }); }` */
export function fastproof(on: On, config: CypressConfig, options: CypressFastProofOptions = {}): CypressConfig {
  return new CypressFastProof(options).register(on, config);
}
