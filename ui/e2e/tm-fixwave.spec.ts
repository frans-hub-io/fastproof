import { expect, test } from '@playwright/test';
import path from 'node:path';
import { expectNoAxeViolations, openCase, openManualLaunch } from './tm-helpers';

test('412: "Save my changes" keeps what the other tab changed and names "you (another tab)"', async ({ page }) => {
  const n = await openCase(page, 'Pay with a new card');
  await page.getByTestId('case-edit').click();
  await page.getByTestId('case-name-input').fill('Pay with a new card (edited)');
  await page.evaluate(async (num) => {
    const path = `/api/v1/projects/web-shop/cases/${String(num)}`;
    const cur = await fetch(path);
    const etag = cur.headers.get('ETag') ?? '';
    const body = (await cur.json()) as { tags: string[] };
    await fetch(path, { method: 'PATCH', headers: { 'Content-Type': 'application/json', 'If-Match': etag }, body: JSON.stringify({ tags: [...body.tags, 'concurrent'] }) });
  }, n);
  await page.getByTestId('case-save').click();
  const dialog = page.getByTestId('conflict-dialog');
  await expect(dialog).toContainText('saved by you (another tab)');
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Name' })).toContainText('Pay with a new card (edited)');
  await expect(dialog.getByTestId('diff-field').filter({ hasText: 'Tags' })).toHaveCount(0);
  await page.getByTestId('conflict-mine').click();
  await expect(page.getByText(`TC-${String(n)} is now version`)).toBeVisible();
  const saved = await page.evaluate(async (num) => (await (await fetch(`/api/v1/projects/web-shop/cases/${String(num)}`)).json()) as { name: string; tags: string[] }, n);
  expect(saved.name).toBe('Pay with a new card (edited)');
  expect(saved.tags).toContain('concurrent');
});

test('the runner suggests "In progress" while steps are untested, like the API', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeVisible();
  await expect(page.getByTestId('suggested-status')).toHaveAttribute('data-status', 'in_progress');
  await expect(page.getByTestId('finish-panel')).toContainText(/Suggested:\s*In progress/);
  await expect(page.getByTestId('suggested-untested')).toContainText(/untested · finishing marks them skipped: passed/);
  await page.getByTestId('queue-item').nth(1).click();
  await expect(page.getByTestId('suggested-status')).toHaveAttribute('data-status', 'untested');
});

test('a lead reassigns run items; they show up in the tester\'s My runs and open in the runner', async ({ page }) => {
  await page.goto('/p/web-shop/my-runs');
  await expect(page.getByTestId('my-run-item').first()).toBeVisible();
  const before = await page.getByTestId('my-run-item').count();
  expect(before).toBeGreaterThan(0);
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  const olehRows = page.getByTestId('manual-item').filter({ has: page.getByTestId('manual-item-assignee').filter({ hasText: 'oleh@example.com' }) }).filter({ hasText: 'Untested' });
  const names: string[] = [];
  for (let i = 0; i < 3; i++) {
    const row = olehRows.nth(i);
    names.push(((await row.getByRole('link').innerText()).match(/TC-\d+/) ?? [''])[0]);
    await row.getByTestId('manual-item-select').check();
  }
  await expect(page.getByTestId('manual-bulk')).toContainText('3 selected');
  await page.getByTestId('manual-bulk-assign').click();
  const dialog = page.getByTestId('assign-dialog');
  await dialog.getByTestId('assign-input').fill('alex');
  await page.getByRole('option', { name: 'Alex Doe (alex@example.com)' }).click();
  await expect(dialog.getByTestId('assign-input')).toHaveValue('alex@example.com');
  await dialog.getByTestId('assign-confirm').click();
  await expect(page.getByText('Assigned to alex@example.com')).toBeVisible();
  await expect(page.getByTestId('assign-dialog')).toHaveCount(0);

  await page.getByRole('link', { name: 'My runs' }).click();
  await expect(page.getByTestId('my-run-item')).toHaveCount(before + 3);
  for (const label of names) await expect(page.getByTestId('my-run-item').filter({ hasText: new RegExp(`${label}(?!\\d)`) })).toHaveCount(1);
  await page.getByTestId('my-run-item').filter({ hasText: new RegExp(`${names[0] ?? 'TC-'}(?!\\d)`) }).getByRole('link').click();
  await expect(page).toHaveURL(/\/run\//);
  await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toContainText(names[0] ?? 'TC-');
});

test('an unknown assignee is allowed with a warning', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-item-reassign').first().click();
  const dialog = page.getByTestId('assign-dialog');
  await dialog.getByTestId('assign-input').fill('olehh@example.com');
  await expect(dialog.getByText('Not a known user of this project')).toBeVisible();
  await expect(dialog.getByTestId('assign-confirm')).toBeEnabled();
});

