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

  await page.getByRole('tab', { name: 'Results' }).click();
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

test('390 px: notification deliveries show their event in the visible part of the table', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await page.goto('/p/web-shop/notifications');
  const rows = page.getByTestId('delivery-row');
  await expect(rows.first()).toBeVisible();
  const count = await rows.count();
  for (let i = 0; i < Math.min(count, 12); i++) {
    const event = rows.nth(i).locator('td').nth(2);
    await expect(event).toHaveText(/^(Launch finished|Gate failed|New failures|Manual run closed)$/);
    const box = await event.boundingBox();
    expect(box && box.x >= 0 && box.x < 390 - 40, `Event cell of row ${String(i)} starts inside the screen`).toBe(true);
    const visible = await event.evaluate((td) => {
      const el = td.firstElementChild ?? td;
      const color = getComputedStyle(el).color;
      const bg = getComputedStyle(document.body).backgroundColor;
      return color !== bg && getComputedStyle(el).visibility === 'visible' && Number(getComputedStyle(el).opacity) > 0.5;
    });
    expect(visible).toBe(true);
  }
  await noHorizontalScroll(page);
});

test('phone width: the tree fills the screen and a test opens in a drawer', async ({ page }) => {
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: '#41 web-shop nightly' }).click();
  await expect(page.getByTestId('explorer-tree')).toBeVisible();
  await expect(page.getByTestId('explorer-detail-pane')).toHaveCount(0);
  const tree = await page.getByTestId('explorer-tree').boundingBox();
  const viewport = page.viewportSize();
  expect(tree && viewport ? tree.width / viewport.width : 0).toBeGreaterThan(0.85);
  await noHorizontalScroll(page);
  const failed = page.locator('[data-testid="explorer-test"][data-status="failed"]').first();
  const name = (await failed.getAttribute('title')) ?? '';
  await failed.click();
  const drawer = page.getByRole('dialog');
  await expect(drawer.getByTestId('explorer-detail-name')).toHaveText(name);
  await expect(drawer.getByTestId('step-tree')).toBeVisible();
  await noHorizontalScroll(page);
  await page.goBack();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByTestId('explorer-tree')).toBeVisible();
});

test('phone width: grouping, filters and the environment switcher fit the screen', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: '#41 web-shop nightly' }).click();
  await expect(page.getByTestId('environments')).toBeVisible();
  await expect(page.getByTestId('environment-row')).toHaveCount(2);
  await expect(page.getByTestId('explorer-tree')).toBeVisible();
  await noHorizontalScroll(page);
  await page.getByTestId('explorer-filters').click();
  const menu = page.getByTestId('explorer-filters-menu');
  await expect(menu).toBeVisible();
  const box = await menu.boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 360).toBe(true);
  await menu.getByTestId('filter-retried').check();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('active-filter-retried')).toBeVisible();
  await page.getByTestId('explorer-view').click();
  await page.getByRole('option', { name: 'Custom…', exact: true }).click();
  await expect(page.getByTestId('explorer-custom-levels')).toBeVisible();
  await noHorizontalScroll(page);
});
