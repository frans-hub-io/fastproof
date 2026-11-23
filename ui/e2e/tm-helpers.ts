import AxeBuilder from '@axe-core/playwright';
import { expect, type Page } from '@playwright/test';

export const MOD = process.platform === 'darwin' ? 'Meta' : 'Control';

/** Opens a case by searching the list for its exact name. */
export async function openCase(page: Page, name: string): Promise<number> {
  await page.goto('/p/web-shop/cases');
  await page.getByTestId('case-search').fill(name);
  const link = page.getByTestId('case-link').filter({ hasText: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`) }).first();
  await expect(link).toBeVisible();
  await link.click();
  await expect(page.getByTestId('case-title')).toContainText(name);
  const m = /\/cases\/(\d+)/.exec(page.url());
  return Number(m?.[1] ?? 0);
}

/** The launch that carries the seeded manual run (#38). */
export async function openManualLaunch(page: Page): Promise<string> {
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: /^#38 / }).first().click();
  await expect(page.getByTestId('tab-manual')).toBeVisible();
  const m = /\/launches\/([^/?#]+)/.exec(page.url());
  return m?.[1] ?? '';
}

export async function expectNoAxeViolations(page: Page, label: string): Promise<void> {
  // Modals and drawers fade in; contrast is only meaningful once transitions have finished.
  await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== 'running'));
  const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']).analyze();
  const summary = result.violations.map((v) => `${v.id} (${v.impact ?? ''}): ${v.nodes.slice(0, 3).map((n) => n.target.join(' ')).join(' | ')}`);
  expect(summary, `axe on ${label}`).toEqual([]);
}

export async function noHorizontalScroll(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);
}
