import { defineConfig, devices } from '@playwright/test';
import { join } from 'node:path';
import { BASE_URL, BIN, ENABLED, PORT, WORK_DIR, adminTokensFile, secretsKeyFile } from './e2e-real/env';

const external = !!process.env.FASTPROOF_E2E_URL;
const database = process.env.FASTPROOF_E2E_DATABASE_URL;

if (ENABLED && !external && !database) throw new Error('set FASTPROOF_E2E_DATABASE_URL (an empty database) or FASTPROOF_E2E_URL (a running server)');

export default defineConfig({
  testDir: './e2e-real',
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  globalSetup: './e2e-real/global-setup.ts',
  use: {
    baseURL: BASE_URL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer:
    ENABLED && !external
      ? {
          command: `"${BIN}" serve`,
          url: `${BASE_URL}/healthz`,
          reuseExistingServer: false,
          timeout: 180_000,
          stdout: 'ignore',
          stderr: 'pipe',
          env: {
            FASTPROOF_DATABASE_URL: database ?? '',
            FASTPROOF_ADMIN_TOKENS_FILE: adminTokensFile(),
            FASTPROOF_SECRETS_KEY_FILE: secretsKeyFile(),
            FASTPROOF_BLOB_DIR: join(WORK_DIR, 'blobs'),
            FASTPROOF_SPOOL_DIR: join(WORK_DIR, 'spool'),
            FASTPROOF_LISTEN: `127.0.0.1:${String(PORT)}`,
            FASTPROOF_OPS_LISTEN: `127.0.0.1:${String(PORT + 1)}`,
            FASTPROOF_LOG_FORMAT: 'text',
            RUST_LOG: 'warn',
          },
        }
      : undefined,
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], viewport: { width: 1400, height: 900 } } }],
});
