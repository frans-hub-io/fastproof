import { expect, test, type Page } from '@playwright/test';
import { ADMIN_TOKEN, BASE_URL, ENABLED, PROJECT, seeded } from './env';
import { signInWithToken, Watcher } from './watch';

test.skip(!ENABLED, 'set FASTPROOF_E2E_REAL=1 to run against a real server');

const P = `/p/${PROJECT}`;

async function pickOption(page: Page, testId: string, option: string | RegExp): Promise<void> {
  await page.getByTestId(testId).click();
  await page.getByRole('option', { name: option }).first().click();
}

test('sign in with an API token from the start page, browse, sign out', async ({ page }) => {
  const w = new Watcher(page, BASE_URL);
  w.allow({ path: /^\/api\/v1\/me$/, status: 401 });
  await page.goto('/');
  await expect(page).toHaveURL(/\/sign-in\?return=%2F/);
  await expect(page.getByTestId('sign-in-sso')).toHaveCount(0);
  await page.getByTestId('sign-in-token').fill('not-a-valid-token-value');
  await page.getByTestId('sign-in-submit').click();
  await expect(page.getByText('The server did not accept this token')).toBeVisible();
  await page.getByTestId('sign-in-token').fill(ADMIN_TOKEN);
  await page.getByTestId('sign-in-submit').click();
  await expect(page.getByTestId('project-card').first()).toBeVisible();
  expect(page.url()).not.toContain(ADMIN_TOKEN);
  await page.getByRole('link', { name: /Project Web shop/ }).click();
  await expect(page.getByTestId('launch-row').first()).toBeVisible();
  await page.goto(`${P}/dashboard`);
  await expect(page.getByTestId('chart-pass-rate').or(page.getByText(/No launches/))).toBeVisible();
  await page.getByTestId('user-menu').click();
  await page.getByTestId('logout').click();
  await expect(page).toHaveURL(/\/sign-in/);
  expect(await page.evaluate(() => window.sessionStorage.getItem('fastproof.apiToken'))).toBeNull();
  await page.goto(`${P}/dashboard`);
  await expect(page).toHaveURL(/\/sign-in\?return=%2Fp%2Fshop%2Fdashboard/);
  w.check();
});

test('admin: projects, launches, launch with clusters, gate, live logs and test plan', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto('/');
  await expect(page.getByTestId('project-card')).toHaveCount(1);
  await page.goto(P);
  await expect.poll(() => page.getByTestId('launch-row').count()).toBeGreaterThanOrEqual(7);

  await page.goto(`${P}/launches/${s.launches['nightly 3'] ?? ''}`);
  await expect(page.getByRole('heading', { name: /nightly 3/ })).toBeVisible();
  await expect(page.getByTestId('explorer-test').first()).toBeVisible();
  await page.getByRole('tab', { name: 'Results' }).click();
  await expect(page.getByTestId('result-row').first()).toBeVisible();
  await page.getByRole('tab', { name: /Failures/ }).click();
  const cluster = page.getByTestId('cluster').first();
  await expect(cluster).toBeVisible();
  await cluster.getByRole('button').first().click();
  await cluster.getByTestId('triage-cluster').click();
  await pickOption(page, 'triage-type', 'Product bug');
  await page.getByTestId('triage-form').getByLabel('Comment').fill(`Fails since the checkout redesign (${Date.now().toString(36)})`);
  await page.getByTestId('triage-save').click();
  await expect(page.getByText(/results? updated/)).toBeVisible();
  await expect(cluster.getByTestId('defect-chip').first()).toHaveText('Product bug');

  await page.getByTestId('open-test-plan').click();
  await page.getByTestId('test-plan-create').click();
  await expect(page.getByTestId('test-plan-download')).toBeVisible();
  const download = page.waitForEvent('download');
  await page.getByTestId('test-plan-download').click();
  expect((await download).suggestedFilename()).toBe('testplan.json');
  await page.keyboard.press('Escape');

  await page.goto(`${P}/launches/${s.runningLaunchId}`);
  await expect(page.getByRole('heading', { name: /checkout live run/ })).toBeVisible();
  await page.getByTestId('tab-logs').click();
  await expect(page.getByTestId('log-stream-state')).toHaveAttribute('data-state', 'live');
  await expect.poll(() => page.getByTestId('log-line').count()).toBeGreaterThan(0);
  await page.getByTestId('log-result-filter').click();
  await page.getByRole('option', { name: 'pays with a saved card' }).click();
  await expect.poll(async () => (await page.getByTestId('log-count').innerText()).replace(/\D/g, '')).toBe('40');
  await page.getByTestId('log-level-filter').click();
  await page.getByRole('option', { name: 'Errors only' }).click();
  await expect.poll(async () => (await page.getByTestId('log-count').innerText()).replace(/\D/g, '')).toBe('8');
  w.check();
});

