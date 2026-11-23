import { randomBytes } from 'node:crypto';
import { extname, resolve } from 'node:path';

import {
  Client,
  LAUNCH_ENV,
  SEED_ENV,
  resolveConfig,
  traceFor,
  type ClientSummary,
  type Config,
  type FastProofOptions,
  type TraceContext,
} from '@franshub/fastproof-playwright';
import type { AttachmentFile, Label, Link, Parameter, ReportError, ReportResult, ReportStep, Status } from '@franshub/fastproof-playwright';
import type { Attachment, StepResult, TestResult } from 'allure-js-commons';
import type { LifecycleListener } from 'allure-js-commons/sdk/reporter';

const MAX_STEPS = 2000;
const MAX_DEPTH = 32;
const MAX_LABELS = 200;
const MAX_TEXT = 4 * 1024 * 1024;
export const TRACE_LABEL = 'traceparent';
const TRACE_CONTENT_TYPE = 'application/vnd.allure.playwright-trace';

export interface AllureFastProofOptions {
  /** FastProof reporter options (URL, token, project, launch, secrets, spool); the environment fills the rest. */
  fastproof?: FastProofOptions;
  /** Directory of the Allure writer (default `allure-results`, the Allure default); attachments are read from it. */
  resultsDir?: string;
  /** Add a `traceparent` label to the Allure result (default true). */
  traceLabel?: boolean;
}

/** Where the adapter is in the run when it calls the listener; Playwright knows it, other adapters do not. */
export interface Hint {
  testId: string;
  retry: number;
}

interface Attempt {
  uuid: string;
  id: string;
  retry: number;
  testId: string | undefined;
  trace: TraceContext;
  start: number;
  done: boolean;
}

const STATUSES = new Set<Status>(['passed', 'failed', 'broken', 'skipped']);

function statusOf(s: string | undefined): Status {
  return s && STATUSES.has(s as Status) ? (s as Status) : 'unknown';
}

function cut(s: string, max: number): string {
  return s.length <= max ? s : s.slice(0, max) + '\n…[truncated]';
}

/**
 * Mirrors Allure results to FastProof. `listener` plugs into the `listeners` option of an Allure adapter
 * (`allure-playwright`, or any adapter built on `allure-js-commons`); the adapter keeps writing allure-results.
 * Listener errors are caught by Allure; reporting problems never reach the tests.
 */
export class FastProofAllure {
  readonly cfg: Config;
  readonly client: Client | undefined;
  readonly listener: LifecycleListener;
  readonly seed = randomBytes(16).toString('hex');
  private readonly runId = randomBytes(6).toString('hex');
  private readonly resultsDir: string;
  private readonly traceLabel: boolean;
  private readonly attempts = new Map<string, Attempt>();
  private readonly counts = new Map<string, number>();
  private hinted: Hint | undefined;
  private started = false;
  private missing = 0;

  constructor(
    options: AllureFastProofOptions = {},
    private readonly out: (m: string) => void = (m) => process.stderr.write(m + '\n'),
  ) {
    this.cfg = resolveConfig(options.fastproof ?? {});
    this.resultsDir = resolve(options.resultsDir ?? process.env.ALLURE_RESULTS_DIR ?? 'allure-results');
    this.traceLabel = options.traceLabel ?? !['0', 'false', 'no', 'off'].includes((process.env.FASTPROOF_ALLURE_TRACE_LABEL ?? '').toLowerCase());
    if (this.cfg.enabled) {
      process.env[SEED_ENV] = this.seed;
      process.env[LAUNCH_ENV] = this.cfg.launch.externalId;
      this.client = new Client(this.cfg, 'allure-js', this.out);
    } else if (this.cfg.reason && this.cfg.reason !== 'disabled' && !this.cfg.quiet) {
      this.out(`[fastproof] reporting is off: ${this.cfg.reason}`);
    }
    this.listener = {
      afterTestResultStart: (r) => this.guard(() => this.begin(r)),
      afterStepStop: (s) => this.guard(() => this.stepStopped(s)),
      afterTestResultWrite: (r) => this.guard(() => this.written(r)),
    };
  }

  private guard(fn: () => void): void {
    if (!this.client) return;
    try {
      fn();
    } catch (e) {
      this.client.warnOnce(`hook:${String(e).slice(0, 60)}`, `reporter error (ignored): ${String(e)}`);
    }
  }

  /** Starts the launch and the background sender. */
  start(): void {
    if (this.started || !this.client) return;
    this.started = true;
    this.client.start();
  }

