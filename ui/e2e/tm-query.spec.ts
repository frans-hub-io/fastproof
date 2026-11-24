import { expect, test, type Page } from '@playwright/test';
import { expectNoAxeViolations, noHorizontalScroll } from './tm-helpers';

const bar = (page: Page) => page.getByTestId('query-bar');

async function openQueryMode(page: Page) {
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-row').first()).toBeVisible();
  await page.getByTestId('filter-mode').getByText('Query', { exact: true }).click();
  await expect(bar(page)).toBeVisible();
}

async function setQuery(page: Page, text: string) {
  await bar(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Backspace');
  await page.keyboard.type(text);
  await page.keyboard.press('Escape');
}

test('type a query: completion, the error underline and message, apply changes the list', async ({ page }) => {
  await openQueryMode(page);
  const all = await page.getByTestId('case-row').count();
  await bar(page).click();
  await page.keyboard.type('statu');
  const popup = page.locator('.cm-tooltip-autocomplete');
  await expect(popup).toContainText('status');
  await expect(popup).toContainText('status');
  await page.waitForTimeout(200);
  await page.keyboard.press('Enter');
  await expect(bar(page)).toContainText('status');
  await page.keyboard.type(' ');
  await expect(popup).toContainText('in');
  await page.keyboard.type('= dra');
  await expect(popup).toContainText('draft');
  await page.waitForTimeout(200);
  await page.keyboard.press('Enter');
  await expect(bar(page)).toHaveText('status = draft');
  await expect(page.getByTestId('query-count')).toContainText(/cases? match/);
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/query=status\+%3D\+draft/);
  await expect.poll(() => page.getByTestId('case-row').count()).toBeLessThan(all);
  await expect(page.getByTestId('case-row').first()).toBeVisible();

  await setQuery(page, 'status = active and colour = red');
  await expect(page.getByTestId('query-error')).toContainText('unknown attribute colour');
  await expect(page.locator('.cm-lintRange-error')).toHaveText('colour');
  await expect(page.getByTestId('query-apply')).toBeDisabled();
  await bar(page).click();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/query=status\+%3D\+draft$/);

  await setQuery(page, 'name = "abc');
  await expect(page.getByTestId('query-error')).toContainText('not closed');
  await setQuery(page, 'status = active');
  await expect(page.getByTestId('query-error')).toHaveCount(0);
  await page.getByTestId('query-apply').click();
  await expect(page).toHaveURL(/query=status\+%3D\+active$/);
});

test('field keys and values complete from the schema', async ({ page }) => {
  await openQueryMode(page);
  await bar(page).click();
  await page.keyboard.type('field[pri');
  const popup = page.locator('.cm-tooltip-autocomplete');
  await expect(popup).toContainText('priority');
  await page.waitForTimeout(200);
  await page.keyboard.press('Enter');
  await expect(bar(page)).toHaveText('field[priority]');
  await page.keyboard.type(' = ');
  await expect(popup).toContainText('Critical');
});

test('chips serialize to a query and come back', async ({ page }) => {
  await page.goto('/p/web-shop/cases?status=active&automation=manual&tag=smoke&q=pay');
  await expect(page.getByTestId('case-row').first()).toBeVisible();
  await page.getByTestId('filter-mode').getByText('Query', { exact: true }).click();
  await expect(bar(page)).toHaveText('status = active and automated = false and tag = "smoke" and (text ~ "pay" or name ~ "pay")');
  await expect(page).toHaveURL(/query=/);
  await page.getByTestId('filter-mode').getByText('Filters', { exact: true }).click();
  await expect(page.getByTestId('filter-mode-confirm')).toHaveCount(0);
  await expect(page).not.toHaveURL(/query=/);
  await expect(page.getByTestId('chip-status-active')).toBeChecked();
  await expect(page.getByTestId('case-search')).toHaveValue('pay');

  await page.getByTestId('filter-mode').getByText('Query', { exact: true }).click();
  await setQuery(page, 'status = draft');
  await page.getByTestId('query-apply').click();
  await page.getByTestId('filter-mode').getByText('Filters', { exact: true }).click();
  const confirm = page.getByTestId('filter-mode-confirm');
  await expect(confirm).toBeVisible();
  await confirm.getByTestId('filter-mode-confirm-ok').click();
  await expect(page).not.toHaveURL(/query=/);
  await expect(page.getByTestId('chip-status-active')).toBeChecked();
});

test('a short search text is dropped with a note', async ({ page }) => {
  await page.goto('/p/web-shop/cases?status=active&q=ab');
  await page.getByTestId('filter-mode').getByText('Query', { exact: true }).click();
  await expect(bar(page)).toHaveText('status = active');
  await expect(page.getByTestId('query-note')).toContainText('shorter than 3 characters');
});

test('the query link keeps the mode and the folder tree and trash stay usable', async ({ page }) => {
  await page.goto('/p/web-shop/cases?query=status%20%3D%20active');
  await expect(bar(page)).toHaveText('status = active');
  const tree = page.getByTestId('case-tree-view');
  await tree.getByRole('treeitem', { name: 'Storefront' }).click();
  await expect(page).toHaveURL(/folder=/);
  await expect(page).toHaveURL(/query=/);
  await page.getByTestId('toggle-archived').click();
  await expect(page).toHaveURL(/archived=true/);
  await expect(bar(page)).toBeVisible();
});

