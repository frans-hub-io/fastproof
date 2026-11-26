import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function analysisOn(page: Page, scheme = 'dark', clickhouse = false) {
  await page.addInitScript(
    ([s, ch]: [string, boolean]) => {
      try {
        localStorage.setItem('fastproof-mock-analysis', 'on');
        localStorage.setItem('franshub-color-scheme', s);
        if (ch) localStorage.setItem('fastproof-mock-clickhouse', 'on');
        else localStorage.removeItem('fastproof-mock-clickhouse');
      } catch {
        /* ignore */
      }
    },
    [scheme, clickhouse] as [string, boolean],
  );
}

test('analytics on PostgreSQL: sections, banner, grouping, window and portfolio notice', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: 'Analytics' }).click();
  await expect(page.getByRole('heading', { name: 'Analytics' })).toBeVisible();
  await expect(page.getByTestId('analytics-pg-banner')).toContainText('Last 30 days; longer history and the portfolio need ClickHouse.');
  await expect(page.getByTestId('analytics-failure-row')).toHaveCount(5);
  for (const id of ['analytics-defects', 'analytics-flaky', 'analytics-mttr', 'analytics-retries', 'analytics-slow-steps', 'analytics-durations']) {
    await expect(page.getByTestId(id)).toBeVisible();
  }
  await page.getByTestId('analytics-group-by').click();
  await page.getByRole('option', { name: 'Suite' }).click();
  await expect(page.getByTestId('analytics-failure-row')).toHaveCount(3);
  await expect(page).toHaveURL(/groupBy=suite/);
  await page.getByTestId('analytics-days').getByText('90 days').click();
  await expect(page).toHaveURL(/days=90/);
  await expect(page.getByTestId('analytics-failures-backend')).toHaveText('PostgreSQL, last 30 days (partial)');
  await page.getByTestId('analytics-failure-row').first().waitFor();
  await page.getByTestId('analytics-group-by').click();
  await page.getByRole('option', { name: 'Unique error' }).click();
  await page.getByTestId('analytics-failure-row').first().getByRole('link').click();
  await expect(page).toHaveURL(/\/errors\/3f2a9c1d8e7b6a50/);
  await page.goto('/p/web-shop/analytics?tab=portfolio');
  await expect(page.getByTestId('analytics-portfolio-needs-clickhouse')).toBeVisible();
});

test('analytics with ClickHouse: a year of history and the portfolio', async ({ page }) => {
  await analysisOn(page, 'dark', true);
  await page.goto('/p/web-shop/analytics?days=365');
  await expect(page.getByTestId('analytics-failures-backend')).toHaveText('ClickHouse');
  await expect(page.getByTestId('analytics-pg-banner')).toHaveCount(0);
  await page.getByTestId('analytics-tab-portfolio').click();
  await expect(page.getByTestId('analytics-portfolio-row').first()).toBeVisible();
  await page.getByTestId('analytics-portfolio-row').nth(1).getByRole('link').click();
  await expect(page.getByRole('heading', { name: 'Analytics' })).toBeVisible();
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`analytics screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      const overflow = async () => {
        if (width < 500) {
          const o = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
          expect(o).toBeLessThanOrEqual(0);
        }
      };
      const shoot = async (name: string) => {
        await page.waitForTimeout(600);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: true });
      };
      await analysisOn(page, scheme);
      await page.goto('/p/web-shop/analytics');
      await expect(page.getByTestId('analytics-failure-row')).toHaveCount(5);
      await expect(page.getByTestId('analytics-durations')).toBeVisible();
      await shoot('analytics-pg');
      await overflow();
      await analysisOn(page, scheme, true);
      await page.goto('/p/web-shop/analytics?tab=portfolio&days=90');
      await expect(page.getByTestId('analytics-portfolio-row').first()).toBeVisible();
      await shoot('analytics-portfolio');
      await overflow();
    });
  }
}
