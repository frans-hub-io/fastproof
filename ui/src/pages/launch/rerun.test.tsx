import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { launchView, projectLaunches } from '../../mocks/fixtures';
import { renderApp } from '../../test/render';

function failingLaunch() {
  const d = projectLaunches('web-shop').find((x) => x.launch.number === 41);
  if (!d) throw new Error('no failing launch in the fixtures');
  return d;
}

describe('rerun from a launch', { timeout: 60000 }, () => {
  it('starts a rerun of the failed tests and lists it', async () => {
    const d = failingLaunch();
    renderApp(`/p/web-shop/launches/${d.launch.id}`);
    await userEvent.click(await screen.findByTestId('open-rerun', {}, { timeout: 15000 }));
    const modal = await screen.findByTestId('rerun-modal');
    const counts = launchView(d).counts;
    const expected = counts.failed + counts.broken;
    expect(within(modal).getByTestId('rerun-count')).toHaveTextContent(`${String(expected)} test`);
    await userEvent.click(within(modal).getByTestId('rerun-start'));
    const done = await within(modal).findByTestId('rerun-done');
    expect(done).toHaveTextContent('rerun-');
  });

  it('reruns selected results', async () => {
    const d = failingLaunch();
    renderApp(`/p/web-shop/launches/${d.launch.id}?tab=results`);
    await userEvent.click(await screen.findByTestId('rerun-pick-mode', {}, { timeout: 15000 }));
    const rows = await screen.findAllByTestId('result-row', {}, { timeout: 15000 });
    await userEvent.click(rows[0] as HTMLElement);
    await waitFor(() => expect(screen.getByTestId('rerun-pick-bar')).toHaveTextContent('1 selected'));
    await userEvent.click(screen.getByTestId('rerun-selected'));
    const modal = await screen.findByTestId('rerun-modal');
    expect(within(modal).getByTestId('rerun-count')).toHaveTextContent('1 test will run again');
    await userEvent.click(within(modal).getByTestId('rerun-start'));
    expect(await within(modal).findByTestId('rerun-done')).toBeInTheDocument();
  });
});
