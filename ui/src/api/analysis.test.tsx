import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { mockCapabilities, resetAnalysis } from '../mocks/analysis';
import { RICH_LAUNCH_ID } from '../mocks/fixtures';
import { Providers } from '../test/render';
import { Shell } from '../Shell';
import { request } from './client';
import type { AnalysisCapabilities, TriageHistory, TriageUndoResult } from './analysis';

afterEach(() => resetAnalysis());

describe('analysis', () => {
  it('hides the Errors and Analytics pages until the server has them', async () => {
    render(
      <Providers route="/p/web-shop">
        <Shell>
          <p>page</p>
        </Shell>
      </Providers>,
    );
    expect(await screen.findByText('Flaky tests')).toBeInTheDocument();
    expect(screen.queryByText('Errors')).not.toBeInTheDocument();
    expect(screen.queryByText('Analytics')).not.toBeInTheDocument();
  });

  it('shows them when the capabilities say so', async () => {
    mockCapabilities.instanceEnabled = true;
    mockCapabilities.features = { errors: true, analytics: true };
    render(
      <Providers route="/p/web-shop">
        <Shell>
          <p>page</p>
        </Shell>
      </Providers>,
    );
    expect(await screen.findByText('Errors')).toBeInTheDocument();
    expect(screen.getByText('Analytics')).toBeInTheDocument();
  });

  it('journals mock triage and undoes it', async () => {
    const caps = await request<AnalysisCapabilities>('/api/v1/projects/web-shop/analysis/capabilities');
    expect(caps.enabled).toBe(false);
    const clusters = await request<{ items: { fingerprint: string; sampleResultId: string }[] }>(`/api/v1/projects/web-shop/launches/${RICH_LAUNCH_ID}/clusters`);
    const result = clusters.items[0]?.sampleResultId ?? '';
    const out = await request<{ updated: number; batchId: string | null }>('/api/v1/projects/web-shop/triage', { method: 'POST', json: { resultIds: [result], defectTypeId: 'productBug' } });
    expect(out.updated).toBe(1);
    expect(out.batchId).toBeTruthy();
    const h = await request<TriageHistory>(`/api/v1/projects/web-shop/triage/history?resultId=${result}`);
    expect(h.items).toHaveLength(1);
    expect(h.items[0]?.via).toBe('api');
    expect(h.items[0]?.undoable).toBe(true);
    const u = await request<TriageUndoResult>('/api/v1/projects/web-shop/triage/undo', { method: 'POST', json: { batchId: out.batchId } });
    expect(u.restored).toBe(1);
    await waitFor(async () => {
      const again = await request<TriageHistory>(`/api/v1/projects/web-shop/triage/history?resultId=${result}`);
      expect(again.items[0]?.via).toBe('undo');
      expect(again.items[1]?.undoable).toBe(false);
    });
  });
});

