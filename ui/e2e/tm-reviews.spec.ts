import { expect, test, type Page } from '@playwright/test';
import { enableReview, segment, spaGo, toastsGone } from './tm-reviews-helpers';
import { expectNoAxeViolations as axe, noHorizontalScroll } from './tm-helpers';

async function expectNoAxeViolations(page: Page, label: string): Promise<void> {
  await toastsGone(page);
  await axe(page, label);
}

test.describe.configure({ timeout: 150_000 });

test('review is off by default: the page, the tab and the settings say so', async ({ page }) => {
  await page.goto('/p/web-shop/reviews');
  await expect(page.getByText('Review is off for this project')).toBeVisible();
  await expect(page.getByTestId('reviews-off-link')).toBeVisible();
  await page.goto('/p/web-shop/cases/1?tab=review');
  await expect(page.getByTestId('review-off')).toBeVisible();
  await page.getByTestId('review-off-link').click();
  await expect(page.getByTestId('workflow-unconfigured')).toBeVisible();
  await expect(page.getByTestId('state-chip')).toHaveCount(0);
});

test('workflow editor: suggested workflow, client checks, policy', async ({ page }) => {
  await enableReview(page);
  const key = page.getByTestId('status-key').first();
  await key.fill('Bad Key');
  await expect(page.getByTestId('workflow-errors')).toContainText('Status 1: the key must start');
  await expect(page.getByTestId('workflow-save')).toBeDisabled();
  await page.getByTestId('workflow-reset').click();
  await expect(page.getByTestId('workflow-errors')).toHaveCount(0);
  await page.getByTestId('status-add').click();
  await expect(page.getByTestId('workflow-status')).toHaveCount(5);
  await expect(page.getByTestId('workflow-errors')).toContainText('Status 5');
  await page.getByTestId('workflow-reset').click();
  await expect(page.getByTestId('workflow-status')).toHaveCount(4);
  await page.getByTestId('policy-approvals').fill('2');
  await page.getByTestId('policy-save').click();
  await expect(page.getByText('2 approvals needed.')).toBeVisible();
});

test('a reviewer approves a case and moves it through the workflow', async ({ page }) => {
  await enableReview(page);
  await spaGo(page, '/p/web-shop/cases/1');
  const chip = page.getByTestId('state-chip');
  await expect(chip).toHaveAttribute('data-state', 'approved');
  const move = async (name: string) => {
    await page.getByTestId('state-menu').click();
    await page.getByRole('menuitem', { name: new RegExp(`^${name}`) }).click();
  };
  await move('Retired');
  await expect(chip).toHaveAttribute('data-state', 'retired');
  await move('Draft');
  await move('In review');
  await expect(chip).toHaveAttribute('data-state', 'in_review');
  await page.getByTestId('state-menu').click();
  const approved = page.getByTestId('state-option').filter({ hasText: 'Approved' });
  await expect(approved).toHaveAttribute('data-allowed', 'false');
  await expect(approved.getByTestId('state-why')).toContainText('needs an approved version');
  await page.keyboard.press('Escape');

  await page.getByRole('tab', { name: 'Review' }).click();
  await expect(page.getByTestId('review-tab')).toBeVisible();
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'pending');
  await expect(page.getByTestId('review-approvals')).toHaveText('Approvals 0 / 1');
  await expect(page.getByTestId('version-diff')).toBeVisible();
  await expect(page.getByTestId('review-tab')).toContainText('New case');
  await expectNoAxeViolations(page, 'review tab');
  await page.getByTestId('review-comment').fill('<b>Looks</b> right');
  await page.getByTestId('review-approve').click();
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'approved');
  await expect(page.getByTestId('vote-comment')).toHaveText('<b>Looks</b> right');
  await expect(page.getByTestId('review-approve')).toBeDisabled();
  await expect(page.getByTestId('review-why')).toContainText('already been decided');

  await move('Approved');
  await expect(chip).toHaveAttribute('data-state', 'approved');
});

test('refused votes: own change, request changes, version moved', async ({ page }) => {
  await enableReview(page);
  await spaGo(page, '/p/web-shop/cases/4?tab=review');
  await expect(page.getByTestId('review-why')).toContainText('cannot approve your own change');
  await expect(page.getByTestId('review-approve')).toBeDisabled();

  await spaGo(page, '/p/web-shop/cases/2?tab=review');
  await expect(page.getByTestId('review-tab')).toBeVisible();
  await page.getByTestId('review-comment').fill('Step 2 has no expected result.');
  await page.getByTestId('review-request').click();
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'changes_requested');
  await expect(page.getByTestId('vote')).toHaveCount(1);

  await spaGo(page, '/p/web-shop/cases/3?tab=review');
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'changes_requested');
  await expect(page.getByTestId('vote-comment')).toContainText('expected result of step 2');
  await expect(page.getByTestId('review-why')).toContainText('already been decided');

  await spaGo(page, '/p/web-shop/cases/5?tab=review');
  await expect(page.getByTestId('review-versions')).toContainText('v1');
  await page.evaluate(() => (window as unknown as { __fastproofMockTm: { touchCase: (n: number) => void } }).__fastproofMockTm.touchCase(5));
  await expect(page.getByTestId('review-versions')).toContainText('v2');
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'pending');
  await page.getByTestId('review-approve').click();
  await expect(page.getByTestId('review-state')).toHaveAttribute('data-state', 'approved');
});

