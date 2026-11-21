import { defineConfig } from '@playwright/test';

import { testPlanGrep } from '../dist/index.js';

const browser = process.env.E2E_BROWSER === '1';

export default defineConfig({
  testDir: './tests',
  outputDir: process.env.E2E_OUTPUT_DIR ?? './test-results',
  retries: 1,
  workers: 2,
  grep: testPlanGrep(),
  reporter: [
    ['list'],
    [
      require.resolve('../dist/index.js'),
      {
        launchName: 'e2e',
        flushTimeoutMs: 20000,
        secrets: { patterns: ['card-\\d{4}'] },
        attributes: { suite: 'e2e' },
      },
    ],
  ],
  use: {
    fastproofPropagate: 'all',
  },
  projects: [
    { name: 'api', testMatch: /api\.spec\.ts/ },
    ...(browser
      ? [
          {
            name: 'browser',
            testMatch: /browser\.spec\.ts/,
            use: { browserName: 'chromium' as const, headless: true, screenshot: 'on' as const, trace: 'on' as const, video: 'on' as const },
          },
        ]
      : []),
  ],
});
