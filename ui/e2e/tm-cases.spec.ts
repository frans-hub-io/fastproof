import { expect, test } from '@playwright/test';
import { MOD, openCase } from './tm-helpers';

test('folder → case → steps → save → version → restore', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  const tree = page.getByTestId('case-tree-view');
  await tree.getByRole('treeitem', { name: 'Storefront' }).click();
  await expect(page).toHaveURL(/folder=/);
  await tree.getByRole('treeitem', { name: 'Storefront' }).press('ArrowRight');
  await tree.getByRole('treeitem', { name: 'Storefront' }).press('ArrowDown');
  await expect(tree.getByRole('treeitem', { name: 'Catalog' })).toBeFocused();
  await page.keyboard.press('c');
  await expect(tree.getByRole('treeitem', { name: 'Cart' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('case-link').first()).toHaveText('Add a product to the cart');

  const n = await openCase(page, 'Pay with a saved card');
  await expect(page.getByTestId('step-view')).toHaveCount(5);
  await page.getByTestId('case-edit').click();
  const lastAction = page.getByTestId('step-action').last();
  await lastAction.click();
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(page.getByTestId('step-edit')).toHaveCount(6);
  await expect(page.getByTestId('step-action').nth(5)).toBeFocused();
  await page.keyboard.type('Open the **receipt** email');
  await page.getByTestId('step-expected').nth(5).fill('The receipt lists the card ending in 4242');
  await page.getByTestId('step-action').nth(5).focus();
  await page.keyboard.press('Alt+ArrowUp');
  await expect(page.getByTestId('step-action').nth(4)).toHaveValue('Open the **receipt** email');
  await page.keyboard.press('Tab');
  await expect(page.locator('[data-step-path="4.1"]')).toBeVisible();
  await page.keyboard.press('Shift+Tab');
  await expect(page.locator('[data-step-path="5"]')).toBeVisible();
  await page.keyboard.press(`${MOD}+s`);
  await expect(page.getByText(`TC-${String(n)} is now version 5`)).toBeVisible();
  await expect(page.getByTestId('step-view')).toHaveCount(6);
  await expect(page.getByTestId('step-view').nth(4)).toContainText('Open the receipt email');

  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('version-row')).toHaveCount(5);
  await expect(page.getByText('v4 → v5')).toBeVisible();
  const added = page.getByTestId('diff-step').filter({ hasText: 'added' });
  await expect(added).toContainText('Open the **receipt** email');
  await page.getByTestId('version-row').filter({ hasText: 'v4' }).getByTestId('version-restore').click();
  await page.getByTestId('version-restore-confirm').click();
  await expect(page.getByText('Version 4 restored')).toBeVisible();
  await expect(page.getByTestId('version-row')).toHaveCount(6);
  await expect(page.getByTestId('version-row').first()).toContainText('restored');
  await page.getByRole('tab', { name: 'Scenario' }).click();
  await expect(page.getByTestId('step-view')).toHaveCount(5);
});

test('version diff matches steps by id across moves', async ({ page }) => {
  await openCase(page, 'Pay with a saved card');
  await page.getByRole('tab', { name: /Versions/ }).click();
  await page.getByLabel('Compare version 2').check();
  await page.getByLabel('Compare version 3').check();
  await expect(page.getByText('v2 → v3')).toBeVisible();
  await expect(page.getByTestId('diff-field').filter({ hasText: 'Priority' })).toContainText('Critical');
  await expect(page.getByTestId('diff-step').filter({ hasText: 'moved' })).toHaveCount(2);
  const changed = page.getByTestId('diff-step').filter({ hasText: 'changed' });
  await expect(changed).toHaveCount(1);
  await expect(changed).toContainText('masked card');
  await expect(page.getByTestId('diff-step').filter({ hasText: 'removed' })).toHaveCount(0);
});

