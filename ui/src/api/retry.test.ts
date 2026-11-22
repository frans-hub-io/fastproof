import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, request } from './client';
import { busyDelay, isBusy, limiter, MAX_BUSY_ATTEMPTS, retryDelay, shouldRetry, withBusyRetry } from './retry';

afterEach(() => vi.unstubAllGlobals());

const busy = new ApiError(429, { code: 'too_many_requests', message: 'x' });
const unavailable = new ApiError(503, { code: 'unavailable', message: 'x' });
const timeout = new ApiError(503, { code: 'timeout', message: 'x' });

describe('retry policy', () => {
  it('treats 429 and a busy 503 as retryable, a 503 timeout and other errors not', () => {
    expect(isBusy(busy)).toBe(true);
    expect(isBusy(unavailable)).toBe(true);
    expect(isBusy(timeout)).toBe(false);
    expect(isBusy(new ApiError(404, { code: 'not_found', message: 'x' }))).toBe(false);
    expect(isBusy(new Error('x'))).toBe(false);
  });

  it('retries busy answers up to four tries in all, other 4xx never, the rest twice', () => {
    expect(MAX_BUSY_ATTEMPTS).toBe(4);
    expect([0, 1, 2, 3].map((n) => shouldRetry(n, busy))).toEqual([true, true, true, false]);
    expect(shouldRetry(0, new ApiError(404, { code: 'not_found', message: 'x' }))).toBe(false);
    expect(shouldRetry(0, new ApiError(403, { code: 'forbidden', message: 'x' }))).toBe(false);
    expect([0, 1, 2].map((n) => shouldRetry(n, timeout))).toEqual([true, true, false]);
    expect(shouldRetry(0, new Error('network'))).toBe(true);
  });

  it('backs off exponentially with jitter, capped', () => {
    const none = () => 0;
    expect([1, 2, 3, 4, 10].map((n) => busyDelay(n, busy, none))).toEqual([500, 1000, 2000, 4000, 10_000]);
    expect(busyDelay(1, busy, () => 0.999)).toBeLessThan(750);
    expect(retryDelay(1, busy)).toBeGreaterThanOrEqual(500);
  });

  it('honours Retry-After from the response, capped at ten seconds', async () => {
    const answer = (secs: string) => vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'too_many_requests', message: 'busy' } }), { status: 429, headers: { 'Retry-After': secs } }));
    vi.stubGlobal('fetch', answer('3'));
    const err = await request('/x').catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(busyDelay(1, err)).toBe(3000);
    vi.stubGlobal('fetch', answer('600'));
    expect(busyDelay(1, await request('/x').catch((e: unknown) => e))).toBe(10_000);
    vi.stubGlobal('fetch', answer('soon'));
    expect(busyDelay(1, await request('/x').catch((e: unknown) => e), () => 0)).toBe(500);
  });

  it('withBusyRetry retries a busy read, waits as told, and gives up after four tries', async () => {
    const waits: number[] = [];
    const sleep = (ms: number) => {
      waits.push(ms);
      return Promise.resolve();
    };
    let calls = 0;
    const once = () => (++calls === 1 ? Promise.reject(busy) : Promise.resolve('ok'));
    await expect(withBusyRetry(once, sleep)).resolves.toBe('ok');
    expect(calls).toBe(2);
    expect(waits).toHaveLength(1);

    calls = 0;
    await expect(withBusyRetry(() => (calls++, Promise.reject(unavailable)), sleep)).rejects.toBe(unavailable);
    expect(calls).toBe(4);

    calls = 0;
    await expect(withBusyRetry(() => (calls++, Promise.reject(timeout)), sleep)).rejects.toBe(timeout);
    expect(calls).toBe(1);
  });

  it('limits how many calls run at once and runs them all', async () => {
    const run = limiter(2);
    let active = 0;
    let peak = 0;
    const task = async () => {
      active++;
      peak = Math.max(peak, active);
      await new Promise((r) => setTimeout(r, 5));
      active--;
      return 1;
    };
    const out = await Promise.all(Array.from({ length: 7 }, () => run(task)));
    expect(out).toHaveLength(7);
    expect(peak).toBe(2);
    await expect(run(() => Promise.reject(new Error('x')))).rejects.toThrow('x');
    await expect(run(task)).resolves.toBe(1);
  });
});
