import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { request } from '../../api/client';
import { examplesOf, latestResult, type FlakyHint, type FlakyHints } from '../../api/flakyHints';
import type { FlakyTest } from '../../api/types-next';
import { mockCapabilities, resetAnalysis } from '../../mocks/analysis';
import { renderApp } from '../../test/render';

beforeEach(() => {
  mockCapabilities.instanceEnabled = true;
  mockCapabilities.features = { flakyHints: true };
});

afterEach(() => resetAnalysis());

async function flaky(): Promise<FlakyTest[]> {
  return (await request<{ items: FlakyTest[] }>('/api/v1/projects/web-shop/flaky?limit=100')).items;
}

describe('flaky causes', () => {
  it('shows likely-cause chips and opens the evidence panel', async () => {
    const items = await flaky();
    const withHints = items.filter((i) => i.hints.length > 0);
    expect(withHints.length).toBeGreaterThan(0);
    renderApp('/p/web-shop/flaky');
    const table = await screen.findByTestId('flaky-table');
    expect(within(table).getByText('Likely cause')).toBeInTheDocument();
    const chips = await within(table).findAllByTestId('hint-chips');
    expect(chips.length).toBe(withHints.length);
    fireEvent.click(chips[0] as HTMLElement);
    const panel = await screen.findByTestId('hint-panel');
    expect(within(panel).getAllByTestId('hint-card').length).toBeGreaterThan(0);
    expect(within(panel).getAllByTestId('hint-evidence').length).toBeGreaterThan(0);
    expect(within(panel).getByText(/not a learned model/)).toBeInTheDocument();
  });

  it('mutes with an end through triage', async () => {
    const item = (await flaky()).find((i) => i.hints.some((h) => h.kind === 'timing'));
    expect(item).toBeTruthy();
    renderApp('/p/web-shop/flaky');
    const rows = await screen.findAllByTestId('flaky-row');
    const row = rows.find((r) => within(r).queryByText(item?.name ?? '')) as HTMLElement;
    fireEvent.click(within(row).getByTestId('hint-chips'));
    const card = (await screen.findAllByTestId('hint-card')).find((c) => c.getAttribute('data-kind') === 'timing') as HTMLElement;
    expect(within(card).getByTestId('hint-rule')).toBeInTheDocument();
    fireEvent.click(within(card).getByTestId('hint-mute'));
    const dialog = await screen.findByTestId('hint-mute-dialog');
    expect(within(dialog).getByTestId<HTMLInputElement>('hint-mute-reason').value).toMatch(/^Flaky \(timing\): /);
    fireEvent.click(within(dialog).getByTestId('hint-mute-submit'));
    await waitFor(() => expect(screen.queryByTestId('hint-mute-dialog')).not.toBeInTheDocument());
  });

  it('shows a viewer the evidence but no mute, issue or rule action', async () => {
    window.localStorage.setItem('fastproof-mock-role', 'viewer');
    const item = (await flaky()).find((i) => i.hints.some((h) => h.kind === 'timing'));
    expect(item).toBeTruthy();
    renderApp('/p/web-shop/flaky');
    const rows = await screen.findAllByTestId('flaky-row');
    const row = rows.find((r) => within(r).queryByText(item?.name ?? '')) as HTMLElement;
    fireEvent.click(within(row).getByTestId('hint-chips'));
    const panel = await screen.findByTestId('hint-panel');
    expect(within(panel).getAllByTestId('hint-evidence').length).toBeGreaterThan(0);
    expect(within(panel).queryByTestId('hint-mute')).toBeNull();
    expect(within(panel).queryByTestId('hint-issue')).toBeNull();
    expect(within(panel).queryByTestId('hint-rule')).toBeNull();
  });

  it('hides the column without the capability and answers 404 for an unknown series', async () => {
    mockCapabilities.features = {};
    renderApp('/p/web-shop/flaky');
    const table = await screen.findByTestId('flaky-table');
    expect(within(table).queryByText('Likely cause')).not.toBeInTheDocument();
    await expect(request<FlakyHints>('/api/v1/projects/web-shop/flaky/00000000-0000-7000-8000-000000000000/hints')).rejects.toThrow();
  });

  it('reads examples leniently and picks the newest result', () => {
    const h = (examples: unknown): FlakyHint => ({ kind: 'retry', confidence: 'low', summary: '', sample: 3, actions: [], evidence: { examples } as FlakyHint['evidence'] });
    expect(examplesOf(h('nope'))).toEqual([]);
    const a = h([
      { launchId: 'l1', resultId: 'r1', at: '2026-11-01T00:00:00Z', status: 'failed' },
      { launchId: 'l2', at: '2026-11-03T00:00:00Z', status: 'failed' },
      { launchId: 'l3', resultId: 'r3', at: '2026-11-02T00:00:00Z', status: 'passedOnRetry' },
      { nonsense: true },
    ]);
    expect(examplesOf(a)).toHaveLength(3);
    expect(latestResult([a])?.resultId).toBe('r3');
    expect(latestResult([])).toBeNull();
  });
});
