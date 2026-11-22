import { expect, test } from '@playwright/test';
import { noHorizontalScroll, openManualLaunch } from './tm-helpers';

test.use({ viewport: { width: 360, height: 780 } });

test('360 px: the Finish buttons of the runner are 44 px touch targets', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('mobile-step')).toBeVisible();
  for (const s of ['passed', 'failed', 'blocked', 'skipped']) {
    const box = await page.getByTestId(`finish-${s}`).boundingBox();
    expect(box?.height ?? 0, `finish-${s}`).toBeGreaterThanOrEqual(44);
  }
  await noHorizontalScroll(page);
});

test('360 px: case fields, my runs, the manual tab with a selection and the print view fit', async ({ page }) => {
  await page.goto('/p/web-shop/settings/case-fields');
  await expect(page.getByTestId('field-row').first()).toBeVisible();
  await noHorizontalScroll(page);
  await page.getByTestId('field-new').click();
  await expect(page.getByTestId('field-dialog')).toBeVisible();
  await noHorizontalScroll(page);
  await page.keyboard.press('Escape');

  await page.goto('/p/web-shop/my-runs');
  await expect(page.getByTestId('my-run-item').first()).toBeVisible();
  await noHorizontalScroll(page);

  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-item-select').first().check();
  await expect(page.getByTestId('manual-bulk')).toBeVisible();
  await noHorizontalScroll(page);
  await page.getByTestId('manual-bulk-result').click();
  await expect(page.getByTestId('set-result-dialog')).toBeVisible();
  await noHorizontalScroll(page);
  await page.keyboard.press('Escape');

  await page.getByTestId('manual-print').click();
  await expect(page.getByTestId('print-item').first()).toBeVisible();
  await noHorizontalScroll(page);
});
