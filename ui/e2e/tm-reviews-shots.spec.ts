import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { enableReview, spaGo, toastsGone } from './tm-reviews-helpers';

const dir = process.env.FASTPROOF_TM_SHOTS;

test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the workflow and review screenshots');

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await toastsGone(page);
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`review screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(240_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/reviews');
      await expect(page.getByText('Review is off for this project').first()).toBeVisible();
      await shoot(page, 'reviews-off', width, scheme, false);
      await enableReview(page);
      await shoot(page, 'workflow-settings', width, scheme);
      await page.getByTestId('status-category').first().click();
      await page.getByRole('option', { name: 'Outdated' }).click();
      await page.getByTestId('workflow-save').click();
      await expect(page.getByTestId('remap-dialog')).toBeVisible();
      await shoot(page, 'workflow-remap', width, scheme, false);
      await page.keyboard.press('Escape');
      await spaGo(page, '/p/web-shop/reviews');
      await expect(page.getByTestId(width < 500 ? 'review-card' : 'review-row').first()).toBeVisible();
      await shoot(page, 'reviews-queue', width, scheme, false);
      await spaGo(page, '/p/web-shop/cases/1');
      await expect(page.getByTestId('state-chip')).toBeVisible();
      await page.getByTestId('state-menu').click();
      await expect(page.getByTestId('state-option').first()).toBeVisible();
      await shoot(page, 'state-menu', width, scheme, false);
      await page.keyboard.press('Escape');
      await spaGo(page, '/p/web-shop/cases/1?tab=review');
      await expect(page.getByTestId('review-tab')).toBeVisible();
      await expect(page.getByTestId('version-diff')).toBeVisible();
      await shoot(page, 'case-review', width, scheme);
      await spaGo(page, '/p/web-shop/reviews');
      await page.getByTestId(width < 500 ? 'review-card' : 'review-row').filter({ hasText: /v3\s*→\s*v4/ }).first().getByTestId('review-link').click();
      await expect(page.getByTestId('review-tab')).toBeVisible();
      await expect(page.getByTestId('version-diff')).toBeVisible();
      await shoot(page, 'case-review-diff', width, scheme);
      await spaGo(page, '/p/web-shop/reviews?view=batch');
      await page.getByTestId('batch-load').click();
      await expect(page.getByTestId('version-diff').first()).toBeVisible();
      await shoot(page, 'reviews-batch', width, scheme, false);
    });
  }
}
