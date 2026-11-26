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

test('flaky causes: chips, evidence panel, mute with an end and a rule draft', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/flaky');
  await expect(page.getByTestId('flaky-table')).toBeVisible();
  await expect(page.getByRole('columnheader', { name: 'Likely cause' })).toBeVisible();
  const timing = page.getByTestId('flaky-row').filter({ has: page.locator('[data-testid="hint-chip"][data-kind="timing"]') }).first();
  await timing.getByTestId('hint-chips').click();
  const panel = page.getByTestId('hint-panel');
  await expect(panel).toBeVisible();
  const card = panel.locator('[data-testid="hint-card"][data-kind="timing"]');
  await expect(card.getByTestId('hint-summary')).toContainText('time limit');
  await expect(card.getByTestId('hint-evidence')).toContainText('Ceiling (ms)');
  await card.getByTestId('hint-rule').click();
  await expect(card.getByTestId('hint-rule-created')).toContainText('disabled');
  await card.getByTestId('hint-mute').click();
  const dialog = page.getByTestId('hint-mute-dialog');
  await expect(dialog.getByTestId('hint-mute-reason')).toHaveValue(/^Flaky \(timing\): /);
  await dialog.getByText('14 days').click();
  await dialog.getByTestId('hint-mute-submit').click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText('Muted', { exact: true })).toBeVisible();
  await card.getByTestId('hint-rule-created').getByRole('link', { name: 'Review it in Rules' }).click();
  await expect(page.getByText(/^Flaky timing: /).first()).toBeVisible();
});

test('flaky causes: the minimum score setting', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/settings?tab=analysis');
  const field = page.getByTestId('analysis-hint-min-score');
  await expect(field).toBeVisible();
  await field.fill('0.01');
  await expect(page.getByText('0.05 to 1', { exact: true })).toBeVisible();
  await expect(page.getByTestId('analysis-save')).toBeDisabled();
  await field.fill('0.35');
  await page.getByTestId('analysis-save').click();
  await expect(page.getByText('Analysis settings saved')).toBeVisible();
  await expect(page.getByTestId('analysis-hint-min-score')).toHaveValue('0.35');
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`flaky causes screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await analysisOn(page, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: true });
      };
      await page.goto('/p/web-shop/flaky');
      await expect(page.getByTestId('hint-chips').first()).toBeVisible();
      await shoot('flaky-list');
      if (width < 500) {
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
        expect(overflow).toBeLessThanOrEqual(0);
      }
      await page.getByTestId('flaky-row').filter({ has: page.locator('[data-testid="hint-chip"][data-kind="infra"]') }).first().getByTestId('hint-chips').click();
      await expect(page.getByTestId('hint-panel')).toBeVisible();
      await shoot('flaky-panel');
      await page.goto('/p/web-shop/settings?tab=analysis');
      await expect(page.getByTestId('analysis-hint-min-score')).toBeVisible();
      await shoot('flaky-settings');
    });
  }
}
