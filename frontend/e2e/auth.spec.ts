import { expect, test } from "@playwright/test";

test("renders login and admin workspace shell", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Hermes Hub" })).toBeVisible();
  await expect(page.locator("form").getByRole("button", { name: "Sign in" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Invite" })).toBeVisible();
  await expect(page.getByRole("button", { name: "First admin" })).toBeVisible();
});
