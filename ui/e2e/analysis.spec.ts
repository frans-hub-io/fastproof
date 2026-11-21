import { expect, test, type Page } from '@playwright/test';
import { openRichLaunch, openRichResult } from './helpers';

async function openRunningLaunch(page: Page): Promise<void> {
  await page.goto('/p/web-shop');
  await page.getByRole('link', { name: '#42 web-shop nightly' }).click();
  await expect(page.getByRole('heading', { name: '#42 web-shop nightly' })).toBeVisible();
}

async function pickOption(page: Page, testId: string, option: string | RegExp): Promise<void> {
  await page.getByTestId(testId).click();
  await page.getByRole('option', { name: option }).click();
}

test('donut shows the pass rate without skipped results', async ({ page }) => {
  await openRichLaunch(page);
  const rate = page.getByTestId('pass-rate');
  await expect(rate).toContainText('pass rate');
  await rate.hover();
  await expect(page.getByRole('tooltip')).toContainText('passed / (total − skipped)');
});

test('triage a cluster, then a single result with a mute', async ({ page }) => {
  await openRichLaunch(page);
  await expect(page.getByTestId('gate-badge')).toBeVisible();
  await page.getByRole('tab', { name: /Failures/ }).click();
  const cluster = page.getByTestId('cluster').first();
  await expect(cluster).toBeVisible();
  const count = Number(await cluster.getByTestId('cluster-count').innerText());
  await cluster.getByRole('button').first().click();
  await expect(cluster.getByTestId('cluster-results').locator('a')).toHaveCount(count);

  await cluster.getByTestId('triage-cluster').click();
  const form = page.getByTestId('triage-form');
  await pickOption(page, 'triage-type', 'Product bug');
  await form.getByLabel('Comment').fill('Checkout summary regressed after the redesign');
  await page.getByTestId('triage-issue').fill('SHOP-42');
  await expect(page.getByTestId('triage-issue-preview')).toHaveAttribute('href', 'https://issues.example.com/browse/SHOP-42');
  await page.getByTestId('triage-save').click();
  await expect(page.getByText(`${String(count)} result${count === 1 ? '' : 's'} updated`)).toBeVisible();
  await expect(cluster.getByTestId('defect-chip').first()).toHaveText('Product bug');

  await cluster.getByTestId('cluster-results').locator('a').first().click();
  await expect(page.getByTestId('defect-card').getByTestId('defect-chip')).toHaveText('Product bug');
  await expect(page.getByTestId('defect-issue')).toHaveAttribute('href', 'https://issues.example.com/browse/SHOP-42');
  await expect(page.getByTestId('defect-comment')).toHaveText('Checkout summary regressed after the redesign');

  await page.getByTestId('triage-open').click();
  await page.getByTestId('triage-mute').click();
  await expect(page.getByTestId('triage-save')).toBeDisabled();
  await page.getByTestId('triage-reason').fill('Known visual drift, fix in SHOP-42');
  await page.getByTestId('triage-save').click();
  await expect(page.getByTestId('muted-badge')).toContainText('muted until');
});

test('a viewer sees triage results but no triage buttons', async ({ page }) => {
  await page.goto('/?mockRole=viewer');
  await openRichLaunch(page);
  await page.getByRole('tab', { name: /Failures/ }).click();
  const cluster = page.getByTestId('cluster').first();
  await cluster.getByRole('button').first().click();
  await expect(cluster.getByTestId('cluster-results')).toBeVisible();
  await expect(page.getByTestId('triage-cluster')).toHaveCount(0);
  await expect(page.getByTestId('triage-result')).toHaveCount(0);
  await expect(page.getByTestId('open-test-plan')).toHaveCount(0);
  await page.goto('/p/web-shop/rules');
  await expect(page.getByTestId('rule-row').first()).toBeVisible();
  await expect(page.getByTestId('rule-new')).toHaveCount(0);
  await expect(page.getByTestId('rule-edit')).toHaveCount(0);
});

