import { expect, test } from './console-guard';
import type { Page } from '@playwright/test';
import { expectNoAxeViolations, noHorizontalScroll } from './tm-helpers';
import { spaGo, toastsGone } from './tm-reviews-helpers';

const P = '/api/v1/projects/web-shop';

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

const rows = (page: Page) => page.getByTestId('requirement-row');
const row = (page: Page, n: number) => page.locator(`[data-testid="requirement-row"][data-number="${String(n)}"]`);

test('the requirements tree expands lazily, filters and explains a bad query', async ({ page, consoleGuard }) => {
  consoleGuard.allow(/status of 400/);
  await page.goto('/p/web-shop/requirements');
  await expect(rows(page)).toHaveCount(4);
  await expect(row(page, 1)).toContainText('Customer can browse the catalog');
  await expect(row(page, 6)).toContainText('SHOP-120');
  await expect(row(page, 6)).toContainText('In Progress');
  await expectNoAxeViolations(page, 'requirements tree');
  await noHorizontalScroll(page);

  await page.getByRole('button', { name: 'Expand REQ-1' }).click();
  await expect(rows(page)).toHaveCount(7);
  await page.getByRole('button', { name: 'Expand REQ-2' }).click();
  await expect(row(page, 3)).toBeVisible();
  await page.getByRole('button', { name: 'Collapse REQ-1' }).click();
  await expect(rows(page)).toHaveCount(4);

  await page.getByTestId('requirements-search').fill('typos');
  await expect(rows(page)).toHaveCount(1);
  await expect(row(page, 3)).toBeVisible();
  await page.getByTestId('requirements-search').fill('REQ-7');
  await expect(row(page, 7)).toBeVisible();
  await page.getByTestId('requirements-search').fill('');
  await page.getByTestId('requirements-status').click();
  await page.getByRole('option', { name: 'obsolete' }).click();
  await expect(rows(page)).toHaveCount(1);
  await expect(row(page, 11)).toBeVisible();
  await page.getByTestId('requirements-status').click();
  await page.getByRole('option', { name: 'obsolete' }).click();

  await page.getByTestId('requirements-query').fill('coverage = uncovered and status = obsolete');
  await expect(row(page, 11)).toBeVisible();
  await expect(rows(page)).toHaveCount(1);
  await page.getByTestId('requirements-query').fill('colour = red');
  await expect(page.getByText('unknown attribute "colour"')).toBeVisible();
});

