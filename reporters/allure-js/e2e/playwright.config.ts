import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  outputDir: process.env.E2E_OUTPUT_DIR ?? './test-results',
  retries: 1,
  workers: 2,
  reporter: [
    ['list'],
    [
      require.resolve('../dist/playwright.js'),
      {
        resultsDir: process.env.E2E_RESULTS_DIR ?? 'allure-results',
        fastproof: {
          launchName: 'e2e',
          flushTimeoutMs: 20000,
          secrets: { patterns: ['card-\\d{4}'] },
          attributes: { suite: 'e2e' },
        },
      },
    ],
  ],
});
