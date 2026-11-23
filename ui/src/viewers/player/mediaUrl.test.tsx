import { act, renderHook, waitFor } from '@testing-library/react';
import { HttpResponse, http } from 'msw';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { server } from '../../mocks/node';
import { Providers } from '../../test/render';
import { requestMediaUrl, useMediaSource } from './mediaUrl';

const URL_PATH = '/api/v1/projects/web-shop/attachments/a1/media-url';

function wrapper({ children }: { children: ReactNode }) {
  return <Providers>{children}</Providers>;
}

describe('media address', () => {
  let n = 0;
  let ttlMs = 15 * 60_000;
  beforeEach(() => {
    n = 0;
    ttlMs = 15 * 60_000;
    server.use(
      http.post(URL_PATH, () => {
        n += 1;
        return HttpResponse.json({ url: `/api/v1/media/t${String(n)}`, expiresAt: new Date(Date.now() + ttlMs).toISOString() });
      }),
    );
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('rejects an address that is not a local path', async () => {
    server.use(http.post(URL_PATH, () => HttpResponse.json({ url: 'https://evil.example.com/x', expiresAt: new Date().toISOString() })));
    await expect(requestMediaUrl('web-shop', 'a1')).rejects.toThrow(/invalid media address/);
    server.use(http.post(URL_PATH, () => HttpResponse.json({ url: '//evil.example.com/x', expiresAt: new Date().toISOString() })));
    await expect(requestMediaUrl('web-shop', 'a1')).rejects.toThrow(/invalid media address/);
  });

  it('loads the address and replaces it a minute before it expires', async () => {
    ttlMs = 5 * 60_000;
    const { result } = renderHook(() => useMediaSource('web-shop', 'a1'), { wrapper });
    await waitFor(() => expect(result.current.src).toBe('/api/v1/media/t1'));
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'] });
    act(() => {
      result.current.refresh().catch(() => undefined);
    });
    vi.useRealTimers();
    await waitFor(() => expect(result.current.src).toBe('/api/v1/media/t2'));
  });

  it('refreshes on request and stops after too many refreshes in a short time', async () => {
    const { result } = renderHook(() => useMediaSource('web-shop', 'a1'), { wrapper });
    await waitFor(() => expect(result.current.src).toBe('/api/v1/media/t1'));
    const outcomes: boolean[] = [];
    for (let i = 0; i < 4; i++) {
      await act(async () => {
        outcomes.push(await result.current.refresh());
      });
    }
    expect(outcomes).toEqual([true, true, true, false]);
    expect(result.current.src).toBe('/api/v1/media/t4');
  });

  it('reports a failure to get the first address', async () => {
    server.use(http.post(URL_PATH, () => HttpResponse.json({ code: 'forbidden', message: 'no' }, { status: 403 })));
    const { result } = renderHook(() => useMediaSource('web-shop', 'a1'), { wrapper });
    await waitFor(() => expect(result.current.error).not.toBeNull());
    expect(result.current.src).toBeUndefined();
  });
});
