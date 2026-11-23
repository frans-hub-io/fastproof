import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { renderApp } from '../test/render';

describe('defect types in settings', () => {
  it('has a copy button for the id of every type, for readers and writers alike', async () => {
    renderApp('/p/web-shop/settings?tab=defects');
    const rows = await screen.findAllByTestId('defect-type-row');
    expect(rows.length).toBeGreaterThanOrEqual(5);
    for (const row of rows) {
      expect(within(row).getByRole('button', { name: /^Copy the id of / })).toBeInTheDocument();
    }
    expect(within(rows[0] as HTMLElement).getByRole('button', { name: 'Copy the id of Product bug' })).toBeInTheDocument();
  });
});