test('create, edit with a stale copy, archive and restore a requirement', async ({ page, consoleGuard }) => {
  consoleGuard.allow(/status of 4\d\d/);
  await page.goto('/p/web-shop/requirements');
  await page.getByTestId('new-requirement').click();
  await expect(page.getByTestId('requirement-save')).toBeDisabled();
  await page.getByTestId('requirement-title').fill('Gift wrapping');
  await page.getByTestId('requirement-parent').fill('6');
  await page.getByTestId('requirement-issue-key').fill('SHOP-120');
  await page.getByTestId('requirement-save').click();
  await expect(page.getByTestId('requirement-error')).toContainText('issue key');
  await page.getByTestId('requirement-issue-key').fill('SHOP-400');
  await page.getByTestId('requirement-save').click();
  await expect(page.getByTestId('requirement-heading')).toContainText('REQ-12');
  await expect(page.getByTestId('requirement-heading')).toContainText('Gift wrapping');
  await expect(page.getByRole('link', { name: /REQ-6 Customer can check out/ })).toBeVisible();

  await page.getByTestId('requirement-edit').click();
  const patched = await api(page, 'PATCH', `${P}/requirements/12`, { title: 'Gift wrapping (renamed)' }, { 'If-Match': '"r1"' });
  expect(patched.status).toBe(200);
  await page.getByTestId('requirement-title').fill('Gift wrapping and cards');
  await page.getByTestId('requirement-save').click();
  await expect(page.getByTestId('requirement-stale')).toContainText('changed after you opened it');
  await page.getByRole('button', { name: 'Reload' }).click();
  await expect(page.getByTestId('requirement-heading')).toContainText('Gift wrapping (renamed)');

  await page.getByTestId('requirement-edit').click();
  await page.getByTestId('requirement-parent').fill('12');
  await page.getByTestId('requirement-save').click();
  await expect(page.getByTestId('requirement-error')).toContainText('cannot move under itself');
  await page.getByTestId('requirement-parent').fill('');
  await page.getByTestId('requirement-title').fill('Gift wrapping and cards');
  await page.getByTestId('requirement-save').click();
  await expect(page.getByTestId('requirement-heading')).toContainText('Gift wrapping and cards');

  await page.getByTestId('requirement-archive').click();
  await page.getByTestId('requirement-archive-confirm').click();
  await expect(page.getByTestId('requirements-table')).toBeVisible();
  await expect(row(page, 12)).toHaveCount(0);
  await page.getByTestId('requirements-archived').click();
  await expect(page.getByTestId('requirements-archived')).toBeChecked();
  await expect(row(page, 12)).toBeVisible();
  await page.getByRole('button', { name: 'Restore REQ-12' }).click();
  await expect(row(page, 12)).toHaveCount(0);
  await page.getByTestId('requirements-archived').click();
  await expect(page.getByTestId('requirements-archived')).not.toBeChecked();
  await page.getByRole('button', { name: 'Expand REQ-6' }).click();
  await expect(row(page, 12)).toBeVisible();
});

test('a parent that is archived blocks the restore of a child', async ({ page, consoleGuard }) => {
  consoleGuard.allow(/status of 409/);
  await page.goto('/p/web-shop/requirements');
  await expect(rows(page)).toHaveCount(4);
  expect((await api(page, 'DELETE', `${P}/requirements/8`)).status).toBe(204);
  expect((await api(page, 'DELETE', `${P}/requirements/6`)).status).toBe(204);
  const blocked = await api(page, 'POST', `${P}/requirements/8/restore`);
  expect(blocked.status).toBe(409);
  expect(blocked.body.code).toBe('conflict');
  expect((await api(page, 'POST', `${P}/requirements/6/restore`)).status).toBe(200);
});

test('a requirement shows its cases and links, confirms a proposal and removes a link', async ({ page, consoleGuard }) => {
  consoleGuard.allow(/status of 404/);
  await page.goto('/p/web-shop/requirements/3');
  await expect(page.getByTestId('requirement-heading')).toContainText('Search tolerates typos');
  await expect(page.getByRole('link', { name: /REQ-2 Search finds products/ })).toBeVisible();
  await expect(page.getByRole('link', { name: /REQ-1 Customer can browse the catalog/ })).toBeVisible();
  const cases = page.getByTestId('linked-case-row');
  await expect(cases).toHaveCount(2);
  await expect(page.getByTestId('proposed-badge')).toHaveCount(1);
  await expect(page.getByTestId('coverage-summary')).toBeVisible();
  await expectNoAxeViolations(page, 'requirement');
  await noHorizontalScroll(page);

  await page.getByTestId('confirm-link').click();
  await expect(page.getByTestId('proposed-badge')).toHaveCount(0);
  await expect(cases).toHaveCount(2);
  await page.getByRole('button', { name: /^Remove TC-/ }).first().click();
  await expect(cases).toHaveCount(1);

  await page.getByTestId('link-cases-input').fill('abc');
  await expect(page.getByText('Not a case number: abc')).toBeVisible();
  await page.getByTestId('link-cases-input').fill('999999');
  await page.getByTestId('link-cases-submit').click();
  await expect(page.getByText('TC-999999 not found').first()).toBeVisible();
  await toastsGone(page);
  await page.getByTestId('link-cases-input').fill('1, 2');
  await page.getByTestId('link-cases-submit').click();
  await expect(cases).toHaveCount(3);
});

