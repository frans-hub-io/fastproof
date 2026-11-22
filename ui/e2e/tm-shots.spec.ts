import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openCase, openManualLaunch } from './tm-helpers';

const dir = process.env.FASTPROOF_TM_SHOTS;

test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the review screenshots');

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(180_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/cases');
      await expect(page.getByTestId(width < 500 ? 'case-card' : 'case-row').first()).toBeVisible();
      await shoot(page, 'cases', width, scheme, false);
      await openCase(page, 'Pay with a saved card');
      await shoot(page, 'case', width, scheme);
      await page.getByTestId('case-edit').click();
      await shoot(page, 'case-edit', width, scheme);
      await page.getByTestId('case-cancel').click();
      await page.getByRole('tab', { name: /Versions/ }).click();
      await expect(page.getByTestId('version-diff')).toBeVisible();
      await shoot(page, 'case-versions', width, scheme);
      await openCase(page, 'Apply a discount code at checkout');
      await expect(page.getByTestId('suggestion')).toBeVisible();
      await shoot(page, 'case-suggestion', width, scheme);
      await page.goto('/p/web-shop/plans');
      await expect(page.getByTestId('plan-card').first()).toBeVisible();
      await shoot(page, 'plans', width, scheme);
      await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
      await expect(page.getByTestId('plan-item').first()).toBeVisible();
      await shoot(page, 'plan', width, scheme);
      await openManualLaunch(page);
      await page.getByTestId('tab-manual').click();
      await expect(page.getByTestId('manual-items')).toBeVisible();
      await shoot(page, 'launch-manual', width, scheme);
      await page.getByTestId('continue-testing').click();
      await expect(page.getByTestId('run-case-title')).toBeVisible();
      await shoot(page, 'runner', width, scheme);
      await page.goto('/p/web-shop/cases/import');
      await page.getByTestId('import-file').setInputFiles(path.join(import.meta.dirname, 'fixtures', 'testrail-cases.csv'));
      await expect(page.getByTestId('import-mapping')).toBeVisible();
      await shoot(page, 'import-mapping', width, scheme);
      await page.getByTestId('import-dry-run').click();
      await expect(page.getByTestId('report-create')).toBeVisible();
      await shoot(page, 'import-report', width, scheme);
    });
  }
}
