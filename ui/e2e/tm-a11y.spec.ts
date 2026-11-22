import { expect, test } from '@playwright/test';
import path from 'node:path';
import { expectNoAxeViolations, openCase, openManualLaunch } from './tm-helpers';

test.describe.configure({ timeout: 150_000 });

test('axe: test cases, a case in view and edit mode, versions, unlinked tests', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-row').first()).toBeVisible();
  await page.getByTestId('case-check').first().check();
  await expect(page.getByTestId('bulk-bar')).toBeVisible();
  await expectNoAxeViolations(page, 'cases');
  await page.getByTestId('tab-unlinked').click();
  await expect(page.getByTestId('unlinked-row').first()).toBeVisible();
  await expectNoAxeViolations(page, 'unlinked tests');
  await openCase(page, 'Apply a discount code at checkout');
  await expect(page.getByTestId('suggestion')).toBeVisible();
  await expectNoAxeViolations(page, 'case');
  await page.getByTestId('case-edit').click();
  await expect(page.getByTestId('steps-editor')).toBeVisible();
  await expectNoAxeViolations(page, 'case editor');
  await page.getByTestId('case-cancel').click();
  await page.getByRole('tab', { name: 'Details' }).click();
  await expect(page.getByTestId('details-tab')).toBeVisible();
  await expectNoAxeViolations(page, 'case details');
  await page.getByRole('tab', { name: 'History' }).click();
  await expect(page.getByTestId('history-tab')).toBeVisible();
  await expectNoAxeViolations(page, 'case history');
  await page.getByRole('tab', { name: /Versions/ }).click();
  await expect(page.getByTestId('version-row').first()).toBeVisible();
  await expectNoAxeViolations(page, 'case versions');
});

test('axe: plans, plan builder, runner, manual tab', async ({ page }) => {
  await page.goto('/p/web-shop/plans');
  await expect(page.getByTestId('plan-card').first()).toBeVisible();
  await expectNoAxeViolations(page, 'plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
  await expect(page.getByTestId('plan-item').first()).toBeVisible();
  await expectNoAxeViolations(page, 'plan');
  await page.getByTestId('plan-start').click();
  await expect(page.getByTestId('start-dialog')).toBeVisible();
  await expectNoAxeViolations(page, 'start launch dialog');
  await page.keyboard.press('Escape');

  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await expect(page.getByTestId('manual-items')).toBeVisible();
  await expectNoAxeViolations(page, 'manual tab');
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await expectNoAxeViolations(page, 'runner');
  await page.keyboard.press('d');
  await expect(page.getByTestId('defect-drawer')).toBeVisible();
  await expectNoAxeViolations(page, 'defect drawer');
  await page.keyboard.press('Escape');
  await page.getByTestId('open-help').click();
  await expectNoAxeViolations(page, 'runner help');
  await page.keyboard.press('Escape');
});

test('axe: import wizard', async ({ page }) => {
  await page.goto('/p/web-shop/cases/import');
  await expect(page.getByTestId('import-drop')).toBeVisible();
  await expectNoAxeViolations(page, 'import upload');
  await page.getByTestId('import-file').setInputFiles(path.join(import.meta.dirname, 'fixtures', 'testrail-cases.csv'));
  await expect(page.getByTestId('import-mapping')).toBeVisible();
  await expectNoAxeViolations(page, 'import mapping');
  await page.getByTestId('import-dry-run').click();
  await expect(page.getByTestId('report-create')).toBeVisible();
  await expectNoAxeViolations(page, 'import report');
});

test('axe in the light theme: cases, case, plan, manual tab, runner', async ({ page }) => {
  await page.addInitScript(() => {
    try {
      localStorage.setItem('franshub-color-scheme', 'light');
    } catch {
      /* ignore */
    }
  });
  await page.goto('/p/web-shop/cases');
  await expect(page.getByTestId('case-row').first()).toBeVisible();
  await expect(page.locator('html')).toHaveAttribute('data-mantine-color-scheme', 'light');
  await expectNoAxeViolations(page, 'cases (light)');
  await openCase(page, 'Pay with a saved card');
  await expectNoAxeViolations(page, 'case (light)');
  await page.goto('/p/web-shop/plans');
  await page.getByTestId('plan-card').filter({ hasText: 'Release 2.14 regression' }).click();
  await expect(page.getByTestId('plan-item').first()).toBeVisible();
  await expectNoAxeViolations(page, 'plan (light)');
  await openManualLaunch(page);
  await page.getByTestId('tab-manual').click();
  await expect(page.getByTestId('manual-items')).toBeVisible();
  await expectNoAxeViolations(page, 'manual tab (light)');
  await page.getByTestId('continue-testing').click();
  await expect(page.getByTestId('run-case-title')).toBeFocused();
  await expectNoAxeViolations(page, 'runner (light)');
});
