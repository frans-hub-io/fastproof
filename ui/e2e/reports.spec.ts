import { expect, test } from '@playwright/test';
import { openRichLaunch, openRichResult, trackForeignRequests } from './helpers';

test('projects → launches → launch → result', async ({ page, baseURL }) => {
  const foreign = trackForeignRequests(page, baseURL ?? '');
  await page.goto('/');
  await expect(page.getByTestId('project-card')).toHaveCount(3);
  await page.getByRole('link', { name: 'Project Web shop' }).click();
  await expect(page).toHaveURL(/\/p\/web-shop$/);
  await expect(page.getByTestId('launch-row')).toHaveCount(30);
  await page.getByTestId('load-more-sentinel').scrollIntoViewIfNeeded();
  await expect(page.getByTestId('launch-row')).toHaveCount(41);

  await page.getByRole('link', { name: '#41 web-shop nightly' }).click();
  await expect(page.getByTestId('stat-total')).toContainText('200');
  await expect(page.getByTestId('upload-error')).toContainText('limit_exceeded');
  await expect(page.getByTestId('ci-link')).toHaveAttribute('target', '_blank');

  await page.locator('label').filter({ hasText: /^Failed \d+$/ }).click();
  await expect(page).toHaveURL(/status=failed/);
  await expect.poll(() => page.getByTestId('result-row').count()).toBeLessThan(30);
  await expect(page.getByTestId('result-row').first()).toContainText('Failed');

  await page.getByRole('tab', { name: /Failures/ }).click();
  await expect(page.getByTestId('failure-group').first()).toBeVisible();

  await page.getByRole('tab', { name: 'Tree' }).click();
  await page.getByTestId('tree-node').filter({ hasText: 'checkout.spec.ts' }).getByRole('button', { name: /checkout\.spec\.ts/ }).last().click();
  await expect(page.getByTestId('tree-selection')).toContainText('checkout.spec.ts');

  await page.getByTestId('result-row').filter({ hasText: 'pays with saved card' }).click();
  await expect(page.getByRole('tab', { name: 'Attempt 2' })).toHaveAttribute('aria-selected', 'true');
  await expect(page.getByTestId('history-strip').locator('[data-current="true"]')).toHaveCount(1);
  await page.getByRole('tab', { name: 'Attempt 1' }).click();
  await expect(page.getByTestId('attempt-1')).toBeVisible();
  expect(foreign).toEqual([]);
});

test('filters launches by branch from the URL', async ({ page }) => {
  await page.goto('/p/web-shop?branch=feature%2Fcheckout-v2');
  const rows = page.getByTestId('launch-row');
  await expect(rows.first()).toBeVisible();
  for (const text of await rows.allInnerTexts()) expect(text).toContain('feature/checkout-v2');
});

test('j and k move through results and Enter opens one', async ({ page }) => {
  await openRichLaunch(page);
  await page.getByTestId('result-row').first().waitFor();
  await page.locator('body').click({ position: { x: 5, y: 300 } });
  await page.keyboard.press('j');
  await page.keyboard.press('j');
  await expect(page.locator('[data-testid="result-row"][data-selected="true"]')).toHaveAttribute('data-list-index', '1');
  await page.keyboard.press('k');
  await expect(page.locator('[data-testid="result-row"][data-selected="true"]')).toHaveAttribute('data-list-index', '0');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/\/p\/web-shop\/results\//);
});

test('steps, error diff and logs on the result page', async ({ page }) => {
  await openRichResult(page);
  await expect(page.getByTestId('step-tree')).toBeVisible();
  await expect(page.locator('[data-testid="step"][data-status="failed"]').first()).toBeVisible();
  await expect(page.getByTestId('diff-view').first()).toBeVisible();
  await page.getByRole('tab', { name: 'Logs' }).click();
  const stdout = page.getByTestId('stdout');
  await expect(stdout).toContainText('BAD GATEWAY');
  const colored = stdout.locator('span[style*="--fp-ansi"]');
  expect(await colored.count()).toBeGreaterThan(3);
  await page.getByTestId('test-case-link').click();
  await expect(page.getByTestId('history-row')).toHaveCount(42);
  await expect(page.getByTestId('status-timeline').locator('button')).toHaveCount(42);
});

