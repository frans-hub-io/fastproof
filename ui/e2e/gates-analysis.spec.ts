import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function analysisOn(page: Page, scheme = 'dark') {
  await page.addInitScript((s: string) => {
    try {
      localStorage.setItem('fastproof-mock-analysis', 'on');
      localStorage.setItem('franshub-color-scheme', s);
    } catch {
      /* ignore */
    }
  }, scheme);
}

test('the history shows verdicts per gate and launch, and filters by branch', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/gates');
  await page.getByTestId('tab-gate-history').click();
  await expect(page.getByTestId('gate-history-gate').first()).toBeVisible();
  const all = await page.getByTestId('gate-history-row').count();
  expect(all).toBeGreaterThan(0);
  await page.getByTestId('gate-history-branch-filter').fill('no-such-branch');
  await expect(page.getByText('No gate evaluations')).toBeVisible();
  await page.getByTestId('gate-history-branch-filter').fill('');
  await expect(page.getByTestId('gate-history-row').first()).toBeVisible();
});

test('a draft gate is simulated over stored launches', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/gates');
  await page.getByTestId('tab-gate-simulation').click();
  await expect(page.getByTestId('gate-simulation')).toBeVisible();
  await page.getByTestId('gate-simulation-from').click();
  await page.getByRole('option').first().click();
  await page.getByTestId('gate-simulation-launches').fill('10');
  await page.getByTestId('gate-simulation-run').click();
  await expect(page.getByTestId('gate-simulation-result')).toBeVisible();
  await expect(page.getByText(/would fail/)).toBeVisible();
});

test('the gate editor offers the new metrics only with Analysis', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('franshub-color-scheme', 'dark'));
  await page.goto('/p/web-shop/gates');
  await page.getByTestId('gate-add').click();
  await page.getByTestId('gate-rule-metric').first().click();
  await expect(page.getByRole('option', { name: 'New unique errors' })).toHaveCount(0);
  await page.keyboard.press('Escape');
  await page.keyboard.press('Escape');

  await analysisOn(page);
  await page.goto('/p/web-shop/gates');
  await page.getByTestId('gate-add').click();
  await page.getByTestId('gate-rule-metric').first().click();
  await expect(page.getByRole('option', { name: 'New unique errors' })).toBeVisible();
  await page.getByRole('option', { name: 'To investigate' }).click();
  await expect(page.getByText(/ReportPortal: To investigate/)).toBeVisible();
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`gate analytics screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await analysisOn(page, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: true });
      };
      await page.goto('/p/web-shop/gates');
      await page.getByTestId('tab-gate-history').click();
      await expect(page.getByTestId('gate-history-row').first()).toBeVisible();
      await shoot('gates-history');
      await page.getByTestId('tab-gate-simulation').click();
      await page.getByTestId('gate-simulation-from').click();
      await page.getByRole('option').first().click();
      await page.getByTestId('gate-simulation-run').click();
      await expect(page.getByTestId('gate-simulation-result')).toBeVisible();
      await shoot('gates-simulation');
      await page.getByRole('tab', { name: 'Gates' }).click();
      await page.getByTestId('gate-add').click();
      await page.getByTestId('gate-rule-metric').first().click();
      await shoot('gates-editor-metrics');
    });
  }
}
