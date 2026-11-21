import { expect, test } from "@playwright/test";
import * as allure from "allure-js-commons";

const PNG_1X1 = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
  "base64",
);

test.describe("Cart", () => {
  test(
    "adds item to cart",
    {
      tag: ["@cart", "@regression"],
      annotation: [
        { type: "issue", description: "https://issues.example.com/browse/SHOP-101" },
        { type: "note", description: "covers the happy path" },
      ],
    },
    async ({ baseURL }, testInfo) => {
      await allure.description("Adds a single item to an empty cart and checks the total.");
      await allure.epic("Web shop");
      await allure.feature("Cart");
      await allure.story("Add item");
      await allure.severity("critical");
      await allure.owner("alice");
      await allure.tag("regression");
      await allure.allureId("1001");
      await allure.issue("https://issues.example.com/browse/SHOP-101", "SHOP-101");
      await allure.tms("https://tms.example.com/cases/TC-1001", "TC-1001");
      await allure.link("https://docs.example.com/cart", "Cart docs");
      await allure.parameter("baseURL", String(baseURL));
      await allure.parameter("apiToken", "s3cr3t-token", { mode: "masked" });
      await allure.parameter("requestId", "req-0001", { mode: "hidden" });
      console.log(`cart service at ${baseURL}`);
      console.error("warning: cart cache is cold");
      await test.step("open cart", async () => {
        await test.step("request empty cart", async () => {
          await testInfo.attach("request.txt", { body: "GET /api/cart\nAccept: application/json\n", contentType: "text/plain" });
        });
        await test.step("check cart is empty", async () => {
          await testInfo.attach("cart.json", {
            body: JSON.stringify({ items: [], total: 0 }),
            contentType: "application/json",
          });
          expect([].length).toBe(0);
        });
      });
      await test.step("add item", async () => {
        const shot = testInfo.outputPath("cart.png");
        const fs = await import("node:fs/promises");
        await fs.writeFile(shot, PNG_1X1);
        await testInfo.attach("cart.png", { path: shot, contentType: "image/png" });
        await allure.step("verify total", async (ctx) => {
          await ctx.parameter("currency", "EUR");
          expect(19.99 * 1).toBe(19.99);
        });
      });
    },
  );
});
