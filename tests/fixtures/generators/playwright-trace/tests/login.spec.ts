import { test } from '@playwright/test';
test('login', async ({ page }) => {
  await page.setContent('<label for=p>Password</label><input id=p type=password><input id=u>');
  await page.getByLabel('Password').fill('hunter2-SECRET');
  await page.locator('#u').pressSequentially('typed-SECRET2');
  await page.locator('#u').press('Enter');
});
