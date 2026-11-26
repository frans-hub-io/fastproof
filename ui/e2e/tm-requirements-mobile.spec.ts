import { expect, test, type Locator, type Page } from '@playwright/test';
import { noHorizontalScroll } from './tm-helpers';
import { spaGo } from './tm-reviews-helpers';

test.use({ viewport: { width: 360, height: 780 } });

async function notTruncated(scope: Locator): Promise<void> {
  const cut = await scope.locator('.mantine-Badge-label').evaluateAll((els) => els.filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.textContent));
  expect(cut, 'truncated chips').toEqual([]);
}

async function tableFits(table: Locator): Promise<void> {
  const over = await table.evaluate((t) => t.getBoundingClientRect().right - document.documentElement.clientWidth);
  expect(over, 'table scrolls sideways').toBeLessThanOrEqual(1);
}

async function inside(page: Page, inner: Locator, outer: Locator): Promise<void> {
  const o = await outer.boundingBox();
  expect(o).not.toBeNull();
  for (const box of await inner.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().toJSON() as { left: number; right: number }))) {
    expect(box.left).toBeGreaterThanOrEqual((o?.x ?? 0) - 1);
    expect(box.right).toBeLessThanOrEqual((o?.x ?? 0) + (o?.width ?? 0) + 1);
  }
  await noHorizontalScroll(page);
}

test('360 px: the requirements tree, a requirement, coverage and the case tab fit', async ({ page }) => {
  await page.goto('/p/web-shop/requirements');
  await expect(page.getByTestId('requirement-row').first()).toBeVisible();
  await page.getByRole('button', { name: 'Expand REQ-1' }).click();
  await expect(page.locator('[data-testid="requirement-row"][data-number="2"]')).toBeVisible();
  await noHorizontalScroll(page);

  await spaGo(page, '/p/web-shop/requirements/3');
  await expect(page.getByTestId('linked-case-row')).toHaveCount(2);
  await noHorizontalScroll(page);

  await spaGo(page, '/p/web-shop/coverage');
  await expect(page.getByTestId('coverage-row').first()).toBeVisible();
  await page.getByRole('button', { name: 'Expand REQ-6' }).click();
  await expect(page.locator('[data-testid="coverage-row"][data-number="7"]')).toBeVisible();
  await notTruncated(page.getByTestId('coverage-table'));
  await tableFits(page.getByTestId('coverage-table'));
  await noHorizontalScroll(page);
  await spaGo(page, '/p/web-shop/coverage?scope=window&days=14&branch=main');
  await expect(page.locator('[data-testid="coverage-row"][data-coverage="partial"]').first()).toBeVisible();
  await notTruncated(page.getByTestId('coverage-table'));
  await tableFits(page.getByTestId('coverage-table'));

  await spaGo(page, '/p/web-shop/requirements/7');
  const first = await page.getByTestId('linked-case-row').first().getAttribute('data-case');
  await spaGo(page, `/p/web-shop/cases/${String(first)}?tab=requirements`);
  await expect(page.getByTestId('case-requirement-row').first()).toBeVisible();
  await noHorizontalScroll(page);
});

test('360 px: the requirement coverage widget shows whole chips and counts', async ({ page }) => {
  await page.goto('/p/web-shop/dashboards');
  await page.getByRole('button', { name: 'New dashboard' }).click();
  await page.getByLabel('Name').fill('Coverage board');
  await page.getByRole('button', { name: 'Empty dashboard' }).click();
  await page.getByRole('button', { name: 'Add widget' }).click();
  await page.getByRole('menuitem', { name: 'Requirement coverage' }).click();
  const table = page.getByTestId('requirement-coverage-table');
  await expect(table.getByRole('row')).toHaveCount(5);
  await expect(table).toContainText('Uncovered');
  await expect(table).toContainText(/\d+\/\d+ passed/);
  await notTruncated(table);
  await inside(page, page.getByTestId('requirement-coverage-cell'), page.getByTestId('requirement-coverage'));
});
