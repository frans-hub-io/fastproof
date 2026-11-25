import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const dir = process.env.FASTPROOF_CI_SHOTS;

async function shoot(page: Page, name: string, width: number, scheme: string, full = false) {
  if (!dir) return;
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(dir, `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

/** Router navigation without a reload: the mock server keeps its state in the page. */
async function navigate(page: Page, to: string) {
  await page.evaluate((path) => {
    window.history.pushState({}, '', path);
    window.dispatchEvent(new PopStateEvent('popstate'));
  }, to);
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`run a plan in CI ${String(width)} ${scheme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);

      await page.goto('/p/web-shop/settings?tab=integrations');
      await expect(page.getByTestId('rerun-enabled')).toBeChecked();
      await page.getByTestId('rerun-variable-add').click();
      await page.getByTestId('rerun-variable-name').first().fill('browser');
      await expect(page.getByTestId('rerun-variable-name').first()).toHaveValue('BROWSER');
      await page.getByTestId('rerun-variable-values').first().pressSequentially('chromium,firefox,');
      await page.getByTestId('rerun-variable-default').first().fill('chromium');
      await page.getByTestId('rerun-variable-add').click();
      await page.getByTestId('rerun-variable-name').nth(1).fill('SUITE');
      await page.getByTestId('rerun-refs').pressSequentially('release/*,');
      await page.getByTestId('rerun-variable-add').scrollIntoViewIfNeeded();
      await shoot(page, 'settings-rerun', width, scheme);
      await page.getByTestId('rerun-variable-name').nth(1).fill('fastproof_x');
      await expect(page.getByText('not FASTPROOF_', { exact: false })).toBeVisible();
      await expect(page.getByTestId('integrations-save')).toBeDisabled();
      await page.getByTestId('rerun-variable-name').nth(1).fill('SUITE');
      await page.getByTestId('integrations-save').click();
      await expect(page.getByTestId('integrations-save')).toBeEnabled();

      await navigate(page, '/p/web-shop/plans');
      await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
      await page.getByTestId('plan-start').click();
      const dialog = page.getByTestId('start-dialog');
      await dialog.getByTestId('start-ci').check();
      await dialog.getByTestId('ci-envs').pressSequentially('stage,broken,');
      await dialog.getByTestId('ci-var-SUITE').fill('smoke');
      await dialog.getByTestId('ci-ref').fill('release/2.14');
      await expect(dialog.getByTestId('testplan-command-preview')).toHaveCount(0);
      await shoot(page, 'start-dialog', width, scheme);
      await dialog.getByTestId('start-submit').click();
      const started = page.getByTestId('launch-started');
      await expect(started.getByTestId('ci-result')).toHaveCount(2);
      await expect(started.getByTestId('ci-failed')).toContainText('1 of 2');
      await shoot(page, 'start-results', width, scheme);
      await page.keyboard.press('Escape');
      const list = page.getByTestId('plan-dispatches');
      await expect(list.getByTestId('plan-dispatch')).toHaveCount(2);
      await expect(list).toContainText('SUITE=smoke');
      await list.scrollIntoViewIfNeeded();
      await shoot(page, 'plan-dispatches', width, scheme);

      await list.getByTestId('plan-dispatch').filter({ hasText: 'stage' }).getByRole('link').first().click();
      await expect(page.getByTestId('plan-check')).toContainText('does not match its plan');
      await shoot(page, 'plan-check', width, scheme);
    });
  }
}
