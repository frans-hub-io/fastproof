import { expect, test, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, BIN, ENABLED, FIXTURES, PROJECT, WORK_DIR } from './env';
import { signInWithToken, Watcher } from './watch';

test.skip(!ENABLED, 'set FASTPROOF_E2E_REAL=1 to run against a real server');

const P = `/p/${PROJECT}`;
const API = `/api/v1/projects/${PROJECT}`;
const MOD = process.platform === 'darwin' ? 'Meta' : 'Control';

interface Reply<T> {
  status: number;
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
  return { status: res.status, body: (text ? JSON.parse(text) : null) as T, etag: res.headers.get('etag') };
}

async function ok<T>(r: Promise<Reply<T>>, want = 200): Promise<T> {
  const x = await r;
  expect(x.status, JSON.stringify(x.body)).toBe(want);
  return x.body;
}

interface Folder {
  id: string;
  name: string;
}
interface CaseDto {
  number: number;
  name: string;
  revision: number;
  tags: string[];
  folderId?: string | null;
}

async function folder(name: string, parentId?: string): Promise<Folder> {
  return ok(api<Folder>('POST', `${API}/case-folders`, { name, ...(parentId ? { parentId } : {}) }), 201);
}

async function caseIn(folderId: string, name: string, steps = 2): Promise<CaseDto> {
  const body = { description: `Checks that **${name}** works.`, preconditions: '- A signed-in customer', links: [], steps: Array.from({ length: steps }, (_, i) => ({ action: `Step ${String(i + 1)} of ${name}`, expected: `Result ${String(i + 1)}`, data: '', attachments: [], steps: [] })) };
  return ok(api<CaseDto>('POST', `${API}/cases`, { name, folderId, status: 'active', body }), 201);
}

function cli(args: string[]): string {
  const tokenFile = join(WORK_DIR, 'admin-token');
  writeFileSync(tokenFile, ADMIN_TOKEN);
  return execFileSync(BIN, args, {
    env: { PATH: process.env.PATH ?? '', FASTPROOF_URL: BASE_URL, FASTPROOF_TOKEN_FILE: tokenFile, FASTPROOF_PROJECT: PROJECT },
    stdio: ['ignore', 'pipe', 'inherit'],
  }).toString();
}

async function openPlan(page: Page, name: string): Promise<void> {
  await page.goto(`${P}/plans`);
  await page.getByTestId('plan-card').filter({ hasText: name }).click();
  await expect(page.getByRole('heading', { name })).toBeVisible();
}

const run = randomBytes(3).toString('hex');

test.beforeEach(async ({ page }) => {
  await signInWithToken(page, ADMIN_TOKEN);
});

test('real: create a case, add steps with the editor, preview Markdown, save, compare and restore a version', async ({ page }) => {
  const w = new Watcher(page, BASE_URL);
  const top = await folder(`Editor ${run}`);
  await folder('Payment', top.id);
  await page.goto(`${P}/cases`);
  await page.getByTestId('new-case').click();
  await page.getByTestId('new-case-name').fill(`Pay by card ${run}`);
  await page.getByTestId('new-case-folder').click();
  await page.getByRole('option', { name: `Editor ${run} / Payment` }).click();
  await page.getByTestId('new-case-create').click();
  await expect(page.getByTestId('steps-editor')).toBeVisible();
  const n = Number(/\/cases\/(\d+)/.exec(page.url())?.[1]);
  expect(n).toBeGreaterThan(0);

  await page.getByTestId('case-description').fill('Pays with a **saved** card; see [spec](https://example.com/spec)');
  await page.getByRole('radiogroup', { name: 'Description: write or preview' }).getByText('Preview').click();
  const preview = page.getByTestId('case-description-preview');
  await expect(preview.locator('strong')).toHaveText('saved');
  await expect(preview.getByRole('link', { name: 'spec' })).toHaveAttribute('href', 'https://example.com/spec');
  await page.getByTestId('step-add').click();
  await page.getByTestId('step-action').first().fill('Open the cart');
  await page.getByTestId('step-expected').first().fill('The cart lists one item');
  await page.getByTestId('step-action').first().press(`${MOD}+Enter`);
  await expect(page.getByTestId('step-action').nth(1)).toBeFocused();
  await page.keyboard.type('Press **Pay**');
  await page.getByTestId('step-expected').nth(1).fill('The order number is shown');
  await page.keyboard.press(`${MOD}+s`);
  await expect(page.getByText(`TC-${String(n)} is now version 2`)).toBeVisible();
  await expect(page.getByTestId('step-view')).toHaveCount(2);
  await expect(page.getByTestId('step-view').nth(1).locator('strong')).toHaveText('Pay');

  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('version-row')).toHaveCount(2);
  await expect(page.getByText('v1 → v2')).toBeVisible();
  await expect(page.getByTestId('diff-step').filter({ hasText: 'added' })).toHaveCount(2);
  await page.getByTestId('version-row').filter({ hasText: 'v1' }).getByTestId('version-restore').click();
  await page.getByTestId('version-restore-confirm').click();
  await expect(page.getByText('Version 1 restored')).toBeVisible();
  await expect(page.getByTestId('version-row')).toHaveCount(3);
  await expect(page.getByTestId('version-row').first()).toContainText('restored');
  await page.getByRole('tab', { name: 'Scenario' }).click();
  await expect(page.getByTestId('step-view')).toHaveCount(0);

  const c = await ok(api<CaseDto & { version: number }>('GET', `${API}/cases/${String(n)}`));
  expect(c.version).toBe(3);
  w.check();
});

