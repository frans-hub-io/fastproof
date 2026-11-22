import { expect, test, type Page } from '@playwright/test';
import { openRichLaunch } from './helpers';

const tree = (page: Page) => page.getByTestId('explorer-tree');
const group = (page: Page, name: string) => tree(page).locator('[data-testid="explorer-group"]').filter({ has: page.getByText(name, { exact: true }) });
const testRow = (page: Page, name: string) => tree(page).locator(`[data-testid="explorer-test"][title="${name}"]`);
const chip = (page: Page, label: string) => page.getByTestId('explorer').locator('label').filter({ hasText: new RegExp(`^${label} \\d+$`) });
const groups = (page: Page) => tree(page).getByTestId('explorer-group');

test('the launch opens on the tree: failing paths are open down to the tests', async ({ page }) => {
  await openRichLaunch(page);
  await expect(page.getByRole('tab', { name: 'Tree' })).toHaveAttribute('aria-selected', 'true');
  await expect(tree(page)).toBeVisible();
  const first = groups(page).first();
  await expect(first).toHaveAttribute('data-status', 'failed');
  await expect(first).toHaveAttribute('aria-expanded', 'true');
  await expect(first.locator('[data-count="failed"]')).toBeVisible();
  await expect(tree(page).locator('[data-testid="explorer-test"][data-status="failed"]').first()).toBeVisible();

  await page.getByTestId('explorer-collapse-all').click();
  await expect(tree(page).getByTestId('explorer-test')).toHaveCount(0);
  await expect(first).toHaveAttribute('aria-expanded', 'false');
  await expect(group(page, 'e2e')).toBeVisible();
  await group(page, 'e2e').click();
  await expect(group(page, 'e2e')).toHaveAttribute('aria-expanded', 'true');
  await expect(group(page, 'checkout.spec.ts')).toBeVisible();
  await group(page, 'checkout.spec.ts').click();
  await group(page, 'Checkout').click();
  await expect(testRow(page, 'pays with saved card')).toBeVisible();
  await expect(testRow(page, 'pays with saved card').getByTestId('explorer-retries')).toHaveText('×2');
  await expect(testRow(page, 'pays with saved card')).toHaveAttribute('aria-level', '4');

  await page.reload();
  await expect(testRow(page, 'pays with saved card'), 'the expansion is remembered in the session').toBeVisible();
  await expect(groups(page).first()).toHaveAttribute('aria-expanded', 'false');
  await page.getByTestId('explorer-expand-all').click();
  await expect.poll(() => tree(page).getByTestId('explorer-test').count()).toBeGreaterThan(15);
});

test('selecting a nested failed test shows it beside the tree, with back and forward', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('explorer-search').fill('pays with saved card');
  await testRow(page, 'pays with saved card').click();
  await expect(page).toHaveURL(/[?&]test=[0-9a-f-]{36}/);
  const detail = page.getByTestId('explorer-detail');
  await expect(detail.getByTestId('explorer-detail-name')).toHaveText('pays with saved card');
  await expect(detail.getByTestId('step-tree')).toBeVisible();
  await expect(detail.locator('[data-testid="step"][data-status="failed"]').first()).toBeVisible();
  await expect(detail.getByTestId('diff-view').first()).toBeVisible();
  await expect(detail.getByTestId('history-strip')).toBeVisible();
  await expect(detail.getByRole('tab', { name: 'Attempt 2' })).toHaveAttribute('aria-selected', 'true');
  await detail.getByRole('tab', { name: /Attachments/ }).click();
  await expect(detail.getByTestId('attachment-item').first()).toBeVisible();
  await expect(testRow(page, 'pays with saved card')).toHaveAttribute('aria-selected', 'true');

  await page.getByTestId('explorer-search').fill('');
  await expect(page).not.toHaveURL(/q=/);
  const other = tree(page).locator('[data-testid="explorer-test"][data-status="failed"]:not([title="pays with saved card"])').first();
  const otherName = (await other.getAttribute('title')) ?? '';
  await other.click();
  await expect(detail.getByTestId('explorer-detail-name')).toHaveText(otherName);
  await expect(page).not.toHaveURL(/section=/);
  await page.goBack();
  await expect(detail.getByTestId('explorer-detail-name')).toHaveText('pays with saved card');
  await page.goForward();
  await expect(detail.getByTestId('explorer-detail-name')).toHaveText(otherName);
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('explorer-empty-detail')).toBeVisible();

  await page.getByTestId('explorer-search').fill('pays with saved card');
  await testRow(page, 'pays with saved card').click();
  await detail.getByTestId('explorer-open-full').click();
  await expect(page).toHaveURL(/\/p\/web-shop\/results\//);
  await expect(page.getByRole('heading', { name: 'pays with saved card' })).toBeVisible();
});

test('a deep link opens the test and reveals it in the tree', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('explorer-collapse-all').click();
  await page.getByTestId('explorer-search').fill('rejects an expired card');
  await expect(testRow(page, 'rejects an expired card')).toBeVisible();
  await testRow(page, 'rejects an expired card').click();
  await expect(page).toHaveURL(/test=/);
  const url = new URL(page.url());
  url.searchParams.delete('q');
  url.searchParams.set('view', 'suites');
  await page.evaluate(() => window.sessionStorage.clear());
  await page.goto(url.toString());
  await expect(page.getByTestId('explorer-detail-name')).toHaveText('rejects an expired card');
  await expect(testRow(page, 'rejects an expired card')).toHaveAttribute('aria-selected', 'true');
  await expect(testRow(page, 'rejects an expired card')).toBeInViewport();
});

