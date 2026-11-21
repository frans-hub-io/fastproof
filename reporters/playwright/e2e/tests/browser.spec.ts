import { expect, test } from '../../dist/fixture.js';

test('renders a page', async ({ page }) => {
  await page.setContent('<main><h1>FastProof</h1><p>reporter e2e</p></main>');
  await test.step('check heading', async () => {
    await expect(page.getByRole('heading')).toHaveText('FastProof');
  });
});
