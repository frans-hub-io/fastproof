import { expect, test, type Page } from '@playwright/test';
import { openRichResult } from './helpers';

const SHOTS = process.env.FASTPROOF_SHOTS_DIR;

async function openVideo(page: Page): Promise<void> {
  await openRichResult(page);
  await page.getByTestId('tab-result-video').click();
  await expect(page.getByTestId('video-player')).toBeVisible();
  await page.waitForFunction(() => {
    const v = document.querySelector<HTMLVideoElement>('[data-testid="video-viewer"]');
    return !!v && v.readyState >= 1;
  });
}

const state = (page: Page) =>
  page.evaluate(() => {
    const v = document.querySelector<HTMLVideoElement>('[data-testid="video-viewer"]');
    if (!v) return null;
    return { t: v.currentTime, d: v.duration, paused: v.paused, src: v.currentSrc, seekEnd: v.seekable.length ? v.seekable.end(v.seekable.length - 1) : 0 };
  });

test('plays the recording from a short-lived address', async ({ page }) => {
  await openVideo(page);
  const s0 = await state(page);
  expect(s0?.src).toMatch(/\/api\/v1\/media\/m\d+-/);
  await page.getByTestId('video-play').click();
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(0.5);
  await page.getByTestId('video-play').click();
  expect((await state(page))?.paused).toBe(true);
});

test('seeks to 80 percent within a second', async ({ page }) => {
  await openVideo(page);
  const before = await state(page);
  const total = Number.isFinite(before?.d) ? (before?.d ?? 0) : (before?.seekEnd ?? 0);
  expect(total).toBeGreaterThan(5);
  const seek = page.getByTestId('video-seek');
  const box = await seek.boundingBox();
  if (!box) throw new Error('no seek bar');
  const started = Date.now();
  await page.mouse.click(box.x + box.width * 0.8, box.y + box.height / 2);
  await expect.poll(async () => (await state(page))?.t ?? 0, { timeout: 1000 }).toBeGreaterThan(total * 0.78);
  expect(Date.now() - started).toBeLessThan(1500);
  expect((await state(page))?.t ?? 0).toBeLessThan(total * 0.82);
});

test('a step click seeks and the current step follows playback', async ({ page }) => {
  await openVideo(page);
  const rows = page.getByTestId('video-step');
  const target = rows.filter({ hasText: 'Go to checkout' });
  await target.click();
  const t = (await state(page))?.t ?? 0;
  expect(t).toBeGreaterThan(1);
  await expect(target).toHaveAttribute('aria-current', 'step');
  await rows.filter({ hasText: 'Open the product page' }).click();
  await expect(rows.filter({ hasText: 'Open the product page' })).toHaveAttribute('aria-current', 'step');
  await page.getByTestId('video-play').click();
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(2);
  const current = page.locator('[data-testid="video-step"][aria-current="step"]');
  await expect(current).toHaveCount(1);
  await expect.poll(async () => (await current.textContent()) ?? '', { timeout: 15_000 }).not.toContain('Open the product page');
  await page.getByTestId('video-play').click();
});

test('shows the steps on the timeline, the failure, and keeps the nudge in the address', async ({ page }) => {
  await openVideo(page);
  expect(await page.getByTestId('video-marker').count()).toBeGreaterThan(4);
  await expect(page.getByTestId('video-failure')).toHaveCount(1);
  await expect(page.getByTestId('video-approximate')).toBeVisible();
  await page.getByTestId('video-offset-plus').click();
  await page.getByTestId('video-offset-plus').click();
  await expect(page.getByTestId('video-offset')).toHaveText('+1.0 s');
  expect(page.url()).toMatch(/vo=[0-9a-f-]+%3A1000/);
  await page.reload();
  await expect(page.getByTestId('video-offset')).toHaveText('+1.0 s');
});

test('takes a new address after the old one expires and keeps the place', async ({ page }) => {
  await openVideo(page);
  const first = (await state(page))?.src ?? '';
  const total = Number.isFinite((await state(page))?.d) ? ((await state(page))?.d ?? 0) : ((await state(page))?.seekEnd ?? 0);
  const box = await page.getByTestId('video-seek').boundingBox();
  if (!box) throw new Error('no seek bar');
  await page.mouse.click(box.x + box.width * 0.4, box.y + box.height / 2);
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(total * 0.38);
  const at = (await state(page))?.t ?? 0;
  const expired = await page.evaluate(() => (window as unknown as { __fpExpireMediaUrls: () => number }).__fpExpireMediaUrls());
  expect(expired).toBeGreaterThan(0);
  await page.evaluate(() => {
    const v = document.querySelector<HTMLVideoElement>('[data-testid="video-viewer"]');
    if (v) v.src = `${v.currentSrc}?again=1`;
  });
  await expect.poll(async () => (await state(page))?.src ?? '', { timeout: 10_000 }).not.toBe(first);
  await expect.poll(async () => (await state(page))?.t ?? 0, { timeout: 10_000 }).toBeGreaterThan(at - 0.5);
  expect((await state(page))?.t ?? 0).toBeLessThan(at + 1.5);
  await expect(page.getByTestId('video-problem')).toHaveCount(0);
  await page.getByTestId('video-play').click();
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(at + 0.5);
});

test('keyboard shortcuts work with focus in the player', async ({ page }) => {
  await openVideo(page);
  await page.getByTestId('video-player').focus();
  await page.keyboard.press('ArrowRight');
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(4.5);
  await page.keyboard.press('Home');
  await expect.poll(async () => (await state(page))?.t ?? 1).toBeLessThan(0.1);
  await page.keyboard.press('.');
  await expect.poll(async () => (await state(page))?.t ?? 0).toBeGreaterThan(0.03);
  await expect(page.getByTestId('video-live')).toContainText(/Position/);
});

for (const scheme of ['light', 'dark'] as const) {
  test(`screenshots on desktop, ${scheme}`, async ({ page }) => {
    test.skip(!SHOTS, 'set FASTPROOF_SHOTS_DIR to take screenshots');
    await page.emulateMedia({ colorScheme: scheme });
    await page.addInitScript((s) => window.localStorage.setItem('mantine-color-scheme-value', s), scheme);
    await openVideo(page);
    await page.getByTestId('video-step').filter({ hasText: 'Submit the order' }).click();
    const box = await page.getByTestId('video-seek').boundingBox();
    if (box) await page.mouse.move(box.x + box.width * 0.45, box.y + box.height / 2);
    await page.waitForTimeout(800);
    await page.screenshot({ path: `${SHOTS ?? ''}/desktop-${scheme}.png` });
  });
}

test.describe('narrow screen', () => {
  test.use({ viewport: { width: 360, height: 740 } });

  for (const scheme of ['light', 'dark'] as const) {
    test(`fits 360 px without sideways scrolling, ${scheme}`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.addInitScript((s) => window.localStorage.setItem('mantine-color-scheme-value', s), scheme);
      await openVideo(page);
      const overflow = await page.evaluate(() => ({ doc: document.documentElement.scrollWidth, win: window.innerWidth }));
      expect(overflow.doc).toBeLessThanOrEqual(overflow.win);
      const player = await page.getByTestId('video-player').boundingBox();
      expect(player?.width ?? 999).toBeLessThanOrEqual(360);
      await expect(page.getByTestId('video-play')).toBeVisible();
      await expect(page.getByRole('button', { name: 'Full screen' })).toBeVisible();
      if (SHOTS) {
        await page.screenshot({ path: `${SHOTS}/narrow-${scheme}.png`, fullPage: true });
      }
    });
  }
});
