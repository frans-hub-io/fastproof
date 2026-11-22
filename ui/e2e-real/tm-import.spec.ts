import { expect, test } from '@playwright/test';
import { randomBytes } from 'node:crypto';
import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, BASE_URL, ENABLED, WORK_DIR } from './env';
import { signInWithToken, Watcher } from './watch';

test.skip(!ENABLED, 'set FASTPROOF_E2E_REAL=1 to run against a real server');

const run = randomBytes(3).toString('hex');
const KEY = `imp-${run}`;
const API = `/api/v1/projects/${KEY}`;
const TESTRAIL = join(import.meta.dirname, '..', 'e2e', 'fixtures', 'testrail-cases.csv');

async function api(method: string, path: string, body?: unknown): Promise<{ status: number; body: unknown }> {
  const res = await fetch(`${BASE_URL}${path}`, {
    method,
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}`, Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as unknown) : null };
}

interface CaseSummary {
  number: number;
  name: string;
}
interface CaseDto {
  name: string;
  body: { steps: { action: string; expected: string }[] };
  folder: { path: { name: string }[] } | null;
  keys: { kind: string; value: string }[];
}

test.beforeAll(async () => {
  const p = await api('POST', '/api/v1/projects', { key: KEY, name: `Import ${run}` });
  expect(p.status).toBe(201);
});

test.beforeEach(async ({ page }) => {
  await signInWithToken(page, ADMIN_TOKEN);
});

test('real: import a TestRail CSV (preview, dry run, apply) and a generic CSV, then export NDJSON and a zip', async ({ page }) => {
  const w = new Watcher(page, BASE_URL);
  await page.goto(`/p/${KEY}/cases/import`);
  await expect(page.getByRole('heading', { name: 'Import test cases' })).toBeVisible();
  await page.getByTestId('import-file').setInputFiles(TESTRAIL);
  await expect(page.getByTestId('import-mapping')).toBeVisible();
  await expect(page.getByTestId('import-format')).toHaveValue('TestRail CSV');
  await expect(page.getByRole('radio', { name: 'One row per step' })).toBeChecked();
  await expect(page.getByTestId('import-map')).toHaveCount(2);
  await expect(page.getByTestId('import-map').first()).toHaveValue('Field: Priority');
  await page.getByTestId('import-dry-run').click();
  const report = page.getByTestId('import-report');
  await expect(report.getByTestId('report-create')).toContainText('3');
  await expect(report.getByTestId('report-unknown-values')).toContainText('priority: Urgent');
  await expect(report.getByTestId('report-errors')).toContainText('invalid_case');
  await report.getByTestId('import-apply').click();
  const progress = page.getByTestId('import-progress');
  await expect(progress).toContainText(/Done: \d+ rows imported/);
  await expect(progress.getByTestId('import-result')).toContainText('3 created');

  const list = { body: (await api('GET', `${API}/cases?limit=50`)).body as { items: CaseSummary[] } };
  expect(list.body.items.map((c) => c.name).sort()).toEqual(['Partial refund of one item', 'Refund a paid order', 'Refund to a gift card']);
  const refund = list.body.items.find((c) => c.name === 'Refund a paid order');
  const c = (await api('GET', `${API}/cases/${String(refund?.number)}`)).body as CaseDto;
  expect(c.body.steps.map((s) => s.action)).toEqual(['Open the order and press Refund', 'Confirm the refund']);
  expect(c.folder?.path.map((p) => p.name)).toEqual(['Payments', 'Refunds']);
  expect(c.keys).toContainEqual({ kind: 'testrail', value: 'C9001' });

  const generic = join(WORK_DIR, `generic-${run}.csv`);
  writeFileSync(generic, 'Key,Summary,Folder,Steps,Expected\nG-1,Ship to a locker,Delivery / Lockers,"1. Choose a locker\n2. Pay","1. The locker is reserved\n2. Paid"\n');
  await page.getByRole('button', { name: 'Import another file' }).click();
  await page.getByTestId('import-file').setInputFiles(generic);
  await expect(page.getByTestId('import-format')).toHaveValue('CSV with my own columns');
  await expect(page.getByRole('combobox', { name: 'Map Summary to' })).toHaveValue('Name');
  await expect(page.getByRole('combobox', { name: 'Map Steps to' })).toHaveValue('Step action');
  await page.getByTestId('import-dry-run').click();
  await expect(page.getByTestId('import-report').getByTestId('report-create')).toContainText('1');
  await page.getByTestId('import-apply').click();
  await expect(page.getByTestId('import-progress').getByTestId('import-result')).toContainText('1 created');

  await page.goto(`/p/${KEY}/cases`);
  await page.getByTestId('open-export').click();
  const [nd] = await Promise.all([page.waitForEvent('download'), page.getByTestId('export-ndjson').click()]);
  const ndPath = join(WORK_DIR, `export-${run}.ndjson`);
  await nd.saveAs(ndPath);
  const lines = readFileSync(ndPath, 'utf8').trim().split('\n');
  expect(lines).toHaveLength(5);
  expect((JSON.parse(lines[0] ?? '{}') as { format: string }).format).toBe('fastproof-cases');
  expect(lines.some((l) => l.includes('Ship to a locker'))).toBe(true);

  await page.getByTestId('open-export').click();
  const [zip] = await Promise.all([page.waitForEvent('download', { timeout: 60_000 }), page.getByTestId('export-zip').click()]);
  const zipPath = join(WORK_DIR, `export-${run}.zip`);
  await zip.saveAs(zipPath);
  const head = readFileSync(zipPath).subarray(0, 2).toString('latin1');
  expect(head).toBe('PK');
  expect(statSync(zipPath).size).toBeGreaterThan(100);
  w.check();
});
