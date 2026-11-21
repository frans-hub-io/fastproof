import { expect, test } from "@playwright/test";
import * as allure from "allure-js-commons";

test.describe("Catalog", () => {
  for (const [query, minLength] of [["laptop", 3], ["phone", 2], ["tablet", 1]] as const) {
    test(`search for ${query}`, async () => {
      await allure.parameter("query", query);
      expect(query.length).toBeGreaterThan(minLength);
    });
  }

  test("legacy export", async () => {
    test.skip(true, "export endpoint is retired");
  });

  test.skip("bulk import", async () => {});

  test.fixme("price history chart", async () => {});

  test("rounding bug", async () => {
    test.fail(true, "https://issues.example.com/browse/SHOP-77");
    expect(0.1 + 0.2).toBe(0.3);
  });
});