test('admin: result with logs, similar failures and mock interactions; test case', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  w.allow({ path: /\/mock-interactions$/, status: 409 });
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/results/${s.failedResultId}`);
  await expect(page.getByTestId('defect-card')).toBeVisible();
  await page.getByTestId('tab-result-logs').click();
  await expect(page.getByTestId('log-console')).toBeVisible();
  await page.getByRole('tab', { name: /Mock/ }).click();
  await expect(page.getByText('FastMock is not linked to this project')).toBeVisible();
  await page.goto(`${P}/results/${s.liveResultId}`);
  await page.getByTestId('tab-result-logs').click();
  await expect.poll(async () => (await page.getByTestId('log-count').innerText()).replace(/\D/g, '')).toBe('40');
  await page.goto(`${P}/test-cases/${s.testCaseId}`);
  await expect(page.getByTestId('history-row').first()).toBeVisible();
  w.check();
});

test('admin: rules (create, dry run, apply), gates, notifications, integrations, secrets', async ({ page }) => {
  const s = seeded();
  const run = Date.now().toString(36);
  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/rules`);
  await page.getByTestId('rule-new').click();
  await page.getByTestId('rule-name').fill(`Assertion failures ${run}`);
  await page.getByTestId('rule-message-regex').fill('assert');
  await pickOption(page, 'rule-defect-type', 'Automation bug');
  await pickOption(page, 'dry-run-launch', /nightly 2/);
  await page.getByTestId('dry-run').click();
  await expect(page.getByTestId('dry-run-result')).toContainText(/\d+ results? would match/);
  await page.getByTestId('rule-save').click();
  const row = page.getByTestId('rule-row').filter({ hasText: `Assertion failures ${run}` });
  await expect(row).toBeVisible();
  await row.getByTestId('rule-apply-open').click();
  await pickOption(page, 'apply-launch', /nightly 2/);
  await page.getByTestId('rule-apply-run').click();
  await expect(page.getByTestId('rule-apply-result')).toContainText(/updated|queued|done/);
  await page.keyboard.press('Escape');

  await page.goto(`${P}/gates`);
  await page.getByTestId('gate-add').click();
  await page.getByTestId('gate-name').fill(`no broken tests ${run}`);
  await pickOption(page, 'gate-rule-metric', 'Broken results');
  await page.getByTestId('gate-save').click();
  await expect(page.getByTestId('gate-card').filter({ hasText: `no broken tests ${run}` })).toBeVisible();
  await page.goto(`${P}/launches/${s.launches['nightly 3'] ?? ''}`);
  await expect(page.getByTestId('gate-badge')).toBeVisible();
  await page.getByTestId('gate-badge').click();
  await expect(page.getByTestId('gate-details')).toContainText('Broken results ≤ 0');

  await page.goto(`${P}/settings?tab=secrets`);
  await expect(page.getByTestId('secrets-table').or(page.getByText('No secrets yet'))).toBeVisible();
  await page.getByTestId('secret-name').fill(`hook-key-${run}`);
  await page.getByTestId('secret-value').fill('e2e-secret-value-0001');
  await page.getByTestId('secret-save').click();
  await expect(page.getByTestId('secret-row').filter({ hasText: `hook-key-${run}` })).toBeVisible();
  expect(await page.content()).not.toContain('e2e-secret-value-0001');

  await page.goto(`${P}/notifications`);
  await page.getByTestId('notification-add').click();
  await page.getByTestId('notification-target').fill(`https://hooks.example.com/e2e/${run}`);
  await page.getByTestId('notification-save').click();
  await expect(page.getByTestId('notification-card').filter({ hasText: `https://hooks.example.com/e2e/${run}` })).toBeVisible();
  await expect(page.getByTestId('deliveries-table').or(page.getByText(/No deliveries/))).toBeVisible();

  await page.goto(`${P}/settings?tab=integrations`);
  await expect(page.getByTestId('integrations-save')).toBeVisible();
  if (!(await page.getByTestId('vcs-enabled').isChecked())) await page.getByTestId('vcs-enabled').click();
  await pickOption(page, 'vcs-token', `hook-key-${run}`);
  await page.getByTestId('integrations-save').click();
  await expect(page.getByText('Integrations saved')).toBeVisible();
  await page.goto(`${P}/settings`);
  await page.getByTestId('flaky-history-depth').fill('12');
  await page.getByTestId('settings-save').click();
  await expect(page.getByText('General settings saved')).toBeVisible();
  w.check();
});