test('Print report renders every item with its steps and actual results in print media', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-tab').locator('label').filter({ hasText: /^Failed$/ }).click();
  await expect(page.getByTestId('manual-item').first()).toContainText('Failed');
  await page.getByTestId('manual-print').click();
  await expect(page.getByTestId('manual-print-view')).toBeVisible();
  const total = await page.getByTestId('print-item').count();
  expect(total).toBeGreaterThan(10);
  await page.emulateMedia({ media: 'print' });
  await expect(page.getByTestId('print-now')).toBeHidden();
  await expect(page.getByRole('navigation', { name: 'Main' })).toBeHidden();
  await expect(page.getByRole('banner')).toBeHidden();
  const failed = page.getByTestId('print-item').filter({ has: page.locator('[data-testid="print-step"][data-status="failed"]') }).first();
  await expect(failed).toBeVisible();
  await expect(failed.getByText(/The confirmation page stays blank/)).toBeVisible();
  await expect(failed.getByTestId('print-item-meta')).toContainText(/Version \d+ · run 1 · assignee .* · executed by maria@example\.com/);
  await expect(page.getByTestId('print-item').filter({ hasText: 'Untested' }).first()).toBeVisible();
  await page.emulateMedia({ media: 'screen' });
});

test('add cases to a running launch from the Manual tab', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await expect(page.getByTestId('manual-item').first()).toBeVisible();
  const before = await page.getByTestId('manual-item').count();
  await page.getByTestId('manual-add-cases').click();
  const dialog = page.getByTestId('add-cases-dialog');
  await dialog.getByTestId('add-cases-input').fill('delivery');
  const option = page.getByRole('option').first();
  const label = (await option.innerText()).replace(/^TC-\d+\s+/, '').trim();
  await option.click();
  await dialog.getByTestId('add-cases-assignee').fill('maria@');
  await page.getByRole('option', { name: 'maria@example.com' }).click();
  await dialog.getByTestId('add-cases-confirm').click();
  await expect(page.getByText('Added 1 case')).toBeVisible();
  await expect(page.getByTestId('manual-item')).toHaveCount(before + 1);
  await expect(page.getByTestId('manual-item').filter({ hasText: label })).toContainText('maria@example.com');
});

test('launch list and plan page show the manual runs', async ({ page }) => {
  await page.goto('/p/web-shop');
  await expect(page.getByTestId('launch-row').filter({ has: page.getByTestId('launch-manual') }).first()).toContainText('#38 ');
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
  await expect(page.getByTestId('plan-launch')).toHaveCount(1);
  await page.getByTestId('plan-start').click();
  await page.getByTestId('start-submit').click();
  await expect(page.getByTestId('launch-started')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('plan-launch')).toHaveCount(2);
  await expect(page.getByTestId('plan-launch').first()).toContainText('manual open');
});

test('folders move with a dialog, reorder with buttons and move by drag', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  const tree = page.getByTestId('case-tree-view');
  const top = () => tree.locator('[role="treeitem"][aria-level="1"]');
  await expect(top()).toHaveText(['Storefront', 'Shop API', 'Mobile web', 'Automated'].map((t) => new RegExp(`^${t}`)));

  await tree.getByRole('treeitem', { name: 'Shop API' }).click();
  await page.getByTestId('folder-up').click();
  await expect(top().first()).toContainText('Shop API');

  await tree.getByRole('treeitem', { name: 'Mobile web' }).click();
  await page.getByTestId('folder-move').click();
  const dialog = page.getByTestId('folder-move-dialog');
  await dialog.getByTestId('folder-move-target').click();
  await page.getByRole('option', { name: 'Storefront', exact: true }).click();
  await dialog.getByTestId('folder-move-confirm').click();
  await expect(page.getByText('Folder moved')).toBeVisible();
  await expect(top()).toHaveCount(3);
  await tree.getByRole('treeitem', { name: 'Storefront' }).press('ArrowRight');
  await expect(tree.getByRole('treeitem', { name: 'Mobile web' })).toHaveAttribute('aria-level', '2');

  await tree.getByRole('treeitem', { name: 'Automated' }).dragTo(tree.getByRole('treeitem', { name: 'Shop API' }));
  await expect(top()).toHaveCount(2);
});

test('a rerun keeps the earlier execution and the runner shows it', async ({ page }) => {
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await page.getByTestId('manual-item').filter({ hasText: 'Failed' }).first().getByRole('link').click();
  await expect(page.getByTestId('run-case-title')).toBeVisible();
  await expect(page.getByTestId('run-executions')).toHaveCount(0);
  await page.getByTestId('rerun').click();
  await expect(page.getByTestId('live-region')).toContainText('run 2 started');
  const earlier = page.getByTestId('run-execution');
  await expect(earlier).toHaveCount(1);
  await expect(earlier).toContainText('Run 1');
  await expect(earlier).toContainText('Failed');
  await expect(earlier).toContainText('The confirmation page stays blank');
});