test('the queue filters and the batch review of several cases', async ({ page }) => {
  await enableReview(page);
  await spaGo(page, '/p/web-shop/reviews');
  await expect(page.getByTestId('review-row').first()).toBeVisible();
  const all = await page.getByTestId('review-row').count();
  await expect(page.getByTestId('reviews-more')).toBeVisible();
  await page.getByTestId('reviews-more').click();
  await expect.poll(() => page.getByTestId('review-row').count()).toBeGreaterThan(all);
  await expectNoAxeViolations(page, 'reviews queue');

  await segment(page, 'Mine');
  await expect(page.getByTestId('review-row').first()).toBeVisible();
  await expect(page.getByTestId('review-row').filter({ hasNotText: 'alex@example.com' })).toHaveCount(0);
  await segment(page, 'Waiting for me');
  await expect(page.getByTestId('review-row').first()).toBeVisible();
  await expect(page.getByTestId('review-row').filter({ hasText: 'alex@example.com' })).toHaveCount(0);
  await segment(page, 'Everyone');
  await expect(page.getByTestId('review-row').filter({ hasText: 'alex@example.com' }).first()).toBeVisible();

  await page.getByTestId('tab-batch').click();
  await page.getByTestId('batch-load').click();
  await expect(page.getByTestId('batch-item').first()).toBeVisible();
  await expect(page.getByTestId('version-diff').first()).toBeVisible();
  await expectNoAxeViolations(page, 'batch review');
  await page.getByTestId('batch-include').nth(1).uncheck();
  const total = await page.getByTestId('batch-item').count();
  await expect(page.getByTestId('batch-count')).toContainText(`${String(total - 1)} of ${String(total)}`);
  await page.getByTestId('batch-comment').fill('Reviewed together.');
  await page.getByTestId('batch-approve').click();
  await expect(page.getByTestId('batch-result')).toBeVisible();
  await expect(page.getByTestId('batch-skipped').first()).toHaveAttribute('data-code', 'self_approval');
  await expect(page.getByTestId('batch-skipped').first()).toContainText('your own change');
  await page.getByTestId('tab-queue').click();
  await expect(page.getByTestId('tab-queue')).toHaveAttribute('aria-selected', 'true');
  await page.getByTestId('filter-state').click();
  await page.getByRole('option', { name: 'Approved' }).click();
  await expect(page.getByTestId('review-row').first()).toContainText('Approved');
});

test('batch scope: a test plan limits the changes', async ({ page }) => {
  await enableReview(page);
  await spaGo(page, '/p/web-shop/reviews?view=batch');
  await segment(page, 'Test plan');
  await page.getByTestId('batch-plan').click();
  await page.getByRole('option', { name: 'Checkout smoke' }).click();
  await page.getByTestId('batch-load').click();
  await expect(page.getByTestId('batch-item').first()).toBeVisible();
  const n = await page.getByTestId('batch-item').count();
  expect(n).toBeLessThan(100);
});

test('axe: workflow settings', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=workflow');
  await expect(page.getByTestId('workflow-unconfigured')).toBeVisible();
  await expectNoAxeViolations(page, 'workflow settings (none)');
  await page.getByTestId('workflow-use-suggested').click();
  await expect(page.getByTestId('workflow-form')).toBeVisible();
  await expectNoAxeViolations(page, 'workflow settings');
  await page.getByTestId('workflow-remove').click();
  await expect(page.getByTestId('workflow-remove-confirm')).toBeVisible();
  await expectNoAxeViolations(page, 'remove workflow dialog');
});

test('changing a category asks where the cases go', async ({ page }) => {
  await enableReview(page);
  await page.getByTestId('status-category').first().click();
  await page.getByRole('option', { name: 'Outdated' }).click();
  await page.getByTestId('workflow-save').click();
  await expect(page.getByTestId('remap-dialog')).toBeVisible();
  await expect(page.getByTestId('remap-confirm')).toBeDisabled();
  await expectNoAxeViolations(page, 'remap dialog');
  await page.getByTestId('remap-select').first().click();
  await page.getByRole('option', { name: 'In review (in_review)' }).click();
  await page.getByTestId('remap-confirm').click();
  await expect(page.getByTestId('remap-job')).toHaveAttribute('data-status', 'done');
  await page.getByTestId('workflow-remove').click();
  await page.getByTestId('workflow-remove-confirm').click();
  await expect(page.getByTestId('workflow-unconfigured')).toBeVisible();
});

test.describe('at 360 px', () => {
  test.use({ viewport: { width: 360, height: 780 } });

  test('the queue shows cards and nothing overflows', async ({ page }) => {
    await enableReview(page);
    await noHorizontalScroll(page);
    await spaGo(page, '/p/web-shop/reviews');
    await expect(page.getByTestId('review-card').first()).toBeVisible();
    await expect(page.getByTestId('review-row')).toHaveCount(0);
    await noHorizontalScroll(page);
    await expectNoAxeViolations(page, 'reviews at 360');
    await spaGo(page, '/p/web-shop/cases/1?tab=review');
    await expect(page.getByTestId('review-tab')).toBeVisible();
    await noHorizontalScroll(page);
    await spaGo(page, '/p/web-shop/reviews?view=batch');
    await page.getByTestId('batch-load').click();
    await expect(page.getByTestId('batch-item').first()).toBeVisible();
    await noHorizontalScroll(page);
  });
});
