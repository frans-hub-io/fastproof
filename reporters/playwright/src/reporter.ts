import { randomBytes } from 'node:crypto';
import { extname, relative, sep } from 'node:path';

import type {
  FullConfig,
  FullResult,
  Reporter,
  Suite,
  TestCase,
  TestError,
  TestResult,
  TestStep,
} from '@playwright/test/reporter';

import { Client, cut } from './client';
import { type Config, type FastProofOptions, resolveConfig } from './config';
import { FULL_NAME_SEPARATOR, ID_ANNOTATIONS, fullNameOf } from './testplan';
import { LAUNCH_ENV, SEED_ENV, TRACE_ANNOTATION, parseTraceparent, traceFor } from './trace';
import type { AttachmentFile, Label, Link, Parameter, ReportError, ReportResult, ReportStep, Status } from './types';

/** Step categories kept in the result's step tree; only `test.step` is streamed live. */
const TREE_CATEGORIES = new Set(['test.step', 'hook', 'expect', 'pw:api']);
const MAX_STEPS = 2000;
const MAX_OUTPUT = 4 * 1024 * 1024;
const URL_RE = /^https?:\/\//;

// eslint-disable-next-line no-control-regex
const ANSI = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;

export function stripAnsi(s: string): string {
  return s.replace(ANSI, '');
}

export function statusOf(test: Pick<TestCase, 'expectedStatus'>, result: Pick<TestResult, 'status'>): Status {
  const expectFail = test.expectedStatus === 'failed';
  switch (result.status) {
    case 'passed':
      return expectFail ? 'failed' : 'passed';
    case 'failed':
    case 'timedOut':
      return expectFail ? 'passed' : 'failed';
    case 'skipped':
      return 'skipped';
    case 'interrupted':
      return 'broken';
    default:
      return 'unknown';
  }
}

type Annotation = { type: string; description?: string };

/**
 * FastProof reporter for Playwright. Hooks only queue work; uploads run in the background and `onEnd`
 * drains them within `flushTimeoutMs`. Reporting problems never change the run's outcome.
 */
export class FastProofReporter implements Reporter {
  readonly cfg: Config;
  private client: Client | undefined;
  private readonly seed: string;
  private readonly runId: string;
  private rootDir = process.cwd();
  private readonly out: (m: string) => void;

  constructor(options: FastProofOptions = {}, out?: (m: string) => void) {
    this.cfg = resolveConfig(options);
    this.out = out ?? ((m) => process.stderr.write(m + '\n'));
    this.seed = randomBytes(16).toString('hex');
    this.runId = randomBytes(6).toString('hex');
    if (this.cfg.enabled) {
      process.env[SEED_ENV] = this.seed;
      process.env[LAUNCH_ENV] = this.cfg.launch.externalId;
    }
  }

  printsToStdio(): boolean {
    return false;
  }

  onBegin(config: FullConfig, _suite: Suite): void {
    this.rootDir = config.rootDir;
    if (!this.cfg.enabled) {
      if (this.cfg.reason && this.cfg.reason !== 'disabled' && !this.cfg.quiet)
        this.out(`[fastproof] reporting is off: ${this.cfg.reason}`);
      return;
    }
    try {
      this.client = new Client(this.cfg, 'playwright', this.out);
      this.client.start();
    } catch (e) {
      this.out(`[fastproof] reporting is off: ${String(e)}`);
      this.client = undefined;
    }
  }

  private guard(fn: () => void): void {
    if (!this.client) return;
    try {
      fn();
    } catch (e) {
      this.client.warnOnce(`hook:${String(e).slice(0, 60)}`, `reporter error (ignored): ${String(e)}`);
    }
  }

  /** Client id of one attempt: unique per run, stable across HTTP retries and spool flushes. */
  resultId(test: TestCase, retry: number): string {
    return `pw-${this.runId}-${test.id}-${retry}`;
  }

  private annotations(test: TestCase, result: TestResult): Annotation[] {
    const r = (result as { annotations?: Annotation[] }).annotations;
    return r && r.length ? r : test.annotations;
  }