test('412: a code sync during editing shows the banner and the conflict dialog', async ({ page }) => {
  const n = await openCase(page, 'Pay with a new card');
  await page.getByTestId('case-edit').click();
  await page.getByTestId('case-name-input').fill('Pay with a new card (edited)');
  await page.evaluate((num) => (window as unknown as { __fastproofMockTm: { touchCase: (n: number) => number } }).__fastproofMockTm.touchCase(num), n);
  await expect(page.getByTestId('case-changed-banner')).toContainText('the code sync');
  await page.getByTestId('case-save').click();
  const dialog = page.getByTestId('conflict-dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('saved by ci through the code sync');
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Name' })).toContainText('Pay with a new card (edited)');
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Description' })).toContainText('Updated by the code sync');
  await page.getByTestId('conflict-mine').click();
  await expect(page.getByText(`TC-${String(n)} is now version`)).toBeVisible();
  await expect(page.getByTestId('case-title')).toContainText('Pay with a new card (edited)');
  await expect(page.getByTestId('conflict-dialog')).toHaveCount(0);
});

test('a draft survives a reload and code-owned fields are locked', async ({ page }) => {
  await openCase(page, 'Expired card is rejected');
  await page.getByTestId('case-edit').click();
  await page.getByTestId('case-preconditions').fill('A card that expired last month');
  await page.waitForTimeout(800);
  await page.reload();
  await expect(page.getByTestId('draft-banner')).toBeVisible();
  await page.getByTestId('draft-restore').click();
  await expect(page.getByTestId('case-preconditions')).toHaveValue('A card that expired last month');
  await page.getByTestId('case-cancel').click();

  await page.goto('/p/web-shop/cases?automation=automated');
  await page.getByTestId('case-link').first().click();
  await expect(page.getByTestId('from-code').first()).toBeVisible();
  await page.getByTestId('case-edit').click();
  await expect(page.getByTestId('case-name-input')).toHaveCount(0);
  await expect(page.getByTestId('case-description-locked')).toBeVisible();
  await page.getByRole('tab', { name: 'Details' }).click();
  await expect(page.getByTestId('field-severity')).toBeDisabled();
  await expect(page.getByTestId('field-priority')).toBeEnabled();
});

test('AI draft: review it; AI suggestion: accept it', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await page.getByTestId('case-filters').getByText('AI draft', { exact: true }).click();
  await expect(page).toHaveURL(/aiUnreviewed=true/);
  await expect(page.getByTestId('case-row')).toHaveCount(1);
  await page.getByTestId('case-link').first().click();
  await expect(page.getByTestId('ai-draft-badge')).toBeVisible();
  await page.getByTestId('case-reviewed').click();
  await expect(page.getByTestId('live-region')).toHaveText('Marked as reviewed');
  await expect(page.getByTestId('ai-draft-badge')).toHaveCount(0);

  await openCase(page, 'Apply a discount code at checkout');
  const s = page.getByTestId('suggestion');
  await expect(s).toContainText('Suggested by fastproof-cases');
  await expect(s.getByTestId('diff-step').filter({ hasText: 'added' })).toContainText('Remove the coupon');
  await s.getByTestId('suggestion-accept').click();
  await expect(page.getByText('Suggestion accepted')).toBeVisible();
  await expect(page.getByTestId('suggestion')).toHaveCount(0);
  await expect(page.getByTestId('step-view').last()).toContainText('Remove the coupon');
  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('version-row').first()).toContainText('AI suggestion');
});

test('bulk move with the keyboard alternative to drag and drop, and unlinked tests', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await page.getByTestId('case-search').fill('Search');
  await expect(page).toHaveURL(/q=Search/);
  await page.getByTestId('case-check').nth(0).check();
  await page.getByTestId('case-check').nth(1).check();
  await expect(page.getByTestId('bulk-bar')).toContainText('2 selected');
  await page.getByTestId('bulk-move').click();
  await page.getByLabel('Find a folder').fill('mobile web');
  await page.keyboard.press('Enter');
  await expect(page.getByText(/2 updated/)).toBeVisible();
  await page.getByTestId('case-search').fill('');
  await page.getByTestId('case-tree-view').getByRole('treeitem', { name: 'Mobile web' }).click();
  await expect(page.getByTestId('case-row')).toHaveCount(10);

  await page.getByTestId('tab-unlinked').click();
  const row = page.getByTestId('unlinked-row').first();
  const name = (await row.locator('p').first().textContent()) ?? '';
  await row.getByTestId('unlinked-link').click();
  await page.getByTestId('link-case-search').fill('TC-1');
  await page.getByTestId('link-case-option').first().click();
  await expect(page.getByText('Linked')).toBeVisible();
  await expect(page.getByTestId('unlinked-row').filter({ hasText: name })).toHaveCount(0);
});

test('Ctrl+K finds a case by its number and name', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-row').first()).toBeVisible();
  await page.keyboard.press(`${MOD}+k`);
  await page.keyboard.type('TC-3 ');
  await expect(page.getByRole('option', { name: /TC-3 Search corrects a typo/ })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.keyboard.press(`${MOD}+k`);
  await page.keyboard.type('saved card');
  await page.getByRole('option', { name: /Pay with a saved card/ }).click();
  await expect(page.getByTestId('case-title')).toContainText('Pay with a saved card');
});
