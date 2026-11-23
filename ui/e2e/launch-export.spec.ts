import { expect, test } from '@playwright/test';
import { openRichLaunch } from './helpers';

const shots = process.env.FASTPROOF_SHOTS;

for (const scheme of ['light', 'dark'] as const) {
  for (const [name, viewport] of [['desktop', { width: 1280, height: 800 }], ['phone', { width: 360, height: 740 }]] as const) {
    test(`launch export menu (${scheme}, ${name})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.setViewportSize(viewport);
      await openRichLaunch(page);
      await page.getByTestId('export-menu').click();
      await expect(page.getByTestId('export-html')).toBeVisible();
      await expect(page.getByTestId('export-pdf')).toBeVisible();
      await expect(page.getByTestId('export-xlsx')).toBeVisible();
      if (shots) await page.screenshot({ path: `${shots}/export-menu-${scheme}-${name}.png` });
    });
  }
}

test('exports the HTML report as a file', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('export-menu').click();
  const download = page.waitForEvent('download');
  await page.getByTestId('export-html').click();
  expect((await download).suggestedFilename()).toMatch(/^launch-web-shop-41\.html$/);
});
