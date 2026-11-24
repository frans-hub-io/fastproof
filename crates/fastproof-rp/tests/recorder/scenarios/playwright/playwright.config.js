const { defineConfig } = require('@playwright/test');

const apiVersion = process.env.RP_API_VERSION || 'v2';
const rpConfig = {
  apiKey: 'fp_example_recording_token',
  endpoint: `http://127.0.0.1:8089/rp/api/${apiVersion}`,
  project: 'example-project',
  launch: 'playwright-recording',
  description: 'recorded run',
  attributes: [{ key: 'build', value: '42' }, { key: 'branch', value: 'main' }, { value: 'smoke' }],
  includeTestSteps: process.env.RP_STEPS !== '0',
  restClientConfig: { timeout: 30000 },
};
if (process.env.RP_LAUNCH_ID) rpConfig.launchId = process.env.RP_LAUNCH_ID;

module.exports = defineConfig({
  testDir: './tests',
  retries: 1,
  workers: 2,
  reporter: [['@reportportal/agent-js-playwright', rpConfig]],
  use: { screenshot: 'on', trace: 'on', launchOptions: {} },
});
