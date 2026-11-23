import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { RICH_LAUNCH_ID, launchResults } from '../mocks/fixtures';
import { server } from '../mocks/node';
import { analyze } from '../mocks/stageC';
import { renderApp } from '../test/render';

const ESC = '\u001b';
const COLORED = `Error: ${ESC}[2mexpect(${ESC}[22m${ESC}[31mlocator${ESC}[39m).toBeVisible()\n\nCall log:`;
const PLAIN = 'Error: expect(locator).toBeVisible()';

function coloredResults() {
  return launchResults(RICH_LAUNCH_ID)
    .map((r) => analyze('web-shop', r))
    .filter((r) => r.status === 'failed' || r.status === 'broken')
    .slice(0, 3)
    .map((r) => ({ ...r, message: COLORED }));
}

describe('ANSI escape codes in message lists', () => {
  it('are not shown in the launch result list', async () => {
    server.use(http.get('/api/v1/projects/:key/launches/:id/results', () => HttpResponse.json({ items: coloredResults(), next: null })));
    renderApp(`/p/web-shop/launches/${RICH_LAUNCH_ID}?tab=results`);
    const rows = await screen.findAllByTestId('result-row', {}, { timeout: 20_000 });
    expect(rows.length).toBe(3);
    for (const row of rows) {
      expect(within(row).getByText(PLAIN)).toBeInTheDocument();
      expect(row.textContent).not.toContain(ESC);
      expect(row.textContent).not.toMatch(/\[\d+m/);
    }
  });

  it('are not shown in Similar failures', async () => {
    const user = userEvent.setup();
    const [first] = coloredResults();
    server.use(
      http.get('/api/v1/projects/:key/results/:id/similar', () =>
        HttpResponse.json({ items: [{ resultId: first?.id, launchId: RICH_LAUNCH_ID, launchNumber: 40, name: 'other test', message: COLORED, score: 1, defect: null }] }),
      ),
    );
    renderApp(`/p/web-shop/launches/${RICH_LAUNCH_ID}?tab=results`);
    const rows = await screen.findAllByTestId('result-row', {}, { timeout: 20_000 });
    await user.click(rows[0] as HTMLElement);
    await waitFor(() => expect(screen.getByText('Similar failures')).toBeInTheDocument(), { timeout: 20_000 });
    const card = screen.getByText('Similar failures').closest('[data-testid], .mantine-Card-root') ?? document.body;
    await waitFor(() => expect(within(card as HTMLElement).getByText(/^Error: expect\(locator\)\.toBeVisible\(\)/)).toBeInTheDocument(), { timeout: 20_000 });
    expect(card.textContent).not.toContain(ESC);
    expect(card.textContent).not.toMatch(/\[\d+m/);
  });
});
