import { ApiError, retryAfterMs } from './client';

/** Tries of a read the server refused as busy, the first one included. */
export const MAX_BUSY_ATTEMPTS = 4;
const MAX_DELAY_MS = 10_000;

/** The server is busy (429, or 503 `unavailable`): the same request may succeed a moment later. A timeout (503 `timeout`) will not. */
export function isBusy(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 429 || (err.status === 503 && err.code === 'unavailable'));
}

/** How long to wait before the next try: the server's `Retry-After`, else a growing delay with jitter. */
export function busyDelay(failures: number, err: unknown, random: () => number = Math.random): number {
  const told = retryAfterMs(err);
  if (told !== undefined) return Math.min(told, MAX_DELAY_MS);
  return Math.min(MAX_DELAY_MS, 500 * 2 ** Math.max(0, failures - 1)) + Math.floor(random() * 250);
}

/** The retry rule of queries: busy answers up to `MAX_BUSY_ATTEMPTS` tries, other 4xx never, the rest twice. */
export function shouldRetry(failures: number, err: unknown): boolean {
  if (isBusy(err)) return failures < MAX_BUSY_ATTEMPTS - 1;
  return !(err instanceof ApiError && err.status >= 400 && err.status < 500) && failures < 2;
}

export function retryDelay(failures: number, err: unknown): number {
  return isBusy(err) ? busyDelay(failures, err) : Math.min(1000 * 2 ** failures, 30_000);
}

/** Runs a read by hand (outside React Query) with the same busy retries. */
export async function withBusyRetry<T>(run: () => Promise<T>, sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms))): Promise<T> {
  for (let failures = 1; ; failures++) {
    try {
      return await run();
    } catch (e) {
      if (!isBusy(e) || failures >= MAX_BUSY_ATTEMPTS) throw e;
      await sleep(busyDelay(failures, e));
    }
  }
}

/** Lets at most `max` calls run at once; the rest wait their turn. */
export function limiter(max: number): <T>(run: () => Promise<T>) => Promise<T> {
  let active = 0;
  const waiting: Array<() => void> = [];
  return async (run) => {
    if (active >= max) await new Promise<void>((r) => waiting.push(r));
    else active++;
    try {
      return await run();
    } finally {
      const next = waiting.shift();
      if (next) next();
      else active--;
    }
  };
}
