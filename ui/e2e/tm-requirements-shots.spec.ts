import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { spaGo, toastsGone } from './tm-reviews-helpers';

const dir = process.env.FASTPROOF_TM_SHOTS;

test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the requirements screenshots');

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await toastsGone(page);
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== 'running'));
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`requirements screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(240_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/requirements');
      await expect(page.getByTestId('requirement-row').first()).toBeVisible();
      await page.getByRole('button', { name: 'Expand REQ-1' }).click();
      await page.getByRole('button', { name: 'Expand REQ-2' }).click();
      await page.getByRole('button', { name: 'Expand REQ-6' }).click();
      await expect(page.locator('[data-testid="requirement-row"][data-number="3"]')).toBeVisible();
      await shoot(page, 'requirements-tree', width, scheme);

      await spaGo(page, '/p/web-shop/requirements/3');
      await expect(page.getByTestId('linked-case-row')).toHaveCount(2);
      await shoot(page, 'requirement', width, scheme);

      await spaGo(page, '/p/web-shop/coverage');
      await expect(page.getByTestId('coverage-row').first()).toBeVisible();
      await page.getByRole('button', { name: 'Expand REQ-6' }).click();
      await expect(page.locator('[data-testid="coverage-row"][data-number="7"]')).toBeVisible();
      await shoot(page, 'coverage-default', width, scheme);

      await spaGo(page, '/p/web-shop/coverage?scope=window&days=14&branch=main');
      await expect(page.getByTestId('scope-summary')).toContainText('last 14 days');
      await expect(page.getByTestId('coverage-row').first()).toBeVisible();
      await shoot(page, 'coverage-window', width, scheme);

      await spaGo(page, '/p/web-shop/requirements/7');
      await expect(page.getByTestId('linked-case-row').first()).toBeVisible();
      const first = await page.getByTestId('linked-case-row').first().getAttribute('data-case');
      await spaGo(page, `/p/web-shop/cases/${String(first)}?tab=requirements`);
      await expect(page.getByTestId('case-requirement-row').first()).toBeVisible();
      await shoot(page, 'case-tab', width, scheme);

      await spaGo(page, '/p/web-shop/dashboards');
      await page.getByRole('button', { name: 'New dashboard' }).click();
      await page.getByLabel('Name').fill('Coverage board');
      await page.getByRole('button', { name: 'Empty dashboard' }).click();
      await page.getByRole('button', { name: 'Add widget' }).click();
      await page.getByRole('menuitem', { name: 'Requirement coverage' }).click();
      await expect(page.getByTestId('requirement-coverage-bar')).toBeVisible();
      await page.getByRole('button', { name: 'Save' }).click();
      await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
      await shoot(page, 'dashboard-widget', width, scheme);
    });
  }
}
