import { test as base, type TestInfo } from '@playwright/test';

import { inTestPlan, loadTestPlan, type TestPlan } from './testplan';
import {
  LAUNCH_ENV,
  SEED_ENV,
  TRACE_ANNOTATION,
  formatTraceparent,
  randomSpanId,
  traceFor,
  type TraceContext,
} from './trace';

/** Trace context of the current attempt, for FastMock and any traced backend. */
export interface FastProofTrace extends TraceContext {
  /** `traceparent` and `baggage`; `newSpan` makes a fresh span id under the same trace (one per request). */
  headers(opts?: { newSpan?: boolean }): Record<string, string>;
  /** A copy of the environment with `TRACEPARENT` and `BAGGAGE`, for child processes. */
  env(from?: NodeJS.ProcessEnv): NodeJS.ProcessEnv;
}

/**
 * `off` (default): nothing is injected. `all`: `extraHTTPHeaders` carries `traceparent` and `baggage` for
 * every request of the page, context and `request` fixtures. A list of origins: only browser requests to
 * those origins get them (through `context.route`); API calls add `fastproofTrace.headers()` themselves.
 */
export type FastProofPropagate = 'off' | 'all' | string[];

function trace(info: TestInfo): FastProofTrace {
  const t = traceFor(process.env[SEED_ENV], process.env[LAUNCH_ENV] ?? '', info.testId, info.retry);
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

function originOf(u: string): string | undefined {
  try {
    return new URL(u).origin;
  } catch {
    return undefined;
  }
}

let cachedPlan: TestPlan | null | undefined;
function plan(): TestPlan | undefined {
  if (cachedPlan === undefined) cachedPlan = loadTestPlan() ?? null;
  return cachedPlan ?? undefined;
}

/** Applies the test plan (`FASTPROOF_TESTPLAN` or `ALLURE_TESTPLAN_PATH`) inside a test: skips tests not in it. */
export function applyTestPlan(info: TestInfo, p: TestPlan | undefined = plan()): void {
  if (!p) return;
  const ok = inTestPlan(p, {
    titlePath: info.titlePath,
    line: info.line,
    column: info.column,
    annotations: info.annotations,
  });
  if (!ok) info.skip(true, 'not in the test plan');
}

/** Headers for `request.newContext({ extraHTTPHeaders })` and similar: `traceparent` and `baggage`. */
export function traceHeaders(info: TestInfo): Record<string, string> {
  return trace(info).headers();
}

export const test = base.extend<{
  fastproofTrace: FastProofTrace;
  fastproofPropagate: FastProofPropagate;
  fastproofTestPlan: void;
}>({
  fastproofPropagate: ['off', { option: true }],
  fastproofTrace: async ({}, use, info) => {
    const t = trace(info);
    info.annotations.push({ type: TRACE_ANNOTATION, description: t.traceparent });
    await use(t);
  },
  fastproofTestPlan: [
    async ({}, use, info) => {
      applyTestPlan(info);
      await use();
    },
    { auto: true },
  ],
  extraHTTPHeaders: async ({ extraHTTPHeaders, fastproofPropagate, fastproofTrace }, use) => {
    if (fastproofPropagate === 'all') {
      await use({ ...extraHTTPHeaders, ...fastproofTrace.headers() });
      return;
    }
    await use(extraHTTPHeaders);
  },
  context: async ({ context, fastproofPropagate, fastproofTrace }, use) => {
    if (Array.isArray(fastproofPropagate) && fastproofPropagate.length) {
      const allowed = new Set(fastproofPropagate.map((o) => originOf(o) ?? o));
      await context.route(
        (url) => allowed.has(url.origin),
        (route) => route.fallback({ headers: { ...route.request().headers(), ...fastproofTrace.headers({ newSpan: true }) } }),
      );
    }
    await use(context);
  },
});

export { expect } from '@playwright/test';
