# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: catalog.spec.ts >> Catalog >> rounding bug
- Location: tests/catalog.spec.ts:20:3

# Error details

```
Error: expect(received).toBe(expected) // Object.is equality

Expected: 0.3
Received: 0.30000000000000004
```

# Test source

```ts
  1  | import { expect, test } from "@playwright/test";
  2  | import * as allure from "allure-js-commons";
  3  | 
  4  | test.describe("Catalog", () => {
  5  |   for (const [query, minLength] of [["laptop", 3], ["phone", 2], ["tablet", 1]] as const) {
  6  |     test(`search for ${query}`, async () => {
  7  |       await allure.parameter("query", query);
  8  |       expect(query.length).toBeGreaterThan(minLength);
  9  |     });
  10 |   }
  11 | 
  12 |   test("legacy export", async () => {
  13 |     test.skip(true, "export endpoint is retired");
  14 |   });
  15 | 
  16 |   test.skip("bulk import", async () => {});
  17 | 
  18 |   test.fixme("price history chart", async () => {});
  19 | 
  20 |   test("rounding bug", async () => {
  21 |     test.fail(true, "https://issues.example.com/browse/SHOP-77");
> 22 |     expect(0.1 + 0.2).toBe(0.3);
     |                       ^ Error: expect(received).toBe(expected) // Object.is equality
  23 |   });
  24 | });
  25 | 
```