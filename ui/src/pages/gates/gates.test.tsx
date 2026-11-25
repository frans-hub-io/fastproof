import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { request } from '../../api/client';
import type { GateHistory, GateSimulation as Sim } from '../../api/gatesHistory';
import { availableMetrics, ruleHint, rulesValid } from '../../lib/gates';
import { Providers } from '../../test/render';
import { GateHistory as HistoryTab } from './GateHistory';
import { GateSimulation as SimulationTab } from './GateSimulation';

const BASE = '/api/v1/projects/web-shop/gates';

describe('gate analytics mock API', () => {
  it('lists evaluations newest first with aggregates on the first page only', async () => {
    const first = await request<GateHistory>(`${BASE}/history?limit=2`);
    expect(first.items.length).toBe(2);
    expect(first.gates.length).toBeGreaterThan(0);
    expect(first.gates[0]?.evaluations).toBeGreaterThan(0);
    const at = first.items.map((i) => Date.parse(i.evaluatedAt));
    expect(at[0]).toBeGreaterThanOrEqual(at[1] ?? 0);
    expect(first.next).toBeTruthy();
    const second = await request<GateHistory>(`${BASE}/history?limit=2&before=${first.next ?? ''}`);
    expect(second.gates).toEqual([]);
    expect(second.items[0]?.launchId).not.toBe(first.items[0]?.launchId);
  });

  it('simulates a draft gate over the launches', async () => {
    const sim = await request<Sim>(`${BASE}/simulate`, { method: 'POST', json: { gate: { name: 'draft', rules: [{ metric: 'failed', op: 'lte', value: 0 }] }, launches: 5 } });
    expect(sim.evaluated + sim.skipped).toBeLessThanOrEqual(5);
    expect(sim.items.every((i) => i.status === 'passed' || i.status === 'failed')).toBe(true);
    await expect(request(`${BASE}/simulate`, { method: 'POST', json: { gate: { name: 'draft', rules: [] } } })).rejects.toThrow();
  });
});

describe('gate pages', () => {
  it('shows the history', async () => {
    render(
      <Providers route="/p/web-shop/gates">
        <HistoryTab project="web-shop" />
      </Providers>,
    );
    expect((await screen.findAllByTestId('gate-history-row')).length).toBeGreaterThan(0);
    expect(screen.getAllByTestId('gate-history-gate').length).toBeGreaterThan(0);
  });

  it('runs a simulation and lists the launches', async () => {
    render(
      <Providers route="/p/web-shop/gates">
        <SimulationTab project="web-shop" />
      </Providers>,
    );
    await userEvent.click(screen.getByTestId('gate-simulation-run'));
    expect(await screen.findByTestId('gate-simulation-result')).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByTestId('gate-simulation-row').length).toBeGreaterThan(0));
  });
});

describe('gate rule helpers', () => {
  it('offers the analysis metrics only with Analysis or when a rule uses them', () => {
    const base = [{ metric: 'failed' as const, op: 'lte' as const, value: 0 }];
    expect(availableMetrics(false, base)).not.toContain('newErrors');
    expect(availableMetrics(true, base)).toEqual(expect.arrayContaining(['newErrors', 'toInvestigate', 'total']));
    expect(availableMetrics(false, [{ metric: 'total', op: 'gte', value: 1 }])).toContain('total');
  });

  it('explains a baseline per metric and validates rules', () => {
    expect(ruleHint({ metric: 'newErrors', op: 'lte', value: 0, baseline: 'main' })).toContain('history of main');
    expect(ruleHint({ metric: 'total', op: 'gte', value: -1, baseline: 'main' })).toContain('Difference');
    expect(rulesValid([])).toBe(false);
    expect(rulesValid([{ metric: 'passRate', op: 'gte', value: 101 }])).toBe(false);
    expect(rulesValid([{ metric: 'toInvestigate', op: 'lte', value: 0 }])).toBe(true);
    expect(rulesValid([{ metric: 'total', op: 'gte', value: -20, baseline: 'main' }])).toBe(true);
    expect(rulesValid([{ metric: 'total', op: 'gte', value: -20 }])).toBe(false);
  });
});
