const { test, expect } = require('@playwright/test');

test.describe('checkout', () => {
  test('passes with steps', async ({ page }) => {
    await test.step('open page', async () => {
      await page.setContent('<h1>hello</h1>');
    });
    await test.step('check heading', async () => {
      await test.step('inner step', async () => {
        await expect(page.locator('h1')).toHaveText('hello');
      });
    });
  });

  test('fails on first attempt then passes', async ({ page }, testInfo) => {
    await page.setContent('<p id="x">retry</p>');
    expect(testInfo.retry).toBeGreaterThan(0);
  });

  test('always fails', async ({ page }) => {
    await page.setContent('<p>broken</p>');
    await expect(page.locator('p')).toHaveText('fixed', { timeout: 500 });
  });

  test.describe('nested group', () => {
    test('nested pass', async () => {});
    test.skip('skipped by code', async () => {});
  });

  test('annotated @smoke', { tag: '@smoke', annotation: { type: 'issue', description: 'PROJ-1' } }, async () => {});

  test('with attachment and logs', async ({ page }, testInfo) => {
    console.log('stdout line');
    console.error('stderr line');
    await testInfo.attach('note.txt', { body: 'hello attachment', contentType: 'text/plain' });
    await testInfo.attach('data.json', { body: JSON.stringify({ a: 1 }), contentType: 'application/json' });
  });
});

for (const n of [1, 2]) {
  test(`parametrized ${n}`, async () => {
    expect(n).toBeGreaterThan(0);
  });
}
