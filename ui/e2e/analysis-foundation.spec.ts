import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichResult } from './helpers';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function analysisOn(page: Page, on: boolean, scheme = 'dark') {
  await page.addInitScript(
    ([flag, s]: [string, string]) => {
      try {
        if (flag === 'on') localStorage.setItem('fastproof-mock-analysis', 'on');
        localStorage.setItem('franshub-color-scheme', s);
      } catch {
        /* ignore */
      }
    },
    [on ? 'on' : 'off', scheme] as [string, string],
  );
}

test('without Analysis the pages look as before, and results have a Triage tab', async ({ page }) => {
  await analysisOn(page, false);
  await page.goto('/p/web-shop/gates');
  await expect(page.getByRole('heading', { name: 'Quality gates' })).toBeVisible();
  await expect(page.getByTestId('tab-gate-history')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Errors' })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Analytics' })).toHaveCount(0);
  await page.goto('/p/web-shop/settings');
  await expect(page.getByRole('tab', { name: 'Workflow' })).toBeVisible();
  await expect(page.getByTestId('tab-settings-analysis')).toHaveCount(0);
  await page.goto('/p/web-shop/flaky');
  await expect(page.getByTestId('flaky-table')).toBeVisible();
  await expect(page.getByRole('columnheader', { name: 'Likely cause' })).toHaveCount(0);
  await openRichResult(page);
  await page.getByTestId('tab-result-triage').click();
  await expect(page.getByTestId('triage-history')).toBeVisible();
});

test('with Analysis on, its pages and tabs appear', async ({ page }) => {
  await analysisOn(page, true);
  await page.goto('/p/web-shop/gates');
  await page.getByTestId('tab-gate-history').click();
  await expect(page.getByTestId('gate-history')).toBeVisible();
  await page.getByTestId('tab-gate-simulation').click();
  await expect(page.getByTestId('gate-simulation')).toBeVisible();
  await page.getByRole('link', { name: 'Errors' }).click();
  await expect(page.getByRole('heading', { name: 'Unique errors' })).toBeVisible();
  await page.getByRole('link', { name: 'Analytics' }).click();
  await expect(page.getByRole('heading', { name: 'Analytics' })).toBeVisible();
  await page.goto('/p/web-shop/errors/0123456789abcdef');
  await expect(page.getByRole('heading', { name: 'Unique error' })).toBeVisible();
  await page.goto('/p/web-shop/settings?tab=analysis');
  await expect(page.getByTestId('analysis-settings')).toBeVisible();
  await page.goto('/p/web-shop/flaky');
  await expect(page.getByRole('columnheader', { name: 'Likely cause' })).toBeVisible();
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`analysis foundation screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await analysisOn(page, true, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: width < 500 });
      };
      await page.goto('/p/web-shop/gates');
      await page.getByTestId('tab-gate-history').click();
      await shoot('gates-tabs');
      await page.goto('/p/web-shop/settings?tab=analysis');
      await expect(page.getByTestId('analysis-settings')).toBeVisible();
      await shoot('settings-analysis');
      await page.goto('/p/web-shop/flaky');
      await expect(page.getByTestId('flaky-table')).toBeVisible();
      await shoot('flaky-cause');
      await page.goto('/p/web-shop/errors');
      await shoot('errors-stub');
      await openRichResult(page);
      await page.getByTestId('tab-result-triage').click();
      await shoot('result-triage');
    });
  }
}
