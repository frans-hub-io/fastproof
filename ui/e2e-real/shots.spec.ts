import { expect, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { ADMIN_TOKEN, ENABLED, PROJECT, seeded } from './env';
import { signInWithToken } from './watch';

const DIR = process.env.FASTPROOF_E2E_SHOTS;

test.skip(!ENABLED || !DIR, 'set FASTPROOF_E2E_REAL=1 and FASTPROOF_E2E_SHOTS=<dir> to take screenshots');

const P = `/p/${PROJECT}`;

function pages(): [string, string, (page: Page) => Promise<void>][] {
  const s = seeded();
  const nightly = s.launches['nightly 3'] ?? '';
  return [
    ['projects', '/', async (p) => {
      await expect(p.getByTestId('project-card').first()).toBeVisible();
    }],
    ['launches', P, async (p) => {
      await expect(p.getByTestId('launch-row').first()).toBeVisible();
    }],
    ['launch', `${P}/launches/${nightly}`, async (p) => {
      await expect(p.getByTestId('explorer-test').first()).toBeVisible();
    }],
    ['launch-explorer-test', `${P}/launches/${nightly}?test=${s.failedResultId}`, async (p) => {
      await expect(p.getByTestId('explorer-detail').getByTestId('step-tree').or(p.getByTestId('explorer-detail').getByText('No steps were reported.'))).toBeVisible();
    }],
    ['launch-explorer-playwright', `${P}/launches/${s.launches['playwright 1'] ?? ''}?view=suites`, async (p) => {
      await p.getByTestId('explorer-expand-all').click();
      await expect(p.getByTestId('explorer-test').first()).toBeVisible();
    }],
    ['launch-results', `${P}/launches/${nightly}?tab=results`, async (p) => {
      await expect(p.getByTestId('result-row').first()).toBeVisible();
    }],
    [
      'launch-failures',
      `${P}/launches/${nightly}`,
      async (p) => {
        await p.getByRole('tab', { name: /Failures/ }).click();
        await expect(p.getByTestId('cluster').first()).toBeVisible();
      },
    ],
    [
      'live-logs',
      `${P}/launches/${s.runningLaunchId}`,
      async (p) => {
        await p.getByTestId('tab-logs').click();
        await expect(p.getByTestId('log-line').first()).toBeVisible();
      },
    ],
    ['result', `${P}/results/${s.failedResultId}`, async (p) => {
      await expect(p.getByTestId('defect-card')).toBeVisible();
    }],
    ['test-case', `${P}/test-cases/${s.testCaseId}`, async (p) => {
      await expect(p.getByTestId('history-row').first()).toBeVisible();
    }],
    ['rules', `${P}/rules`, async (p) => {
      await expect(p.getByTestId('rule-new')).toBeVisible();
    }],
    ['gates', `${P}/gates`, async (p) => {
      await expect(p.getByTestId('gate-add')).toBeVisible();
    }],
    ['notifications', `${P}/notifications`, async (p) => {
      await expect(p.getByTestId('notification-add')).toBeVisible();
    }],
    ['settings', `${P}/settings`, async (p) => {
      await expect(p.getByTestId('flaky-history-depth')).toBeVisible();
    }],
    ['dashboard', `${P}/dashboard`, async (p) => {
      await expect(p.getByTestId('chart-pass-rate')).toBeVisible();
    }],
    ['flaky', `${P}/flaky`, async (p) => {
      await expect(p.locator('main')).toBeVisible();
    }],
    ['compare', `${P}/compare?a=${s.launches['nightly 2'] ?? ''}&b=${nightly}`, async (p) => {
      await expect(p.getByTestId('compare-summary')).toBeVisible();
    }],
    ['jobs', '/admin/jobs', async (p) => {
      await expect(p.getByTestId('job-row').first()).toBeVisible();
    }],
  ];
}

for (const [mode, viewport, scheme] of [
  ['dark', { width: 1400, height: 900 }, 'dark'],
  ['light', { width: 1400, height: 900 }, 'light'],
  ['phone', { width: 393, height: 851 }, 'dark'],
] as const) {
  test(`screenshots: ${mode}`, async ({ page }) => {
    test.setTimeout(300_000);
    const dir = join(DIR ?? '', mode);
    mkdirSync(dir, { recursive: true });
    await page.setViewportSize(viewport);
    await page.addInitScript((c) => window.localStorage.setItem('franshub-color-scheme', c), scheme);
    await page.goto('/sign-in');
    await expect(page.getByTestId('sign-in')).toBeVisible();
    await page.screenshot({ path: join(dir, 'sign-in.png') });
    await signInWithToken(page, ADMIN_TOKEN);
    for (const [name, path, ready] of pages()) {
      await page.goto(path);
      await ready(page);
      await page.waitForTimeout(400);
      await page.screenshot({ path: join(dir, `${name}.png`), fullPage: mode !== 'phone' });
    }
  });
}