test('status chips and search filter the tree and keep the ancestors', async ({ page }) => {
  await openRichLaunch(page);
  await chip(page, 'Failed').click();
  await expect(page).toHaveURL(/status=failed/);
  await expect.poll(async () => new Set(await tree(page).getByTestId('explorer-test').evaluateAll((els) => els.map((e) => e.getAttribute('data-status'))))).toEqual(new Set(['failed']));
  await expect(groups(page).first()).toHaveAttribute('data-status', 'failed');
  const statuses = await tree(page).getByTestId('explorer-group').evaluateAll((els) => els.map((e) => e.getAttribute('data-status')));
  expect(new Set(statuses)).toEqual(new Set(['failed']));
  await chip(page, 'Failed').click();

  await page.getByTestId('explorer-search').fill('pays with saved card');
  await expect(page).toHaveURL(/q=pays/);
  await expect(tree(page).getByTestId('explorer-test')).toHaveCount(1);
  await expect(testRow(page, 'pays with saved card')).toBeVisible();
  await expect(tree(page).getByTestId('explorer-group')).toHaveCount(3);
  await expect(group(page, 'e2e')).toHaveAttribute('aria-expanded', 'true');
  await expect(group(page, 'Checkout')).toHaveAttribute('aria-expanded', 'true');

  await page.getByTestId('explorer-search').fill('cart.spec');
  await expect(group(page, 'cart.spec.ts')).toBeVisible();
  await expect.poll(() => tree(page).getByTestId('explorer-test').count(), { message: 'a matching group keeps its tests' }).toBe(20);
  await page.getByTestId('explorer-search').fill('no such test at all');
  await expect(page.getByText('No tests match')).toBeVisible();
});

test('keyboard: arrows move, open and close groups, Enter shows a test', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('explorer-collapse-all').click();
  await expect(tree(page).getByTestId('explorer-test')).toHaveCount(0);
  await tree(page).focus();
  const active = () => tree(page).locator('[data-active="true"]');
  await expect(active()).toHaveAttribute('data-index', '0');
  const firstName = await active().getAttribute('title');
  await page.keyboard.press('ArrowRight');
  await expect(active()).toHaveAttribute('aria-expanded', 'true');
  await page.keyboard.press('ArrowRight');
  await expect(active()).toHaveAttribute('data-index', '1');
  await page.keyboard.press('ArrowLeft');
  await expect(active()).toHaveAttribute('data-index', '0');
  await expect(active()).toHaveAttribute('title', firstName ?? '');
  await page.keyboard.press('ArrowLeft');
  await expect(active()).toHaveAttribute('aria-expanded', 'false');
  await page.keyboard.press('ArrowDown');
  await expect(active()).toHaveAttribute('data-index', '1');
  await page.keyboard.press('ArrowUp');
  await page.keyboard.press('End');
  await page.keyboard.press('Home');
  await expect(active()).toHaveAttribute('data-index', '0');

  for (let i = 0; i < 12; i++) {
    const kind = await active().getAttribute('data-testid');
    if (kind === 'explorer-test') break;
    await page.keyboard.press(kind === 'explorer-group' && (await active().getAttribute('aria-expanded')) === 'false' ? 'ArrowRight' : 'ArrowDown');
  }
  await expect(active()).toHaveAttribute('data-testid', 'explorer-test');
  const name = await active().getAttribute('title');
  await page.keyboard.press('Enter');
  await expect(page.getByTestId('explorer-detail-name')).toHaveText(name ?? '');
  await expect(page).toHaveURL(/test=/);
});

test('views, sort and big groups read page by page', async ({ page }) => {
  await page.addInitScript(() => window.localStorage.setItem('fastproof.mock.treeBudget', '30'));
  await openRichLaunch(page);
  await page.getByTestId('explorer-view').getByText('Features').click();
  await expect(page).toHaveURL(/view=features/);
  await expect(async () => {
    await page.getByTestId('explorer-collapse-all').click();
    await expect(group(page, 'Checkout')).toBeVisible({ timeout: 1_000 });
  }).toPass({ timeout: 15_000 });
  await page.getByTestId('explorer-view').getByText('Packages').click();
  await expect(page).toHaveURL(/view=packages/);
  await page.getByTestId('explorer-view').getByText('Suites').click();
  await expect(page).not.toHaveURL(/view=/);
  await expect(async () => {
    await page.getByTestId('explorer-expand-all').click();
    expect(await tree(page).getByTestId('explorer-test').count(), 'paged groups load as they scroll in').toBeGreaterThan(30);
  }).toPass({ timeout: 30_000 });

  await page.getByTestId('explorer-sort').click();
  await page.getByRole('menuitem', { name: /Duration/ }).click();
  await expect(page).toHaveURL(/sort=duration/);
  await chip(page, 'Failed').click();
  await expect.poll(async () => new Set(await tree(page).getByTestId('explorer-test').evaluateAll((els) => els.map((e) => e.getAttribute('data-status'))))).toEqual(new Set(['failed']));
});
