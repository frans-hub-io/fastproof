import { expect, test } from './console-guard';
import type { Page } from '@playwright/test';
import path from 'node:path';
import { expectNoAxeViolations, MOD, noHorizontalScroll } from './tm-helpers';

const SS1 = '0190a1b2-c3d4-7e5f-8a9b-000000000001';
const P = '/api/v1/projects/web-shop';
const dir = process.env.FASTPROOF_TM_SHOTS;

interface Reply {
  status: number;
  etag: string | null;
  body: Record<string, unknown>;
}

async function api(page: Page, method: string, url: string, body?: unknown, headers: Record<string, string> = {}): Promise<Reply> {
  return page.evaluate(
    async ({ method, url, body, headers }) => {
      const r = await fetch(url, { method, headers: { 'Content-Type': 'application/json', ...headers }, body: body === undefined ? undefined : JSON.stringify(body) });
      const text = await r.text();
      return { status: r.status, etag: r.headers.get('ETag'), body: (text ? JSON.parse(text) : {}) as Record<string, unknown> };
    },
    { method, url, body, headers },
  );
}

/** Navigates inside the app without a reload (the mock server lives in the page and a reload would reset it). */
async function go(page: Page, url: string): Promise<void> {
  await page.evaluate((u) => {
    window.history.pushState({}, '', u);
    window.dispatchEvent(new PopStateEvent('popstate'));
  }, url);
}

