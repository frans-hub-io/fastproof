import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichResult } from './helpers';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function prefs(page: Page, scheme = 'dark', analysis = true) {
  await page.addInitScript(
    ([s, a]: [string, boolean]) => {
      try {
        if (a) localStorage.setItem('fastproof-mock-analysis', 'on');
        localStorage.setItem('franshub-color-scheme', s);
      } catch {
        /* ignore */
      }
    },
    [scheme, analysis] as [string, boolean],
  );
}

test('a failed result lists the signals of its attempts in the triage tab', async ({ page }) => {
  await prefs(page);
  await openRichResult(page);
  await page.getByTestId('tab-result-triage').click();
  const block = page.getByTestId('result-signals');
  await expect(block).toBeVisible();
  await expect(block.getByText('never change the defect', { exact: false })).toBeVisible();
  await expect(block.getByTestId('result-signal').first()).toBeVisible();
  await expect(block.getByTestId('result-signal-source').filter({ hasText: 'Grid' })).toHaveCount(1);
  await expect(block.getByText('Browser ran out of memory')).toBeVisible();
  await expect(block.getByTestId('result-signal-detail').filter({ hasText: 'path: /payments' }).first()).toBeVisible();
  await expect(block.getByTestId('result-signal-confidence').first()).toHaveText('this attempt');
});

test('without Analysis the result shows no signals block', async ({ page }) => {
  await prefs(page, 'dark', false);
  await openRichResult(page);
  await page.getByTestId('tab-result-triage').click();
  await expect(page.getByTestId('tab-result-triage')).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('result-signals')).toHaveCount(0);
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`signals screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await prefs(page, scheme);
      await openRichResult(page);
      await page.getByTestId('tab-result-triage').click();
      const block = page.getByTestId('result-signals');
      await expect(block).toBeVisible();
      await block.scrollIntoViewIfNeeded();
      await page.waitForTimeout(400);
      await page.screenshot({ path: path.join(shots ?? '.', `signals-result-${String(width)}-${scheme}.png`), fullPage: width < 500 });
      await block.screenshot({ path: path.join(shots ?? '.', `signals-block-${String(width)}-${scheme}.png`) });
    });
  }
}