test('attachment viewers', async ({ page, context }) => {
  await openRichResult(page);
  await page.getByRole('tab', { name: /Attachments/ }).click();
  const item = (name: string | RegExp) => page.getByTestId('attachment-item').filter({ hasText: name });
  const viewer = page.getByTestId('attachment-viewer');

  await item('screenshot.png').click();
  await expect(viewer.getByTestId('image-viewer')).toBeVisible();
  expect(await viewer.getByTestId('image-viewer').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(480);
  await page.keyboard.press('Escape');

  await item('checkout-summary (expected').click();
  const box = viewer.getByTestId('compare-slider-box');
  await expect(box).toBeVisible();
  const bb = await box.boundingBox();
  if (!bb) throw new Error('no slider box');
  await page.mouse.click(bb.x + bb.width * 0.9, bb.y + bb.height / 2);
  await expect(viewer.getByRole('slider')).toHaveAttribute('aria-valuenow', /^(8\d|9\d)/);
  await viewer.getByText('Side by side').click();
  await expect(viewer.getByRole('img', { name: /diff/ })).toBeVisible();
  await page.keyboard.press('Escape');

  await item('Screenshot diff').click();
  await expect(viewer.getByTestId('image-compare')).toBeVisible();
  await expect(viewer.getByRole('img', { name: 'Screenshot diff: actual' })).toHaveAttribute('src', /^data:image\/png;base64,/);
  await page.keyboard.press('Escape');

  await item('video.webm').click();
  const video = viewer.getByTestId('video-viewer');
  await expect(video).toBeVisible();
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.readyState)).toBeGreaterThan(0);
  await page.keyboard.press('Escape');

  await item('POST /payments').click();
  await expect(viewer.getByTestId('http-exchange')).toContainText('502 Bad Gateway');
  await viewer.getByRole('tab', { name: 'Request' }).click();
  await expect(viewer.getByText('redacted').first()).toBeVisible();
  await page.keyboard.press('Escape');

  await item('order.json').click();
  await expect(viewer.getByTestId('json-tree')).toContainText('"customer"');
  await page.keyboard.press('Escape');

  await item('browser-console.log').click();
  await expect(viewer.getByTestId('text-viewer')).toContainText('Payment gateway answered');
  await page.keyboard.press('Escape');

  const trace = item('trace.zip');
  const href = await trace.getAttribute('href');
  expect(href).toMatch(/^\/trace\/index\.html\?trace=/);
  expect(decodeURIComponent(href?.split('trace=')[1] ?? '')).toMatch(/\/api\/v1\/projects\/web-shop\/attachments\/[0-9a-f-]+\/content$/);
  await expect(trace).toHaveAttribute('target', '_blank');

  for (const name of ['report.html', 'badge.svg']) {
    const link = item(name);
    await expect(link).toHaveAttribute('target', '_blank');
    await expect(link).toHaveAttribute('href', /\/attachments\/[0-9a-f-]+\/content$/);
  }
  const pagePromise = context.waitForEvent('page');
  await item('report.html').click();
  const popup = await pagePromise;
  await popup.close();
  await expect(page.locator('iframe')).toHaveCount(0);
});

test('command palette jumps to projects and recent launches', async ({ page }) => {
  await openRichLaunch(page);
  await page.goto('/');
  await expect(page.getByTestId('project-card').first()).toBeVisible();
  await page.keyboard.press('Control+k');
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('input')).toBeFocused();
  await expect(dialog.getByText('Recent launches')).toBeVisible();
  await page.keyboard.type('#41');
  await expect(dialog.getByText('#41 web-shop nightly')).toBeVisible();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: '#41 web-shop nightly' })).toBeVisible();
  await expect(dialog).toHaveCount(0);
  await page.keyboard.press('Control+k');
  await expect(page.getByRole('dialog').locator('input')).toBeFocused();
  await page.keyboard.type('Payments');
  await expect(page.getByRole('dialog').getByText('Payments API')).toBeVisible();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/\/p\/payments-api$/);
});

test('running launch updates live', async ({ page }) => {
  await page.goto('/p/web-shop');
  const progress = page.getByTestId('launch-progress');
  await expect(progress).toContainText('Running');
  await expect(page.getByTestId('live-state')).toHaveText('Live');
  const first = await progress.innerText();
  await expect.poll(async () => progress.innerText(), { timeout: 20_000 }).not.toBe(first);
});
