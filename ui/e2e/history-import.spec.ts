import { expect, test } from '@playwright/test';

const HISTORY = JSON.stringify({ 'd41d8cd98f00b204e9800998ecf8427e.d41d8cd98f00b204e9800998ecf8427e': { items: [] } });

test('import Allure 2 history from the settings tab', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=history');
  await expect(page.getByRole('heading', { name: 'Import Allure history' })).toBeVisible();
  await expect(page.getByTestId('history-submit')).toBeDisabled();
  await page.locator('input[type="file"]').setInputFiles({ name: 'history.json', mimeType: 'application/json', buffer: Buffer.from(HISTORY) });
  await page.getByTestId('history-branch').fill('main');
  await page.getByTestId('history-submit').click();
  await expect(page.getByTestId('history-import-row')).toContainText('main');
  await expect(page.getByTestId('history-import-status')).toHaveText('done', { timeout: 15_000 });
  await expect(page.getByTestId('history-import-row')).toContainText('3 of 3 reports');
  await expect(page.getByTestId('history-import-warnings')).toContainText('does not know');
});

test('a file the import does not take is explained before sending', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=history');
  await page.locator('input[type="file"]').setInputFiles({ name: 'notes.txt', mimeType: 'text/plain', buffer: Buffer.from('x') });
  await page.getByTestId('history-submit').click();
  await expect(page.getByText(/notes\.txt: expected history\.json/)).toBeVisible();
  await expect(page.getByTestId('history-import-row')).toHaveCount(0);
});

test('Allure 3 takes one history file of any name', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=history');
  await page.getByText('Allure 3', { exact: true }).click();
  await page.locator('input[type="file"]').setInputFiles({ name: 'hist.jsonl', mimeType: 'application/json', buffer: Buffer.from('{}\n') });
  await page.getByTestId('history-submit').click();
  await expect(page.getByTestId('history-import-row')).toContainText('Allure 3');
});

const shots = process.env.FASTPROOF_HISTORY_SHOTS;

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_HISTORY_SHOTS to a directory to take the review screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: `${shots ?? '.'}/${name}-${String(width)}-${scheme}.png`, fullPage: true });
      };
      await page.goto('/p/web-shop/settings?tab=history');
      await expect(page.getByRole('heading', { name: 'Import Allure history' })).toBeVisible();
      await shoot('history-empty');
      await page.locator('input[type="file"]').setInputFiles([
        { name: 'history.json', mimeType: 'application/json', buffer: Buffer.from(HISTORY) },
        { name: 'retry-trend.json', mimeType: 'application/json', buffer: Buffer.from('[]') },
      ]);
      await page.getByTestId('history-branch').fill('main');
      await shoot('history-chosen');
      await page.getByTestId('history-submit').click();
      await expect(page.getByTestId('history-import-status')).toHaveText('done', { timeout: 15_000 });
      await shoot('history-done');
    });
  }
}
