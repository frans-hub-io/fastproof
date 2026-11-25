import { expect, type Page } from '@playwright/test';

/** Client-side navigation: the mock server keeps its state in the page, so a full load would reset it. */
export async function spaGo(page: Page, url: string): Promise<void> {
  await page.evaluate((u) => {
    window.history.pushState({}, '', u);
    window.dispatchEvent(new PopStateEvent('popstate'));
  }, url);
}

/** Opens the workflow settings, optionally adopts the suggested workflow, and turns review on for manual cases. */
export async function enableReview(page: Page, opts: { workflow?: boolean; mode?: string } = {}): Promise<void> {
  await page.goto('/p/web-shop/settings?tab=workflow');
  await expect(page.getByTestId('review-policy')).toBeVisible();
  if (opts.workflow !== false) {
    await page.getByTestId('workflow-use-suggested').click();
    await expect(page.getByTestId('workflow-form')).toBeVisible();
  }
  await page.getByTestId('policy-mode').click();
  await page.getByRole('option', { name: opts.mode ?? 'Manual cases only' }).click();
  await page.getByTestId('policy-save').click();
  await expect(page.getByText('Review policy saved')).toBeVisible();
}

/** Toasts have no accessible name on their close button (shell default), so axe runs once they are gone. */
export async function toastsGone(page: Page): Promise<void> {
  await expect(page.locator('.mantine-Notification-root')).toHaveCount(0, { timeout: 15_000 });
}

/** Picks an option of a Mantine SegmentedControl by its visible label. */
export async function segment(page: Page, name: string): Promise<void> {
  await page.locator('label').filter({ hasText: new RegExp(`^${name}$`) }).click();
}
