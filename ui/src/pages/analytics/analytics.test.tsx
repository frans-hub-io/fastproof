import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { formatSeconds, windowNote } from '../../api/analytics';
import { mockAnalytics, resetAnalytics } from '../../mocks/analytics';
import { renderApp } from '../../test/render';

afterEach(() => resetAnalytics());

describe('analytics', () => {
  it('shows every section from PostgreSQL with the banner, and regroups the failures', async () => {
    renderApp('/p/web-shop/analytics');
    const top = await screen.findByTestId('analytics-failures');
    expect(await screen.findByTestId('analytics-pg-banner')).toHaveTextContent('Last 30 days; longer history and the portfolio need ClickHouse.');
    expect(within(top).getAllByTestId('analytics-failure-row').length).toBe(5);
    expect(within(top).getByTestId('analytics-failures-backend')).toHaveTextContent('PostgreSQL, last 30 days');
    for (const id of ['analytics-defects', 'analytics-flaky', 'analytics-mttr', 'analytics-retries', 'analytics-slow-steps', 'analytics-durations']) {
      expect(await screen.findByTestId(id)).toBeInTheDocument();
    }
    expect(screen.getByTestId('analytics-mttr-p50')).toHaveTextContent('1.6 d');
    expect(screen.getAllByTestId('analytics-duration-row')).toHaveLength(3);
    fireEvent.click(screen.getByTestId('analytics-group-by'));
    fireEvent.click(await screen.findByRole('option', { name: 'Defect group' }));
    await waitFor(() => expect(within(screen.getByTestId('analytics-failures')).getByText('To investigate')).toBeInTheDocument());
  });

  it('asks for ClickHouse in the portfolio without it and lists projects with it', async () => {
    renderApp('/p/web-shop/analytics?tab=portfolio');
    expect(await screen.findByTestId('analytics-portfolio-needs-clickhouse')).toBeInTheDocument();
  });

  it('lists the portfolio with ClickHouse and keeps the banner away', async () => {
    mockAnalytics.clickhouse = true;
    renderApp('/p/web-shop/analytics?tab=portfolio&days=90');
    const t = await screen.findByTestId('analytics-portfolio');
    expect(within(t).getAllByTestId('analytics-portfolio-row').length).toBeGreaterThan(1);
    expect(within(t).getByTestId('analytics-portfolio-backend')).toHaveTextContent('ClickHouse');
    expect(screen.queryByTestId('analytics-pg-banner')).not.toBeInTheDocument();
  });

  it('formats windows and seconds', () => {
    expect(windowNote(undefined)).toBe('');
    expect(windowNote({ backend: 'postgres', from: '', to: '', partial: true })).toBe('PostgreSQL, last 30 days (partial)');
    expect(windowNote({ backend: 'clickhouse', from: '', to: '', partial: false })).toBe('ClickHouse');
    expect(formatSeconds(null)).toBe('—');
    expect(formatSeconds(45)).toBe('45 s');
    expect(formatSeconds(600)).toBe('10 min');
    expect(formatSeconds(5400)).toBe('1.5 h');
    expect(formatSeconds(140_000)).toBe('1.6 d');
    expect(formatSeconds(1_300_000)).toBe('15 d');
  });
});
