import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const dir = process.env.FASTPROOF_TM_SHOTS;

test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the review screenshots');

async function shoot(page: Page, name: string, width: number, scheme: string) {
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`) });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`query shots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(120_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      const bar = page.getByTestId('query-bar');
      await page.goto('/p/web-shop/cases?query=status%20%3D%20active%20and%20automated%20%3D%20false');
      await expect(bar).toBeVisible();
      await expect(page.getByTestId('query-count')).toBeVisible();
      await shoot(page, 'cases-query', width, scheme);
      await bar.click();
      await page.keyboard.press('End');
      await page.keyboard.type(' and field["Priority"] in [');
      await expect(page.locator('.cm-tooltip-autocomplete')).toBeVisible();
      await shoot(page, 'cases-query-completion', width, scheme);
      await page.keyboard.press('Escape');
      await page.keyboard.press('ControlOrMeta+a');
      await page.keyboard.type('status = active and colour = red or (tag = "smoke"');
      await page.keyboard.press('Escape');
      await expect(page.getByTestId('query-error')).toBeVisible();
      await shoot(page, 'cases-query-error', width, scheme);
      await page.goto('/p/web-shop/plans');
      await page.getByTestId('plan-card').filter({ hasText: 'Checkout smoke' }).click();
      await page.getByTestId('plan-filter-open').click();
      const dialog = page.getByTestId('filter-dialog');
      await dialog.getByTestId('filter-dialog-mode').getByText('Query', { exact: true }).click();
      await dialog.getByTestId('query-bar').click();
      await page.keyboard.type('status in [draft, active] and lastRunAt < now() - 14d and tag = "smoke"');
      await page.keyboard.press('Escape');
      await expect(dialog.getByTestId('query-count')).toBeVisible();
      await shoot(page, 'plan-dialog-query', width, scheme);
    });
  }
}
