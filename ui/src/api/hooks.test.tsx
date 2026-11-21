import { QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { describe, expect, it } from 'vitest';
import { RICH_LAUNCH_ID } from '../mocks/fixtures';
import { testClient } from '../test/render';
import { auditChunk, flatItems, treeRoots, useLaunches, useResult, useResults, useTree } from './hooks';

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={testClient()}>{children}</QueryClientProvider>;
}

describe('query hooks', () => {
  it('pages launches with the before cursor', async () => {
    const { result } = renderHook(() => useLaunches('web-shop', { debug: false }), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(flatItems(result.current.data)).toHaveLength(30);
    expect(result.current.hasNextPage).toBe(true);
    await act(() => result.current.fetchNextPage());
    await waitFor(() => expect(flatItems(result.current.data).length).toBeGreaterThan(30));
    const numbers = flatItems(result.current.data).map((l) => l.number);
    expect(new Set(numbers).size).toBe(numbers.length);
    expect(numbers).not.toContain(17);
  });

  it('filters launches by branch', async () => {
    const { result } = renderHook(() => useLaunches('web-shop', { branch: 'feature/checkout-v2', debug: false }), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const items = flatItems(result.current.data);
    expect(items.length).toBeGreaterThan(0);
    expect(items.every((l) => l.vcs?.branch === 'feature/checkout-v2')).toBe(true);
  });

  it('filters results by several statuses', async () => {
    const { result } = renderHook(() => useResults('web-shop', RICH_LAUNCH_ID, { status: 'failed,broken' }), { wrapper });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const items = flatItems(result.current.data);
    expect(items.length).toBeGreaterThan(0);
    expect(items.every((r) => r.status === 'failed' || r.status === 'broken')).toBe(true);
  });

  it('loads the tree root children and a result detail', async () => {
    const tree = renderHook(() => useTree('web-shop', RICH_LAUNCH_ID, 'feature'), { wrapper });
    await waitFor(() => expect(tree.result.current.isSuccess).toBe(true));
    expect(tree.result.current.data?.map((n) => n.name)).toContain('Checkout');
    const results = renderHook(() => useResults('web-shop', RICH_LAUNCH_ID, { q: 'pays with saved card' }), { wrapper });
    await waitFor(() => expect(results.result.current.isSuccess).toBe(true));
    const id = flatItems(results.result.current.data)[0]?.id ?? '';
    const detail = renderHook(() => useResult('web-shop', id), { wrapper });
    await waitFor(() => expect(detail.result.current.isSuccess).toBe(true));
    expect(detail.result.current.data?.attempts).toHaveLength(2);
    expect(detail.result.current.data?.history.length).toBeGreaterThan(1);
  });

  it('accepts both list shapes for the tree and the audit log', () => {
    const node = { name: 'a', path: ['a'], counts: { total: 0, passed: 0, failed: 0, broken: 0, skipped: 0, unknown: 0, flaky: 0, muted: 0 }, children: [] };
    expect(treeRoots({ ...node, children: [node] })).toEqual([node]);
    expect(treeRoots([node])).toEqual([node]);
    expect(auditChunk({ items: [], next: '5' })).toEqual({ items: [], next: '5' });
    expect(auditChunk({ items: [], next: null })).toEqual({ items: [], next: null });
  });
});
