import { expect, test } from '@playwright/test';

test('sync a filter plan: the diff first, then apply', async ({ page }) => {
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Checkout smoke' }).click();
  await expect(page.getByTestId('plan-filter')).toContainText('Filter: in Storefront / Checkout and below; status active; manual');
  const before = Number((await page.getByTestId('plan-count-total').textContent())?.match(/\d+/)?.[0]);
  await page.getByTestId('plan-sync').click();
  const dialog = page.getByTestId('sync-dialog');
  await expect(dialog.getByTestId('sync-added')).toHaveText('+ 2 added');
  await expect(dialog.getByTestId('sync-removed')).toContainText('− 1 removed');
  await dialog.getByTestId('sync-apply').click();
  await expect(page.getByText('Plan synced')).toBeVisible();
  await expect(page.getByTestId('plan-count-total')).toHaveText(`${String(before + 1)} cases`);
  await page.getByTestId('plan-sync').click();
  await expect(page.getByTestId('sync-dialog')).toContainText('already matches');
});

test('build a plan: tri-state folders with the keyboard, folder assignment, a filter', async ({ page }) => {
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Mobile web sanity' }).click();
  await expect(page.getByTestId('plan-count-total')).toHaveText('3 cases');
  const tree = page.getByTestId('plan-tree-view');
  const folder = tree.getByRole('treeitem', { name: 'Mobile web' });
  await expect(folder).toHaveAttribute('aria-checked', 'mixed');
  await folder.focus();
  await page.keyboard.press(' ');
  await expect(folder).toHaveAttribute('aria-checked', 'true');
  await expect(page.getByTestId('plan-count-total')).toHaveText('8 cases');
  await page.keyboard.press('ArrowRight');
  const firstCase = tree.getByTestId('plan-tree-case').first();
  await expect(firstCase).toHaveAttribute('aria-checked', 'true');
  await page.keyboard.press('ArrowDown');
  await expect(firstCase).toBeFocused();
  await page.keyboard.press(' ');
  await expect(firstCase).toHaveAttribute('aria-checked', 'false');
  await expect(folder).toHaveAttribute('aria-checked', 'mixed');
  await expect(page.getByTestId('plan-count-total')).toHaveText('7 cases');

  const group = page.getByTestId('plan-group').filter({ hasText: 'Mobile web' });
  await group.getByTestId('folder-assign-input').fill('maria@example.com');
  await group.getByTestId('folder-assign-apply').click();
  await expect(page.getByText('Assigned to maria@example.com')).toBeVisible();
  const assignees = group.getByTestId('plan-item-assignee');
  await expect(assignees).toHaveCount(7);
  for (const a of await assignees.all()) await expect(a).toHaveValue('maria@example.com');

  await page.goto('/p/web-shop/plans');
  await page.getByTestId('new-plan').click();
  await page.getByTestId('plan-name').fill('Cart regression');
  await page.getByTestId('plan-create').click();
  await expect(page.getByRole('heading', { name: 'Cart regression' })).toBeVisible();
  await page.getByTestId('plan-filter-open').click();
  await page.getByTestId('filter-folder').click();
  await page.getByRole('option', { name: 'Storefront / Cart' }).click();
  await page.getByTestId('filter-save').click();
  const dialog = page.getByTestId('sync-dialog');
  await expect(dialog.getByTestId('sync-added')).toHaveText(/\+ \d+ added/);
  const added = Number((await dialog.getByTestId('sync-added').textContent())?.match(/\d+/)?.[0]);
  expect(added).toBeGreaterThan(5);
  await dialog.getByTestId('sync-apply').click();
  await expect(page.getByTestId('plan-count-total')).toHaveText(`${String(added)} cases`);
  await expect(page.getByTestId('plan-filter')).toContainText('in Storefront / Cart');
});

test('start a combined launch from a plan and get the testplan command', async ({ page }) => {
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
  await expect(page.getByTestId('plan-count-automated')).toHaveText('6 automated');
  await page.getByTestId('plan-start').click();
  const dialog = page.getByTestId('start-dialog');
  await expect(dialog.getByTestId('start-automated')).toBeChecked();
  await expect(dialog.getByTestId('testplan-command-preview')).toContainText('fastproof testplan --launch plan-');
  await dialog.getByTestId('start-submit').click();
  const started = page.getByTestId('launch-started');
  await expect(started.getByTestId('testplan-command')).toContainText('> testplan.json');
  await expect(started.getByTestId('testplan-command')).toContainText('ALLURE_TESTPLAN_PATH');
  await started.getByTestId('go-run').click();
  await expect(page.getByTestId('run-case-title')).toBeVisible();
  await expect(page.getByTestId('queue-item')).toHaveCount(4);
  await page.getByTestId('queue-mine').uncheck();
  await expect(page.getByTestId('queue-item')).toHaveCount(40);
});