test('a change made elsewhere reaches an open requirement and the tree through requirement.updated', async ({ page }) => {
  const touch = (n: number, title: string) =>
    page.evaluate(([num, t]) => (window as unknown as { __fastproofMockRequirements: { touchRequirement: (n: number, title: string) => number } }).__fastproofMockRequirements.touchRequirement(num, t), [n, title] as const);
  await page.goto('/p/web-shop/requirements/3');
  await expect(page.getByTestId('requirement-heading')).toContainText('Search tolerates typos');
  await expect(async () => {
    await touch(3, 'Search tolerates two typos');
    await expect(page.getByTestId('requirement-heading')).toContainText('Search tolerates two typos', { timeout: 2_000 });
  }).toPass({ timeout: 15_000 });

  await spaGo(page, '/p/web-shop/requirements');
  await expect(row(page, 10)).toContainText('Customer account');
  await expect(async () => {
    await touch(10, 'Customer account and profile');
    await expect(row(page, 10)).toContainText('Customer account and profile', { timeout: 2_000 });
  }).toPass({ timeout: 15_000 });
});

test('the case tab lists the requirements of a case and links one', async ({ page, consoleGuard }) => {
  consoleGuard.allow(/status of 404/);
  await page.goto('/p/web-shop/requirements/7');
  const first = await page.getByTestId('linked-case-row').first().getAttribute('data-case');
  await spaGo(page, `/p/web-shop/cases/${String(first)}?tab=requirements`);
  const tab = page.getByTestId('requirements-tab');
  await expect(tab).toBeVisible();
  await expect(page.getByTestId('case-requirement-row')).toHaveCount(1);
  await expect(page.getByTestId('case-requirement-row')).toContainText('REQ-7');
  await expectNoAxeViolations(page, 'case requirements tab');
  await page.getByTestId('link-requirement-input').fill('REQ-10');
  await page.getByTestId('link-requirement-submit').click();
  await expect(page.getByTestId('case-requirement-row')).toHaveCount(2);
  await toastsGone(page);
  await page.getByRole('button', { name: 'Remove REQ-10' }).click();
  await expect(page.getByTestId('case-requirement-row')).toHaveCount(1);
  await page.getByTestId('link-requirement-input').fill('REQ-999');
  await page.getByTestId('link-requirement-submit').click();
  await expect(page.getByText('requirement not found').first()).toBeVisible();
});

test('coverage drills down, switches scope in the URL and exports', async ({ page }) => {
  await page.goto('/p/web-shop/coverage');
  const rowsC = page.getByTestId('coverage-row');
  await expect(rowsC).toHaveCount(4);
  await expect(page.locator('[data-testid="coverage-row"][data-number="10"]')).toHaveAttribute('data-coverage', /uncovered|failed|notRun|partial|passed/);
  await expect(page.locator('[data-testid="coverage-row"][data-number="11"]')).toHaveAttribute('data-coverage', 'uncovered');
  await expect(page.getByTestId('changed-after-run').first()).toBeVisible();
  await expectNoAxeViolations(page, 'coverage');
  await noHorizontalScroll(page);

  await page.getByRole('button', { name: 'Expand REQ-6' }).click();
  await expect(rowsC).toHaveCount(6);
  await page.getByRole('button', { name: 'Expand REQ-7' }).click();
  await expect(rowsC).toHaveCount(7);
  await page.getByRole('button', { name: 'Show cases of REQ-8' }).click();
  await expect(page.getByTestId('coverage-case-row')).toHaveCount(1);

  await page.getByTestId('scope-kind').getByText('Window').click();
  await page.getByTestId('scope-days').fill('14');
  await page.getByTestId('scope-branch').fill('main');
  await page.getByTestId('scope-apply').click();
  await expect(page).toHaveURL(/scope=window/);
  await expect(page).toHaveURL(/days=14/);
  await expect(page.getByTestId('scope-summary')).toContainText('last 14 days');
  await expect(rowsC).toHaveCount(4);

  await page.getByTestId('scope-kind').getByText('Launches').click();
  await expect(page.getByTestId('scope-apply')).toBeDisabled();
  await page.getByTestId('scope-launches').fill('0198aa00-0000-7000-8000-000000000901, 0198aa00-0000-7000-8000-000000000902');
  await page.getByTestId('scope-apply').click();
  await expect(page.getByTestId('scope-summary')).toContainText('2 launches');

  await page.getByTestId('export-menu').click();
  await page.getByTestId('export-csv').click();
  await expect(page.getByTestId('export-download')).toBeVisible();
  await expect(page.getByTestId('export-download')).toHaveAttribute('href', /coverage\/exports\/.+\/download\?token=/);
});