  private base(test: TestCase, result: TestResult): ReportResult {
    const m = this.client!.masker;
    const [, project = '', , ...rest] = test.titlePath();
    const titlePath = test.titlePath().slice(2);
    const describes = rest.slice(0, -1).filter(Boolean);
    const file = titlePath[0] ?? '';
    const annotations = this.annotations(test, result);
    const labels: Label[] = [{ name: 'framework', value: 'playwright' }];
    if (project) labels.push({ name: 'parentSuite', value: project });
    labels.push({ name: 'suite', value: file });
    if (describes.length) labels.push({ name: 'subSuite', value: describes.join(FULL_NAME_SEPARATOR) });
    if (project) labels.push({ name: 'project', value: project });
    for (const t of test.tags) labels.push({ name: 'tag', value: t.replace(/^@/, '') });
    const links: Link[] = [];
    let testCaseId: string | undefined;
    for (const a of annotations) {
      if (a.type === TRACE_ANNOTATION) continue;
      const d = m.mask(a.description ?? '');
      if (ID_ANNOTATIONS.includes(a.type) && d) testCaseId = d;
      if (URL_RE.test(d) && ['issue', 'tms', 'link', 'fail', 'fixme'].includes(a.type))
        links.push({ url: d, type: a.type === 'tms' || a.type === 'link' ? a.type : 'issue' });
      labels.push({ name: a.type, value: d });
    }
    if (result.workerIndex >= 0) labels.push({ name: 'thread', value: `worker-${result.workerIndex}` });
    const parameters: Parameter[] = [];
    if (project) parameters.push({ name: 'project', value: m.mask(project) });
    const repeat = (test.parent.project() as { repeatEach?: number } | undefined)?.repeatEach ?? 1;
    if (repeat > 1) parameters.push({ name: 'repeat', value: String(test.repeatEachIndex) });
    const r: ReportResult = {
      id: this.resultId(test, result.retry),
      name: m.mask(test.title),
      fullName: m.mask(fullNameOf(titlePath)),
      status: 'running',
      start: result.startTime.getTime(),
      retry: result.retry,
      labels,
      parameters,
      traceparent: this.traceparent(test, result, annotations),
      source: {
        file: relative(this.rootDir, test.location.file).split(sep).join('/'),
        line: test.location.line,
      },
    };
    if (links.length) r.links = links;
    if (testCaseId) r.testCaseId = testCaseId;
    return r;
  }

  private traceparent(test: TestCase, result: TestResult, annotations: Annotation[]): string {
    for (let i = annotations.length - 1; i >= 0; i--) {
      const a = annotations[i]!;
      if (a.type === TRACE_ANNOTATION && parseTraceparent(a.description)) return a.description!;
    }
    return traceFor(this.seed, this.cfg.launch.externalId, test.id, result.retry).traceparent;
  }

  onTestBegin(test: TestCase, result: TestResult): void {
    this.guard(() => this.client!.openRunning(this.base(test, result)));
  }

  private stepPath(step: TestStep): string {
    const names: string[] = [];
    for (let s: TestStep | undefined = step; s; s = s.parent) if (s.category === 'test.step') names.unshift(s.title);
    return names.join(FULL_NAME_SEPARATOR);
  }

  onStepBegin(test: TestCase, result: TestResult, step: TestStep): void {
    if (step.category !== 'test.step') return;
    this.guard(() =>
      this.client!.log(this.resultId(test, result.retry), result.retry, 'info', 'step', `▶ ${step.title}`, this.stepPath(step)),
    );
  }

  onStepEnd(test: TestCase, result: TestResult, step: TestStep): void {
    if (step.category !== 'test.step' && !(step.category === 'expect' && step.error)) return;
    this.guard(() => {
      const failed = Boolean(step.error);
      const msg = failed
        ? `✘ ${step.title}: ${stripAnsi(step.error?.message ?? '').split('\n')[0]}`
        : `✔ ${step.title} (${Math.round(step.duration)} ms)`;
      this.client!.log(this.resultId(test, result.retry), result.retry, failed ? 'error' : 'info', 'step', msg, this.stepPath(step));
    });
  }

  onStdOut(chunk: string | Buffer, test?: TestCase, result?: TestResult): void {
    if (!test || !result) return;
    this.guard(() =>
      this.client!.log(this.resultId(test, result.retry), result.retry, 'info', 'stdout', String(chunk).replace(/\n$/, '')),
    );
  }

  onStdErr(chunk: string | Buffer, test?: TestCase, result?: TestResult): void {
    if (!test || !result) return;
    this.guard(() =>
      this.client!.log(this.resultId(test, result.retry), result.retry, 'warn', 'stderr', String(chunk).replace(/\n$/, '')),
    );
  }

