import { expect, test } from "@playwright/test";

test("renders channel and session workspace panels", async ({ page }) => {
  await page.route("**/api/auth/me", async (route) => {
    await route.fulfill({
      json: {
        user: {
          id: "user-1",
          email: "admin@example.com",
          role: "admin",
          status: "active",
        },
      },
    });
  });
  await page.route("**/api/admin/users", async (route) => {
    await route.fulfill({
      json: {
        users: [
          {
            id: "user-1",
            email: "admin@example.com",
            role: "admin",
            status: "active",
          },
        ],
      },
    });
  });
  await page.route("**/api/invites", async (route) => {
    await route.fulfill({ json: { invites: [] } });
  });
  await page.route("**/api/admin/hermes-instances", async (route) => {
    await route.fulfill({ json: { hermes_instances: [] } });
  });
  await page.route("**/api/admin/model-config", async (route) => {
    await route.fulfill({
      json: {
        model_config: {
          provider_name: "openai-compatible",
          provider_base_url: "https://provider.example/v1",
          default_model: "gpt-4.1-mini",
          allowed_models: ["gpt-4.1-mini"],
          allow_streaming: true,
          request_timeout_seconds: 60,
        },
      },
    });
  });
  await page.route("**/api/channels", async (route) => {
    await route.fulfill({
      json: {
        channels: [{ id: "channel-1", name: "Research", description: "Default" }],
      },
    });
  });
  await page.route("**/api/workspace/status", async (route) => {
    await route.fulfill({
      json: {
        hermes_instance: {
          id: "instance-1",
          user_id: "user-1",
          kind: "managed_docker",
          status: "running",
          base_url: "http://hermes-user-user-1:8000",
        },
      },
    });
  });
  await page.route("**/api/channels/channel-1/sessions", async (route) => {
    await route.fulfill({
      json: {
        sessions: [{ id: "session-1", channel_id: "channel-1", kind: "agent", title: "Run" }],
      },
    });
  });

  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Channels" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Hermes instance", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Session" })).toBeVisible();
});
