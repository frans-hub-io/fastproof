import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function analysisOn(page: Page, scheme = 'dark') {
  await page.addInitScript((s: string) => {
    try {
      localStorage.setItem('fastproof-mock-analysis', 'on');
      localStorage.setItem('franshub-color-scheme', s);
    } catch {
      /* ignore */
    }
  }, scheme);
}

test('unique errors: list, filters, one error, ignore and merge', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: 'Errors' }).click();
  await expect(page.getByRole('heading', { name: 'Unique errors' })).toBeVisible();
  const rows = page.getByTestId('error-row');
  await expect(rows).toHaveCount(8);
  await expect(rows.first().getByTestId('error-state')).toHaveText('Regressed');
  await page.getByTestId('errors-state').click();
  await page.getByRole('option', { name: 'Resolved' }).click();
  await expect(rows).toHaveCount(2);
  await page.getByTestId('errors-state').click();
  await page.getByRole('option', { name: 'Resolved' }).click();
  await page.getByLabel('Search messages').fill('Timeout');
  await expect(rows).toHaveCount(1);
  await rows.first().getByRole('link').first().click();
  await expect(page.getByRole('heading', { name: 'Unique error' })).toBeVisible();
  await expect(page.getByTestId('error-timeline')).toBeVisible();
  await expect(page.getByTestId('error-decisions')).toContainText('Automation bug');
  await page.getByTestId('error-ignore').click();
  await expect(page.getByTestId('error-activate')).toBeVisible();
  await expect(page.locator('[data-testid="error-state"]').first()).toHaveText('Ignored');
  await page.getByTestId('error-activate').click();
  await expect(page.getByTestId('error-ignore')).toBeVisible();
  const url = page.url();
  await page.goto('/p/web-shop/errors?state=new');
  const target = (await page.getByTestId('error-row').first().locator('.fp-mono').first().textContent()) ?? '';
  await page.goto(url);
  await page.getByTestId('error-merge-target').fill(target.trim());
  await page.getByTestId('error-merge').click();
  await expect(page.getByTestId('error-alias-of')).toContainText(target.trim());
  await page.getByTestId('error-split').click();
  await expect(page.getByTestId('error-alias-of')).toHaveCount(0);
});

test('a regressed error shows its commit range with the compare link', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/errors?state=regressed');
  await page.getByTestId('error-row').first().getByRole('link').first().click();
  await expect(page.getByTestId('error-commits')).toContainText('Came back between');
  await expect(page.getByTestId('error-compare')).toHaveAttribute('href', /compare/);
  await expect(page.getByTestId('error-stat-regressions')).toHaveText('2');
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`unique errors screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await analysisOn(page, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: true });
      };
      await page.goto('/p/web-shop/errors');
      await expect(page.getByTestId('error-row')).toHaveCount(8);
      await shoot('errors-list');
      if (width < 500) {
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
        expect(overflow).toBeLessThanOrEqual(0);
      }
      await page.goto('/p/web-shop/errors?state=regressed');
      await page.getByTestId('error-row').first().getByRole('link').first().click();
      await expect(page.getByTestId('error-detail')).toBeVisible();
      await shoot('error-detail');
      if (width < 500) {
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
        expect(overflow).toBeLessThanOrEqual(0);
      }
    });
  }
}
