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
          config_kind: "llm",
          provider_name: "openai-compatible",
          provider_base_url: "https://provider.example/v1",
          default_model: "gpt-4.1-mini",
          allowed_models: ["gpt-4.1-mini"],
          api_type: "chat_completions",
          reasoning_effort: null,
          allow_streaming: true,
          request_timeout_seconds: 60,
        },
        model_configs: [
          {
            config_kind: "llm",
            provider_name: "openai-compatible",
            provider_base_url: "https://provider.example/v1",
            default_model: "gpt-4.1-mini",
            allowed_models: ["gpt-4.1-mini"],
            api_type: "chat_completions",
            reasoning_effort: null,
            allow_streaming: true,
            request_timeout_seconds: 60,
          },
          {
            config_kind: "image",
            provider_name: "openai-compatible",
            provider_base_url: "https://provider.example/v1",
            default_model: "gpt-image-1",
            allowed_models: ["gpt-image-1"],
            api_type: "images_generations",
            reasoning_effort: null,
            allow_streaming: false,
            request_timeout_seconds: 60,
          },
          {
            config_kind: "title",
            provider_name: "openai-compatible",
            provider_base_url: "https://provider.example/v1",
            default_model: "gpt-4.1-mini",
            allowed_models: ["gpt-4.1-mini"],
            api_type: "chat_completions",
            reasoning_effort: null,
            allow_streaming: false,
            request_timeout_seconds: 60,
          },
        ],
        required_models_ready: false,
        missing_required_model_config_kinds: ["llm", "title"],
      },
    });
  });
  await page.route("**/api/channels", async (route) => {
    await route.fulfill({
      json: {
        channels: [{ id: "channel-1", name: "hermes-hub", description: "Default" }],
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
  await page.route("**/api/channels/channel-1/sessions/session-1/messages", async (route) => {
    await route.fulfill({
      json: {
        messages: [
          {
            id: "message-1",
            session_id: "session-1",
            role: "assistant",
            content: "stored answer",
            attachments: [],
            created_at: 1,
          },
        ],
      },
    });
  });

  await page.goto("/");

  await expect(page.getByRole("heading", { name: "hermes-hub" })).toBeVisible();
  await expect(page.getByRole("button", { name: "对话" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "New chat" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Run" })).toBeVisible();
  await expect(page.getByText("stored answer")).toBeVisible();
  await expect(page.getByRole("button", { name: "用户管理" })).toBeVisible();
  await expect(page.getByRole("button", { name: "模型配置管理" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Hermes 管理" })).toBeVisible();
  await expect(page.locator(".message-list")).toHaveCSS("overflow-y", "auto");
});
