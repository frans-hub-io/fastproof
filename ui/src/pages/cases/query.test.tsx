import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MantineProvider } from '@mantine/core';
import { Notifications } from '@mantine/notifications';
import { describe, expect, it, vi } from 'vitest';
import { renderWith } from '../../test/render';
import { QueryBar } from './QueryBar';
import { SavedQueries } from './SavedQueries';

const wrap = (ui: React.ReactElement) =>
  renderWith(
    <MantineProvider>
      <Notifications />
      {ui}
    </MantineProvider>,
  );

describe('SavedQueries', () => {
  it('lists personal and shared queries and applies one', async () => {
    const onApply = vi.fn();
    const user = userEvent.setup();
    wrap(<SavedQueries project="web-shop" query="status = active" onApply={onApply} />);
    await user.click(screen.getByTestId('saved-queries'));
    expect(await screen.findByText('My queries')).toBeInTheDocument();
    expect(screen.getByText('Shared')).toBeInTheDocument();
    await user.click(await screen.findByText('Drafts to finish'));
    expect(onApply).toHaveBeenCalledWith('status = draft');
  });

  it('saves the current query with a name', async () => {
    const user = userEvent.setup();
    wrap(<SavedQueries project="web-shop" query="tag = smoke" onApply={() => undefined} />);
    await user.click(screen.getByTestId('saved-queries'));
    await user.click(await screen.findByTestId('save-query'));
    const dialog = await screen.findByTestId('save-query-dialog');
    await user.type(within(dialog).getByTestId('save-query-name'), 'Smoke tests');
    await user.click(within(dialog).getByTestId('save-query-submit'));
    expect(await screen.findByText('Query saved')).toBeInTheDocument();
  });

  it('shows the error of a taken name', async () => {
    const user = userEvent.setup();
    wrap(<SavedQueries project="web-shop" query="tag = smoke" onApply={() => undefined} />);
    await user.click(screen.getByTestId('saved-queries'));
    await user.click(await screen.findByTestId('save-query'));
    const dialog = await screen.findByTestId('save-query-dialog');
    await user.type(within(dialog).getByTestId('save-query-name'), 'Drafts to finish');
    await user.click(within(dialog).getByTestId('save-query-submit'));
    expect(await screen.findByText('Query not saved')).toBeInTheDocument();
  });
});

describe('QueryBar', () => {
  it('shows the validation error under the bar', async () => {
    wrap(<QueryBar project="web-shop" value="colour = red" onChange={() => undefined} onApply={() => undefined} />);
    expect(await screen.findByTestId('query-error', undefined, { timeout: 5000 })).toHaveTextContent('unknown attribute colour');
  });

  it('shows the count for a good query', async () => {
    wrap(<QueryBar project="web-shop" value="status = active" onChange={() => undefined} onApply={() => undefined} />);
    await waitFor(() => expect(screen.getByTestId('query-count')).toHaveTextContent(/cases? match/), { timeout: 5000 });
  });

  it('shows the error of the list request while it matches the text', async () => {
    wrap(<QueryBar project="web-shop" value="" onChange={() => undefined} onApply={() => undefined} serverError={{ position: 0, length: 3, expected: ['=', 'in'], near: 'abc', message: 'unexpected abc' }} />);
    expect(await screen.findByTestId('query-error')).toHaveTextContent('unexpected abc. Expected: =, in.');
  });
});
