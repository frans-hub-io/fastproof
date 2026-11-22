import { expect, test } from '@playwright/test';
import { randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, ENABLED, FIXTURES, PROJECT, seeded } from './env';
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

  await page.getByTestId('explorer-view').click();
  await page.getByRole('option', { name: 'Behaviors', exact: true }).click();
  await expect(tree.getByTestId('explorer-group').filter({ hasText: 'Web shop' })).toBeVisible();
  await page.getByTestId('explorer-view').click();
  await page.getByRole('option', { name: 'Packages', exact: true }).click();
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

async function api(method: string, path: string, body?: BodyInit, contentType = 'application/json'): Promise<{ status: number; body: Record<string, unknown> }> {
  const res = await fetch(`${BASE_URL}${path}`, { method, headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, 'Content-Type': contentType }, body });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as Record<string, unknown>) : {} };
}

test('explorer: a Playwright launch with two projects switches environments, groups by a label and filters', async ({ page }) => {
  const l = await api('POST', `/api/v1/projects/${PROJECT}/launches`, JSON.stringify({ externalId: `e2e-envs-${randomBytes(4).toString('hex')}`, name: 'playwright projects', vcs: { branch: 'e2e/environments' } }));
  expect(l.status).toBeLessThan(300);
  const id = String(l.body.id);
  const u = await api('POST', `/api/v1/projects/${PROJECT}/launches/${id}/uploads?fileName=results.json`, readFileSync(join(FIXTURES, 'playwright/results.json')), 'application/octet-stream');
  expect(u.status).toBe(202);
  await expect
    .poll(async () => (await api('GET', `/api/v1/projects/${PROJECT}/launches/${id}/uploads`)).body.items as { status: string }[] | undefined, { timeout: 60_000 })
    .toEqual([expect.objectContaining({ status: 'done' })]);
  expect((await api('POST', `/api/v1/projects/${PROJECT}/launches/${id}/finish`, '{}')).status).toBeLessThan(300);

  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/launches/${id}`);
  const box = page.getByTestId('environments');
  await expect(box.getByTestId('environment-row')).toHaveCount(2);
  const staging = box.locator('label').filter({ hasText: /^staging · \d+$/ });
  const n = Number(/(\d+)$/.exec((await staging.textContent()) ?? '')?.[1]);
  await staging.click();
  await expect(page).toHaveURL(/env=staging/);
  await expect(page.getByTestId('stat-total')).toContainText(String(n));
  await expect(page.getByTestId('explorer').getByText(new RegExp(`^${String(n)} tests`))).toBeVisible();

  await page.getByTestId('explorer-view').click();
  await page.getByRole('option', { name: 'Custom…', exact: true }).click();
  await page.getByTestId('explorer-custom-levels').click();
  await page.getByRole('option', { name: /^tag/ }).click();
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(/by=tag/);
  const tree = page.getByTestId('explorer-tree');
  await expect(tree.locator('[data-testid="explorer-group"][aria-level="1"]').first()).toBeVisible();

  await page.getByTestId('explorer-filters').click();
  const menu = page.getByTestId('explorer-filters-menu');
  const failing = await api('GET', `/api/v1/projects/${PROJECT}/launches/${id}/facets?env=staging`);
  const kinds = (failing.body.attachments as { value: string; count: number }[]).filter((a) => a.count > 0);
  expect(kinds.length).toBeGreaterThan(0);
  const kind = kinds[0];
  await menu.getByTestId(`filter-attachment-${kind?.value ?? ''}`).check();
  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(new RegExp(`attachment=${kind?.value ?? ''}`));
  await expect(page.getByTestId('explorer').getByText(new RegExp(`^${String(kind?.count)} of ${String(n)} tests`))).toBeVisible();
  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('result-row')).toHaveCount(kind?.count ?? -1);
  w.check();
});

