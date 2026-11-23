import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const SEEDED = 'd0000000-0000-4000-8000-000000000001';
const dir = process.env.FASTPROOF_DASH_SHOTS;

test('a dashboard renders its charts and lists, with data tables for screen readers', async ({ page }) => {
  await page.goto(`/p/web-shop/dashboards/${SEEDED}`);
  await expect(page.getByRole('heading', { name: 'Release health' })).toBeVisible();
  await expect(page.getByTestId('widget-w2').locator('svg.recharts-surface').first()).toBeVisible();
  await expect(page.getByTestId('widget-w3').locator('svg.recharts-surface').first()).toBeVisible();
  await expect(page.getByTestId('widget-w5').getByRole('link').first()).toBeVisible();
  await expect(page.getByTestId('widget-w2').locator('table caption')).toHaveCount(1);
});

test('edit mode adds, resizes and saves a widget', async ({ page }) => {
  await page.goto('/p/web-shop/dashboards');
  await page.getByRole('button', { name: 'New dashboard' }).click();
  await page.getByLabel('Name').fill('E2E board');
  await page.getByRole('button', { name: 'Empty dashboard' }).click();
  await expect(page).toHaveURL(/edit=1/);
  await page.getByRole('button', { name: 'Add widget' }).click();
  await page.getByRole('menuitem', { name: 'Overall statistics' }).click();
  await expect(page.getByTestId('widget-w1').locator('svg.recharts-surface').first()).toBeVisible();
  await page.getByRole('button', { name: 'Settings of Overall statistics' }).click();
  await page.getByLabel('Items').fill('5');
  await page.getByRole('button', { name: 'Apply' }).click();
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
  await expect(page.getByTestId('widget-w1')).toBeVisible();
});

async function shoot(page: Page, name: string, width: number, scheme: string) {
  await page.waitForTimeout(600);
  await page.screenshot({ path: path.join(dir ?? '.', `${name}-${String(width)}-${scheme}.png`), fullPage: true });
}

for (const scheme of ['dark', 'light']) {
  test(`no serious accessibility violations (${scheme})`, async ({ page }) => {
    await page.addInitScript((s) => {
      try {
        localStorage.setItem('franshub-color-scheme', s);
      } catch {
        /* ignore */
      }
    }, scheme);
    await page.goto(`/p/web-shop/dashboards/${SEEDED}`);
    await expect(page.getByTestId('widget-w2').locator('svg.recharts-surface').first()).toBeVisible();
    await page.waitForTimeout(800);
    const r = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa']).analyze();
    expect(r.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical').map((v) => `${v.id}: ${v.nodes[0]?.html.slice(0, 120) ?? ''}`)).toEqual([]);
  });

  for (const width of [1280, 360]) {
    test(`screenshots ${String(width)} ${scheme}`, async ({ page }) => {
      test.skip(!dir, 'set FASTPROOF_DASH_SHOTS to a directory to take the review screenshots');
      await page.setViewportSize({ width, height: width < 500 ? 780 : 900 });
      await page.addInitScript((s) => {
        try {
          localStorage.setItem('franshub-color-scheme', s);
        } catch {
          /* ignore */
        }
      }, scheme);
      await page.goto('/p/web-shop/dashboards');
      await expect(page.getByTestId('dashboard-card').first()).toBeVisible();
      await shoot(page, 'dashboards-list', width, scheme);
      await page.goto(`/p/web-shop/dashboards/${SEEDED}`);
      await expect(page.getByTestId('widget-w2').locator('svg.recharts-surface').first()).toBeVisible();
      await shoot(page, 'dashboard-view', width, scheme);
      await page.goto(`/p/web-shop/dashboards/${SEEDED}?edit=1`);
      await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
      await shoot(page, 'dashboard-edit', width, scheme);
    });
  }
}