  /** Runs `fn` (an adapter call that fires listener events synchronously) knowing which test it is about. */
  hint<T>(h: Hint, fn: () => T): T {
    this.hinted = h;
    try {
      return fn();
    } finally {
      this.hinted = undefined;
    }
  }

  private mask(s: string): string {
    return this.client!.masker.mask(s);
  }

  private attemptFor(): Attempt | undefined {
    const h = this.hinted;
    const open = [...this.attempts.values()].filter((a) => !a.done);
    if (h) return open.find((a) => a.testId === h.testId && a.retry === h.retry);
    return open.length === 1 ? open[0] : undefined;
  }

  private identity(r: TestResult): Partial<ReportResult> {
    const m = (s: string) => this.mask(s);
    const out: Partial<ReportResult> = { name: m(cut(r.name ?? r.fullName ?? 'test', 1000)) };
    if (r.fullName) out.fullName = m(r.fullName);
    if (r.testCaseId) out.testCaseId = r.testCaseId;
    const params = (r.parameters ?? []).map((p) => this.parameter(p));
    out.parameters = params;
    return out;
  }

  private parameter(p: Parameter): Parameter {
    const out: Parameter = { name: this.mask(p.name) };
    if (p.mode === 'masked' || p.mode === 'hidden') {
      out.mode = p.mode;
      out.value = '****';
    } else if (p.value !== undefined && p.value !== null) out.value = this.mask(cut(String(p.value), 1000));
    if (p.excluded) out.excluded = true;
    return out;
  }

  private begin(r: TestResult): void {
    const client = this.client!;
    const h = this.hinted;
    const key = `${r.fullName ?? r.name}\u0000${r.testCaseId ?? ''}\u0000${(r.parameters ?? [])
      .filter((p) => !p.excluded && p.name !== 'Retry')
      .map((p) => `${p.name}=${p.value}`)
      .sort()
      .join('&')}`;
    const counted = this.counts.get(key) ?? 0;
    this.counts.set(key, counted + 1);
    const retry = h?.retry ?? counted;
    const trace = traceFor(h ? this.seed : undefined, this.cfg.launch.externalId, h?.testId ?? r.uuid, retry);
    const a: Attempt = {
      uuid: r.uuid,
      id: `aj-${this.runId}-${r.uuid}`,
      retry,
      testId: h?.testId,
      trace,
      start: r.start ?? Date.now(),
      done: false,
    };
    this.attempts.set(r.uuid, a);
    if (this.traceLabel && !r.labels.some((l) => l.name === TRACE_LABEL)) r.labels.push({ name: TRACE_LABEL, value: trace.traceparent });
    client.openRunning({ id: a.id, ...this.identity(r), status: 'running', start: a.start, retry } as ReportResult);
  }

  private stepStopped(s: StepResult): void {
    const a = this.attemptFor();
    if (!a || !s.name) return;
    const failed = s.status === 'failed' || s.status === 'broken';
    const ms = s.start && s.stop && s.stop >= s.start ? ` (${s.stop - s.start} ms)` : '';
    const msg = failed ? `✘ ${s.name}${s.statusDetails?.message ? `: ${s.statusDetails.message.split('\n')[0]}` : ''}` : `✔ ${s.name}${ms}`;
    this.client!.log(a.id, a.retry, failed ? 'error' : 'info', 'step', msg, s.name);
  }

  private written(r: TestResult): void {
    const client = this.client!;
    let a = this.attempts.get(r.uuid);
    if (!a) {
      const n = this.counts.get(`late:${r.fullName ?? r.name}`) ?? 0;
      this.counts.set(`late:${r.fullName ?? r.name}`, n + 1);
      a = { uuid: r.uuid, id: `aj-${this.runId}-${r.uuid}`, retry: n, testId: undefined, trace: traceFor(undefined, this.cfg.launch.externalId, r.uuid, n), start: r.start ?? Date.now(), done: false };
      this.missing++;
    }
    a.done = true;
    this.attempts.delete(r.uuid);
    const files: AttachmentFile[] = [];
    const seen = new Set<string>();
    const walk = (atts: Attachment[] | undefined) => {
      for (const at of atts ?? []) {
        if (!at.source || seen.has(at.source) || at.source.includes('/') || at.source.includes('\\') || at.source.includes('..')) continue;
        seen.add(at.source);
        files.push(this.file(at));
      }
    };
    walk(r.attachments);
    const budget = { steps: 0 };
    const steps = (r.steps ?? []).map((s) => this.step(s, 1, budget, walk)).filter((s): s is ReportStep => s !== undefined);
    const res: ReportResult = {
      id: a.id,
      name: '',
      ...this.identity(r),
      status: statusOf(r.status),
      retry: a.retry,
      traceparent: a.trace.traceparent,
      labels: this.labels(r.labels),
    };
    if (r.description) res.description = this.mask(cut(r.description, 64 * 1024));
    const start = r.start ?? a.start;
    res.start = start;
    res.stop = Math.max(r.stop ?? Date.now(), start);
    res.durationMs = res.stop - start;
    const links: Link[] = (r.links ?? []).filter((l) => l.url).map((l) => ({ url: l.url, ...(l.name ? { name: this.mask(l.name) } : {}), ...(l.type ? { type: l.type } : {}) }));
    if (links.length) res.links = links;
    const err = this.error(r.statusDetails);
    if (err) res.error = err;
    else if (res.status === 'failed' || res.status === 'broken') res.error = { message: 'the test failed without details' };
    if (steps.length) res.steps = steps;
    client.finish(res, files, res.status === 'passed' || res.status === 'skipped');
  }