test('real: a change by someone else while editing shows the banner (SSE) and the 412 dialog', async ({ page }) => {
  const w = new Watcher(page, BASE_URL);
  w.allow({ path: /\/cases\/\d+$/, status: 412 });
  const f = await folder(`Conflict ${run}`);
  const c = await caseIn(f.id, `Refund an order ${run}`);
  await page.goto(`${P}/cases/${String(c.number)}`);
  await expect(page.getByTestId('case-title')).toContainText(c.name);
  await page.getByTestId('case-edit').click();
  await page.getByTestId('case-name-input').fill(`${c.name} (mine)`);
  const cur = await api<CaseDto>('GET', `${API}/cases/${String(c.number)}`);
  await ok(api('PATCH', `${API}/cases/${String(c.number)}`, { tags: ['changed-elsewhere'] }, { 'If-Match': cur.etag ?? '' }));
  await expect(page.getByTestId('case-changed-banner')).toBeVisible();
  await page.getByTestId('case-save').click();
  const dialog = page.getByTestId('conflict-dialog');
  // The page and the API call use the same admin token, so the dialog names "you (another tab)".
  await expect(dialog).toContainText(`Revision ${String(cur.body.revision + 1)} was saved by you (another tab)`);
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Name' })).toContainText('(mine)');
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Tags' })).toHaveCount(0);
  await dialog.getByTestId('conflict-mine').click();
  await expect(page.getByText(new RegExp(`TC-${String(c.number)} is now version`))).toBeVisible();
  await expect(page.getByTestId('case-title')).toContainText('(mine)');
  const saved = await api<CaseDto>('GET', `${API}/cases/${String(c.number)}`);
  expect(saved.body.name).toBe(`${c.name} (mine)`);
  expect(saved.body.tags).toContain('changed-elsewhere');
  expect(saved.body.folderId).toBe(f.id);
  w.check();
});

test('real: a plan by filter with a sync diff, a launch from it, a keyboard-only run and the Manual tab', async ({ page }) => {
  test.setTimeout(180_000);
  const w = new Watcher(page, BASE_URL);
  const f = await folder(`Smoke ${run}`);
  for (const name of ['Sign in', 'Search', 'Add to cart']) await caseIn(f.id, `${name} ${run}`);
  const planName = `Smoke plan ${run}`;
  await page.goto(`${P}/plans`);
  await page.getByTestId('new-plan').click();
  await page.getByTestId('plan-name').fill(planName);
  await page.getByTestId('plan-create').click();
  await expect(page.getByRole('heading', { name: planName })).toBeVisible();
  await page.getByTestId('plan-filter-open').click();
  await page.getByTestId('filter-folder').click();
  await page.getByRole('option', { name: `Smoke ${run}` }).click();
  await page.getByTestId('filter-save').click();
  const sync = page.getByTestId('sync-dialog');
  await expect(sync.getByTestId('sync-added')).toHaveText('+ 3 added');
  await sync.getByTestId('sync-apply').click();
  await expect(page.getByTestId('plan-count-total')).toHaveText('3 cases');

  await caseIn(f.id, `Pay ${run}`);
  await page.getByTestId('plan-sync').click();
  await expect(page.getByTestId('sync-dialog').getByTestId('sync-added')).toHaveText('+ 1 added');
  await page.getByTestId('sync-dialog').getByTestId('sync-apply').click();
  await expect(page.getByTestId('plan-count-total')).toHaveText('4 cases');

  await page.getByTestId('plan-start').click();
  const start = page.getByTestId('start-dialog');
  await start.getByText('Me', { exact: true }).click();
  await start.getByTestId('start-submit').click();
  await page.getByTestId('launch-started').getByTestId('go-run').click();
  const queue = page.getByTestId('queue-item');
  await expect(queue).toHaveCount(4);
  const live = page.getByTestId('live-region');
  const current = page.locator('[data-testid="run-step"][data-current="true"]');
  for (let i = 0; i < 4; i++) {
    await expect(queue.nth(i)).toHaveAttribute('aria-current', 'true');
    await expect(current).toBeVisible();
    await expect(page.getByTestId('run-case-title')).toBeFocused();
    if (i === 1) {
      await page.keyboard.press('f');
      await expect(page.getByTestId('step-actual')).toBeFocused();
      await page.keyboard.type('Search returns nothing');
      await page.keyboard.press('Escape');
      await page.keyboard.press('j');
      await page.keyboard.press('p');
    } else {
      await page.keyboard.press('Shift+P');
    }
    await page.keyboard.press(`${MOD}+Enter`);
    await expect(queue.nth(i)).toHaveAttribute('data-status', i === 1 ? 'failed' : 'passed');
  }
  await expect(live).toContainText('All cases in the queue are done');
  await expect(page.getByTestId('save-state')).toHaveAttribute('data-state', 'saved');

  await page.getByRole('link', { name: new RegExp(`^#\\d+ ${planName}`) }).click();
  await expect(page.getByTestId('manual-tab')).toBeVisible();
  await expect(page.getByTestId('manual-done')).toHaveText('4 / 4');
  const mine = page.getByTestId('manual-assignees').getByRole('row').filter({ hasText: 'e2e-admin' });
  await expect(mine.getByRole('cell').nth(2)).toHaveText('3');
  await expect(mine.getByRole('cell').nth(3)).toHaveText('1');
  await expect(page.getByTestId('stat-total')).toContainText('4');
  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('result-row')).toHaveCount(4);
  await expect(page.getByTestId('result-row').first().getByTestId('manual-icon')).toBeVisible();
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-close').click();
  await page.getByTestId('manual-close-confirm').click();
  await expect(page.getByTestId('manual-reopen')).toBeVisible();
  await expect(page.locator('[data-launch-status="finished"]')).toBeVisible();
  w.check();
});

