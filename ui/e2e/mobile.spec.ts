import { expect, test, type Page } from '@playwright/test';

async function noHorizontalScroll(page: Page) {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);
}

test('phone width: every page fits and the navigation drawer works', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByTestId('project-card').first()).toBeVisible();
  await noHorizontalScroll(page);

  await page.getByRole('link', { name: 'Project Web shop' }).click();
  await expect(page.getByTestId('launch-row').first()).toBeVisible();
  await noHorizontalScroll(page);

  await page.getByRole('link', { name: '#41 web-shop nightly' }).click();
  await expect(page.getByTestId('counts-donut')).toBeVisible();
  await noHorizontalScroll(page);

  await page.getByTestId('result-row').filter({ hasText: 'pays with saved card' }).click();
  await expect(page.getByRole('heading', { name: 'pays with saved card' })).toBeVisible();
  await noHorizontalScroll(page);

  await page.getByRole('tab', { name: /Attachments/ }).click();
  await page.getByTestId('attachment-item').filter({ hasText: 'POST /payments' }).click();
  await expect(page.getByTestId('http-exchange')).toBeVisible();
  await page.keyboard.press('Escape');

  await page.getByRole('button', { name: 'Toggle navigation' }).click();
  await page.getByRole('dialog').getByRole('link', { name: 'API tokens' }).click();
  await expect(page).toHaveURL(/\/settings\/tokens$/);
  await expect(page.getByTestId('token-row').first()).toBeVisible();
  await noHorizontalScroll(page);
});

test('phone width: notification deliveries show their event', async ({ page }) => {
  await page.goto('/p/web-shop/notifications');
  const row = page.getByTestId('delivery-row').first();
  await expect(row).toBeVisible();
  const event = row.locator('td').nth(2);
  await expect(event).not.toHaveText(/^\s*(—)?\s*$/);
  await noHorizontalScroll(page);
});
