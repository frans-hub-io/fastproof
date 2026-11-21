import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  outputDir: "./test-results",
  retries: 2,
  workers: 1,
  fullyParallel: false,
  reporter: [
    ["list"],
    [
      "allure-playwright",
      {
        resultsDir: "allure-results",
        detail: true,
        suiteTitle: true,
        environmentInfo: { app: "fastproof-samples", stage: "ci" },
        categories: [
          { name: "Assertion failures", matchedStatuses: ["failed"] },
          { name: "Unexpected errors", matchedStatuses: ["broken"] },
        ],
      },
    ],
    ["json", { outputFile: "results.json" }],
    ["junit", { outputFile: "junit.xml" }],
    [
      "playwright-ctrf-json-reporter",
      {
        outputDir: "ctrf",
        outputFile: "ctrf-report.json",
        annotations: true,
        testType: "api",
        appName: "fastproof-samples",
        appVersion: "1.0.0",
        testEnvironment: "staging",
        buildName: "fastproof-samples",
        buildNumber: 42,
        buildUrl: "https://ci.example.com/builds/42",
        repositoryName: "fastproof-samples",
        repositoryUrl: "https://git.example.com/example/fastproof-samples",
        branchName: "main",
        commit: "0123456789abcdef0123456789abcdef01234567",
      },
    ],
  ],
  projects: [
    { name: "staging", use: { baseURL: "https://staging.example.com" } },
    { name: "production", use: { baseURL: "https://www.example.com" }, grep: /@smoke/ },
  ],
});
