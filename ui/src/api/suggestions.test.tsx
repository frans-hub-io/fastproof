import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';
import { mockCapabilities, resetAnalysis } from '../mocks/analysis';
import { launchDef, RICH_LAUNCH_ID } from '../mocks/fixtures';
import { resetSuggestions } from '../mocks/suggestions';
import { Clusters } from '../pages/launch/Clusters';
import { SuggestionCard } from '../pages/launch/ClusterSuggestion';
import { AnalysisSettings } from '../pages/settings/AnalysisSettings';
import { Providers } from '../test/render';
import { request } from './client';
import { suggestionChip, type LaunchSuggestions, type Suggestion } from './suggestions';

afterEach(() => {
  resetAnalysis();
  resetSuggestions();
});

function on() {
  mockCapabilities.instanceEnabled = true;
  mockCapabilities.features = { suggestions: true };
}

describe('suggestions', () => {
  it('names the type, band, launch and issue in the chip', () => {
    const s = { defectName: 'Product bug', group: 'productBug', band: 'high', precision: 0.91, issueKey: 'PAY-1234', sample: { launchNumber: 431 } } as unknown as Suggestion;
    expect(suggestionChip(s)).toBe('Product bug · high 91% · as in #431, PAY-1234');
    expect(suggestionChip({ ...s, precision: null, issueKey: null, sample: null })).toBe('Product bug · high');
  });

  it('accepts a cluster suggestion and journals it as a suggestion', async () => {
    const v = await request<LaunchSuggestions>(`/api/v1/projects/web-shop/launches/${RICH_LAUNCH_ID}/suggestions`);
    const c = v.clusters.find((x) => x.suggestions.length);
    expect(c).toBeTruthy();
    const s = c?.suggestions[0];
    const out = await request<{ updated: number; batchId: string | null }>(`/api/v1/projects/web-shop/suggestions/${s?.id ?? ''}/accept`, { method: 'POST', json: {} });
    expect(out.updated).toBeGreaterThan(0);
    const h = await request<{ items: { via: string }[] }>(`/api/v1/projects/web-shop/triage/history?batchId=${out.batchId ?? ''}`);
    expect(h.items[0]?.via).toBe('suggestion');
    await expect(request(`/api/v1/projects/web-shop/suggestions/${s?.id ?? ''}/accept`, { method: 'POST', json: {} })).rejects.toMatchObject({ status: 409 });
  });

  it('shows chips and the why of a cluster when the feature is on', async () => {
    on();
    const def = launchDef(RICH_LAUNCH_ID);
    if (!def) throw new Error('no launch');
    render(
      <Providers route="/p/web-shop">
        <Clusters project="web-shop" launch={def.launch} />
      </Providers>,
    );
    const chips = await screen.findAllByTestId('suggestion-chip');
    expect(chips.length).toBeGreaterThan(0);
    const cluster = chips[0]?.closest('[data-testid="cluster"]') as HTMLElement;
    await userEvent.click(within(cluster).getAllByRole('button')[0] as HTMLElement);
    const card = (await within(cluster).findAllByTestId('suggestion'))[0] as HTMLElement;
    await userEvent.click(within(card).getByTestId('suggestion-why-toggle'));
    await waitFor(() => expect(within(card).getByTestId('suggestion-why')).toBeVisible());
    expect(within(card).getByText(/nearest-decision search/)).toBeInTheDocument();
  });

  it('links only http(s) issue and compare URLs in the why', async () => {
    const v = await request<LaunchSuggestions>(`/api/v1/projects/web-shop/launches/${RICH_LAUNCH_ID}/suggestions`);
    const c = v.clusters.find((x) => x.suggestions.length);
    const base = c?.suggestions[0];
    if (!c || !base) throw new Error('no suggestion');
    const s = { ...base, comment: 'remembered', issueUrl: 'javascript:alert(1)', issueKey: 'PAY-1' } as Suggestion;
    const cluster = { ...c, commits: { first: 'abcdef1234', lastGood: '1234abcdef', compareUrl: 'javascript:alert(2)' } };
    render(
      <Providers route="/p/web-shop">
        <SuggestionCard project="web-shop" s={s} cluster={cluster} canTriage canRule />
      </Providers>,
    );
    await userEvent.click(screen.getByTestId('suggestion-why-toggle'));
    await waitFor(() => expect(screen.getByTestId('suggestion-why')).toBeVisible());
    expect(screen.queryByTestId('suggestion-issue')).not.toBeInTheDocument();
    expect(screen.getByTestId('suggestion-why').textContent).toContain('"remembered" · PAY-1');
    expect(screen.queryByText('compare')).not.toBeInTheDocument();
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
  });

  it('renders nothing while the feature is off', async () => {
    const def = launchDef(RICH_LAUNCH_ID);
    if (!def) throw new Error('no launch');
    render(
      <Providers route="/p/web-shop">
        <Clusters project="web-shop" launch={def.launch} />
      </Providers>,
    );
    expect(await screen.findAllByTestId('cluster')).not.toHaveLength(0);
    expect(screen.queryByTestId('suggestion-chip')).not.toBeInTheDocument();
  });

  it('edits the analysis settings with a warning for auto-apply and runs a backtest', async () => {
    on();
    render(
      <Providers route="/p/web-shop/settings">
        <AnalysisSettings project="web-shop" canWrite />
      </Providers>,
    );
    expect(await screen.findByTestId('backtest-none')).toBeInTheDocument();
    await userEvent.click(screen.getByTestId('analysis-auto-apply'));
    expect(screen.getByTestId('analysis-auto-warning')).toBeInTheDocument();
    await userEvent.click(screen.getByTestId('analysis-save'));
    await waitFor(async () => {
      const s = await request<{ analysis?: { autoApply: { enabled: boolean } } }>('/api/v1/projects/web-shop/settings');
      expect(s.analysis?.autoApply.enabled).toBe(true);
    });
    await userEvent.click(screen.getByTestId('backtest-run'));
    const report = await request<{ bands: unknown[] }>('/api/v1/projects/web-shop/analysis/backtest/latest');
    expect(report.bands).toHaveLength(3);
  });
});