test('real: a combined launch from a plan, its testplan command, an allure-results upload into it and the counts', async ({ page }) => {
  test.setTimeout(240_000);
  const w = new Watcher(page, BASE_URL);
  let automated: CaseDto[] = [];
  for (let i = 0; i < 60 && automated.length < 2; i++) {
    automated = (await ok(api<{ items: CaseDto[] }>('GET', `${API}/cases?automation=automated&limit=2`))).items;
    if (automated.length < 2) await new Promise((r) => setTimeout(r, 2000));
  }
  expect(automated.length, 'the code sync made cases of the uploaded tests').toBe(2);
  const f = await folder(`Combined ${run}`);
  const manual = [await caseIn(f.id, `Checkout by phone ${run}`, 1), await caseIn(f.id, `Gift card ${run}`, 1)];
  const planName = `Release ${run}`;
  await ok(api('POST', `${API}/test-plans`, { kind: 'managed', name: planName, caseNumbers: [...manual, ...automated].map((c) => c.number) }), 201);
  await openPlan(page, planName);
  await expect(page.getByTestId('plan-count-automated')).toHaveText('2 automated');
  await page.getByTestId('plan-start').click();
  const start = page.getByTestId('start-dialog');
  await expect(start.getByTestId('start-automated')).toBeChecked();
  const externalId = `e2e-combined-${run}`;
  await start.getByLabel('External id for CI').fill(externalId);
  await expect(start.getByTestId('testplan-command-preview')).toContainText(`fastproof testplan --launch ${externalId}`);
  await start.getByTestId('start-submit').click();
  const started = page.getByTestId('launch-started');
  await expect(started.getByTestId('testplan-command')).toContainText(externalId);
  await expect(started.getByTestId('testplan-command')).toContainText('testplan.json');

  const plan = JSON.parse(cli(['testplan', '--launch', externalId])) as { tests: { selector: string }[] };
  expect(plan.tests.length).toBeGreaterThanOrEqual(2);
  cli(['upload', join(FIXTURES, 'pytest/allure-results'), '--launch', externalId, '--name', planName, '--branch', 'main', '--timeout', '300']);

  await started.getByRole('link', { name: new RegExp(planName) }).click();
  await page.getByTestId('tab-manual').click();
  await expect(page.getByTestId('manual-done')).toHaveText('0 / 2');
  await expect(page.getByTestId('manual-tab')).toContainText('Automated');
  const automatedCount = Number((await page.getByTestId('manual-tab').getByText('results from CI').locator('xpath=preceding-sibling::*[1]').textContent()) ?? '0');
  expect(automatedCount).toBeGreaterThan(0);
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await page.keyboard.press('p');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(page.getByTestId('queue-item').first()).toHaveAttribute('data-status', 'passed');
  await page.getByRole('link', { name: new RegExp(planName) }).click();
  await page.getByRole('tab', { name: 'Results' }).click();
  await page.getByTestId('source-filter').getByText('Manual').click();
  await expect(page.getByTestId('result-row')).toHaveCount(1);
  await page.getByTestId('source-filter').getByText('Automated').click();
  await expect(page.getByTestId('result-row')).toHaveCount(automatedCount);
  await expect(page.getByTestId('stat-total')).toContainText(String(automatedCount + 1));
  w.check();
});

