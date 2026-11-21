import { expect, type Page } from '@playwright/test';

export async function openRichLaunch(page: Page): Promise<void> {
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: '#41 web-shop nightly' }).click();
  await expect(page.getByRole('heading', { name: '#41 web-shop nightly' })).toBeVisible();
}

export async function openRichResults(page: Page): Promise<void> {
  await openRichLaunch(page);
  await page.getByRole('tab', { name: 'Results' }).click();
}

export async function openRichResult(page: Page): Promise<void> {
  await openRichResults(page);
  await page.getByTestId('result-row').filter({ hasText: 'pays with saved card' }).click();
  await expect(page.getByRole('heading', { name: 'pays with saved card' })).toBeVisible();
}

export function trackForeignRequests(page: Page, origin: string): string[] {
  const foreign: string[] = [];
  page.on('request', (r) => {
    const url = r.url();
    if (url.startsWith('data:') || url.startsWith('blob:')) return;
    if (!url.startsWith(origin)) foreign.push(url);
  });
  return foreign;
}