  private error(e: TestError | undefined): ReportError | undefined {
    if (!e) return undefined;
    const m = this.client!.masker;
    const out: ReportError = {};
    if (e.message) out.message = m.mask(stripAnsi(e.message));
    if (e.stack) out.trace = m.mask(stripAnsi(e.stack));
    else if (e.value) out.message = m.mask(stripAnsi(e.value));
    return out;
  }

  private steps(steps: TestStep[], budget: { n: number }): ReportStep[] {
    const m = this.client!.masker;
    const out: ReportStep[] = [];
    for (const s of steps) {
      if (budget.n >= MAX_STEPS) break;
      if (!TREE_CATEGORIES.has(s.category)) {
        out.push(...this.steps(s.steps, budget));
        continue;
      }
      budget.n++;
      const start = s.startTime.getTime();
      const step: ReportStep = {
        name: m.mask(s.title),
        status: s.error ? 'failed' : 'passed',
        start,
        stop: start + Math.max(0, Math.round(s.duration)),
        durationMs: Math.max(0, Math.round(s.duration)),
      };
      const err = this.error(s.error);
      if (err) step.error = err;
      const children = this.steps(s.steps, budget);
      if (children.length) step.steps = children;
      out.push(step);
    }
    return out;
  }

  private output(chunks: (string | Buffer)[]): string | undefined {
    if (!chunks.length) return undefined;
    return cut(this.client!.masker.mask(chunks.map(String).join('')), MAX_OUTPUT);
  }

  /** Builds the final result of an attempt and the files to upload with it. */
  buildFinal(test: TestCase, result: TestResult): { result: ReportResult; files: AttachmentFile[] } {
    const m = this.client!.masker;
    const r = this.base(test, result);
    r.status = statusOf(test, result);
    const duration = Math.max(0, Math.round(result.duration));
    r.durationMs = duration;
    r.stop = (r.start ?? Date.now()) + duration;
    const err = this.error(result.errors[0] ?? result.error);
    if (err) r.error = err;
    const steps = this.steps(result.steps, { n: 0 });
    if (steps.length) r.steps = steps;
    const stdout = this.output(result.stdout);
    const stderr = this.output(result.stderr);
    if (stdout) r.stdout = stdout;
    if (stderr) r.stderr = stderr;
    const files: AttachmentFile[] = [];
    if (this.cfg.attachments) {
      for (const a of result.attachments) {
        let name = m.mask(a.name) || 'attachment';
        if (a.path && !/\.[A-Za-z0-9]{1,8}$/.test(name) && extname(a.path)) name += extname(a.path);
        const contentType = a.contentType || 'application/octet-stream';
        if (a.path) files.push({ name, contentType, path: a.path });
        else if (a.body) files.push({ name, contentType, body: a.body });
      }
    }
    return { result: r, files };
  }

  onTestEnd(test: TestCase, result: TestResult): void {
    this.guard(() => {
      const { result: r, files } = this.buildFinal(test, result);
      this.client!.finish(r, files, r.status === 'passed' || r.status === 'skipped');
    });
  }

  async onEnd(result: FullResult): Promise<void> {
    if (!this.client) return;
    try {
      const s = await this.client.close(result.status === 'interrupted' ? 'interrupted' : 'finished');
      const where = s.launchNumber !== undefined ? `launch #${s.launchNumber}` : 'launch';
      if (s.spooled) {
        this.out(
          `[fastproof] ${s.sent} results sent, ${s.spooled} saved to ${s.spoolDir}` +
            (s.offlineReason ? ` (${s.offlineReason})` : ''),
        );
        if (this.cfg.uploadHint)
          this.out(
            `[fastproof] the next run sends them, or now: npx fastproof-playwright flush --spool ${this.cfg.spoolDir}` +
              ' (with FASTPROOF_URL, FASTPROOF_TOKEN and FASTPROOF_PROJECT set); a Playwright JSON report can also be' +
              ' sent with `fastproof upload <report.json> --format playwright`',
          );
      } else if (!this.cfg.quiet) {
        this.out(`[fastproof] ${where}: ${s.sent} results, ${s.attachments} attachments sent`);
      }
      if (s.lost) this.out(`[fastproof] ${s.lost} results could not be reported`);
    } catch (e) {
      this.out(`[fastproof] reporting error (ignored): ${String(e)}`);
    }
  }
}
