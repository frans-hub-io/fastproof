import { render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { mockCapabilities, resetAnalysis } from '../mocks/analysis';
import { launchResults, resultDetail, RICH_LAUNCH_ID, testKind } from '../mocks/fixtures';
import { SignalsBlock } from '../pages/result/SignalsBlock';
import { Providers } from '../test/render';
import type { ResultDetail } from './types';
import { byAttempt, confidenceLabel, detailLines, signalKindLabel, type AttemptSignal } from './signals';

afterEach(() => {
  resetAnalysis();
});

function failedUiResult(): ResultDetail {
  const r = launchResults(RICH_LAUNCH_ID).find((x) => (x.status === 'failed' || x.status === 'broken') && testKind(x.id) === 'ui');
  const d = r ? resultDetail(r.id) : undefined;
  if (!d) throw new Error('no failed browser test in the rich launch');
  return d;
}

function sig(id: number, attempt: number | null, at: string): AttemptSignal {
  return { id, attempt, at, source: 'grid', kind: 'node_lost', confidence: 'exact', detail: {}, resultId: 'r' };
}

describe('signals', () => {
  it('names kinds, confidence and detail as text', () => {
    expect(signalKindLabel('browser_oom')).toBe('Browser ran out of memory');
    expect(signalKindLabel('something_new')).toBe('something_new');
    expect(confidenceLabel('window')).toBe('same time window');
    const d = { node: '<b>n1</b>', memoryMb: 2048, retried: false } as unknown as AttemptSignal['detail'];
    expect(detailLines(d)).toEqual(['memoryMb: 2048', 'node: <b>n1</b>', 'retried: false']);
  });

  it('groups by attempt in time order, unknown attempts last', () => {
    const g = byAttempt([sig(3, 2, '2026-11-25T10:00:02Z'), sig(1, null, '2026-11-25T10:00:00Z'), sig(2, 2, '2026-11-25T10:00:01Z'), sig(4, 1, '2026-11-25T10:00:05Z')]);
    expect(g.map((x) => x.attempt)).toEqual([1, 2, null]);
    expect(g[1]?.items.map((x) => x.id)).toEqual([2, 3]);
  });

  it('renders nothing while the feature is off', async () => {
    const r = failedUiResult();
    render(
      <Providers route="/p/web-shop">
        <SignalsBlock project="web-shop" r={r} />
      </Providers>,
    );
    await new Promise((res) => setTimeout(res, 50));
    expect(screen.queryByTestId('result-signals')).not.toBeInTheDocument();
  });

  it('lists the signals of a failed browser test with their source and detail', async () => {
    mockCapabilities.instanceEnabled = true;
    mockCapabilities.features = { signals: true };
    const r = failedUiResult();
    render(
      <Providers route="/p/web-shop">
        <SignalsBlock project="web-shop" r={r} />
      </Providers>,
    );
    const block = await screen.findByTestId('result-signals');
    const cards = within(block).getAllByTestId('result-signal');
    expect(cards.length).toBeGreaterThanOrEqual(3);
    expect(within(block).getAllByTestId('result-signal-source').map((x) => x.textContent)).toContain('Grid');
    expect(within(block).getByText('Browser ran out of memory')).toBeInTheDocument();
    expect(within(block).getAllByTestId('result-signal-detail').some((x) => x.textContent.includes('path: /payments'))).toBe(true);
    expect(within(block).getByText(/never change the defect/)).toBeInTheDocument();
  });
});
