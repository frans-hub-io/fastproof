import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["tests/**/*.test.ts"],
    setupFiles: ["allure-vitest/setup"],
    retry: 2,
    fileParallelism: false,
    tags: [{ name: "smoke" }, { name: "payments" }, { name: "cart" }],
    reporters: [
      "default",
      [
        "allure-vitest/reporter",
        {
          resultsDir: "allure-results",
          environmentInfo: { app: "fastproof-samples", stage: "ci" },
          categories: [
            { name: "Assertion failures", matchedStatuses: ["failed"] },
            { name: "Unexpected errors", matchedStatuses: ["broken"] },
          ],
        },
      ],
      ["junit", { outputFile: "junit.xml", suiteName: "fastproof-samples", addFileAttribute: true }],
    ],
  },
});
