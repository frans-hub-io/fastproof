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

beforeAll(() => {
  server.listen({ onUnhandledFrame: 'error' });
  const patched = globalThis.fetch;
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit) =>
    patched(typeof input === 'string' && input.startsWith('/') ? new URL(input, window.location.origin) : input, init);
});
afterEach(() => {
  cleanup();
  server.resetHandlers();
  resetMockState();
  window.localStorage.clear();
});
afterAll(() => server.close());
