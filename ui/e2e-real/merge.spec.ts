import { expect, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, ENABLED, PROJECT } from './env';
import { signInWithToken } from './watch';

test.skip(!ENABLED, 'set FASTPROOF_E2E_REAL=1 to run against a real server');

const SHOTS = process.env.FASTPROOF_E2E_SHOTS;

async function api(method: string, path: string, body?: unknown): Promise<{ status: number; body: unknown }> {
  const res = await fetch(`${BASE_URL}${path}`, {
    method,
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as unknown) : null };
}

interface L {
  id: string;
  number: number;
}

async function shard(name: string, run: string, tests: [string, string, string][]): Promise<L> {
  const l = (await api('POST', `/api/v1/projects/${PROJECT}/launches`, { name, externalId: `merge-${run}-${name}`, vcs: { branch: 'merge-e2e' } })).body as L;
  const t0 = Date.parse('2026-11-23T10:00:00Z');
  const items = tests.map(([id, test, status], i) => ({
    id,
    name: test,
    fullName: `com.example.checkout.CheckoutTest#${test}`,
    status,
    start: t0 + i * 1000 + (name.endsWith('2') ? 60_000 : 0),
    stop: t0 + i * 1000 + 800 + (name.endsWith('2') ? 60_000 : 0),
    ...(status === 'failed' ? { error: { message: `${test}: expected 200 but was 503` } } : {}),
  }));
  const r = await api('POST', `/api/v1/projects/${PROJECT}/launches/${l.id}/results`, items);
  expect(r.status).toBe(202);
  await api('POST', `/api/v1/projects/${PROJECT}/launches/${l.id}/finish`, {});
  return l;
}

async function shoot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return;
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(SHOTS, `${name}.png`) });
}

for (const [mode, width, scheme] of [
  ['desktop-dark', 1280, 'dark'],
  ['desktop-light', 1280, 'light'],
  ['phone-dark', 360, 'dark'],
  ['phone-light', 360, 'light'],
] as const) {
  test(`merge shards in the UI and undo: ${mode}`, async ({ page }) => {
    if (SHOTS) mkdirSync(SHOTS, { recursive: true });
    const run = `${mode}-${String(Date.now())}`;
    const a = await shard(`checkout shard 1`, run, [
      ['1', 'payByCard', 'failed'],
      ['2', 'applyCoupon', 'passed'],
    ]);
    const b = await shard(`checkout shard 2`, run, [
      ['1', 'payByCard', 'passed'],
      ['2', 'refund', 'passed'],
    ]);
    await page.setViewportSize({ width, height: 900 });
    await page.addInitScript((c) => window.localStorage.setItem('franshub-color-scheme', c), scheme);
    await signInWithToken(page, ADMIN_TOKEN);
    await page.goto(`/p/${PROJECT}?q=${encodeURIComponent('checkout shard')}`);
    const rowOf = (l: L) => page.getByTestId('launch-row').filter({ hasText: `#${String(l.number)} ` });
    await rowOf(a).getByTestId('launch-select').check();
    await rowOf(b).getByTestId('launch-select').check();
    await expect(page.getByTestId('merge-bar')).toContainText('2 selected');
    await shoot(page, `merge-list-${mode}`);
    await page.getByTestId('merge-open').click();
    await expect(page.getByTestId('merge-preview')).toContainText('1 test was run in more than one of them');
    await page.getByTestId('merge-name').fill(`checkout ${run}`);
    await shoot(page, `merge-dialog-${mode}`);
    await page.getByTestId('merge-submit').click();
    const banner = page.getByTestId('merged-from');
    await expect(banner).toContainText(`#${String(a.number)}`);
    await expect(page.getByText(`checkout ${run}`).first()).toBeVisible();
    await shoot(page, `merge-target-${mode}`);

    const target = page.url().split('/launches/')[1]?.split('?')[0] ?? '';
    const results = ((await api('GET', `/api/v1/projects/${PROJECT}/launches/${target}/results`)).body as { items: { name: string; attemptCount: number; status: string; flakyInRun: boolean }[] }).items;
    const pay = results.find((r) => r.name === 'payByCard');
    expect(results).toHaveLength(3);
    expect(pay).toMatchObject({ attemptCount: 2, status: 'passed', flakyInRun: true });

    await page.goto(`/p/${PROJECT}/launches/${a.id}`);
    await expect(page.getByTestId('merged-into')).toBeVisible();
    await shoot(page, `merge-source-${mode}`);

    await page.goto(`/p/${PROJECT}/launches/${target}`);
    await page.getByTestId('undo-merge').click();
    await expect(page).toHaveURL(new RegExp(`/p/${PROJECT}$`));
    const back = await api('GET', `/api/v1/projects/${PROJECT}/launches/${a.id}`);
    expect(back.body).not.toHaveProperty('mergedInto');
    expect((await api('GET', `/api/v1/projects/${PROJECT}/launches/${target}`)).status).toBe(404);
  });
}
