import { expect, test } from '@playwright/test';
import { MOD, openManualLaunch } from './tm-helpers';

test('a manual run done with the keyboard only, from the first to the last case', async ({ page }) => {
  const launchId = await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await expect(page.getByTestId('manual-tab')).toBeVisible();
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  const queue = page.getByTestId('queue-item');
  await expect(queue).toHaveCount(4);
  const live = page.getByTestId('live-region');

  await page.keyboard.press('Shift+?');
  await expect(page.getByTestId('runner-help')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('runner-help')).toHaveCount(0);

  // Case 1 (in progress, its first step passed): fail the current step with an actual result, finish with the suggestion.
  const steps = page.getByTestId('run-step');
  const current = page.locator('[data-testid="run-step"][data-current="true"]');
  const path = (await current.getAttribute('data-path')) ?? '';
  await page.keyboard.press('f');
  await expect(page.getByTestId('step-actual')).toBeFocused();
  await page.keyboard.type('The pay button stays disabled');
  await page.keyboard.press('Escape');
  await expect(live).toHaveText(`Step ${path}: failed`);
  await page.keyboard.press('j');
  await page.keyboard.press('k');
  await expect(current).toHaveAttribute('data-path', path);
  await expect(current).toHaveAttribute('data-status', 'failed');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(page.getByTestId('finish-ask')).toBeVisible();
  await expect(page.getByTestId('finish-ask-confirm')).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('finish-ask')).toHaveCount(0);
  await expect(live).toContainText(/finished: failed\. Next: TC-\d+/);
  await expect(queue.nth(0)).toHaveAttribute('data-status', 'failed');

  // Case 2: p on every step (each p moves to the next step).
  await expect(queue.nth(1)).toHaveAttribute('aria-current', 'true');
  await expect(current).toBeVisible();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  const count = await steps.count();
  for (let i = 0; i < count; i++) await page.keyboard.press('p');
  await expect(page.getByTestId('finish-panel')).toContainText(/Suggested:\s*Passed/);
  await expect(page.getByTestId('finish-panel')).not.toContainText('untested');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(queue.nth(1)).toHaveAttribute('data-status', 'passed');

  // Case 3: block the first step, pass the rest.
  await expect(queue.nth(2)).toHaveAttribute('aria-current', 'true');
  await expect(current).toBeVisible();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await page.keyboard.press('b');
  await page.keyboard.press('Shift+P');
  await expect(live).toContainText('remaining steps passed');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(queue.nth(2)).toHaveAttribute('data-status', 'blocked');

  // Case 4: skip with n and come back with Shift+N; then pass everything.
  await expect(queue.nth(3)).toHaveAttribute('aria-current', 'true');
  await expect(current).toBeVisible();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await page.keyboard.press('Shift+N');
  await expect(queue.nth(2)).toHaveAttribute('aria-current', 'true');
  await page.keyboard.press('n');
  await expect(queue.nth(3)).toHaveAttribute('aria-current', 'true');
  await expect(current).toBeVisible();
  await page.keyboard.press('Shift+P');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(live).toContainText('All cases in the queue are done');
  await expect(queue.nth(3)).toHaveAttribute('data-status', 'passed');
  await expect(page.getByTestId('save-state')).toHaveAttribute('data-state', 'saved');

  await page.getByRole('link', { name: /^#38 / }).click();
  await expect(page).toHaveURL(new RegExp(`/launches/${launchId}\\?tab=manual`));
  const mine = page.getByTestId('manual-assignees').getByRole('row').filter({ hasText: 'alex@example.com' });
  await expect(mine).toContainText('alex@example.com');
  await expect(mine.getByRole('cell').nth(2)).toHaveText('2');
  await expect(mine.getByRole('cell').nth(7)).toHaveText('0');
});

test('single-key shortcuts can be turned off; saves retry after a network failure', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await page.getByTestId('open-help').click();
  await page.getByTestId('single-keys-toggle').uncheck();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('runner-help')).toHaveCount(0);
  const current = page.locator('[data-testid="run-step"][data-current="true"]');
  const path = await current.getAttribute('data-path');
  await page.keyboard.press('p');
  await expect(current).toHaveAttribute('data-status', 'untested');
  await page.keyboard.press('ArrowDown');
  await expect(page.locator('[data-testid="run-step"][data-current="true"]')).not.toHaveAttribute('data-path', path ?? '');

  await page.evaluate(() => (window as unknown as { __fastproofMockTm: { failSteps: (n: number) => void } }).__fastproofMockTm.failSteps(1));
  await page.locator('[data-testid="run-step"][data-current="true"]').getByTestId('set-passed').click();
  await expect(page.getByTestId('save-state')).toHaveAttribute('data-state', 'retrying');
  await expect(page.locator('[data-testid="run-step"][data-current="true"], [data-testid="run-step"][data-status="passed"]').first()).toBeVisible();
  await page.getByRole('button', { name: 'Retry now' }).click();
  await expect(page.getByTestId('save-state')).toHaveAttribute('data-state', 'saved');
});

test('quick defect from a failed step with the new-issue link', async ({ page, context }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await page.keyboard.press('f');
  await page.keyboard.type('Blank page after Pay');
  await page.keyboard.press('Escape');
  await page.keyboard.press('d');
  const drawer = page.getByTestId('defect-drawer');
  await expect(drawer.getByTestId('defect-comment')).toHaveValue(/failed: .*\n.*\nActual: Blank page after Pay/);
  await context.route('https://gitlab.example.com/**', (r) => r.fulfill({ body: 'new issue' }));
  const popupP = page.waitForEvent('popup');
  await drawer.getByTestId('defect-new-issue').click();
  const popup = await popupP;
  expect(popup.url()).toContain('https://gitlab.example.com/shop/web-shop/-/issues/new?');
  expect(new URL(popup.url()).searchParams.get('issue[description]')).toContain('Actual: Blank page after Pay');
  await popup.close();
  await drawer.getByTestId('defect-issue').fill('https://gitlab.example.com/shop/web-shop/-/issues/77');
  await drawer.getByTestId('defect-save').click();
  await expect(page.getByText('Defect saved')).toBeVisible();
});

test('launch shows manual results with a hand icon and a source filter; close and reopen the run', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('manual-icon').first()).toBeVisible();
  await page.getByTestId('source-filter').getByText('Manual').click();
  await expect(page).toHaveURL(/source=manual/);
  const rows = page.getByTestId('result-row');
  await expect(rows.first()).toBeVisible();
  for (const r of (await rows.all()).slice(0, 10)) await expect(r.getByTestId('manual-icon')).toBeVisible();
  await page.getByTestId('source-filter').getByText('Automated').click();
  await expect(rows.first()).toBeVisible();
  await expect(page.getByTestId('result-row').getByTestId('manual-icon')).toHaveCount(0);

  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-close').click();
  await page.getByTestId('manual-close-confirm').click();
  await expect(page.getByText('Manual run closed')).toBeVisible();
  await expect(page.getByTestId('manual-reopen')).toBeVisible();
  await expect(page.getByTestId('manual-done')).toHaveText(/^(\d+) \/ \1$/);
  await page.getByTestId('manual-reopen').click();
  await expect(page.getByTestId('manual-close')).toBeVisible();
});
