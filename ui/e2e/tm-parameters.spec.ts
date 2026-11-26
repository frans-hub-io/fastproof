import { expect, test } from './console-guard';
import type { Page } from '@playwright/test';
import path from 'node:path';
import { expectNoAxeViolations, noHorizontalScroll } from './tm-helpers';

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

/** A parameterized case (three rows, one switched off) in the seeded manual run: its number and run item. */
async function parameterized(page: Page): Promise<{ n: number; launch: string; item: string }> {
  await page.goto('/p/web-shop');
  await expect(page.getByRole('link', { name: /^#38 / }).first()).toBeVisible();
  const c = await api(page, 'POST', `${P}/cases`, {
    name: 'Pay in every currency',
    status: 'draft',
    body: {
      description: '',
      preconditions: 'Open the shop in **${browser}**',
      links: [],
      steps: [
        { id: 's_add', action: 'Add a book to the cart', expected: 'The cart has one book', data: '', attachments: [], steps: [] },
        { id: 's_pay', action: 'Pay in ${currency}', expected: 'The receipt shows ${currency}', data: 'card ${card}', attachments: [], steps: [] },
      ],
      parameters: {
        columns: ['browser', 'currency', 'card'],
        rows: [
          { id: 'r_ff', values: { browser: 'firefox', currency: 'EUR', card: '4242 4242 4242 4242' }, enabled: true },
          { id: 'r_sf', values: { browser: 'safari', currency: 'GBP', card: '4000 0566 5566 5556' }, enabled: false },
          { id: 'r_cr', values: { browser: 'chromium', currency: 'UAH', card: '5555 5555 5555 4444' }, enabled: true },
        ],
      },
    },
  });
  expect(c.status).toBe(201);
  const n = Number(c.body.number);
  const launch = await manualLaunch(page);
  const added = await api(page, 'POST', `${P}/launches/${launch}/run-items`, { caseNumbers: [n] });
  expect(added.body.added).toBe(1);
  const list = await api(page, 'GET', `${P}/launches/${launch}/run-items?limit=500`);
  const item = (list.body.items as { id: string; caseNumber: number }[]).find((x) => x.caseNumber === n)?.id ?? '';
  return { n, launch, item };
}

test('a parameterized case: the parameters tab, the editor with the generator, and the iterations in the runner', async ({ page }) => {
  test.setTimeout(180_000);
  const { n, launch, item } = await parameterized(page);
  await go(page, `/p/web-shop/cases/${String(n)}?tab=parameters`);
  await expect(page.getByTestId('parameters-view')).toBeVisible();
  await expect(page.getByTestId('parameters-iterations')).toHaveText('2 iterations per run');
  await noHorizontalScroll(page);
  await expectNoAxeViolations(page, 'parameters tab');

  await page.getByTestId('parameters-edit').click();
  await page.getByTestId('parameters-generate-open').click();
  const dialog = page.getByTestId('parameters-generate');
  await expect(dialog).toBeVisible();
  await expect(dialog.getByTestId('generate-values-currency')).toHaveValue(/EUR/);
  await expectNoAxeViolations(page, 'generate dialog');
  await dialog.getByTestId('generate-run').click();
  await expect(dialog).toBeHidden();
  const rows = page.getByTestId('parameters-row');
  await expect(rows).not.toHaveCount(0);
  await page.getByTestId('parameters-save').click();
  await expect(page.getByTestId('parameters-view')).toBeVisible();
  const saved = await api(page, 'GET', `${P}/cases/${String(n)}`);
  const params = (saved.body.body as { parameters: { generator?: { mode: string } } }).parameters;
  expect(params.generator?.mode).toBe('pairwise');

  await go(page, `/p/web-shop/cases/${String(n)}?edit=1`);
  await expect(page.getByTestId('step-placeholders').first()).toBeVisible();
  await expect(page.getByTestId('step-placeholders').first().locator('[data-known="true"]')).toHaveCount(2);
  await page.getByTestId('case-cancel').click();

  await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
  const strip = page.getByTestId('iteration-strip');
  await expect(strip.getByTestId('iteration-chip')).toHaveCount(2);
  const it = page.getByTestId('iteration');
  await expect(it).toHaveAttribute('data-row', 'r_ff');
  await expect(it).toContainText('Pay in EUR');
  await expect(it).toContainText('card 4242 4242 4242 4242');
  await expect(it.getByText('${currency}')).toHaveCount(0);
  await noHorizontalScroll(page);
  await expectNoAxeViolations(page, 'iterations runner');
  await it.getByTestId('iteration-step').first().getByTestId('set-passed').click();
  await page.getByTestId('iteration-finish-passed').click();
  await expect(it).toHaveAttribute('data-row', 'r_cr');
  await expect(it).toContainText('Pay in UAH');
  await expect(strip.getByTestId('iteration-chip').first()).toHaveAttribute('data-status', 'passed');
  await page.keyboard.press('[');
  await expect(it).toHaveAttribute('data-row', 'r_ff');
  await page.keyboard.press(']');
  await expect(it).toHaveAttribute('data-row', 'r_cr');
  await page.getByTestId('iteration-finish-failed').click();
  await expect(strip.getByTestId('iteration-chip').nth(1)).toHaveAttribute('data-status', 'failed');
  const v = await api(page, 'GET', `${P}/run-items/${item}`);
  expect(v.body.status).toBe('failed');
});

async function failIteration(page: Page, item: string, row: string): Promise<void> {
  const put = await api(page, 'PUT', `${P}/run-items/${item}/iterations/${row}/steps/2`, { status: 'failed', actual: 'The card was declined' });
  expect(put.status).toBe(200);
  const cur = await api(page, 'GET', `${P}/run-items/${item}`);
  const done = await api(page, 'PATCH', `${P}/run-items/${item}/iterations/${row}`, { status: 'failed' }, { 'If-Match': cur.etag ?? '' });
  expect(done.status).toBe(200);
}

test('closing the run with skipped skips the open iterations of a failed item too', async ({ page }) => {
  test.setTimeout(120_000);
  const { launch, item } = await parameterized(page);
  await failIteration(page, item, 'r_ff');
  const before = await api(page, 'GET', `${P}/run-items/${item}`);
  expect(before.body.status).toBe('failed');
  expect((before.body.iterations as { status: string }[]).map((i) => i.status)).toEqual(['failed', 'untested']);

  const closed = await api(page, 'POST', `${P}/launches/${launch}/manual/close`, { untestedTo: 'skipped' });
  expect(closed.status).toBe(200);
  const after = await api(page, 'GET', `${P}/run-items/${item}`);
  expect(after.body.status).toBe('failed');
  expect((after.body.iterations as { status: string }[]).map((i) => i.status)).toEqual(['failed', 'skipped']);

  await go(page, `/p/web-shop/launches/${launch}/manual/print`);
  const its = page.getByTestId('print-iteration');
  await expect(its).toHaveCount(2);
  await expect(its.nth(0)).toHaveAttribute('data-status', 'failed');
  await expect(its.nth(1)).toHaveAttribute('data-status', 'skipped');
  await expect(its.nth(0)).toContainText('browser=firefox');
  await expect(its.nth(0)).toContainText('The card was declined');
  await noHorizontalScroll(page);
  await expectNoAxeViolations(page, 'manual report with iterations');
});

test('a defect on one iteration and on the whole item, and the iterations of an earlier run', async ({ page }) => {
  test.setTimeout(150_000);
  const { launch, item } = await parameterized(page);
  await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
  await expect(page.getByTestId('iteration-defect')).toBeDisabled();
  const step2 = page.getByTestId('iteration').getByTestId('iteration-step').nth(1);
  await step2.getByTestId('set-failed').click();
  const actual = page.getByTestId('iteration-actual');
  await actual.fill('The card was declined');
  await actual.blur();
  await page.getByTestId('iteration-finish-failed').click();
  await expect(page.getByTestId('iteration-strip').getByTestId('iteration-chip').first()).toHaveAttribute('data-status', 'failed');
  await page.getByTestId('iteration-strip').getByTestId('iteration-chip').first().click();
  await expect(page.getByTestId('iteration')).toHaveAttribute('data-row', 'r_ff');
  await page.getByTestId('iteration-defect').click();
  const drawer = page.getByTestId('defect-drawer');
  await expect(drawer.getByTestId('defect-scope')).toContainText('iteration #1');
  await expect(drawer.getByTestId('defect-comment')).toHaveValue(/Iteration #1: browser=firefox · currency=EUR · card=4242 4242 4242 4242 \(failed\)/);
  await expect(drawer.getByTestId('defect-comment')).toHaveValue(/Step 2 failed: Pay in EUR/);
  await expectNoAxeViolations(page, 'iteration defect drawer');
  await drawer.getByTestId('defect-issue').fill('PAY-9');
  await drawer.getByTestId('defect-save').click();
  await expect(page.getByTestId('iteration-defect-info')).toBeVisible();
  await expect(page.getByTestId('item-defect-info')).toBeVisible();
  await page.getByTestId('item-defect').click();
  await expect(page.getByTestId('defect-drawer').getByTestId('defect-scope')).toContainText('every failed or blocked iteration');
  await expect(page.getByTestId('defect-drawer').getByTestId('defect-comment')).toHaveValue(/1 of 2 iterations not passed/);
  await page.keyboard.press('Escape');

  const cur = await api(page, 'GET', `${P}/run-items/${item}`);
  expect((await api(page, 'POST', `${P}/run-items/${item}/rerun`, undefined, { 'If-Match': cur.etag ?? '' })).status).toBe(200);
  await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
  const earlier = page.getByTestId('execution-iterations');
  await expect(earlier).toBeVisible();
  await expect(earlier.getByTestId('execution-iteration')).toHaveCount(2);
  await expect(earlier.getByTestId('execution-iteration').first()).toContainText('Step 2 failed: The card was declined');
  await noHorizontalScroll(page);
  await expect(page.locator('.mantine-Notification-root')).toHaveCount(0, { timeout: 20_000 });
  await expectNoAxeViolations(page, 'earlier runs with iterations');
});

test.describe('parameters screenshots', () => {
  test.skip(!dir, 'set FASTPROOF_TM_SHOTS to a directory to take the parameters screenshots');
  for (const scheme of ['dark', 'light']) {
    for (const width of [1280, 360]) {
      test(`parameters ${String(width)} ${scheme}`, async ({ page }) => {
        test.setTimeout(180_000);
        await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
        await page.addInitScript((s) => {
          try {
            localStorage.setItem('franshub-color-scheme', s);
          } catch {
            /* ignore */
          }
        }, scheme);
        const shot = async (name: string, full = width < 500) => {
          await page.evaluate(() => window.scrollTo(0, 0));
          await page.waitForTimeout(400);
          await page.screenshot({ path: path.join(dir ?? '.', `parameters-${name}-${String(width)}-${scheme}.png`), fullPage: full });
        };
        const { n, launch, item } = await parameterized(page);
        await go(page, `/p/web-shop/cases/${String(n)}?tab=parameters`);
        await expect(page.getByTestId('parameters-view')).toBeVisible();
        await noHorizontalScroll(page);
        await shot('tab');
        await page.getByTestId('parameters-edit').click();
        await expect(page.getByTestId('parameters-editor')).toBeVisible();
        await noHorizontalScroll(page);
        await shot('editor');
        await page.getByTestId('parameters-generate-open').click();
        await expect(page.getByTestId('parameters-generate')).toBeVisible();
        await expect(page.getByTestId('generate-run')).toBeInViewport();
        await shot('generate', false);
        await page.keyboard.press('Escape');
        await page.getByRole('button', { name: 'Cancel' }).click();
        await go(page, `/p/web-shop/cases/${String(n)}?edit=1`);
        await expect(page.getByTestId('step-placeholders').first()).toBeVisible();
        await shot('steps-editor');
        await page.getByTestId('case-cancel').click();
        await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
        await expect(page.getByTestId('iteration')).toBeVisible();
        await page.getByTestId('iteration').getByTestId('iteration-step').first().getByTestId('set-passed').click();
        await page.getByTestId('iteration-finish-passed').click();
        await expect(page.getByTestId('iteration')).toHaveAttribute('data-row', 'r_cr');
        await noHorizontalScroll(page);
        await shot('runner');
        await go(page, '/p/web-shop');
        await failIteration(page, item, 'r_cr');
        await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
        await expect(page.getByTestId('iteration')).toBeVisible();
        await page.getByTestId('iteration-defect').click();
        await expect(page.getByTestId('defect-drawer')).toBeVisible();
        await shot('defect', false);
        await page.keyboard.press('Escape');
        await go(page, `/p/web-shop/launches/${launch}/manual/print`);
        await expect(page.getByTestId('print-iteration').first()).toBeVisible();
        await noHorizontalScroll(page);
        await page.getByTestId('print-item').filter({ hasText: 'Pay in every currency' }).screenshot({ path: path.join(dir ?? '.', `parameters-report-${String(width)}-${scheme}.png`) });
        await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
        const cur = await api(page, 'GET', `${P}/run-items/${item}`);
        await api(page, 'POST', `${P}/run-items/${item}/rerun`, undefined, { 'If-Match': cur.etag ?? '' });
        await go(page, `/p/web-shop/launches/${launch}/run/${item}`);
        await expect(page.getByTestId('execution-iterations')).toBeVisible();
        await noHorizontalScroll(page);
        await shot('earlier-runs', true);
      });
    }
  }
});
