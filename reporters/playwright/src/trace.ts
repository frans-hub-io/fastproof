import { createHash, randomBytes } from 'node:crypto';

/** Environment variable through which the reporter hands its trace seed to the worker processes. */
export const SEED_ENV = 'FASTPROOF_TRACE_SEED';
/** Environment variable with the launch external id, for `baggage`. */
export const LAUNCH_ENV = 'FASTPROOF_TRACE_LAUNCH';
/** Annotation that carries the `traceparent` a test actually used. */
export const TRACE_ANNOTATION = 'fastproof.traceparent';

export interface TraceContext {
  traceId: string;
  spanId: string;
  traceparent: string;
  baggage: string;
}

function nonZero(hex: string): string {
  return /^0+$/.test(hex) ? hex.slice(0, -1) + '1' : hex;
}

export function randomSpanId(): string {
  return nonZero(randomBytes(8).toString('hex'));
}

export function formatTraceparent(traceId: string, spanId: string): string {
  return `00-${traceId}-${spanId}-01`;
}

export function parseTraceparent(v: string | undefined | null): { traceId: string; spanId: string } | undefined {
  const m = /^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$/.exec((v ?? '').trim());
  if (!m || /^0+$/.test(m[1]!) || /^0+$/.test(m[2]!)) return undefined;
  return { traceId: m[1]!, spanId: m[2]! };
}

function baggageValue(v: string): string {
  return encodeURIComponent(v).slice(0, 256);
}

export function formatBaggage(launch: string, test: string, attempt: number): string {
  return [
    `fastproof.launch=${baggageValue(launch)}`,
    `fastproof.test=${baggageValue(test)}`,
    `fastproof.attempt=${attempt}`,
  ].join(',');
}

/**
 * The trace context of one attempt. With a seed (random per run) the ids are derived, so the reporter
 * and the worker compute the same ids without talking to each other; without one they are random.
 */
export function traceFor(seed: string | undefined, launch: string, testId: string, attempt: number): TraceContext {
  let traceId: string;
  let spanId: string;
  if (seed) {
    const h = createHash('sha256').update(`${seed}\u0000${testId}\u0000${attempt}`).digest('hex');
    traceId = nonZero(h.slice(0, 32));
    spanId = nonZero(h.slice(32, 48));
  } else {
    traceId = nonZero(randomBytes(16).toString('hex'));
    spanId = randomSpanId();
  }
  return {
    traceId,
    spanId,
    traceparent: formatTraceparent(traceId, spanId),
    baggage: formatBaggage(launch, testId, attempt),
  };
}
