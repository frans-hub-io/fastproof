import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { manualLaunchId } from '../../mocks/tmFixtures';
import { renderApp } from '../../test/render';

describe('manual runner announcements', () => {
  it('announces step status changes politely with the step number and never the timer', async () => {
    const user = userEvent.setup();
    renderApp(`/p/web-shop/launches/${manualLaunchId()}/run`);
    const live = await screen.findByTestId('live-region', {}, { timeout: 20_000 });
    await screen.findAllByTestId('run-step', {}, { timeout: 20_000 });
    expect(live).toHaveAttribute('aria-live', 'polite');
    expect(live).toHaveAttribute('aria-atomic', 'true');

    const current = document.querySelector('[data-testid="run-step"][data-current="true"]');
    const path = current?.getAttribute('data-path') ?? '';
    expect(path).not.toBe('');
    await user.keyboard('f');
    await user.keyboard('{Escape}');
    await waitFor(() => expect(live).toHaveTextContent(`Step ${path}: failed`));

    const timer = screen.getByRole('timer');
    expect(live.contains(timer)).toBe(false);
    expect(timer.closest('[aria-live="polite"], [aria-live="assertive"]')).toBeNull();
    expect(timer).toHaveAttribute('aria-live', 'off');
    const before = live.textContent;
    const timerText = timer.textContent;
    await new Promise((r) => setTimeout(r, 2200));
    expect(screen.getByRole('timer').textContent).not.toBe(timerText);
    expect(live.textContent).toBe(before);
  });
});
