import { defineConfig, devices } from '@playwright/test';

const port = Number(process.env.FASTPROOF_E2E_PORT ?? '5181');
const external = process.env.FASTPROOF_E2E_URL;
const baseURL = external ?? `http://127.0.0.1:${String(port)}`;

export default defineConfig({
  testDir: './e2e',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer: external
    ? undefined
    : {
        command: `npx vite --port ${String(port)} --strictPort`,
        url: baseURL,
        reuseExistingServer: false,
        timeout: 120_000,
        env: { VITE_API: 'mock' },
      },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'], viewport: { width: 1400, height: 900 } }, testIgnore: /mobile\.spec\.ts/ },
    { name: 'mobile', use: { ...devices['Pixel 7'], browserName: 'chromium' }, testMatch: /mobile\.spec\.ts/ },
  ],
});