test('real: run management, My runs, rerun history, print view, plan launches and case fields', async ({ page }) => {
  test.setTimeout(180_000);
  const w = new Watcher(page, BASE_URL);
  const f = await folder(`Fixwave ${run}`);
  const cases = [await caseIn(f.id, `Pay by card ${run}`, 2), await caseIn(f.id, `Pay by voucher ${run}`, 1)];
  const planName = `Fixwave plan ${run}`;
  const plan = await ok(api<{ id: string }>('POST', `${API}/test-plans`, { kind: 'managed', name: planName, caseNumbers: cases.map((c) => c.number) }), 201);
  const started = await ok(api<{ launch: { id: string; number: number } }>('POST', `${API}/test-plans/${plan.id}/launches`, { name: planName, assign: 'none', includeAutomated: false }), 201);
  const launchId = started.launch.id;

  await page.goto(`${P}/launches/${launchId}?tab=manual`);
  await expect(page.getByTestId('manual-item')).toHaveCount(2);
  await page.getByTestId('manual-select-all').check();
  await page.getByTestId('manual-bulk-assign').click();
  const assign = page.getByTestId('assign-dialog');
  await assign.getByTestId('assign-input').fill('e2e-ad');
  await page.getByRole('option', { name: /e2e-admin/ }).click();
  await assign.getByTestId('assign-confirm').click();
  await expect(page.getByText('Assigned to e2e-admin')).toBeVisible();
  await expect(page.getByTestId('manual-item-assignee').filter({ hasText: 'e2e-admin' })).toHaveCount(2);

  await page.goto(`${P}/my-runs`);
  const group = page.getByTestId('my-runs-launch').filter({ hasText: planName });
  await expect(group.getByTestId('my-run-item')).toHaveCount(2);
  await group.getByTestId('my-run-item').filter({ hasText: `Pay by card ${run}` }).getByRole('link').click();
  await expect(page.getByTestId('run-case-title')).toContainText(`Pay by card ${run}`);
  await page.keyboard.press('p');
  await expect(page.getByTestId('suggested-status')).toHaveAttribute('data-status', 'in_progress');
  await page.keyboard.press('f');
  await page.keyboard.type('Declined although the card is valid');
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('suggested-status')).toHaveAttribute('data-status', 'failed');
  await page.keyboard.press(`${MOD}+Enter`);
  await expect(page.getByTestId('queue-item').filter({ hasText: `Pay by card ${run}` })).toHaveAttribute('data-status', 'failed');
  await page.getByTestId('queue-item').filter({ hasText: `Pay by card ${run}` }).click();
  await expect(page.getByTestId('save-state')).toHaveAttribute('data-state', 'saved');
  await page.getByTestId('rerun').click();
  await expect(page.getByTestId('live-region')).toContainText('run 2 started');
  const earlier = page.getByTestId('run-execution');
  await expect(earlier).toHaveCount(1);
  await expect(earlier).toContainText('Failed');
  await expect(earlier).toContainText('Declined although the card is valid');

  await page.goto(`${P}/launches/${launchId}/manual/print`);
  await expect(page.getByTestId('print-item')).toHaveCount(2);
  await expect(page.getByTestId('print-item').filter({ hasText: `Pay by voucher ${run}` })).toContainText('Untested');

  await page.goto(P);
  await expect(page.getByTestId('launch-row').filter({ hasText: planName }).getByTestId('launch-manual')).toHaveAttribute('data-state', 'open');
  await openPlan(page, planName);
  await expect(page.getByTestId('plan-launch').filter({ hasText: planName })).toHaveCount(1);

  await page.goto(`${P}/settings/case-fields`);
  await page.getByTestId('field-new').click();
  const dialog = page.getByTestId('field-dialog');
  await dialog.getByTestId('field-name').fill(`Browser ${run}`);
  await dialog.getByTestId('field-key').fill(`browser_${run}`);
  await dialog.getByTestId('field-option-add').click();
  await dialog.getByTestId('field-option-value').fill('firefox');
  await dialog.getByTestId('field-save').click();
  const row = page.getByTestId('field-row').filter({ hasText: `Browser ${run}` });
  await expect(row).toContainText('firefox');
  await row.getByTestId('field-up').click();
  await expect(row.getByTestId('field-up')).toBeEnabled();
  await row.getByTestId('field-archive').click();
  await expect(page.getByTestId('field-row').filter({ hasText: `Browser ${run}` })).toHaveCount(0);
  w.check();
});
