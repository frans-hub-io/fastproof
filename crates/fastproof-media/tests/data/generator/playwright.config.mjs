import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  outputDir: "./test-results",
  workers: 1,
  reporter: [["json", { outputFile: "results.json" }]],
  use: {
    browserName: "chromium",
    viewport: { width: 320, height: 240 },
    video: { mode: "on", size: { width: 320, height: 240 } },
  },
});
