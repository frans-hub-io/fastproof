import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichLaunch, openRichResult } from './helpers';

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

/** A cluster with a suggestion, found again by its fingerprint (the list changes as it is triaged). */
async function suggested(page: Page, n = 0) {
  const c = page.getByTestId('cluster').filter({ has: page.getByTestId('suggestion-chip') }).nth(n);
  const fp = (await c.locator('.fp-mono').last().textContent()) ?? '';
  return page.getByTestId('cluster').filter({ hasText: fp });
}

async function openFailures(page: Page) {
  await openRichLaunch(page);
  await page.getByRole('tab', { name: /Failures/ }).click();
  await expect(page.getByTestId('clusters')).toBeVisible();
}

test('clusters show suggestion chips; accept, reject, why and the rule draft work', async ({ page }) => {
  await analysisOn(page);
  await openFailures(page);
  const chips = page.getByTestId('suggestion-chip');
  await expect(chips.first()).toBeVisible();
  const cluster = await suggested(page);
  const other = await suggested(page, 1);
  await cluster.locator('button').first().click();
  const card = cluster.getByTestId('suggestion').first();
  await expect(card).toBeVisible();
  await card.getByTestId('suggestion-why-toggle').click();
  await expect(card.getByTestId('suggestion-why')).toBeVisible();
  await expect(card.getByText(/nearest-decision search/)).toBeVisible();
  await card.getByTestId('suggestion-accept').click();
  await expect(page.getByText('Suggestion accepted')).toBeVisible();
  await expect(card).toHaveAttribute('data-state', 'accepted');
  await other.locator('button').first().click();
  const second = other.getByTestId('suggestion').first();
  await second.getByTestId('suggestion-reject').click();
  await expect(other.getByTestId('suggestion')).toHaveCount(0);
});

test('the result page has the suggestions panel and the triage history with undo', async ({ page }) => {
  await analysisOn(page);
  await openRichResult(page);
  const panel = page.getByTestId('suggestions-panel');
  await expect(panel).toBeVisible();
  await panel.getByTestId('suggestion-accept').first().click();
  await expect(page.getByText('Suggestion accepted')).toBeVisible();
  await page.getByTestId('tab-result-triage').click();
  await expect(page.getByTestId('triage-history-row').first()).toHaveAttribute('data-via', 'suggestion');
  await page.getByTestId('triage-undo').first().click();
  await expect(page.getByTestId('undo-result')).toBeVisible();
  await expect(page.getByTestId('triage-history-row').first()).toHaveAttribute('data-via', 'undo');
});

test('settings show the analysis form, the auto-apply warning and the backtest', async ({ page }) => {
  await analysisOn(page);
  await page.goto('/p/web-shop/settings?tab=analysis');
  await expect(page.getByTestId('analysis-settings')).toBeVisible();
  await expect(page.getByTestId('backtest-none')).toBeVisible();
  await page.getByTestId('analysis-auto-apply').click();
  await expect(page.getByTestId('analysis-auto-warning')).toBeVisible();
  await page.getByTestId('backtest-run').click();
  await expect(page.getByText('Backtest queued')).toBeVisible();
  await expect(page.getByTestId('backtest-bands')).toBeVisible();
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`suggestion screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await analysisOn(page, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: width < 500 });
      };
      await openFailures(page);
      await expect(page.getByTestId('suggestion-chip').first()).toBeVisible();
      await shoot('suggest-clusters');
      const cluster = await suggested(page);
      await cluster.locator('button').first().click();
      await cluster.getByTestId('suggestion-why-toggle').first().click();
      await expect(cluster.getByTestId('suggestion-why').first()).toBeVisible();
      await shoot('suggest-why');
      await cluster.getByTestId('suggestion-accept').first().click();
      await expect(page.getByText('Suggestion accepted')).toBeVisible();
      await page.goto('/p/web-shop/settings?tab=analysis');
      await page.getByTestId('backtest-run').click();
      await expect(page.getByTestId('backtest-bands')).toBeVisible();
      await page.getByTestId('analysis-auto-apply').click();
      await shoot('suggest-settings');
      await openRichResult(page);
      await expect(page.getByTestId('suggestions-panel')).toBeVisible();
      await shoot('suggest-result');
      await page.getByTestId('suggestions-panel').getByTestId('suggestion-accept').first().click();
      await page.getByTestId('tab-result-triage').click();
      await expect(page.getByTestId('triage-history-row').first()).toBeVisible();
      await shoot('suggest-triage');
    });
  }
}
