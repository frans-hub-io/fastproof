import { expect, test } from '@playwright/test';
import path from 'node:path';

test('import wizard: upload, map, dry-run report, apply with progress', async ({ page }) => {
  await page.goto('/p/web-shop/cases');
  await page.getByTestId('open-import').click();
  await expect(page.getByRole('heading', { name: 'Import test cases' })).toBeVisible();
  await page.getByTestId('import-file').setInputFiles(path.join(import.meta.dirname, 'fixtures', 'testrail-cases.csv'));
  const mapping = page.getByTestId('import-mapping');
  await expect(mapping).toBeVisible();
  await expect(page.getByTestId('import-format')).toHaveValue('TestRail CSV');
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
  await page.getByTestId('import-open-cases').click();
  await page.getByTestId('case-search').fill('refund');
  for (const name of ['Refund a paid order', 'Partial refund of one item', 'Refund to a gift card']) await expect(page.getByTestId('case-link').filter({ hasText: name })).toHaveCount(1);
  await page.getByTestId('case-link').filter({ hasText: 'Refund a paid order' }).click();
  await expect(page.getByTestId('step-view')).toHaveCount(2);
  await expect(page.getByRole('link', { name: 'Refunds' })).toBeVisible();
});
