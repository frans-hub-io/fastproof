import '@testing-library/jest-dom/vitest';
import { cleanup, configure } from '@testing-library/react';
import { afterAll, afterEach, beforeAll } from 'vitest';
import { resetMockState } from '../mocks/handlers';
import { server } from '../mocks/node';

class ResizeObserverStub {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

class IntersectionObserverStub {
  readonly root = null;
  readonly rootMargin = '';
  readonly thresholds = [];
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
  takeRecords(): [] {
    return [];
  }
}

const g = globalThis as Record<string, unknown>;
g.ResizeObserver ??= ResizeObserverStub;
g.IntersectionObserver ??= IntersectionObserverStub;

const w = window as Partial<Window>;
if (typeof w.matchMedia !== 'function') {
  const legacy = { addListener: () => undefined, removeListener: () => undefined };
  window.matchMedia = (query: string) => ({
    ...legacy,
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  });
}

configure({ asyncUtilTimeout: 5000 });

if (!('fonts' in document)) {
  Object.defineProperty(document, 'fonts', { value: { addEventListener: () => undefined, removeEventListener: () => undefined, ready: Promise.resolve() } });
}

const proto = Element.prototype as Partial<Element>;
proto.scrollIntoView ??= () => undefined;

const inflight = new Set<Promise<unknown>>();
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

beforeAll(() => {
  server.listen({ onUnhandledFrame: 'error' });
  const patched = globalThis.fetch;
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const request = patched(typeof input === 'string' && input.startsWith('/') ? new URL(input, window.location.origin) : input, init);
    const settled = request.then(
      () => undefined,
      () => undefined,
    );
    inflight.add(settled);
    void settled.then(() => inflight.delete(settled));
    return request;
  };
});
afterEach(async () => {
  // A request a test did not wait for (such as the session load of a synchronous test) would otherwise update
  // state after the environment is torn down; let it land while the tree is still mounted.
  await Promise.race([Promise.allSettled([...inflight]), sleep(2000)]);
  await sleep(0);
  cleanup();
  server.resetHandlers();
  resetMockState();
  window.localStorage.clear();
});
afterAll(() => server.close());