test('an invalid query in the link shows the error of the list request', async ({ page }) => {
  await page.goto('/p/web-shop/cases?query=colour%20%3D%20red');
  await expect(page.getByText('The query is not valid')).toBeVisible();
  await expect(page.getByTestId('query-error')).toContainText('unknown attribute colour');
});

test('save a query, apply it from the menu, rename and delete it', async ({ page }) => {
  await openQueryMode(page);
  await setQuery(page, 'tag = smoke and status = active');
  await page.getByTestId('query-apply').click();
  await page.getByTestId('saved-queries').click();
  await expect(page.getByText('Active and automated')).toBeVisible();
  await expect(page.getByText('Drafts to finish')).toBeVisible();
  await page.getByTestId('save-query').click();
  const dialog = page.getByTestId('save-query-dialog');
  await dialog.getByTestId('save-query-name').fill('Smoke, active');
  await dialog.getByTestId('save-query-shared').check();
  await dialog.getByTestId('save-query-submit').click();
  await expect(page.getByText('Query saved')).toBeVisible();

  await setQuery(page, 'status = outdated');
  await page.getByTestId('query-apply').click();
  await page.getByTestId('saved-queries').click();
  await page.getByTestId('saved-query-item').filter({ hasText: 'Smoke, active' }).click();
  await expect(bar(page)).toHaveText('tag = smoke and status = active');
  await expect(page).toHaveURL(/query=tag/);

  await page.getByTestId('saved-queries').click();
  await page.getByTestId('manage-queries').click();
  const manage = page.getByTestId('manage-queries-dialog');
  const row = manage.getByTestId('saved-query-row').filter({ hasText: 'Smoke, active' });
  await row.getByTestId('saved-query-rename').click();
  await manage.getByTestId('saved-query-rename-input').fill('Smoke only');
  await page.keyboard.press('Enter');
  await expect(manage.getByTestId('saved-query-row').filter({ hasText: 'Smoke only' })).toBeVisible();
  await manage.getByTestId('saved-query-row').filter({ hasText: 'Smoke only' }).getByTestId('saved-query-delete').click();
  await expect(manage.getByText('Smoke only')).toHaveCount(0);
});

test('a taken name is reported', async ({ page }) => {
  await openQueryMode(page);
  await setQuery(page, 'status = draft');
  await page.getByTestId('saved-queries').click();
  await page.getByTestId('save-query').click();
  await page.getByTestId('save-query-name').fill('Drafts to finish');
  await page.getByTestId('save-query-submit').click();
  await expect(page.getByText('Query not saved')).toBeVisible();
});

test('plan filter dialog in query mode saves a query and shows the sync diff', async ({ page }) => {
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Checkout smoke' }).click();
  await page.getByTestId('plan-filter-open').click();
  const dialog = page.getByTestId('filter-dialog');
  await dialog.getByTestId('filter-dialog-mode').getByText('Query', { exact: true }).click();
  await expect(dialog.getByTestId('filter-save')).toBeDisabled();
  await dialog.getByTestId('query-bar').click();
  await page.keyboard.type('colour = red');
  await expect(dialog.getByTestId('query-error')).toContainText('unknown attribute');
  await expect(dialog.getByTestId('filter-save')).toBeDisabled();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.type('status = draft and automated = false');
  await page.keyboard.press('Escape');
  await expect(dialog.getByTestId('query-count')).toBeVisible();
  const request = page.waitForRequest((r) => r.method() === 'PATCH' && r.url().includes('/test-plans/'));
  await dialog.getByTestId('filter-save').click();
  const body = (await request).postDataJSON() as Record<string, unknown>;
  expect(body).toEqual({ query: 'status = draft and automated = false', filter: null });
  await expect(page.getByTestId('sync-dialog')).toBeVisible();
  await page.getByTestId('sync-dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
  await page.getByTestId('plan-filter-open').click();
  await expect(page.getByTestId('filter-dialog').getByTestId('query-bar')).toHaveText('status = draft and automated = false');
});

test('360 px: the query bar fits and a long query stays inside', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-card').first()).toBeVisible();
  await page.getByTestId('filter-mode').getByText('Query', { exact: true }).click();
  await expect(bar(page)).toBeVisible();
  await noHorizontalScroll(page);
  await setQuery(page, 'status in [draft, active] and field["Priority"] in ["High", "Critical"] and folder in subtree("Payments/Cards") and tag = "a-very-long-tag-name-without-spaces-to-test-overflow"');
  await noHorizontalScroll(page);
  await page.getByTestId('saved-queries').click();
  await expect(page.getByTestId('save-query')).toBeVisible();
  await noHorizontalScroll(page);
});

test('axe: query mode, with an error, and the plan dialog in query mode', async ({ page }) => {
  await page.goto('/p/web-shop/cases?query=status%20%3D%20active%20and%20automated%20%3D%20false');
  await expect(page.getByTestId('query-count')).toBeVisible();
  await expectNoAxeViolations(page, 'query mode');
  await setQuery(page, 'status = active and colour = red');
  await expect(page.getByTestId('query-error')).toBeVisible();
  await expectNoAxeViolations(page, 'query mode with an error');
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Checkout smoke' }).click();
  await page.getByTestId('plan-filter-open').click();
  await page.getByTestId('filter-dialog-mode').getByText('Query', { exact: true }).click();
  await expectNoAxeViolations(page, 'plan dialog in query mode');
});
