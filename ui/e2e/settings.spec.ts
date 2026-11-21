import { expect, test } from '@playwright/test';

test('create a token, see the secret once, revoke it', async ({ page }) => {
  await page.goto('/settings/tokens');
  await expect(page.getByTestId('token-row')).toHaveCount(2);
  await page.getByTestId('create-token').click();
  await page.getByPlaceholder('ci-web-shop').fill('ci-e2e');
  await page.getByPlaceholder('Pick projects').click();
  await page.getByRole('option', { name: 'web-shop' }).click();
  await page.keyboard.press('Escape');
  await page.getByTestId('create-token-submit').click();
  const secret = page.getByTestId('token-value');
  await expect(secret).toHaveText(/^fp_[0-9a-f]{64}$/);
  await page.keyboard.press('Escape');
  await expect(secret).toBeVisible();
  await page.getByRole('button', { name: 'I have stored it' }).click();
  await expect(secret).toHaveCount(0);
  const row = page.locator('[data-testid="token-row"][data-name="ci-e2e"]');
  await expect(row).toContainText('uploader');
  await expect(row).toContainText('web-shop');
  await expect(page.getByText(/fp_[0-9a-f]{64}/)).toHaveCount(0);
  await page.getByRole('button', { name: 'Revoke ci-e2e' }).click();
  await page.getByTestId('confirm-revoke').click();
  await expect(row).toHaveCount(0);
});

test('audit log for admins, denied for viewers', async ({ page }) => {
  await page.goto('/settings/audit');
  await expect(page.getByTestId('audit-row').first()).toBeVisible();
  await page.getByPlaceholder('Action, e.g. token.* or launch.delete').fill('token.*');
  await expect(page.getByTestId('audit-row').first()).toContainText('token.');
  for (const t of await page.getByTestId('audit-row').locator('td:nth-child(3)').allInnerTexts()) expect(t).toMatch(/^token\./);

  await page.goto('/settings/audit?mockRole=viewer');
  await expect(page.getByText('Administrators only')).toBeVisible();
  await expect(page.getByRole('link', { name: 'Audit log' })).toHaveCount(0);
  await page.goto('/?mockRole=admin');
});

test('theme is dark by default and the choice persists', async ({ page }) => {
  await page.goto('/');
  const html = page.locator('html');
  await expect(html).toHaveAttribute('data-mantine-color-scheme', 'dark');
  await page.getByTestId('theme-toggle').click();
  await expect(html).toHaveAttribute('data-mantine-color-scheme', 'light');
  await page.reload();
  await expect(page.getByTestId('project-card').first()).toBeVisible();
  await expect(html).toHaveAttribute('data-mantine-color-scheme', 'light');
  const bg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(bg).not.toBe('rgb(0, 0, 0)');
  await page.getByTestId('theme-toggle').click();
  await expect(html).toHaveAttribute('data-mantine-color-scheme', 'dark');
});