test('import: create unknown fields, status draft, report lists created fields', async ({ page }) => {
  await page.goto('/p/web-shop/cases/import');
  await page.getByTestId('import-file').setInputFiles(path.join(import.meta.dirname, 'fixtures', 'testrail-cases.csv'));
  await expect(page.getByTestId('import-mapping')).toBeVisible();
  await page.getByTestId('import-unknown').click();
  await page.getByRole('option', { name: 'Create fields and options' }).click();
  await page.getByTestId('import-status').click();
  await page.getByRole('option', { name: 'Draft', exact: true }).click();
  await page.getByTestId('import-dry-run').click();
  const report = page.getByTestId('import-report');
  await expect(report.getByTestId('report-create')).toContainText('3');
  await expect(report.getByTestId('report-unknown-fields')).toHaveCount(0);
  await report.getByTestId('import-apply').click();
  await expect(page.getByTestId('import-progress').getByTestId('import-result')).toContainText('3 created');
  await page.getByTestId('import-open-cases').click();
  await page.getByTestId('case-search').fill('Refund a paid order');
  await page.getByTestId('case-link').filter({ hasText: 'Refund a paid order' }).first().click();
  await expect(page.locator('[data-case-status]').first()).toHaveAttribute('data-case-status', 'draft');
});

for (const scheme of ['dark', 'light']) {
  test(`axe (${scheme}): case fields, my runs, print view, manual tab dialogs, folder move`, async ({ page }) => {
    test.setTimeout(150_000);
    await page.addInitScript((s) => {
      try {
        localStorage.setItem('franshub-color-scheme', s);
      } catch {
        /* ignore */
      }
    }, scheme);
    await page.goto('/p/web-shop/settings/case-fields');
    await expect(page.getByTestId('field-row').first()).toBeVisible();
    await expect(page.locator('html')).toHaveAttribute('data-mantine-color-scheme', scheme);
    await expectNoAxeViolations(page, `case fields (${scheme})`);
    await page.getByTestId('field-row').filter({ hasText: 'Platform' }).getByTestId('field-edit').click();
    await expect(page.getByTestId('field-dialog')).toBeVisible();
    await expectNoAxeViolations(page, `field dialog (${scheme})`);
    await page.keyboard.press('Escape');

    await page.goto('/p/web-shop/my-runs');
    await expect(page.getByTestId('my-run-item').first()).toBeVisible();
    await expectNoAxeViolations(page, `my runs (${scheme})`);

    await openManualLaunch(page);
    await page.getByTestId('tab-manual').click();
    await page.getByTestId('manual-item-select').first().check();
    await expect(page.getByTestId('manual-bulk')).toBeVisible();
    await expectNoAxeViolations(page, `manual tab with a selection (${scheme})`);
    await page.getByTestId('manual-bulk-assign').click();
    await expect(page.getByTestId('assign-dialog')).toBeVisible();
    await expectNoAxeViolations(page, `assign dialog (${scheme})`);
    await page.keyboard.press('Escape');
    if (await page.getByTestId('assign-dialog').count()) await page.keyboard.press('Escape');
    await expect(page.getByTestId('assign-dialog')).toHaveCount(0);
    await page.getByTestId('manual-bulk-result').click();
    await page.getByTestId('set-result-dialog').getByLabel('Passed').check();
    await expectNoAxeViolations(page, `set result dialog (${scheme})`);
    await page.keyboard.press('Escape');
    await page.getByTestId('manual-add-cases').click();
    await expect(page.getByTestId('add-cases-dialog')).toBeVisible();
    await page.getByTestId('add-cases-dialog').getByRole('heading', { name: 'Add cases to the run' }).click();
    await expect(page.getByRole('listbox')).toHaveCount(0);
    await expectNoAxeViolations(page, `add cases dialog (${scheme})`);
    await page.keyboard.press('Escape');
    if (await page.getByTestId('add-cases-dialog').count()) await page.keyboard.press('Escape');
    await expect(page.getByTestId('add-cases-dialog')).toHaveCount(0);
    await page.getByTestId('manual-print').click();
    await expect(page.getByTestId('print-item').first()).toBeVisible();
    await expectNoAxeViolations(page, `print view (${scheme})`);

    await page.goto('/p/web-shop/cases');
    await page.getByTestId('case-tree-view').getByRole('treeitem', { name: 'Mobile web' }).click();
    await page.getByTestId('folder-move').click();
    await expect(page.getByTestId('folder-move-dialog')).toBeVisible();
    await page.getByTestId('folder-move-dialog').getByRole('heading', { name: 'Move Mobile web' }).click();
    await expect(page.getByRole('listbox')).toHaveCount(0);
    await expectNoAxeViolations(page, `folder move dialog (${scheme})`);
  });
}
