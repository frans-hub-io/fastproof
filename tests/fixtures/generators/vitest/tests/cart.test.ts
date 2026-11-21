import { mkdirSync, writeFileSync } from "node:fs";
import * as allure from "allure-js-commons";
import { describe, expect, test } from "vitest";

const PNG_1X1 = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
  "base64",
);

describe("Cart", () => {
  test("adds item to cart", { tags: ["cart"] }, async ({ annotate }) => {
    await allure.description("Adds a single item to an empty cart and checks the total.");
    await allure.epic("Web shop");
    await allure.feature("Cart");
    await allure.story("Add item");
    await allure.severity("critical");
    await allure.owner("alice");
    await allure.tag("regression");
    await allure.allureId("2001");
    await allure.issue("https://issues.example.com/browse/SHOP-101", "SHOP-101");
    await allure.tms("https://tms.example.com/cases/TC-2001", "TC-2001");
    await allure.link("https://docs.example.com/cart", "Cart docs");
    await allure.parameter("baseURL", "https://staging.example.com");
    await allure.parameter("apiToken", "s3cr3t-token", { mode: "masked" });
    await allure.parameter("requestId", "req-0001", { mode: "hidden" });
    await annotate("https://issues.example.com/browse/SHOP-101", "issue");
    console.log("cart service at https://staging.example.com");
    console.error("warning: cart cache is cold");
    await allure.step("open cart", async () => {
      await allure.step("request empty cart", async () => {
        await allure.attachment("request.txt", "GET /api/cart\nAccept: application/json\n", "text/plain");
      });
      await allure.step("check cart is empty", async (ctx) => {
        await ctx.parameter("cartId", "c-42");
        await allure.attachment("cart.json", JSON.stringify({ items: [], total: 0 }), "application/json");
        expect([].length).toBe(0);
      });
    });
    await allure.step("add item", async () => {
      mkdirSync("tmp", { recursive: true });
      writeFileSync("tmp/cart.png", PNG_1X1);
      await allure.attachmentPath("cart.png", "tmp/cart.png", "image/png");
      expect(19.99).toBe(19.99);
    });
  });

  test("applies discount code", async () => {
    await allure.feature("Cart");
    await allure.step("apply SAVE10", async () => {
      expect(100 - 5).toBe(90);
    });
  });

  test("calculates tax", { retry: 0 }, () => {
    const order = JSON.parse("null") as { tax: number };
    expect(order.tax).toBe(20);
  });
});
