import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichLaunch } from './helpers';

const dir = process.env.FASTPROOF_RERUN_SHOTS;

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  if (!dir) return;
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(dir, `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`rerun ${String(width)} ${scheme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await openRichLaunch(page);
      await expect(page.getByTestId('open-rerun')).toBeVisible();
      await shoot(page, 'launch', width, scheme, false);
      await page.getByTestId('open-rerun').click();
      const modal = page.getByTestId('rerun-modal');
      await expect(modal).toBeVisible();
      await shoot(page, 'rerun-modal', width, scheme, false);
      await modal.getByTestId('rerun-start').click();
      await expect(modal.getByTestId('rerun-done')).toBeVisible();
      await shoot(page, 'rerun-done', width, scheme, false);
      await modal.getByRole('button', { name: 'Close' }).click();
      await expect(page.getByTestId('rerun-row').first()).toBeVisible();
      await shoot(page, 'launch-reruns', width, scheme, false);

      await page.getByRole('tab', { name: 'Results' }).click();
      await page.getByTestId('rerun-pick-mode').click();
      await page.getByTestId('result-row').first().click();
      await expect(page.getByTestId('rerun-pick-bar')).toContainText('1 selected');
      await shoot(page, 'select', width, scheme, false);

      await page.goto('/p/web-shop/settings?tab=integrations');
      await expect(page.getByTestId('rerun-enabled')).toBeVisible();
      await page.getByTestId('rerun-enabled').scrollIntoViewIfNeeded();
      await shoot(page, 'settings', width, scheme);
    });
  }
}