/** The launch that carries the seeded manual run (#38), reached without a reload. */
async function manualLaunch(page: Page): Promise<string> {
  await go(page, '/p/web-shop');
  await page.getByRole('link', { name: /^#38 / }).first().click();
  await expect(page.getByTestId('tab-manual')).toBeVisible();
  return /\/launches\/([^/?#]+)/.exec(page.url())?.[1] ?? '';
}

/** A case that references SS-1 between two own steps; returns its number. */
async function caseWithShared(page: Page): Promise<number> {
  await page.goto('/p/web-shop/shared-steps');
  await expect(page.getByTestId('shared-card').first()).toBeVisible();
  const r = await api(page, 'POST', `${P}/cases`, {
    name: 'Checkout as a customer',
    status: 'draft',
    body: {
      description: '',
      preconditions: 'A customer account exists',
      links: [],
      steps: [
        { id: 's_own_first', action: 'Open the shop', expected: 'The catalog is shown', data: '', attachments: [], steps: [] },
        { id: 's_ref_login', action: '', expected: '', data: '', attachments: [], steps: [], shared: { id: SS1, number: 1 } },
        { id: 's_own_last', action: 'Pay for the cart', expected: 'The order is placed', data: '', attachments: [], steps: [] },
      ],
    },
  });
  expect(r.status).toBe(201);
  return Number(r.body.number);
}

test('the shared steps list and a shared step with its versions and users', async ({ page }) => {
  await page.goto('/p/web-shop/shared-steps');
  await expect(page.getByTestId('shared-card')).toHaveCount(2);
  await noHorizontalScroll(page);
  await expectNoAxeViolations(page, 'shared steps list');
  await page.getByTestId('shared-search').fill('SS-2');
  await expect(page.getByTestId('shared-card')).toHaveCount(1);
  await page.getByTestId('shared-search').fill('');
  await page.getByTestId('shared-card').filter({ hasText: 'Log in as a customer' }).click();
  await expect(page.getByTestId('shared-title')).toContainText('SS-1');
  await expect(page.getByTestId('shared-step-row')).toHaveCount(4);
  await expectNoAxeViolations(page, 'shared step');
  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('version-diff')).toBeVisible();
  await expectNoAxeViolations(page, 'shared step versions');
  await page.getByRole('tab', { name: /Used by/ }).click();
  await expect(page.getByTestId('shared-usage')).toHaveCount(3);
});

test('edit a shared step into a new version, with the keyboard', async ({ page }) => {
  await page.goto('/p/web-shop/shared-steps/2');
  await page.getByTestId('shared-edit').click();
  await page.getByTestId('shared-name').fill('Add one item to the cart');
  await expect(page.getByTestId('step-insert-shared')).toHaveCount(0);
  await page.getByTestId('step-action').first().focus();
  await page.keyboard.press(`${MOD}+Enter`);
  await page.keyboard.type('Open the cart');
  await page.getByTestId('shared-save').click();
  await expect(page.getByTestId('shared-title')).toContainText('Add one item to the cart');
  await expect(page.getByTestId('shared-step-row')).toHaveCount(3);
  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('shared-version-row')).toHaveCount(2);
});

test('a case shows its shared step as a block, lists its changes, and takes a shared step in the editor', async ({ page }) => {
  const n = await caseWithShared(page);
  await go(page, `/p/web-shop/cases/${String(n)}`);
  const block = page.getByTestId('step-shared-view');
  await expect(block).toContainText('SS-1');
  await expect(block).toContainText('Log in as a customer');
  await expect(page.getByTestId('step-view')).toHaveCount(2);
  await block.getByTestId('shared-toggle').click();
  await expect(page.getByTestId('step-view')).toHaveCount(6);
  await expectNoAxeViolations(page, 'case with a shared step');
  await noHorizontalScroll(page);

  await page.getByTestId('case-edit').click();
  await expect(page.getByTestId('step-shared')).toHaveCount(1);
  await page.getByTestId('step-action').first().focus();
  await page.keyboard.press(`${MOD}+Shift+S`);
  await page.getByTestId('shared-pick-search').fill('cart');
  await page.getByTestId('shared-pick').first().click();
  await expect(page.getByTestId('step-shared')).toHaveCount(2);
  await expectNoAxeViolations(page, 'case editor with shared steps');
  await page.getByTestId('case-cancel').click();

  await go(page, '/p/web-shop/cases/1');
  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('shared-step-change').first()).toContainText('SS-1 v2');
});

test('the runner pins a version, collapses the block and offers the new version of an untouched item', async ({ page }) => {
  const n = await caseWithShared(page);
  const launchId = await manualLaunch(page);
  const added = await api(page, 'POST', `${P}/launches/${launchId}/run-items`, { caseNumbers: [n] });
  expect(added.status).toBe(200);
  const list = await api(page, 'GET', `${P}/launches/${launchId}/run-items?limit=500`);
  const items = list.body.items as { id: string; caseNumber: number }[];
  const item = items.find((i) => i.caseNumber === n);
  expect(item).toBeTruthy();
  const itemId = item?.id ?? '';

  await go(page, `/p/web-shop/launches/${launchId}/run/${itemId}`);
  const head = page.getByTestId('run-shared-head');
  await expect(head).toContainText('SS-1 v2');
  await expect(page.getByTestId('run-step')).toHaveCount(7);
  await expect(page.getByTestId('shared-refresh')).toHaveCount(0);
  await page.getByTestId('run-shared-toggle').click();
  await expect(page.locator('[data-testid="run-step"]:visible')).toHaveCount(3);
  await page.getByTestId('run-shared-toggle').click();
  await expect(page.locator('[data-testid="run-step"]:visible')).toHaveCount(7);
  await expectNoAxeViolations(page, 'runner with a shared step');

  const ss = await api(page, 'GET', `${P}/shared-steps/1`);
  const edit = await api(page, 'PUT', `${P}/shared-steps/1`, { name: 'Log in as a customer', description: '', steps: [{ id: 's_new', action: 'Use single sign-on', expected: 'The shop opens', data: '', attachments: [], steps: [] }] }, { 'If-Match': ss.etag ?? '' });
  expect(edit.status).toBe(200);
  await go(page, '/p/web-shop');
  await go(page, `/p/web-shop/launches/${launchId}/run/${itemId}`);
  await expect(page.getByTestId('run-step')).toHaveCount(7);
  await expect(head).toContainText('v3 available');
  await expect(page.getByTestId('shared-refresh')).toContainText('SS-1 v2 → v3');
  await page.getByTestId('shared-refresh-button').click();
  await expect(page.getByTestId('shared-refresh')).toHaveCount(0);
  await expect(page.getByTestId('run-step')).toHaveCount(4);
  await expect(head).toContainText('SS-1 v3');
});

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`shared step screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the review screenshots');
      test.setTimeout(180_000);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      const n = await caseWithShared(page);
      await go(page, '/p/web-shop/shared-steps');
      await expect(page.getByTestId('shared-card').first()).toBeVisible();
      await shoot(page, 'shared-steps', width, scheme, false);
      await page.getByTestId('shared-card').first().click();
      await expect(page.getByTestId('shared-step-row').first()).toBeVisible();
      await shoot(page, 'shared-step', width, scheme);
      await page.getByRole('tab', { name: /Versions/ }).click();
      await expect(page.getByTestId('version-diff')).toBeVisible();
      await shoot(page, 'shared-step-versions', width, scheme);
      await go(page, `/p/web-shop/cases/${String(n)}`);
      await page.getByTestId('shared-toggle').click();
      await shoot(page, 'case-shared', width, scheme);
      await page.getByTestId('case-edit').click();
      await expect(page.getByTestId('step-shared')).toBeVisible();
      await shoot(page, 'case-edit-shared', width, scheme);
      await page.getByTestId('step-insert-shared').click();
      await expect(page.getByTestId('shared-pick').first()).toBeVisible();
      await shoot(page, 'shared-picker', width, scheme);
      await page.keyboard.press('Escape');
      await page.getByTestId('case-cancel').click();
      const launchId = await manualLaunch(page);
      await api(page, 'POST', `${P}/launches/${launchId}/run-items`, { caseNumbers: [n] });
      const list = await api(page, 'GET', `${P}/launches/${launchId}/run-items?limit=500`);
      const itemId = ((list.body.items as { id: string; caseNumber: number }[]).find((i) => i.caseNumber === n) ?? { id: '' }).id;
      await go(page, `/p/web-shop/launches/${launchId}/run/${itemId}`);
      if (width < 500) await page.getByRole('button', { name: 'Next step' }).click();
      await expect(page.getByTestId('run-shared-head')).toBeVisible();
      await shoot(page, 'runner-shared', width, scheme);
    });
  }
}
