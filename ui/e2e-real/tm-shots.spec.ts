import { expect, test, type Page } from '@playwright/test';
import { randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, ENABLED, PROJECT } from './env';
import { signInWithToken } from './watch';

const dir = process.env.FASTPROOF_TM_SHOTS;

test.skip(!ENABLED || !dir, 'set FASTPROOF_E2E_REAL=1 and FASTPROOF_TM_SHOTS to a directory for the TM screenshots');

const P = `/p/${PROJECT}`;
const API = `/api/v1/projects/${PROJECT}`;

interface Reply<T> {
  body: T;
  etag: string | null;
}

async function api<T>(method: string, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<Reply<T>> {
  const res = await fetch(`${BASE_URL}${path}`, {
    method,
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }), ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path}: ${String(res.status)} ${text}`);
  return { body: (text ? JSON.parse(text) : null) as T, etag: res.headers.get('etag') };
}

interface Seed {
  caseNumber: number;
  planName: string;
  launchId: string;
  itemId: string;
}

let seed: Seed | null = null;

async function seedOnce(): Promise<Seed> {
  if (seed) return seed;
  const run = randomBytes(3).toString('hex');
  const top = (await api<{ id: string }>('POST', `${API}/case-folders`, { name: `Checkout ${run}` })).body;
  const pay = (await api<{ id: string }>('POST', `${API}/case-folders`, { name: 'Payment', parentId: top.id })).body;
  const step = (action: string, expected: string, data = '', steps: unknown[] = []) => ({ action, expected, data, attachments: [], steps });
  const numbers: number[] = [];
  const names = ['Pay with a saved card', 'Pay with a new card', 'Expired card is rejected', '3-D Secure challenge completes'];
  for (const [i, name] of names.entries()) {
    const c = await api<{ number: number }>('POST', `${API}/cases`, {
      name: `${name} ${run}`,
      folderId: pay.id,
      status: i === 2 ? 'draft' : 'active',
      tags: i % 2 ? ['regression'] : ['smoke', 'regression'],
      body: {
        description: `Checks that the customer can **${name.toLowerCase()}**.`,
        preconditions: '- A customer with a saved card\n- Product `TRS-42` in stock',
        links: [{ name: 'Spec', url: 'https://example.com/spec/payments', type: 'link' }],
        steps: [
          step('Open the cart with *Trail running shoes*', 'The cart lists one item'),
          step('Sign in', '', '', [step('Enter the email', 'Accepted', 'email: `buyer@example.com`'), step('Enter the password and press **Sign in**', 'The account menu shows the name')]),
          step('Press **Pay**', 'The confirmation page shows the order number', 'card: `4242`'),
        ],
      },
    });
    numbers.push(c.body.number);
  }
  const first = numbers[0] ?? 0;
  const cur = await api<{ name: string; body: { steps: unknown[] } & Record<string, unknown>; tags: string[]; status: string }>('GET', `${API}/cases/${String(first)}`);
  await api('PUT', `${API}/cases/${String(first)}`, { name: cur.body.name, folderId: pay.id, status: cur.body.status, tags: [...cur.body.tags, 'payments'], body: { ...cur.body.body, steps: [...cur.body.body.steps, step('Open **Account → Orders**', 'The new order is listed as *Paid*')] } }, { 'If-Match': cur.etag ?? '' });
  const planName = `Payments ${run}`;
  const plan = (await api<{ id: string }>('POST', `${API}/test-plans`, { kind: 'managed', name: planName, description: 'Manual payment checks before the release.', filter: { folderIds: [top.id], recursive: true, statuses: ['active'] } })).body;
  const started = (await api<{ launch: { id: string } }>('POST', `${API}/test-plans/${plan.id}/launches`, { name: `${planName} run`, assign: 'me', includeAutomated: false, environment: { env: 'staging' } })).body;
  const items = (await api<{ items: { id: string }[] }>('GET', `${API}/launches/${started.launch.id}/run-items`)).body.items;
  const done = items[0];
  if (done) {
    for (const path of ['1', '2.1', '2.2', '3']) await api('PUT', `${API}/run-items/${done.id}/steps/${path}`, { status: 'passed' });
    const it = await api<unknown>('GET', `${API}/run-items/${done.id}`);
    await api('PATCH', `${API}/run-items/${done.id}`, { status: 'passed' }, { 'If-Match': it.etag ?? '' });
  }
  const second = items[1];
  if (second) await api('PUT', `${API}/run-items/${second.id}/steps/1`, { status: 'failed', actual: 'The cart is empty after sign-in' });
  seed = { caseNumber: first, planName, launchId: started.launch.id, itemId: second?.id ?? '' };
  return seed;
}

async function shoot(page: Page, name: string, width: number, scheme: string, full = width < 500) {
  await page.waitForTimeout(500);
  await page.screenshot({ path: join(dir ?? '.', `real-${name}-${String(width)}-${scheme}.png`), fullPage: full });
}

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`real TM screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.setTimeout(240_000);
      const s = await seedOnce();
      await signInWithToken(page, ADMIN_TOKEN);
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((v) => {
        try {
          localStorage.setItem('franshub-color-scheme', v);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto(`${P}/cases`);
      await expect(page.getByTestId(width < 500 ? 'case-card' : 'case-row').first()).toBeVisible();
      await shoot(page, 'cases', width, scheme, false);
      await page.goto(`${P}/cases/${String(s.caseNumber)}`);
      await expect(page.getByTestId('step-view').first()).toBeVisible();
      await shoot(page, 'case', width, scheme);
      await page.getByTestId('case-edit').click();
      await shoot(page, 'case-edit', width, scheme);
      await page.getByTestId('case-cancel').click();
      await page.getByRole('tab', { name: 'Details' }).click();
      await shoot(page, 'case-details', width, scheme);
      await page.getByRole('tab', { name: /Versions/ }).click();
      await expect(page.getByTestId('version-diff')).toBeVisible();
      await shoot(page, 'case-versions', width, scheme);
      await page.goto(`${P}/cases?tab=unlinked`);
      await page.waitForLoadState('networkidle');
      await shoot(page, 'unlinked', width, scheme, false);
      await page.goto(`${P}/plans`);
      await page.getByTestId('plan-card').filter({ hasText: s.planName }).click();
      await expect(page.getByTestId('plan-item').first()).toBeVisible();
      await shoot(page, 'plan', width, scheme);
      await page.goto(`${P}/plans`);
      await expect(page.getByTestId('plan-card').first()).toBeVisible();
      await shoot(page, 'plans', width, scheme, false);
      await page.goto(`${P}/launches/${s.launchId}?tab=manual`);
      await expect(page.getByTestId('manual-items')).toBeVisible();
      await shoot(page, 'launch-manual', width, scheme);
      await page.goto(`${P}/launches/${s.launchId}/run/${s.itemId}`);
      await expect(page.getByTestId('run-case-title')).toBeVisible();
      await shoot(page, 'runner', width, scheme);
      await page.goto(`${P}/cases/import`);
      await expect(page.getByTestId('import-drop')).toBeVisible();
      await shoot(page, 'import', width, scheme, false);
    });
  }
}
