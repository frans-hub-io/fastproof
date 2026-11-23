import { expect, test } from '@playwright/test';
import { openManualLaunch } from './tm-helpers';

test.use({ colorScheme: 'dark' });

test('print: the light theme is used on paper and an item is not split across pages', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-print').click();
  await expect(page.getByTestId('print-item').first()).toBeVisible();
  await expect(page.locator('html')).toHaveAttribute('data-mantine-color-scheme', 'dark');

  await page.emulateMedia({ media: 'print' });
  await page.evaluate(() => window.dispatchEvent(new Event('beforeprint')));
  await expect(page.locator('html')).toHaveAttribute('data-mantine-color-scheme', 'light');
  const paper = await page.evaluate(() => ({
    html: getComputedStyle(document.documentElement).backgroundColor,
    body: getComputedStyle(document.body).backgroundColor,
    text: getComputedStyle(document.body).color,
    breakInside: getComputedStyle(document.querySelector('[data-testid="print-item"]') as Element).breakInside,
  }));
  expect(paper).toEqual({ html: 'rgb(255, 255, 255)', body: 'rgb(255, 255, 255)', text: 'rgb(0, 0, 0)', breakInside: 'avoid' });

  await page.evaluate(() => window.dispatchEvent(new Event('afterprint')));
  await expect(page.locator('html')).toHaveAttribute('data-mantine-color-scheme', 'dark');
});
