import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';
import path from 'node:path';

const SEEDED = 'd0000000-0000-4000-8000-000000000001';
const dir = process.env.FASTPROOF_DASH_SHOTS;

test('at phone width widgets follow each other without gaps', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 780 });
  await page.goto(`/p/web-shop/dashboards/${SEEDED}`);
  await expect(page.getByTestId('widget-w2').locator('svg.recharts-surface').first()).toBeVisible();
  const boxes = await page.getByTestId('dashboard-grid').locator('section').evaluateAll((els) => els.map((e) => e.getBoundingClientRect()).map((r) => ({ top: r.top, bottom: r.bottom })));
  expect(boxes.length).toBeGreaterThan(2);
  for (let i = 1; i < boxes.length; i++) {
    const [prev, next] = [boxes[i - 1], boxes[i]];
    if (prev && next) expect(next.top - prev.bottom).toBeLessThanOrEqual(16);
  }
});

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

async function newBoard(page: Page) {
  await page.goto('/p/web-shop/dashboards');
  await page.getByRole('button', { name: 'New dashboard' }).click();
  await page.getByLabel('Name').fill('Project widgets');
  await page.getByRole('button', { name: 'Empty dashboard' }).click();
  await expect(page).toHaveURL(/edit=1/);
  for (const [item, id] of [['Cumulative trend', 'w1'], ['Unique bugs', 'w2'], ['Activity stream', 'w3']]) {
    await page.getByRole('button', { name: 'Add widget' }).click();
    await page.getByRole('menuitem', { name: item }).click();
    await expect(page.getByTestId(`widget-${id}`)).toBeVisible();
  }
  await page.getByRole('button', { name: 'Settings of Cumulative trend' }).click();
  await page.getByLabel('Launch attribute').fill('build');
  await page.getByRole('button', { name: 'Apply' }).click();
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
}

test('the cumulative trend, unique bugs and activity widgets show their data', async ({ page }) => {
  await newBoard(page);
  await expect(page.getByTestId('widget-w1').locator('svg.recharts-surface').first()).toBeVisible();
  await expect(page.getByTestId('widget-w1').locator('table caption')).toHaveCount(1);
  const bugs = page.getByTestId('bugs-table');
  await expect(bugs.getByRole('link', { name: 'SHOP-1234' })).toHaveAttribute('href', /^https:\/\//);
  await expect(bugs.getByRole('link', { name: 'SHOP-1180' })).toHaveCount(0);
  await expect(page.getByTestId('activity-list').getByRole('listitem').first()).toBeVisible();
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
      await newBoard(page);
      await expect(page.getByTestId('widget-w1').locator('svg.recharts-surface').first()).toBeVisible();
      await shoot(page, 'dashboard-project-widgets', width, scheme);
    });
  }
}
