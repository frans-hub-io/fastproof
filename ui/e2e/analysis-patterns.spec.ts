import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';
import { openRichResult } from './helpers';

const shots = process.env.FASTPROOF_AN_SHOTS;

async function prefs(page: Page, scheme = 'dark') {
  await page.addInitScript((s: string) => {
    try {
      localStorage.setItem('fastproof-mock-analysis', 'on');
      localStorage.setItem('franshub-color-scheme', s);
    } catch {
      /* ignore */
    }
  }, scheme);
}

async function newLogRule(page: Page) {
  await page.goto('/p/web-shop/rules');
  await page.getByTestId('rule-new').click();
  await page.getByTestId('rule-name').fill('Out of memory in the logs');
  await page.getByTestId('rule-log-mode').getByText('Contains').click();
  await page.getByTestId('rule-log-text').fill('OutOfMemoryError');
  await page.getByTestId('rule-pattern').fill('oom');
}

test('a rule with a log condition and a pattern mark runs after the launch', async ({ page }) => {
  await prefs(page);
  await newLogRule(page);
  await expect(page.getByTestId('rule-after-launch')).toContainText('runs after each launch has finished');
  await expect(page.getByLabel('Enabled: apply to every launch once it has finished')).toBeChecked();
  await page.getByTestId('rule-pattern').fill('Not A Name');
  await expect(page.getByText('Lower-case letters, digits, dot, dash and underscore; up to 64 characters')).toBeVisible();
  await expect(page.getByTestId('rule-save')).toBeDisabled();
  await page.getByTestId('rule-pattern').fill('oom');
  await page.getByTestId('dry-run').click();
  await expect(page.getByTestId('dry-run-result')).toContainText('would match');
  await page.getByTestId('rule-save').click();
  const row = page.getByTestId('rule-row').filter({ hasText: 'Out of memory in the logs' });
  await expect(row.getByTestId('rule-phase')).toHaveText('after launch');
  await expect(row.getByTestId('rule-pattern-badge')).toHaveText('pattern oom');
  await expect(row).toContainText('log contains "OutOfMemoryError"');
  await expect(page.getByTestId('rule-row').first().getByTestId('rule-phase')).toHaveText('on ingest');
  await row.getByTestId('rule-edit').click();
  await expect(page.getByTestId('rule-log-text')).toHaveValue('OutOfMemoryError');
  await expect(page.getByTestId('rule-pattern')).toHaveValue('oom');
});

test('a result shows its pattern marks and the other results with the pattern', async ({ page }) => {
  await prefs(page);
  await openRichResult(page);
  await page.getByTestId('tab-result-triage').click();
  const marks = page.getByTestId('result-patterns');
  await expect(marks.getByTestId('result-pattern')).toHaveCount(1);
  await expect(marks.getByTestId('result-pattern-excerpt')).toContainText('java.lang.OutOfMemoryError');
  await marks.getByTestId('result-pattern-others').click();
  await expect(marks.getByTestId('result-pattern-other').first().or(marks.getByText('No other result of this launch has it.'))).toBeVisible();
});

test('the popular-patterns widget counts log patterns of rules', async ({ page }) => {
  await prefs(page);
  await page.goto('/p/web-shop/dashboards');
  await page.getByRole('button', { name: 'New dashboard' }).click();
  await page.getByLabel('Name').fill('Log patterns');
  await page.getByRole('button', { name: 'Empty dashboard' }).click();
  await expect(page).toHaveURL(/edit=1/);
  await page.getByRole('button', { name: 'Add widget' }).click();
  await page.getByRole('menuitem', { name: 'Most popular error patterns' }).click();
  await expect(page.getByTestId('patterns-table')).toBeVisible();
  await page.getByRole('button', { name: 'Settings of Most popular error patterns' }).click();
  await page.getByTestId('widget-patterns-mode').click();
  await page.getByRole('option', { name: 'Log patterns marked by rules' }).click();
  await page.getByRole('button', { name: 'Apply' }).click();
  await expect(page.getByTestId('log-patterns-table')).toContainText('db-timeout');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
  await expect(page.getByTestId('log-patterns-table')).toContainText('oom');
});

for (const scheme of ['dark', 'light']) {
  for (const width of [1280, 360]) {
    test(`log patterns screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!shots, 'set FASTPROOF_AN_SHOTS to a directory to take the screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await prefs(page, scheme);
      const shoot = async (name: string) => {
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(shots ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: width < 500 });
      };
      await newLogRule(page);
      await page.getByTestId('rule-log-levels').click();
      await page.getByRole('option', { name: 'error' }).click();
      await page.keyboard.press('Escape');
      await shoot('patterns-rule-editor');
      await page.getByTestId('rule-save').click();
      await expect(page.getByTestId('rule-row').filter({ hasText: 'Out of memory in the logs' })).toBeVisible();
      await shoot('patterns-rules');
      await openRichResult(page);
      await page.getByTestId('tab-result-triage').click();
      await page.getByTestId('result-pattern-others').click();
      await shoot('patterns-result');
      await page.goto('/p/web-shop/dashboards');
      await page.getByRole('button', { name: 'New dashboard' }).click();
      await page.getByLabel('Name').fill('Log patterns');
      await page.getByRole('button', { name: 'Empty dashboard' }).click();
      await page.getByRole('button', { name: 'Add widget' }).click();
      await page.getByRole('menuitem', { name: 'Most popular error patterns' }).click();
      await page.getByRole('button', { name: 'Settings of Most popular error patterns' }).click();
      await page.getByTestId('widget-patterns-mode').click();
      await page.getByRole('option', { name: 'Log patterns marked by rules' }).click();
      await page.getByRole('button', { name: 'Apply' }).click();
      await page.getByRole('button', { name: 'Save' }).click();
      await expect(page.getByTestId('log-patterns-table')).toBeVisible();
      await shoot('patterns-widget');
    });
  }
}
