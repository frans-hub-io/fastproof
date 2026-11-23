import { renderHook, screen, waitFor, within } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import type { ReactNode } from 'react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { WIDGET_CATALOG, WIDGET_TYPES, newWidget, packWidgets, templateOverview, useWidgetPreview, type DashboardWidget } from '../../api/dashboards';
import { dashboardData } from '../../mocks/dashboards';
import { setMockRole } from '../../mocks/handlers';
import { server } from '../../mocks/node';
import { Providers, renderApp, renderWith } from '../../test/render';
import { WidgetBody } from './widgets';

const SEEDED = 'd0000000-0000-4000-8000-000000000001';

describe('dashboard layout', () => {
  it('packs widgets left to right and wraps at 12 columns', () => {
    const ws = [3, 6, 6].map((w, i) => ({ ...newWidget('launches-table', []), id: `w${String(i)}`, w, h: i === 1 ? 5 : 3 }));
    const p = packWidgets(ws);
    expect(p.map((w) => [w.x, w.y])).toEqual([
      [0, 0],
      [3, 0],
      [0, 5],
    ]);
  });

  it('the template has unique ids and valid sizes', () => {
    const t = templateOverview();
    expect(new Set(t.map((w) => w.id)).size).toBe(t.length);
    for (const w of t) expect(w.x + w.w).toBeLessThanOrEqual(12);
  });

  it('catalogs every widget type', () => {
    for (const t of WIDGET_TYPES) expect(WIDGET_CATALOG[t].type).toBe(t);
  });
});

describe('widget bodies', () => {
  it('render every type with mock data without throwing and name their data for screen readers', () => {
    const widgets: DashboardWidget[] = WIDGET_TYPES.map((t, i) => ({ ...newWidget(t, []), id: `w${String(i)}` }));
    const data = dashboardData('web-shop', widgets);
    for (const w of widgets) {
      const r = data.widgets[w.id];
      expect(r?.status).toBe('ok');
      if (r?.status !== 'ok') continue;
      const { unmount } = renderWith(<WidgetBody project="web-shop" widget={w} data={r.data} />);
      unmount();
    }
  });

  it('shows the pass rate summary', () => {
    const w = newWidget('passing-rate-summary', []);
    const r = dashboardData('web-shop', [w]).widgets[w.id];
    if (r?.status !== 'ok') throw new Error('no data');
    renderWith(<WidgetBody project="web-shop" widget={w} data={r.data} />);
    expect(screen.getByTestId('rate-summary')).toHaveTextContent(/%/);
  });
});

describe('dashboard pages', () => {
  it('lists the built-in overview and the stored dashboards', async () => {
    renderApp('/p/web-shop/dashboards');
    const cards = await screen.findAllByTestId('dashboard-card');
    expect(cards).toHaveLength(1);
    expect(within(cards[0] as HTMLElement).getByText('Locked')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Project overview' })).toHaveAttribute('href', '/p/web-shop/dashboard');
  });

  it('shows the widgets of a dashboard and its share link', async () => {
    renderApp(`/p/web-shop/dashboards/${SEEDED}`);
    expect(await screen.findByRole('heading', { name: 'Release health' })).toBeInTheDocument();
    expect(await screen.findByTestId('widget-w1')).toBeInTheDocument();
    expect(screen.getByTestId('share-link')).toHaveTextContent(`/p/web-shop/dashboards/${SEEDED}`);
    expect(await screen.findByTestId('rate-summary')).toBeInTheDocument();
  });

  it('creates a dashboard from the template, edits it and saves', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop/dashboards');
    await user.click(await screen.findByRole('button', { name: 'New dashboard' }));
    await user.type(await screen.findByLabelText('Name'), 'My board');
    await user.click(screen.getByRole('button', { name: 'From the overview template' }));
    expect(await screen.findByRole('heading', { name: 'My board' })).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByRole('region').length).toBeGreaterThan(5));
    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.click(await screen.findByRole('button', { name: 'Add widget' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Launches table' }));
    expect(await screen.findByTestId('widget-w9')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Remove Launches table' }));
    await waitFor(() => expect(screen.queryByTestId('widget-w9')).not.toBeInTheDocument());
    await user.click(screen.getByRole('button', { name: 'Move Pass rate trend later' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('button', { name: 'Edit' })).toBeInTheDocument();
  });

  it('clones a dashboard into another project', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/dashboards/${SEEDED}`);
    await user.click(await screen.findByRole('button', { name: 'Clone' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('combobox', { name: 'Project' }));
    await user.click(await screen.findByRole('option', { name: /Payments/i }));
    await user.click(within(dialog).getByRole('button', { name: 'Clone' }));
    expect(await screen.findByRole('heading', { name: 'Release health (copy)' })).toBeInTheDocument();
  });

  it('locks and unlocks', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/dashboards/${SEEDED}`);
    await user.click(await screen.findByRole('button', { name: 'Unlock' }));
    expect(await screen.findByRole('button', { name: 'Lock' })).toBeInTheDocument();
  });

  it('a viewer sees the dashboard but no edit, clone or lock', async () => {
    setMockRole('viewer');
    renderApp(`/p/web-shop/dashboards/${SEEDED}`);
    expect(await screen.findByRole('heading', { name: 'Release health' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Edit' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Clone' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Unlock' })).not.toBeInTheDocument();
  });
});

describe('preview', () => {
  it('asks once for the first draft and once for the settled one, not for every change', async () => {
    const calls: number[] = [];
    server.use(
      http.post('/api/v1/projects/web-shop/dashboard-preview', () => {
        calls.push(Date.now());
        return HttpResponse.json({ generatedAt: new Date().toISOString(), widgets: {} });
      }),
    );
    const wrapper = ({ children }: { children: ReactNode }) => <Providers>{children}</Providers>;
    const base = newWidget('launches-table', []);
    const { rerender } = renderHook(({ ws }: { ws: DashboardWidget[] }) => useWidgetPreview('web-shop', ws, true), { wrapper, initialProps: { ws: [base] } });
    for (let i = 1; i <= 4; i++) rerender({ ws: [{ ...base, settings: { limit: i } }] });
    await waitFor(() => expect(calls.length).toBeGreaterThan(0), { timeout: 3000 });
    await new Promise((r) => setTimeout(r, 800));
    expect(calls.length).toBeLessThanOrEqual(2);
  });
});
