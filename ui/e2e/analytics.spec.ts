import { expect, test } from '@playwright/test';

test('compare picks two launches, shows sections with counts and opens a result', async ({ page }) => {
  await page.goto('/p/web-shop/compare');
  await expect(page).toHaveURL(/a=.+&b=.+|b=.+&a=.+/);
  await expect(page.getByRole('heading', { name: 'Compare launches' })).toBeVisible();
  await expect(page.getByText(/#40 \(A\) → #41 \(B\)/)).toBeVisible();
  const newFailures = page.getByTestId('compare-count-newFailures');
  await expect(newFailures).toBeVisible();
  const count = Number((await newFailures.innerText()).match(/(\d+)\s*$/)?.[1] ?? '0');
  expect(count).toBeGreaterThan(0);
  await expect(page.getByTestId('compare-row')).toHaveCount(count);

  await page.getByTestId('compare-count-durationChanges').click();
  await expect(page.getByTestId('duration-row').first()).toBeVisible();

  await page.getByRole('button', { name: 'Swap A and B' }).click();
  await expect(page.getByText(/#41 \(A\) → #40 \(B\)/)).toBeVisible();

  await page.getByTestId('compare-count-newFailures').click();
  const row = page.getByTestId('compare-row').first();
  if (await row.count()) {
    await row.click();
    await expect(page).toHaveURL(/\/p\/web-shop\/results\//);
  }
});

test('compare from a launch link defaults A to the previous launch', async ({ page }) => {
  await page.goto('/p/web-shop');
  const href = await page.getByRole('link', { name: '#41 web-shop nightly' }).getAttribute('href');
  const id = href?.split('/').pop() ?? '';
  await page.goto(`/p/web-shop/compare?b=${id}`);
  await expect(page.getByText(/#40 \(A\) → #41 \(B\)/)).toBeVisible();
});

test('dashboard renders charts, switches the bucket and filters by branch', async ({ page }) => {
  await page.goto('/p/web-shop/dashboard');
  for (const id of ['chart-pass-rate', 'chart-durations', 'chart-launches', 'chart-defects']) {
    await expect(page.getByTestId(id).locator('svg.recharts-surface').first()).toBeVisible();
  }
  await expect(page.getByTestId('flaky-top').getByRole('link').first()).toBeVisible();
  await expect(page.getByTestId('most-failing').getByRole('link').first()).toBeVisible();
  const ticks = () => page.getByTestId('chart-launches').locator('.recharts-bar-rectangle').count();
  const daily = await ticks();

  await page.getByText('Week', { exact: true }).click();
  await expect(page).toHaveURL(/bucket=week/);
  await expect.poll(ticks).toBeLessThan(daily);

  await page.getByRole('combobox', { name: 'Branch' }).fill('feature/checkout-v2');
  await expect(page).toHaveURL(/branch=feature%2Fcheckout-v2/);
  await expect(page.getByText('Latest pass rate')).toBeVisible();

  await page.getByRole('combobox', { name: 'Branch' }).fill('no-such-branch');
  await expect(page.getByText('No launches in this range')).toBeVisible();
});

test('flaky tests are listed by score with a status strip', async ({ page }) => {
  await page.goto('/p/web-shop/flaky');
  const rows = page.getByTestId('flaky-row');
  await expect(rows.first()).toBeVisible();
  expect(await rows.count()).toBeGreaterThan(3);
  await expect(rows.first().getByTestId('flaky-strip')).toBeVisible();
  await rows.first().getByRole('link').click();
  await expect(page).toHaveURL(/\/p\/web-shop\/test-cases\//);
});

test('scheduler jobs are listed for an admin and hidden from a viewer', async ({ page }) => {
  await page.goto('/admin/jobs');
  await expect(page.getByTestId('job-row')).toHaveCount(6);
  await expect(page.getByTestId('job-row').filter({ hasText: 'retention.apply' })).toContainText('failed');

  await page.goto('/admin/jobs?mockRole=viewer');
  await expect(page.getByText('Access denied')).toBeVisible();
  await expect(page.getByTestId('job-row')).toHaveCount(0);
  await page.evaluate(() => window.localStorage.removeItem('fastproof-mock-role'));
});
