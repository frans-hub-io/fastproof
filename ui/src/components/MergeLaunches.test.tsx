import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { server } from '../mocks/node';
import { renderApp } from '../test/render';

async function pickTwoFinished(): Promise<void> {
  const rows = await screen.findAllByTestId('launch-row');
  const finished = rows.filter((r) => !within(r).queryByTestId('launch-progress'));
  expect(finished.length).toBeGreaterThanOrEqual(2);
  for (const r of finished.slice(0, 2)) fireEvent.click(within(r).getByTestId('launch-select'));
}

describe('merging launches', () => {
  it('selects launches, previews the merge, merges and offers undo on the target', { timeout: 60_000 }, async () => {
    renderApp('/p/web-shop');
    await pickTwoFinished();
    expect(screen.getByTestId('merge-bar')).toHaveTextContent('2 selected');
    fireEvent.click(screen.getByTestId('merge-open'));
    const preview = await screen.findByTestId('merge-preview');
    expect(preview).toHaveTextContent(/attempts of .* tests from 2 launches/);
    expect(screen.getByText(/you can undo the merge until/i)).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('merge-submit'));
    const banner = await screen.findByTestId('merged-from', undefined, { timeout: 20_000 });
    expect(banner).toHaveTextContent(/Merged from #\d+/);
    fireEvent.click(within(banner).getByTestId('undo-merge'));
    await waitFor(() => expect(screen.queryByTestId('merged-from')).not.toBeInTheDocument());
  });

  it('says why the server refuses a merge and keeps the button disabled', async () => {
    server.use(
      http.post('/api/v1/projects/:key/launches/merge', () =>
        HttpResponse.json({ code: 'merge_mixed', message: 'launches of different branches are not merged', details: { field: 'branch' } }, { status: 409 }),
      ),
    );
    renderApp('/p/web-shop');
    await pickTwoFinished();
    fireEvent.click(screen.getByTestId('merge-open'));
    const refused = await screen.findByTestId('merge-refused');
    expect(refused).toHaveTextContent('Only launches of the same branch');
    expect(screen.getByTestId('merge-submit')).toBeDisabled();
  });

  it('asks for a clear confirmation of a merge without undo', async () => {
    renderApp('/p/web-shop');
    await pickTwoFinished();
    fireEvent.click(screen.getByTestId('merge-open'));
    await screen.findByTestId('merge-preview');
    fireEvent.click(screen.getByTestId('merge-final'));
    expect(await screen.findByText(/This cannot be undone/)).toBeInTheDocument();
    expect(screen.getByTestId('merge-submit')).toHaveTextContent('Merge and delete');
  });
});
