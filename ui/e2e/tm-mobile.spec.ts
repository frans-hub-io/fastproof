import { expect, test } from '@playwright/test';
import { noHorizontalScroll, openManualLaunch } from './tm-helpers';

test.use({ viewport: { width: 360, height: 780 } });

test('360 px: case cards, the tree in a drawer, a case and a plan fit', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-card').first()).toBeVisible();
  await expect(page.getByTestId('case-tree')).toHaveCount(0);
  await noHorizontalScroll(page);
  await page.getByTestId('open-tree-drawer').click();
  const drawer = page.getByRole('dialog');
  await drawer.getByRole('treeitem', { name: 'Mobile web' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByTestId('open-tree-drawer')).toHaveText('Mobile web');
  await expect(page.getByTestId('case-card').first()).toContainText('Navigation drawer on a phone');
  await page.getByTestId('case-card').first().getByTestId('case-link').click();
  await expect(page.getByTestId('case-title')).toBeVisible();
  await noHorizontalScroll(page);
  await page.getByTestId('case-edit').click();
  await expect(page.getByTestId('steps-editor')).toBeVisible();
  await noHorizontalScroll(page);

  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
  await expect(page.getByTestId('plan-item').first()).toBeVisible();
  await noHorizontalScroll(page);
});

test('360 px runner: one step per card, a sticky bar of 44 px buttons, camera capture', async ({ page }) => {
  await openManualLaunch(page);
  await noHorizontalScroll(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('continue-testing').click();
  const card = page.getByTestId('mobile-step');
  await expect(card).toBeVisible();
  await expect(page.getByTestId('run-steps')).toHaveCount(0);
  await noHorizontalScroll(page);
  const bar = page.getByTestId('sticky-bar');
  for (const s of ['passed', 'failed', 'blocked', 'skipped']) {
    const box = await bar.getByTestId(`set-${s}`).boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
  }
  const camera = card.getByTestId('step-camera');
  await expect(camera).toHaveAttribute('capture', 'environment');
  await expect(camera).toHaveAttribute('accept', 'image/*,video/*');
  const before = await card.getAttribute('data-path');
  await bar.getByTestId('set-passed').click();
  await expect(card).not.toHaveAttribute('data-path', before ?? '');
  await page.getByTestId('open-queue').click();
  await expect(page.getByRole('dialog').getByTestId('queue-item').first()).toBeVisible();
});
