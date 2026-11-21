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
});
