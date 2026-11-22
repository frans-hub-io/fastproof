import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { VirtuosoMockContext } from 'react-virtuoso';
import { describe, expect, it } from 'vitest';
import { RICH_LAUNCH_ID } from '../../mocks/fixtures';
import { Providers } from '../../test/render';
import { AppRoutes } from '../../App';
import { Shell } from '../../Shell';

function renderLaunch(search = '') {
  return render(
    <VirtuosoMockContext.Provider value={{ viewportHeight: 4000, itemHeight: 32 }}>
      <Providers route={`/p/web-shop/launches/${RICH_LAUNCH_ID}${search}`}>
        <Shell>
          <AppRoutes />
        </Shell>
      </Providers>
    </VirtuosoMockContext.Provider>,
  );
}

describe('launch explorer', () => {
  it('opens on the tree with failing paths expanded down to the tests', { timeout: 40_000 }, async () => {
    renderLaunch();
    expect(await screen.findByTestId('explorer', {}, { timeout: 20_000 })).toBeInTheDocument();
    const groups = await screen.findAllByTestId('explorer-group', {}, { timeout: 20_000 });
    expect(groups.length).toBeGreaterThan(3);
    const failingTests = await screen.findAllByTestId('explorer-test');
    expect(failingTests.length).toBeGreaterThan(0);
    expect(screen.getByTestId('explorer-empty-detail')).toBeInTheDocument();
  });

  it('shows the selected test beside the tree and filters by status', { timeout: 40_000 }, async () => {
    const user = userEvent.setup();
    renderLaunch('?q=pays+with+saved');
    const test = await screen.findByText('pays with saved card', { selector: '[data-testid="explorer-test"] *' });
    await user.click(test);
    const detail = await screen.findByTestId('explorer-detail');
    expect(within(detail).getByTestId('explorer-detail-name')).toHaveTextContent('pays with saved card');
    expect(await within(detail).findByTestId('step-tree')).toBeInTheDocument();
    expect(within(detail).getByTestId('explorer-open-full')).toHaveAttribute('href', expect.stringMatching(/\/p\/web-shop\/results\//));
    await user.click(within(detail).getByTestId('explorer-detail-close'));
    await waitFor(() => expect(screen.queryByTestId('explorer-detail')).toBeNull());
  });

  it('groups by a label preset with a (none) group and narrows by a label filter', { timeout: 40_000 }, async () => {
    renderLaunch('?view=owner');
    await waitFor(() => expect(screen.getAllByTestId('explorer-group').map((g) => g.getAttribute('title'))).toContain('team-checkout'), { timeout: 20_000 });
    const owners = screen.getAllByTestId('explorer-group').filter((g) => g.getAttribute('aria-level') === '1');
    expect(owners.map((g) => g.getAttribute('title')).every((t) => t?.startsWith('team-'))).toBe(true);
    expect(screen.getByTestId('explorer-view')).toHaveValue('Owner');
  });

  it('applies the filters in the URL to the tree and lists them as removable pills', { timeout: 40_000 }, async () => {
    const user = userEvent.setup();
    renderLaunch('?view=severity&severity=critical&retried=true');
    await waitFor(() => expect(screen.getAllByTestId('explorer-group').length).toBeGreaterThan(0), { timeout: 20_000 });
    const top = screen.getAllByTestId('explorer-group').filter((g) => g.getAttribute('aria-level') === '1');
    expect(top.map((g) => g.getAttribute('title'))).toEqual(['critical']);
    expect(screen.getByTestId('active-filter-severity')).toHaveTextContent('Severity: critical');
    expect(screen.getByTestId('active-filter-retried')).toBeInTheDocument();
    await user.click(screen.getByTestId('explorer-clear-filters'));
    await waitFor(() => expect(screen.queryByTestId('explorer-active-filters')).toBeNull());
    await waitFor(() => expect(screen.getAllByTestId('explorer-group').filter((g) => g.getAttribute('aria-level') === '1').length).toBeGreaterThan(1));
  });

  it('shows the filter values with counts and sets one', { timeout: 40_000 }, async () => {
    const user = userEvent.setup();
    renderLaunch();
    await screen.findAllByTestId('explorer-group', {}, { timeout: 20_000 });
    await user.click(screen.getByTestId('explorer-filters'));
    const menu = await screen.findByTestId('explorer-filters-menu');
    const critical = await within(menu).findByTestId('filter-severity-critical', {}, { timeout: 10_000 });
    expect(critical.closest('.mantine-Checkbox-root')?.textContent).toMatch(/critical\s*\d+/);
    await user.click(within(menu).getByTestId('filter-new-failures'));
    expect(await screen.findByTestId('active-filter-newFailures')).toBeInTheDocument();
  });

  it('switches the environment of the summary and the tree', { timeout: 40_000 }, async () => {
    const user = userEvent.setup();
    renderLaunch();
    const box = await screen.findByTestId('environments', {}, { timeout: 20_000 });
    const rows = within(box).getAllByTestId('environment-row');
    expect(rows.map((r) => r.getAttribute('data-env')).sort()).toEqual(['staging-chromium', 'staging-firefox']);
    await screen.findAllByTestId('explorer-group', {}, { timeout: 20_000 });
    const chip = within(box).getByTestId('environment-staging-firefox');
    const firefox = Number(/· (\d+)$/.exec(chip.parentElement?.textContent ?? '')?.[1]);
    expect(firefox).toBeGreaterThan(0);
    await user.click(chip);
    expect(chip).toBeChecked();
    expect(await screen.findByTestId('summary-scope')).toHaveTextContent('Results in staging-firefox');
    await waitFor(() => expect(screen.getByText(new RegExp(`^${String(firefox)} tests`))).toBeInTheDocument());
  });
});