test('admin: compare, dashboard, flaky, tokens, audit, jobs', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  await signInWithToken(page, ADMIN_TOKEN);
  await page.goto(`${P}/compare?a=${s.launches['nightly 2'] ?? ''}&b=${s.launches['nightly 3'] ?? ''}`);
  await expect(page.getByTestId('compare-summary')).toBeVisible();
  await page.goto(`${P}/dashboard`);
  await expect(page.getByTestId('chart-pass-rate')).toBeVisible();
  await page.goto(`${P}/flaky`);
  await expect(page.getByRole('heading', { name: /Flaky/ })).toBeVisible();
  await page.goto('/settings/tokens');
  await expect(page.getByTestId('token-row').first()).toBeVisible();
  await page.goto('/settings/audit');
  await expect(page.getByTestId('audit-row').first()).toBeVisible();
  await page.goto('/admin/jobs');
  await expect(page.getByTestId('job-row').filter({ hasText: 'partitions.ensure' })).toBeVisible();
  w.check();
});

test('viewer: reads everything in the project, writes nothing', async ({ page }) => {
  const s = seeded();
  const w = new Watcher(page, BASE_URL);
  w.allow({ path: /\/mock-interactions$/, status: 409 });
  await signInWithToken(page, s.viewerToken);
  await page.goto('/');
  await expect(page.getByTestId('project-card')).toHaveCount(1);
  await page.goto(`${P}/launches/${s.launches['nightly 3'] ?? ''}`);
  await page.getByRole('tab', { name: /Failures/ }).click();
  await expect(page.getByTestId('cluster').first()).toBeVisible();
  await expect(page.getByTestId('triage-cluster')).toHaveCount(0);
  await expect(page.getByTestId('open-test-plan')).toHaveCount(0);
  await page.goto(`${P}/results/${s.failedResultId}`);
  await expect(page.getByTestId('triage-open')).toHaveCount(0);
  await page.goto(`${P}/rules`);
  await expect(page.getByTestId('rule-new')).toHaveCount(0);
  await page.goto(`${P}/gates`);
  await expect(page.getByTestId('gate-add')).toHaveCount(0);
  await page.goto(`${P}/settings`);
  await expect(page.getByTestId('read-only')).toBeVisible();
  for (const path of [`${P}/dashboard`, `${P}/flaky`, `${P}/notifications`, `${P}/compare`]) {
    await page.goto(path);
    await expect(page.locator('main')).toBeVisible();
  }
  await page.goto('/admin/jobs');
  await expect(page.getByTestId('job-row')).toHaveCount(0);
  w.check();
});