  private labels(labels: Label[] | undefined): Label[] {
    return (labels ?? [])
      .filter((l) => l.name && !l.name.startsWith('_') && l.value !== undefined)
      .slice(0, MAX_LABELS)
      .map((l) => ({ name: l.name, value: this.mask(cut(String(l.value), 1000)) }));
  }

  private error(d: { message?: string; trace?: string; actual?: string; expected?: string } | undefined): ReportError | undefined {
    if (!d || (!d.message && !d.trace)) return undefined;
    const e: ReportError = {};
    if (d.message) e.message = this.mask(cut(d.message, 64 * 1024));
    if (d.trace) e.trace = this.mask(cut(d.trace, MAX_TEXT));
    return e;
  }

  private step(s: StepResult, depth: number, b: { steps: number }, walk: (a: Attachment[] | undefined) => void): ReportStep | undefined {
    if (b.steps >= MAX_STEPS || depth > MAX_DEPTH) return undefined;
    b.steps++;
    const node: ReportStep = { name: this.mask(cut(s.name ?? 'step', 1000)), status: statusOf(s.status) };
    if (s.start) node.start = s.start;
    if (s.stop) node.stop = s.stop;
    if (s.start && s.stop && s.stop >= s.start) node.durationMs = s.stop - s.start;
    const err = this.error(s.statusDetails);
    if (err) node.error = err;
    if (s.parameters?.length) node.parameters = s.parameters.map((p) => this.parameter(p));
    walk(s.attachments);
    const kids = (s.steps ?? []).map((c) => this.step(c, depth + 1, b, walk)).filter((c): c is ReportStep => c !== undefined);
    if (kids.length) node.steps = kids;
    return node;
  }

  private file(at: Attachment): AttachmentFile {
    const ext = extname(at.source);
    const trace = at.type === TRACE_CONTENT_TYPE;
    let name = trace ? 'trace.zip' : at.name || at.source;
    if (!trace && !extname(name) && ext) name += ext;
    return {
      name: this.mask(name),
      contentType: trace ? 'application/zip' : at.type || 'application/octet-stream',
      path: resolve(this.resultsDir, at.source),
    };
  }

  /** Sends what is still running as interrupted, drains the queue and finishes the launch (when asked). */
  async close(status: 'finished' | 'interrupted' = 'finished'): Promise<ClientSummary | undefined> {
    const client = this.client;
    if (!client) return undefined;
    for (const a of this.attempts.values()) {
      const stop = Date.now();
      client.finish(
        {
          id: a.id,
          name: 'unfinished test',
          status: 'broken',
          start: a.start,
          stop,
          durationMs: stop - a.start,
          retry: a.retry,
          traceparent: a.trace.traceparent,
          error: { message: 'the test did not finish (interrupted run)' },
        },
        [],
        false,
      );
    }
    this.attempts.clear();
    const s = await client.close(status);
    if (s.spooled) {
      this.out(`[fastproof] ${s.sent} results sent, ${s.spooled} saved to ${s.spoolDir}${s.offlineReason ? ` (${s.offlineReason})` : ''}`);
      if (this.cfg.uploadHint)
        this.out(`[fastproof] the next run sends them, or now: npx fastproof-playwright flush --spool ${this.cfg.spoolDir}`);
    } else if (!this.cfg.quiet) {
      const where = s.launchNumber !== undefined ? `launch #${s.launchNumber}` : 'launch';
      this.out(`[fastproof] ${where}: ${s.sent} results, ${s.attachments} attachments sent`);
    }
    if (s.lost) this.out(`[fastproof] ${s.lost} results could not be reported`);
    return s;
  }
}
