import { expect, test } from '@playwright/test';
import { ADMIN_TOKEN, BASE_URL, ENABLED, PROJECT, seeded } from './env';
import { signInWithToken, Watcher } from './watch';

test.skip(!ENABLED, 'set FASTPROOF_E2E_REAL=1 to run against a real server');

const P = `/p/${PROJECT}`;

test('explorer: a nested failed Allure test opens beside the tree with its steps', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/launches/${s.launches['nightly 3'] ?? ''}`);
  const tree = page.getByTestId('explorer-tree');
  await expect(tree).toBeVisible();
  const failed = tree.locator('[data-testid="explorer-test"][data-status="failed"]').first();
  await expect(failed).toBeVisible();
  const level = Number(await failed.getAttribute('aria-level'));
  expect(level, 'tests sit under parentSuite › suite').toBeGreaterThanOrEqual(3);
  const name = (await failed.getAttribute('title')) ?? '';
  await failed.click();
  const detail = page.getByTestId('explorer-detail');
  await expect(detail.getByTestId('explorer-detail-name')).toHaveText(name);
  await expect(detail.getByTestId('step-tree')).toBeVisible();
  await expect(detail.getByTestId('step').first()).toBeVisible();
  await expect(page).toHaveURL(/[?&]test=[0-9a-f-]{36}/);

  await page.getByTestId('explorer').locator('label').filter({ hasText: /^Failed \d+$/ }).click();
  await expect.poll(async () => new Set(await tree.getByTestId('explorer-test').evaluateAll((els) => els.map((e) => e.getAttribute('data-status'))))).toEqual(new Set(['failed']));
  await page.getByTestId('explorer').locator('label').filter({ hasText: /^Failed \d+$/ }).click();

  await page.getByTestId('explorer-view').getByText('Features').click();
  await expect(tree.getByTestId('explorer-group').filter({ hasText: 'Web shop' })).toBeVisible();
  await page.getByTestId('explorer-view').getByText('Packages').click();
  await expect(tree.getByTestId('explorer-group').first()).toBeVisible();
  w.check();
});

test('explorer: Playwright and JUnit launches nest down to the tests; a deep link reveals the test', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/launches/${s.launches['playwright 1'] ?? ''}`);
  const tree = page.getByTestId('explorer-tree');
  await page.getByTestId('explorer-expand-all').click();
  const deepest = tree.locator('[data-testid="explorer-test"]').first();
  await expect(deepest).toBeVisible();
  expect(Number(await deepest.getAttribute('aria-level'))).toBeGreaterThanOrEqual(3);

  await page.goto(`${P}/launches/${s.launches['junit 1'] ?? ''}`);
  await page.getByTestId('explorer-expand-all').click();
  await expect(tree.getByTestId('explorer-test').first()).toBeVisible();
  const any = tree.getByTestId('explorer-test').nth(1);
  const name = (await any.getAttribute('title')) ?? '';
  await any.click();
  await expect(page.getByTestId('explorer-detail-name')).toHaveText(name);
  const url = page.url();
  await page.evaluate(() => window.sessionStorage.clear());
  await page.goto(url);
  await expect(page.getByTestId('explorer-detail-name')).toHaveText(name);
  await expect(tree.locator('[data-testid="explorer-test"][aria-selected="true"]')).toHaveAttribute('title', name);
  w.check();
});
