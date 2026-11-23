import { expect, test } from '@playwright/test';

test('edit the flaky history depth and save', async ({ page }) => {
  await page.goto('/p/web-shop/settings');
  const depth = page.getByTestId('flaky-history-depth');
  await expect(depth).toHaveValue('10');
  await depth.fill('1');
  await expect(page.getByText('Must be 2–100')).toBeVisible();
  await expect(page.getByTestId('settings-save')).toBeDisabled();
  await depth.fill('15');
  await page.getByTestId('settings-save').click();
  await expect(page.getByText('General settings saved')).toBeVisible();
  await page.getByRole('tab', { name: 'Defect types' }).click();
  await page.getByRole('tab', { name: 'General' }).click();
  await expect(page.getByTestId('flaky-history-depth')).toHaveValue('15');
});

test('issue tracker preview and defect types', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=integrations');
  await expect(page.getByTestId('tracker-preview').first()).toContainText('https://issues.example.com/browse/SHOP-123');
  await page.goto('/p/web-shop/settings?tab=defects');
  await expect(page.getByTestId('defect-type-row')).toHaveCount(7);
  await expect(page.getByRole('button', { name: 'Delete Product bug' })).toHaveCount(0);
  await page.getByTestId('defect-type-add').click();
  await page.getByLabel('Name', { exact: true }).fill('Flaky backend');
  await page.getByLabel('Short name').fill('fb');
  await page.getByTestId('defect-type-save').click();
  await expect(page.getByTestId('defect-type-row').filter({ hasText: 'Flaky backend' })).toContainText('FB');
});

test('add a gate with a rule', async ({ page }) => {
  await page.goto('/p/web-shop/gates');
  await expect(page.getByTestId('gate-card')).toHaveCount(2);
  await page.getByTestId('gate-add').click();
  await page.getByTestId('gate-name').fill('no broken tests');
  await page.getByTestId('gate-rule-metric').click();
  await page.getByRole('option', { name: 'Broken results' }).click();
  await page.getByTestId('gate-save').click();
  const card = page.getByTestId('gate-card').filter({ hasText: 'no broken tests' });
  await expect(card).toBeVisible();
  await expect(card.getByTestId('gate-rule')).toHaveText('• Broken results ≤ 0');
});

test('add a notification and see deliveries', async ({ page }) => {
  await page.goto('/p/web-shop/notifications');
  await expect(page.getByTestId('notification-card')).toHaveCount(3);
  await expect(page.getByTestId('delivery-row').first()).toBeVisible();
  await page.getByTestId('notification-add').click();
  await page.getByTestId('notification-target').fill('not a url');
  await expect(page.getByText('Must be an http(s) URL')).toBeVisible();
  await expect(page.getByTestId('notification-save')).toBeDisabled();
  await page.getByTestId('notification-target').fill('https://hooks.example.com/e2e');
  await page.getByLabel('Gate failed').check();
  await page.getByTestId('notification-save').click();
  const card = page.getByTestId('notification-card').filter({ hasText: 'https://hooks.example.com/e2e' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('Launch finished');
  await expect(card).toContainText('Gate failed');
});

test('add a Teams notification with routing filters and send a test', async ({ page }) => {
  await page.goto('/p/web-shop/notifications');
  await page.getByTestId('notification-add').click();
  await page.getByTestId('notification-kind').click();
  await page.getByRole('option', { name: 'Microsoft Teams' }).click();
  await expect(page.getByTestId('notification-target')).toHaveCount(0);
  await expect(page.getByText('Pick the secret that holds the workflow URL')).toBeVisible();
  await expect(page.getByTestId('notification-save')).toBeDisabled();
  await page.getByTestId('notification-target-secret').click();
  await expect(page.getByRole('option')).toHaveText(['teams-qa-channel']);
  await page.getByRole('option', { name: 'teams-qa-channel' }).click();
  await page.getByTestId('notification-launch-names').fill('nightly-*');
  await page.getByTestId('notification-launch-names').press('Enter');
  await page.getByTestId('notification-attributes').fill('trigger');
  await page.getByTestId('notification-attributes').press('Enter');
  await expect(page.getByText('Use key=value (the value may be a glob)')).toBeVisible();
  await expect(page.getByTestId('notification-save')).toBeDisabled();
  await page.getByTestId('notification-attributes').press('Backspace');
  await page.getByTestId('notification-attributes').fill('trigger=schedule');
  await page.getByTestId('notification-attributes').press('Enter');
  await page.getByLabel('Gate failed').check();
  await page.getByTestId('notification-save').click();
  const card = page.getByTestId('notification-card').filter({ hasText: 'Microsoft Teams' });
  await expect(card).toBeVisible();
  await expect(card).toContainText('URL in secret teams-qa-channel');
  await expect(card).toContainText('launches: nightly-*');
  await expect(card).toContainText('attributes: trigger=schedule');
  await card.getByTestId('notification-card-test').click();
  await expect(page.getByText('Test notification delivered')).toBeVisible();
  await expect(page.getByText('The receiver answered 202.')).toBeVisible();
});

test('secrets are write-only', async ({ page }) => {
  await page.goto('/p/web-shop/settings?tab=secrets');
  await expect(page.getByTestId('secret-row')).toHaveCount(6);
  await page.getByTestId('secret-name').fill('ops-token');
  await page.getByTestId('secret-value').fill('s3cr3t-value-e2e');
  await expect(page.getByTestId('secret-save')).toBeDisabled();
  await page.getByTestId('secret-purpose').click();
  await page.getByRole('option', { name: 'GitLab or GitHub token' }).click();
  await expect(page.getByText('Required for this purpose')).toBeVisible();
  await page.getByTestId('secret-purpose').click();
  await page.getByRole('option', { name: 'Webhook signing key or URL' }).click();
  await page.getByTestId('secret-save').click();
  await expect(page.getByTestId('secret-row').filter({ hasText: 'ops-token' })).toContainText('webhook');
  await expect(page.getByTestId('secret-value')).toHaveValue('');
  await expect(page.getByText('s3cr3t-value-e2e')).toHaveCount(0);
  expect(await page.content()).not.toContain('s3cr3t-value-e2e');
});

test('viewers see settings, gates and notifications read-only', async ({ page }) => {
  await page.goto('/p/web-shop/settings?mockRole=viewer');
  await expect(page.getByTestId('read-only')).toBeVisible();
  await expect(page.getByTestId('flaky-history-depth')).toBeDisabled();
  await expect(page.getByTestId('settings-save')).toHaveCount(0);
  await page.getByRole('tab', { name: 'Secrets' }).click();
  await expect(page.getByText('Secrets are managed by project admins')).toBeVisible();
  await page.goto('/p/web-shop/gates');
  await expect(page.getByTestId('gate-card').first()).toBeVisible();
  await expect(page.getByTestId('gate-add')).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^Edit / })).toHaveCount(0);
  await page.goto('/p/web-shop/notifications');
  await expect(page.getByTestId('notification-card').first()).toBeVisible();
  await expect(page.getByTestId('notification-add')).toHaveCount(0);
  await expect(page.getByTestId('notification-card-test')).toHaveCount(0);
  await page.evaluate(() => window.localStorage.removeItem('fastproof-mock-role'));
});
