import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { RICH_LAUNCH_ID } from '../mocks/fixtures';
import { setMockRole } from '../mocks/handlers';
import { renderApp } from '../test/render';

describe('pages', () => {
  it('lists projects with their last launch', async () => {
    renderApp('/');
    const cards = await screen.findAllByTestId('project-card');
    expect(cards).toHaveLength(3);
    expect(within(cards[0] as HTMLElement).getByText(/#42 web-shop nightly/)).toBeInTheDocument();
    expect(within(cards[2] as HTMLElement).getByText(/No launches yet/)).toBeInTheDocument();
  });

  it('shows launches with live progress for the running one', async () => {
    renderApp('/p/web-shop');
    const rows = await screen.findAllByTestId('launch-row');
    expect(rows.length).toBe(30);
    expect(within(rows[0] as HTMLElement).getByTestId('launch-progress')).toHaveTextContent(/Running/);
  });

  it('moves the selection with j and k', async () => {
    const user = userEvent.setup();
    renderApp('/p/web-shop');
    await screen.findAllByTestId('launch-row');
    await user.keyboard('jj');
    await waitFor(() => expect(screen.getAllByTestId('launch-row')[1]).toHaveAttribute('data-selected', 'true'));
    await user.keyboard('k');
    await waitFor(() => expect(screen.getAllByTestId('launch-row')[0]).toHaveAttribute('data-selected', 'true'));
  });

  it('shows a launch with counts, failed upload, results and failure groups', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${RICH_LAUNCH_ID}`);
    expect(await screen.findByText('#41 web-shop nightly')).toBeInTheDocument();
    expect(screen.getByTestId('stat-total')).toHaveTextContent('200');
    expect(await screen.findByTestId('upload-error')).toHaveTextContent('limit_exceeded');
    expect(screen.getByTestId('ci-link')).toHaveAttribute('href', expect.stringContaining('/-/pipelines/'));
    expect(screen.getByTestId('commit-link')).toHaveAttribute('href', expect.stringContaining('/-/commit/'));
    await waitFor(() => expect(screen.getAllByTestId('result-row').length).toBe(200));
    await user.click(screen.getByTestId('chip-failed'));
    await waitFor(() => expect(screen.getAllByTestId('result-row').length).toBeLessThan(30));
    await user.click(screen.getByRole('tab', { name: /Failures/ }));
    const groups = await screen.findAllByTestId('cluster');
    expect(groups.length).toBeGreaterThan(1);
  });

  it('shows a result with attempts, steps, history and links', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${RICH_LAUNCH_ID}?q=pays+with+saved+card`);
    await user.click(await screen.findByText('pays with saved card'));
    expect(await screen.findByRole('tab', { name: /Attempt 2/ })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByTestId('history-strip').querySelector('[data-current="true"]')).not.toBeNull();
    expect(screen.getByTestId('step-tree')).toBeInTheDocument();
    expect(screen.getByTestId('description').querySelector('script')).toBeNull();
    expect(screen.getAllByTestId('diff-view').length).toBeGreaterThan(0);
    await user.click(screen.getByRole('tab', { name: /Attachments/ }));
    expect(screen.getAllByTestId('attachment-item').length).toBeGreaterThan(10);
    await user.click(screen.getByRole('tab', { name: /Output/ }));
    expect(screen.getByTestId('stdout')).toHaveTextContent('Starting browser chromium');
  });

  it('shows the test case history with filters', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${RICH_LAUNCH_ID}?q=pays+with+saved+card`);
    await user.click(await screen.findByText('pays with saved card'));
    await user.click(await screen.findByTestId('test-case-link'));
    const rows = await screen.findAllByTestId('history-row');
    expect(rows.length).toBe(42);
    await user.type(screen.getByPlaceholderText('All branches'), 'feature/checkout-v2');
    await waitFor(() => expect(screen.getAllByTestId('history-row').length).toBeLessThan(42));
  });

  it('creates a token and shows the secret once', async () => {
    const user = userEvent.setup();
    renderApp('/settings/tokens');
    await screen.findAllByTestId('token-row');
    await user.click(screen.getByTestId('create-token'));
    await user.type(await screen.findByPlaceholderText('ci-web-shop'), 'ci-new');
    await user.click(screen.getByPlaceholderText('Pick projects'));
    await user.click(await screen.findByRole('option', { name: 'web-shop' }));
    await user.click(screen.getByTestId('create-token-submit'));
    const value = await screen.findByTestId('token-value');
    expect(value.textContent).toMatch(/^fp_[0-9a-f]{64}$/);
    await user.click(screen.getByRole('button', { name: 'I have stored it' }));
    await waitFor(() => expect(screen.queryByTestId('token-value')).toBeNull());
    expect(screen.getByText('ci-new')).toBeInTheDocument();
  });

  it('revokes a token after confirmation', async () => {
    const user = userEvent.setup();
    renderApp('/settings/tokens');
    await screen.findAllByTestId('token-row');
    await user.click(screen.getByRole('button', { name: 'Revoke ci-payments' }));
    await user.click(await screen.findByTestId('confirm-revoke'));
    await waitFor(() => expect(screen.queryByText('ci-payments')).toBeNull());
  });

  it('shows the audit log to admins only', async () => {
    renderApp('/settings/audit');
    expect((await screen.findAllByTestId('audit-row')).length).toBe(100);
  });

  it('denies the audit log to viewers', async () => {
    setMockRole('viewer');
    renderApp('/settings/audit');
    expect(await screen.findByText('Administrators only')).toBeInTheDocument();
  });

  it('shows not found for unknown launches', async () => {
    renderApp('/p/web-shop/launches/00000000-0000-7000-8000-000000000000');
    expect(await screen.findByText('Launch not found')).toBeInTheDocument();
  });
});
