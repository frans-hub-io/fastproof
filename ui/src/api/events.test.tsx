import { QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { testClient } from '../test/render';
import { useProjectEvents } from './events';
import { qk } from './hooks';

type Listener = (e: MessageEvent) => void;

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  static readonly CLOSED = 2;
  readyState = 0;
  url: string;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  listeners = new Map<string, Listener>();
  closed = false;
  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }
  addEventListener(type: string, fn: Listener) {
    this.listeners.set(type, fn);
  }
  close() {
    this.closed = true;
  }
  emit(type: string, data: unknown) {
    this.listeners.get(type)?.(new MessageEvent(type, { data: JSON.stringify(data) }));
  }
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  FakeEventSource.instances = [];
});

describe('useProjectEvents', () => {
  it('subscribes per project and invalidates launch queries on events', () => {
    vi.useFakeTimers();
    vi.stubGlobal('EventSource', FakeEventSource);
    const client = testClient();
    const spy = vi.spyOn(client, 'invalidateQueries');
    const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
    const { result, unmount } = renderHook(() => useProjectEvents('web-shop'), { wrapper });
    const es = FakeEventSource.instances[0];
    expect(es?.url).toBe('/api/v1/projects/web-shop/events');
    expect(result.current).toBe('connecting');
    act(() => es?.onopen?.());
    expect(result.current).toBe('live');
    act(() => {
      es?.emit('launch.updated', { id: 'L1' });
      es?.emit('launch.updated', { id: 'L1' });
      es?.emit('upload.updated', { launchId: 'L1', id: 'U1' });
      vi.advanceTimersByTime(500);
    });
    const keys = spy.mock.calls.map((c) => JSON.stringify(c[0]?.queryKey));
    expect(keys.filter((k) => k === JSON.stringify(qk.launch('web-shop', 'L1')))).toHaveLength(2);
    expect(keys).toContain(JSON.stringify(qk.results('web-shop', 'L1')));
    expect(keys).toContain(JSON.stringify(qk.uploads('web-shop', 'L1')));
    unmount();
    expect(es?.closed).toBe(true);
  });

  it('reports unsupported without EventSource', () => {
    vi.stubGlobal('EventSource', undefined);
    const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={testClient()}>{children}</QueryClientProvider>;
    const { result } = renderHook(() => useProjectEvents('web-shop'), { wrapper });
    expect(result.current).toBe('unsupported');
  });
});
