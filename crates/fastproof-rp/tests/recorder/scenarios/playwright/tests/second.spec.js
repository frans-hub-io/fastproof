const { test, expect } = require('@playwright/test');

test.describe('profile', () => {
  test('renders name', async ({ page }) => {
    await page.setContent('<b>Ada</b>');
    await expect(page.locator('b')).toHaveText('Ada');
  });

  test('fails with assertion', async () => {
    expect(2 + 2).toBe(5);
  });
});