test('rules: dry run against a launch, create, apply retroactively', async ({ page }) => {
  await page.goto('/p/web-shop/rules');
  await expect(page.getByTestId('rule-row')).toHaveCount(3);
  await page.getByTestId('rule-new').click();
  const editor = page.getByTestId('rule-editor');
  await page.getByTestId('rule-name').fill('Continue button timeouts');
  await page.getByTestId('rule-message-regex').fill('Timeout (');
  await expect(editor.getByText(/Invalid regular expression|Unterminated group/)).toBeVisible();
  await expect(page.getByTestId('dry-run')).toBeDisabled();
  await page.getByTestId('rule-message-regex').fill('Timeout \\d+ms exceeded');
  await pickOption(page, 'rule-defect-type', 'Automation bug');
  await pickOption(page, 'dry-run-launch', /^#41 web-shop nightly/);
  await page.getByTestId('dry-run').click();
  const result = page.getByTestId('dry-run-result');
  await expect(result).toContainText(/\d+ results? would match/);
  const matched = Number(/(\d+) results? would match/.exec(await result.innerText())?.[1] ?? '0');
  expect(matched).toBeGreaterThan(0);
  await expect(page.getByTestId('dry-run-match').first()).toContainText('e2e/');
  await page.getByTestId('rule-save').click();
  await expect(page.getByTestId('rule-row')).toHaveCount(4);
  const row = page.getByTestId('rule-row').filter({ hasText: 'Continue button timeouts' });
  await row.getByTestId('rule-apply-open').click();
  await pickOption(page, 'apply-launch', /^#41 web-shop nightly/);
  await page.getByTestId('rule-apply-run').click();
  await expect(page.getByTestId('rule-apply-result')).toContainText(`${String(matched)} results updated`);
});

/** Pushes `batches` batches of lines into every open mock log stream; returns how many streams got them. */
async function pushLogs(page: Page, batches = 1): Promise<number> {
  return page.evaluate((n) => (window as unknown as { __fastproofMockLogs?: (n: number) => number }).__fastproofMockLogs?.(n) ?? 0, batches);
}

async function lineCount(page: Page): Promise<number> {
  return Number((await page.getByTestId('log-count').innerText()).replace(/\D/g, ''));
}

test('live console streams, pauses on scroll, shows gaps and filters by level', async ({ page }) => {
  await openRunningLaunch(page);
  await page.getByTestId('tab-logs').click();
  await expect(page.getByTestId('log-stream-state')).toHaveAttribute('data-state', 'live');
  await expect.poll(() => pushLogs(page, 0), { timeout: 30_000 }).toBeGreaterThan(0);
  const before = await lineCount(page);
  await pushLogs(page, 3);
  await expect.poll(() => lineCount(page), { timeout: 30_000 }).toBeGreaterThan(before);
  // Lines carry the reporter's attempt id; the console shows the test name instead.
  const who = page.locator('.fp-console-result').first();
  await expect(who).toBeVisible();
  await expect(who).not.toHaveText(/^pw-mock-/);

  const list = page.locator('.fp-console');
  await list.scrollIntoViewIfNeeded();
  await expect(async () => {
    const box = await list.boundingBox();
    if (!box) throw new Error('no console');
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, -2500);
    await expect(page.getByTestId('log-paused')).toBeVisible({ timeout: 2_000 });
  }).toPass({ timeout: 30_000 });
  await pushLogs(page, 3);
  await expect(page.getByTestId('log-resume')).toContainText(/new · Jump to latest/, { timeout: 30_000 });
  await page.getByTestId('log-resume').click();
  await expect(page.getByTestId('log-paused')).toHaveCount(0);

  await page.getByTestId('log-level-filter').click();
  await page.getByRole('option', { name: 'Errors only' }).click();
  await expect
    .poll(
      async () => {
        await pushLogs(page, 3);
        return page.locator('[data-testid="log-line"]').count();
      },
      { timeout: 30_000 },
    )
    .toBeGreaterThan(0);
  const levels = await page.locator('[data-testid="log-line"]').evaluateAll((rows) => rows.map((r) => r.getAttribute('data-level')));
  expect(new Set(levels)).toEqual(new Set(['error']));
});

test('live console shows a gap marker when the server drops lines', async ({ page }) => {
  await openRunningLaunch(page);
  await page.getByTestId('tab-logs').click();
  await expect(page.getByTestId('log-gaps')).toContainText(/\d+ lines skipped/, { timeout: 10_000 });
  await page.getByTestId('log-text-filter').fill('zzz-no-such-line');
  await page.getByTestId('log-text-filter').fill('');
  await expect(page.getByTestId('log-gap').first()).toContainText(/lines skipped/, { timeout: 15_000 });
});

test('stored logs of a finished result: 120k lines stay virtualized and can be downloaded', async ({ page }) => {
  await openRichResult(page);
  await page.getByTestId('tab-result-logs').click();
  await expect(page.getByTestId('log-count')).toHaveText('5,000 lines');
  await page.getByTestId('log-load-all').click();
  await expect(page.getByTestId('log-count')).toHaveText('120,000 lines', { timeout: 30_000 });
  expect(await page.locator('[data-testid="log-line"]').count()).toBeLessThan(300);
  await page.getByTestId('log-text-filter').fill('frame tick 1199');
  await expect(page.getByTestId('log-count')).toHaveText(/^[1-9]\d* lines?$/);
  const download = page.waitForEvent('download');
  await page.getByTestId('log-download').click();
  expect((await download).suggestedFilename()).toMatch(/pays_with_saved_card-attempt-2\.log/);
});

test('test plan from a launch filter', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('open-test-plan').click();
  await page.getByTestId('test-plan-create').click();
  const link = page.getByTestId('test-plan-download');
  await expect(link).toBeVisible();
  const href = await link.getAttribute('href');
  const body = await page.evaluate(async (url) => (await fetch(url)).json() as Promise<{ version: string; tests: { selector: string }[] }>, href ?? '');
  expect(body.version).toBe('1.0');
  expect(body.tests.length).toBeGreaterThan(0);
});
