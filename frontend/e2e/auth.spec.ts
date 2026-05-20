import { expect, test } from "@playwright/test";

test("renders login and admin workspace shell", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Hermes Hub" })).toBeVisible();
  await expect(page.locator("form").getByRole("button")).toBeVisible();
  await expect(page.getByLabel("Email")).toBeVisible();
  await expect(page.getByLabel("Password")).toBeVisible();
  await expect(page.locator("aside[aria-label='Primary']")).toHaveCount(0);
});
