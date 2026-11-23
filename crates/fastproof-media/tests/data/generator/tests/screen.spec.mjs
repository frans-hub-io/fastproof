import { test, expect } from "@playwright/test";
import { CLOCK } from "./clock.mjs";

test("screen recording", async ({ page }) => {
  await page.setContent(CLOCK);
  await page.waitForTimeout(5000);
  await expect(page.locator("canvas")).toBeVisible();
});
