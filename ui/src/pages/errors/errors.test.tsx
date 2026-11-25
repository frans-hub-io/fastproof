import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { formatSpan, isFingerprint, type ErrorList } from '../../api/errors';
import { request } from '../../api/client';
import { resetErrors } from '../../mocks/errors';
import { renderApp } from '../../test/render';

afterEach(() => resetErrors());

async function list(q = ''): Promise<ErrorList> {
  return request<ErrorList>(`/api/v1/projects/web-shop/errors${q}`);
}

describe('unique errors', () => {
  it('lists errors with their state, branches and sparkline, and filters them', async () => {
    renderApp('/p/web-shop/errors');
    const table = await screen.findByTestId('errors-table');
    const rows = within(table).getAllByTestId('error-row');
    expect(rows.length).toBe(8);
    expect(within(rows[0] as HTMLElement).getByText('Regressed')).toBeInTheDocument();
    expect(within(table).getAllByTestId('error-spark')).toHaveLength(8);
    expect(within(table).getByText('PAY-1234')).toBeInTheDocument();
    expect(within(table).getAllByText('other branches').length).toBeGreaterThan(0);
    fireEvent.change(screen.getByLabelText('Search messages'), { target: { value: 'ENOTFOUND' } });
    await waitFor(() => expect(screen.getAllByTestId('error-row')).toHaveLength(1));
    expect(within(screen.getByTestId('error-row')).getByText('Ignored')).toBeInTheDocument();
  });

  it('shows one error and ignores, activates and merges it', async () => {
    const first = (await list('?state=active')).items[0];
    const other = (await list('?state=new')).items[0];
    expect(first && other).toBeTruthy();
    renderApp(`/p/web-shop/errors/${first?.fingerprint ?? ''}`);
    expect(await screen.findByTestId('error-detail')).toBeInTheDocument();
    expect(screen.getByTestId('error-timeline')).toBeInTheDocument();
    expect(screen.getByTestId('error-branches')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('error-ignore'));
    await screen.findByTestId('error-activate');
    expect((await list('?state=ignored')).items.map((e) => e.fingerprint)).toContain(first?.fingerprint);
    fireEvent.click(screen.getByTestId('error-activate'));
    await screen.findByTestId('error-ignore');
    fireEvent.change(screen.getByTestId('error-merge-target'), { target: { value: other?.fingerprint ?? '' } });
    fireEvent.click(screen.getByTestId('error-merge'));
    expect(await screen.findByTestId('error-alias-of')).toBeInTheDocument();
    const after = await list();
    expect(after.items.map((e) => e.fingerprint)).not.toContain(first?.fingerprint);
    expect(after.items.find((e) => e.fingerprint === other?.fingerprint)?.aliases).toBe(1);
    fireEvent.click(screen.getByTestId('error-split'));
    await waitFor(() => expect(screen.queryByTestId('error-alias-of')).not.toBeInTheDocument());
  });

  it('answers 404 for an unknown error and formats spans', async () => {
    renderApp('/p/web-shop/errors/0123456789abcdef');
    expect(await screen.findByText('Error not found')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Unique error' })).toBeInTheDocument();
    expect(isFingerprint('0123456789abcdef')).toBe(true);
    expect(isFingerprint('0123456789ABCDEF')).toBe(false);
    expect(formatSpan(null)).toBe('—');
    expect(formatSpan(90)).toBe('1 min');
    expect(formatSpan(3 * 3600 + 600)).toBe('3 h 10 min');
    expect(formatSpan(2 * 86400 + 3600)).toBe('2 d 1 h');
  });
});
