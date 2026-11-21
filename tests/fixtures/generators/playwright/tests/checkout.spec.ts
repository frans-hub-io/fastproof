import { expect, test } from "@playwright/test";
import * as allure from "allure-js-commons";

test.describe("Checkout", () => {
  test("applies discount code", { tag: "@checkout" }, async () => {
    await allure.feature("Checkout");
    await test.step("apply SAVE10", async () => {
      expect(100 - 5).toBe(90);
    });
  });

  test.describe("Tax", () => {
    test.describe.configure({ retries: 0 });

    test("calculates tax", async () => {
      const order = JSON.parse("null") as { tax: number };
      expect(order.tax).toBe(20);
    });
  });

  test("pays via gateway", { tag: ["@smoke", "@payments"] }, async ({}, testInfo) => {
    await allure.feature("Payments");
    console.log(`attempt ${testInfo.retry}`);
    await test.step("call gateway", async () => {
      expect(testInfo.retry, "gateway timed out").toBeGreaterThan(0);
    });
  });
});