test('import from a tracker shows the dry run, applies it and lists the new requirements', async ({ page }) => {
  await page.goto('/p/web-shop/requirements');
  await page.getByTestId('requirements-import').click();
  await expect(page.getByTestId('import-start')).toBeDisabled();
  await page.getByTestId('import-query').fill('project = SHOP and fail');
  await page.getByTestId('import-start').click();
  await expect(page.getByTestId('import-failed')).toContainText('rejected the query');
  await page.getByTestId('import-query').fill('project = SHOP AND issuetype = Story');
  await page.getByTestId('import-start').click();
  await expect(page.getByTestId('import-report')).toContainText('Would create 3');
  await page.getByTestId('import-apply').click();
  await expect(page.getByTestId('import-report')).toContainText('Created 3');
  await page.getByTestId('import-dialog').getByRole('button', { name: 'Close' }).click();
  await expect(row(page, 12)).toContainText('Guest checkout');
  await expect(rows(page)).toHaveCount(7);
});

test('a viewer sees the data without the write actions', async ({ page }) => {
  await page.goto('/p/web-shop/requirements?mockRole=viewer');
  await expect(rows(page)).toHaveCount(4);
  await expect(page.getByTestId('new-requirement')).toHaveCount(0);
  await expect(page.getByTestId('requirements-import')).toHaveCount(0);
  await spaGo(page, '/p/web-shop/requirements/3');
  await expect(page.getByTestId('linked-case-row')).toHaveCount(2);
  await expect(page.getByTestId('requirement-edit')).toHaveCount(0);
  await expect(page.getByTestId('confirm-link')).toHaveCount(0);
  await expect(page.getByTestId('link-cases-input')).toHaveCount(0);
  await page.evaluate(() => window.localStorage.removeItem('fastproof-mock-role'));
});

test('the requirement coverage widget lists the top-level requirements', async ({ page }) => {
  await page.goto('/p/web-shop/dashboards');
  await page.getByRole('button', { name: 'New dashboard' }).click();
  await page.getByLabel('Name').fill('Coverage board');
  await page.getByRole('button', { name: 'Empty dashboard' }).click();
  await page.getByRole('button', { name: 'Add widget' }).click();
  await page.getByRole('menuitem', { name: 'Requirement coverage' }).click();
  const w = page.getByTestId('widget-w1');
  await expect(w.getByTestId('requirement-coverage-bar')).toBeVisible();
  await expect(w.getByTestId('requirement-coverage-table').getByRole('row')).toHaveCount(5);
  await page.getByRole('button', { name: 'Settings of Requirement coverage' }).click();
  await page.getByLabel('Items').fill('2');
  await page.getByRole('button', { name: 'Apply' }).click();
  await expect(w.getByTestId('requirement-coverage-table').getByRole('row')).toHaveCount(3);
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
  await expect(w.getByRole('link', { name: /REQ-1 / })).toBeVisible();
  await toastsGone(page);
  await expectNoAxeViolations(page, 'requirement coverage widget');
});
