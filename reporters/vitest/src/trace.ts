import {
  LAUNCH_ENV,
  SEED_ENV,
  formatTraceparent,
  randomSpanId,
  traceFor,
  type TraceContext,
} from '@franshub/fastproof-playwright';

export interface FastProofTrace extends TraceContext {
  /** `traceparent` and `baggage`; `newSpan` makes a fresh span id under the same trace (one per request). */
  headers(opts?: { newSpan?: boolean }): Record<string, string>;
  /** A copy of the environment with `TRACEPARENT` and `BAGGAGE`, for child processes. */
  env(from?: NodeJS.ProcessEnv): NodeJS.ProcessEnv;
}

/** The part of Vitest's `task` (the `task` of the test context) that identifies an attempt. */
export interface TaskLike {
  id: string;
  result?: { retryCount?: number; repeatCount?: number };
  repeats?: number;
  retry?: number | { count?: number };
}

function attemptOf(task: TaskLike): number {
  const retries = typeof task.retry === 'number' ? task.retry : (task.retry?.count ?? 0);
  const r = task.result;
  return (r?.repeatCount ?? 0) * (retries + 1) + (r?.retryCount ?? 0);
}

/**
 * The trace context of the running attempt, the same one the reporter sends with the result:
 * `fastproofTrace(task).headers()` for HTTP calls to the system under test or FastMock.
 *
 *     test('pays', async ({ task }) => { await fetch(url, { headers: fastproofTrace(task).headers() }); });
 */
export function fastproofTrace(task: TaskLike): FastProofTrace {
  const t = traceFor(process.env[SEED_ENV], process.env[LAUNCH_ENV] ?? '', task.id, attemptOf(task));
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
