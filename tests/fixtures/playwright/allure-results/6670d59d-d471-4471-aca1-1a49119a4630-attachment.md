# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: checkout.spec.ts >> Checkout >> Tax >> calculates tax
- Location: tests/checkout.spec.ts:15:5

# Error details

```
TypeError: Cannot read properties of null (reading 'tax')
```

# Test source

```ts
  1  | import { expect, test } from "@playwright/test";
  2  | import * as allure from "allure-js-commons";
  3  | 
  4  | test.describe("Checkout", () => {
  5  |   test("applies discount code", { tag: "@checkout" }, async () => {
  6  |     await allure.feature("Checkout");
  7  |     await test.step("apply SAVE10", async () => {
  8  |       expect(100 - 5).toBe(90);
  9  |     });
  10 |   });
  11 | 
  12 |   test.describe("Tax", () => {
  13 |     test.describe.configure({ retries: 0 });
  14 | 
  15 |     test("calculates tax", async () => {
  16 |       const order = JSON.parse("null") as { tax: number };
> 17 |       expect(order.tax).toBe(20);
     |                    ^ TypeError: Cannot read properties of null (reading 'tax')
  18 |     });
  19 |   });
  20 | 
  21 |   test("pays via gateway", { tag: ["@smoke", "@payments"] }, async ({}, testInfo) => {
  22 |     await allure.feature("Payments");
  23 |     console.log(`attempt ${testInfo.retry}`);
  24 |     await test.step("call gateway", async () => {
  25 |       expect(testInfo.retry, "gateway timed out").toBeGreaterThan(0);
  26 |     });
  27 |   });
  28 | });
  29 | 
```