import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichResult } from './helpers';

async function createIssue(page: Page): Promise<void> {
  await openRichResult(page);
  await page.getByTestId('triage-open').click();
  await page.getByTestId('triage-create-issue').click();
  await expect(page.getByTestId('issue-form')).toBeVisible();
  await page.getByTestId('issue-note').fill('Seen after the 2.14 deploy');
  await page.getByTestId('issue-preview-toggle').click();
  await expect(page.getByTestId('issue-preview')).toContainText('Seen after the 2.14 deploy');
}

test('create an issue from triage and see its state on the result', async ({ page }) => {
  await createIssue(page);
  await page.getByTestId('issue-create').click();
  await expect(page.getByTestId('issue-created-link')).toHaveAttribute('href', /https:\/\/issues\.example\.com\/browse\/SHOP-\d+/);
  await page.getByTestId('issue-done').click();
  await expect(page.getByTestId('defect-issue')).toContainText('SHOP-');
  await expect(page.getByTestId('defect-card').getByTestId('issue-state')).toHaveText('To Do');
});

test('a closed issue suggests unmuting its tests', async ({ page }) => {
  await page.goto('/p/web-shop/issues');
  await expect(page.getByTestId('issue-row')).toHaveCount(3);
  const suggestion = page.getByTestId('unmute-suggestion');
  await expect(suggestion).toContainText('SHOP-1180');
  await suggestion.getByTestId('unmute-linked').click();
  await expect(page.getByText('1 test unmuted')).toBeVisible();
  await expect(suggestion).toHaveCount(0);
  await page.getByTestId('issues-view').getByText('Closed').click();
  await expect(page.getByTestId('issue-row')).toHaveCount(1);
});

test('viewers see issues but cannot create or unmute', async ({ page }) => {
  await page.goto('/p/web-shop/issues?mockRole=viewer');
  await expect(page.getByTestId('issue-row')).toHaveCount(3);
  await expect(page.getByTestId('unmute-linked')).toHaveCount(0);
  await expect(page.getByTestId('issue-actions')).toHaveCount(0);
});

const dir = process.env.FASTPROOF_TRACKER_SHOTS;

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await page.waitForTimeout(400);
  await page.screenshot({
    path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`),
    fullPage: full,
  });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`tracker screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!dir, 'set FASTPROOF_TRACKER_SHOTS to a directory to take the review screenshots');
      test.setTimeout(180_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/issues');
      await expect(page.getByTestId('issue-row').first()).toBeVisible();
      await shoot(page, 'issues', width, scheme);
      await createIssue(page);
      await shoot(page, 'issue-create', width, scheme, false);
      await page.getByTestId('issue-create').click();
      await expect(page.getByTestId('issue-created')).toBeVisible();
      await shoot(page, 'issue-created', width, scheme, false);
      await page.getByTestId('issue-done').click();
      await expect(page.getByTestId('defect-card').getByTestId('issue-state')).toBeVisible();
      await page.getByTestId('defect-card').scrollIntoViewIfNeeded();
      await shoot(page, 'result-issue', width, scheme, false);
      await page.goto('/p/web-shop/settings?tab=integrations');
      await expect(page.getByTestId('tracker-api').first()).toBeVisible();
      await page.getByTestId('tracker-api').first().scrollIntoViewIfNeeded();
      await shoot(page, 'settings-tracker', width, scheme, false);
    });
  }
}
