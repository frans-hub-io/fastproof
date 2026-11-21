import * as allure from "allure-js-commons";
import { describe, expect, test } from "vitest";

let gatewayAttempts = 0;

describe("Checkout", () => {
  test("pays via gateway", { tags: ["smoke", "payments"] }, async () => {
    await allure.feature("Payments");
    gatewayAttempts += 1;
    console.log(`attempt ${gatewayAttempts}`);
    expect(gatewayAttempts, "gateway timed out").toBeGreaterThan(1);
  });

  test.each([
    ["EUR", 2],
    ["USD", 2],
    ["JPY", 0],
  ])("formats %s with %i decimals", async (currency, decimals) => {
    await allure.parameter("currency", currency);
    expect((1).toFixed(decimals).length).toBe(decimals === 0 ? 1 : decimals + 2);
  });

  test.skip("bulk import", () => {});

  test("legacy export", (ctx) => {
    ctx.skip("export endpoint is retired");
  });

  test.todo("refund flow");

  test.fails("rounding bug", () => {
    expect(0.1 + 0.2).toBe(0.3);
  });
});
