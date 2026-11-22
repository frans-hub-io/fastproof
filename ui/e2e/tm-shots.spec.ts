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

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`fix-wave screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(180_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/settings/case-fields');
      await expect(page.getByTestId('field-row').first()).toBeVisible();
      await shoot(page, 'case-fields', width, scheme);
      await page.getByTestId('field-row').filter({ hasText: 'Platform' }).getByTestId('field-edit').click();
      await expect(page.getByTestId('field-dialog')).toBeVisible();
      await shoot(page, 'case-field-dialog', width, scheme, false);
      await page.keyboard.press('Escape');
      await page.goto('/p/web-shop/my-runs');
      await expect(page.getByTestId('my-run-item').first()).toBeVisible();
      await shoot(page, 'my-runs', width, scheme);
      await page.goto('/p/web-shop');
      await expect(page.getByTestId('launch-manual').first()).toBeVisible();
      await shoot(page, 'launches-manual-badge', width, scheme, false);
      await openManualLaunch(page);
      await page.getByTestId('tab-manual').click();
      await page.getByTestId('manual-item-select').nth(0).check();
      await page.getByTestId('manual-item-select').nth(1).check();
      await shoot(page, 'manual-selection', width, scheme, false);
      await page.getByTestId('manual-bulk-assign').click();
      await page.getByTestId('assign-input').fill('ma');
      await expect(page.getByRole('option').first()).toBeVisible();
      await shoot(page, 'manual-assign', width, scheme, false);
      await page.keyboard.press('Escape');
      if (await page.getByTestId('assign-dialog').count()) await page.keyboard.press('Escape');
      await expect(page.getByTestId('assign-dialog')).toHaveCount(0);
      await page.getByTestId('manual-bulk-result').click();
      await page.getByTestId('set-result-dialog').getByLabel('Failed').check();
      await shoot(page, 'manual-set-result', width, scheme, false);
      await page.keyboard.press('Escape');
      await page.getByTestId('manual-add-cases').click();
      await page.getByTestId('add-cases-input').fill('delivery');
      await expect(page.getByRole('option').first()).toBeVisible();
      await shoot(page, 'manual-add-cases', width, scheme, false);
      await page.keyboard.press('Escape');
      if (await page.getByTestId('add-cases-dialog').count()) await page.keyboard.press('Escape');
      await expect(page.getByTestId('add-cases-dialog')).toHaveCount(0);
      await page.getByTestId('manual-print').click();
      await expect(page.getByTestId('print-item').first()).toBeVisible();
      await shoot(page, 'manual-print', width, scheme, false);
      await page.emulateMedia({ media: 'print' });
      await shoot(page, 'manual-print-media', width, scheme, false);
      await page.emulateMedia({ media: 'screen' });
      await page.goto('/p/web-shop/plans');
      await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
      await expect(page.getByTestId('plan-launch').first()).toBeVisible();
      await page.getByTestId('plan-launches').scrollIntoViewIfNeeded();
      await shoot(page, 'plan-launches', width, scheme, false);
      await openManualLaunch(page);
      await page.getByTestId('tab-manual').click();
      await page.getByTestId('continue-testing').click();
      await expect(page.getByTestId('finish-panel')).toBeVisible();
      await page.getByTestId('finish-panel').scrollIntoViewIfNeeded();
      await shoot(page, 'runner-suggested', width, scheme, false);
      if (width > 500) {
        await page.goto('/p/web-shop/cases');
        await page.getByTestId('case-tree-view').getByRole('treeitem', { name: 'Mobile web' }).click();
        await page.getByTestId('folder-move').click();
        await expect(page.getByTestId('folder-move-dialog')).toBeVisible();
        await shoot(page, 'folder-move', width, scheme, false);
      }
    });
  }
}
