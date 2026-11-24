import { createHash, randomBytes } from 'node:crypto';

import { Client, cut } from './client';
import { type Config, type FastProofOptions, resolveConfig } from './config';
import { type TestPlan, loadTestPlan } from './testplan';
import { LAUNCH_ENV, SEED_ENV, type TraceContext, traceFor } from './trace';

/** Environment variable that hands the run id to worker processes, so they derive the same client ids. */
export const RUN_ENV = 'FASTPROOF_RUN_ID';

const ANSI = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;

export function plain(s: string): string {
  return s.replace(ANSI, '');
}

export function shortHash(s: string): string {
  return createHash('sha1').update(s).digest('hex').slice(0, 16);
}

/**
 * What a reporter for another JavaScript test runner needs around the transport `Client`: the resolved
 * configuration, a run id and trace seed shared with worker processes through the environment, client ids,
 * error guarding and the summary line. Hooks stay synchronous and never throw into the run.
 */
export class ReporterSession {
  readonly cfg: Config;
  readonly seed: string;
  readonly runId: string;
  readonly plan: TestPlan | undefined;
  client: Client | undefined;
  private ended = false;

  constructor(
    options: FastProofOptions & { testPlan?: string },
    readonly reporter: string,
    readonly idPrefix: string,
    readonly out: (m: string) => void = (m) => process.stderr.write(m + '\n'),
    private readonly env: Record<string, string | undefined> = process.env,
  ) {
    this.cfg = resolveConfig(options, env);
    this.seed = randomBytes(16).toString('hex');
    this.runId = randomBytes(6).toString('hex');
    this.plan = loadTestPlan(options.testPlan, env);
    if (this.cfg.enabled) {
      env[SEED_ENV] = this.seed;
      env[LAUNCH_ENV] = this.cfg.launch.externalId;
      env[RUN_ENV] = this.runId;
    }
  }

  get enabled(): boolean {
    return this.client !== undefined;
  }

  /** Starts the launch and the background sender. Returns false (with one notice) when reporting is off. */
  start(): boolean {
    if (this.client) return true;
    if (!this.cfg.enabled) {
      if (this.cfg.reason && this.cfg.reason !== 'disabled' && !this.cfg.quiet)
        this.out(`[fastproof] reporting is off: ${this.cfg.reason}`);
      return false;
    }
    try {
      this.client = new Client(this.cfg, this.reporter, this.out);
      this.client.start();
      return true;
    } catch (e) {
      this.out(`[fastproof] reporting is off: ${String(e)}`);
      this.client = undefined;
      return false;
    }
  }

  guard(fn: (client: Client) => void): void {
    const c = this.client;
    if (!c) return;
    try {
      fn(c);
    } catch (e) {
      c.warnOnce(`hook:${String(e).slice(0, 60)}`, `reporter error (ignored): ${String(e)}`);
    }
  }

  /** Client id of one attempt: unique per run and attempt, stable across HTTP retries and spool flushes. */
  resultId(testKey: string, attempt: number): string {
    return `${this.idPrefix}-${this.runId}-${shortHash(testKey)}-${attempt}`;
  }

  trace(testKey: string, attempt: number): TraceContext {
    return traceFor(this.seed, this.cfg.launch.externalId, testKey, attempt);
  }

  mask(s: string): string {
    return this.client ? this.client.masker.mask(s) : s;
  }

  /** Masked and cut text for a result field. */
  text(s: string | undefined, max: number): string | undefined {
    if (!s) return undefined;
    return cut(this.mask(plain(s)), max);
  }

  /** Drains the queue, spools what is left, finishes the launch when asked and prints the summary. Once. */
  async end(status: 'finished' | 'interrupted'): Promise<void> {
    if (!this.client || this.ended) return;
    this.ended = true;
    try {
      const s = await this.client.close(status);
      const where = s.launchNumber !== undefined ? `launch #${s.launchNumber}` : 'launch';
      if (s.spooled) {
        this.out(
          `[fastproof] ${s.sent} results sent, ${s.spooled} saved to ${s.spoolDir}` +
            (s.offlineReason ? ` (${s.offlineReason})` : ''),
        );
        if (this.cfg.uploadHint)
          this.out(
            `[fastproof] the next run sends them, or now: npx fastproof-playwright flush --spool ${this.cfg.spoolDir}` +
              ' (with FASTPROOF_URL, FASTPROOF_TOKEN and FASTPROOF_PROJECT set)',
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
