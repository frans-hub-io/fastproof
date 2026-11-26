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
      });
    }
  }
});
