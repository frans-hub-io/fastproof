import { expect, test, type Page } from '@playwright/test';
import { openRichLaunch } from './helpers';

const tree = (page: Page) => page.getByTestId('explorer-tree');
const group = (page: Page, name: string) => tree(page).locator('[data-testid="explorer-group"]').filter({ has: page.getByText(name, { exact: true }) });
const testRow = (page: Page, name: string) => tree(page).locator(`[data-testid="explorer-test"][title="${name}"]`);
const chip = (page: Page, label: string) => page.getByTestId('explorer').locator('label').filter({ hasText: new RegExp(`^${label} \\d+$`) });
const groups = (page: Page) => tree(page).getByTestId('explorer-group');
const topGroups = (page: Page) => tree(page).locator('[data-testid="explorer-group"][aria-level="1"]');
async function pickView(page: Page, label: string) {
  await page.getByTestId('explorer-view').click();
  await page.getByRole('option', { name: label, exact: true }).click();
}

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
  await pickView(page, 'Behaviors');
  await expect(page).toHaveURL(/view=features/);
  await expect(async () => {
    await page.getByTestId('explorer-collapse-all').click();
    await expect(group(page, 'Checkout')).toBeVisible({ timeout: 1_000 });
  }).toPass({ timeout: 15_000 });
  await pickView(page, 'Packages');
  await expect(page).toHaveURL(/view=packages/);
  await pickView(page, 'Suites');
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

test('group by a label preset or a custom list of labels; results without a label sit in (none)', async ({ page }) => {
  await openRichLaunch(page);
  await pickView(page, 'Severity');
  await expect(page).toHaveURL(/view=severity/);
  await expect(topGroups(page).filter({ hasText: 'critical' })).toBeVisible();
  await pickView(page, 'Custom…');
  await expect(page).toHaveURL(/view=custom/);
  const levels = page.getByTestId('explorer-custom-levels');
  await levels.click();
  await expect(page.getByRole('option', { name: /^owner/ })).toContainText(/\d+ · 4 values/);
  await page.getByRole('option', { name: /^tag/ }).click();
  await page.getByRole('option', { name: /^framework/ }).click();
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/by=tag%2Cframework|by=tag,framework/);
  await page.getByTestId('explorer-collapse-all').click();
  await expect(topGroups(page).filter({ hasText: 'smoke' })).toBeVisible();
  const none = topGroups(page).filter({ hasText: '(none)' });
  await expect(none, 'tests without a tag').toBeVisible();
  await none.click();
  await expect(tree(page).locator('[data-testid="explorer-group"][aria-level="2"]').filter({ hasText: 'junit5' })).toBeVisible();
  await page.reload();
  await expect(page.getByTestId('explorer-view')).toHaveValue('Custom…');
  await expect(topGroups(page).filter({ hasText: '(none)' })).toBeVisible();
});

test('filters with counts narrow the tree and the result list and live in the URL', async ({ page }) => {
  await openRichLaunch(page);
  const before = await topGroups(page).count();
  await page.getByTestId('explorer-filters').click();
  const menu = page.getByTestId('explorer-filters-menu');
  await expect(menu.getByTestId('filter-severity-critical')).toBeVisible();
  await expect(menu.locator('label').filter({ hasText: /^critical\s*\d+$/ })).toBeVisible();
  await menu.getByTestId('filter-severity-critical').check();
  await menu.getByTestId('filter-attachment-screenshot').check();
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/severity=critical/);
  await expect(page).toHaveURL(/attachment=screenshot/);
  await expect(page.getByTestId('active-filter-severity')).toContainText('critical');
  await expect(page.getByTestId('explorer-filters')).toContainText('2');
  await page.getByTestId('explorer-expand-all').click();
  const shown = await tree(page).getByTestId('explorer-test').count();
  expect(shown).toBeGreaterThan(0);
  await expect(page.getByText(new RegExp(`^${String(shown)} of \\d+ tests`))).toBeVisible();

  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('active-filter-severity')).toBeVisible();
  await expect(page.getByTestId('result-row')).toHaveCount(shown);
  await page.getByTestId('explorer-clear-filters').click();
  await expect(page).not.toHaveURL(/severity=/);
  await expect.poll(() => page.getByTestId('result-row').count()).toBeGreaterThan(shown);
  await page.getByRole('tab', { name: 'Tree' }).click();
  await expect.poll(() => topGroups(page).count()).toBe(before);
});

test('an environment switcher scopes the summary, the tree and the results', async ({ page }) => {
  await openRichLaunch(page);
  const box = page.getByTestId('environments');
  await expect(box.getByTestId('environment-row')).toHaveCount(2);
  const label = (await box.locator('label').filter({ hasText: /^staging-firefox · \d+$/ }).textContent()) ?? '';
  const n = Number(/(\d+)$/.exec(label)?.[1]);
  expect(n).toBeGreaterThan(0);
  await box.locator('label').filter({ hasText: /^staging-firefox/ }).click();
  await expect(page).toHaveURL(/env=staging-firefox/);
  await expect(page.getByTestId('summary-scope')).toHaveText('Results in staging-firefox');
  await expect(page.getByTestId('stat-total')).toContainText(String(n));
  await expect(page.getByTestId('explorer').getByText(new RegExp(`^${String(n)} tests`))).toBeVisible();
  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('result-row')).toHaveCount(n);
  await box.locator('label').filter({ hasText: /^All environments/ }).click();
  await expect(page).not.toHaveURL(/env=/);
});


test('a group page the server refuses as busy loads by itself after a short wait', async ({ page }) => {
  await page.addInitScript(() => window.localStorage.setItem('fastproof.mock.treeBudget', '30'));
  await page.addInitScript(() => {
    const w = window as unknown as { __busy: number; __treeTests: number };
    w.__busy = 0;
    w.__treeTests = 0;
    const real = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
      if (url.includes('/tree/tests')) {
        w.__treeTests++;
        if (w.__busy === 0) {
          w.__busy++;
          return Promise.resolve(new Response(JSON.stringify({ error: { code: 'too_many_requests', message: 'too many concurrent queries for this caller' } }), { status: 429, headers: { 'Content-Type': 'application/json', 'Retry-After': '1' } }));
        }
      }
      return real(input, init);
    };
  });
  await openRichLaunch(page);
  await page.getByTestId('explorer-expand-all').click();
  await expect.poll(() => tree(page).getByTestId('explorer-test').count()).toBeGreaterThan(15);
  const seen = await page.evaluate(() => {
    const w = window as unknown as { __busy: number; __treeTests: number };
    return { busy: w.__busy, calls: w.__treeTests };
  });
  expect(seen.busy, 'one page was refused').toBe(1);
  expect(seen.calls, 'and asked for again').toBeGreaterThan(1);
  await expect(tree(page).getByText(/too many|failed to load|retry/i)).toHaveCount(0);
});
