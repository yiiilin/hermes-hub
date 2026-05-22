import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  // 端到端用例固定英文界面，避免浏览器/系统语言差异导致选择器漂移。
  await page.addInitScript(() => {
    localStorage.setItem("hermes-hub-language", "en");
  });
});

test("renders channel and session workspace panels", async ({ page }) => {
  let historyOneUpdatedAt = 100;
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
        sessions: [
          {
            id: "session-1",
            channel_id: "channel-1",
            kind: "agent",
            title: "Run",
            created_at: 100,
            updated_at: 100,
          },
          ...Array.from({ length: 28 }, (_, index) => ({
            id: `session-extra-${index}`,
            channel_id: "channel-1",
            kind: "agent",
            title: `History ${index + 1}`,
            created_at: 90 - index,
            updated_at: index === 0 ? historyOneUpdatedAt : 90 - index,
          })),
        ],
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

  await expect(page.getByRole("button", { name: "Hermes Hub" })).toBeVisible();
  await expect(page.locator(".chat-sidebar-menu")).not.toContainText("hermes-hub");
  await expect(page.getByRole("heading", { name: "Run" })).toBeVisible();
  await expect(page.getByRole("button", { name: "对话" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "New chat" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Run" })).toBeVisible();
  await expect(page.getByText("stored answer")).toBeVisible();
  await expect(page.locator(".session-scrollbar-thumb")).toBeVisible();
  const sessionListBox = await page.locator(".session-list").evaluate((node) => ({
    clientHeight: node.clientHeight,
    clientWidth: node.clientWidth,
    offsetWidth: (node as HTMLElement).offsetWidth,
    scrollHeight: node.scrollHeight,
  }));
  expect(sessionListBox.scrollHeight).toBeGreaterThan(sessionListBox.clientHeight);
  expect(sessionListBox.offsetWidth).toBe(sessionListBox.clientWidth);
  await expect(page.locator(".message-list")).toHaveCSS("overflow-y", "auto");
  historyOneUpdatedAt = 200;
  await page.getByRole("button", { name: "Refresh" }).click();
  await expect(page.getByRole("heading", { name: "Run" })).toBeVisible();
  await expect(page.getByRole("button", { name: "History 1", exact: true })).toHaveClass(
    /unread/,
  );
  await expect(page.getByRole("button", { name: "User management" })).toBeVisible();
  await page.getByRole("button", { name: "User management" }).click();
  await expect(page.getByRole("heading", { name: "User management" })).toBeVisible();
  await expect(page.getByRole("button", { name: "New chat" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Run" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Model configuration" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Hermes management" })).toBeVisible();
});

test("shows Hermes input state and opens image attachments in a lightbox", async ({ page }) => {
  let messages: Array<{
    id: string;
    session_id: string;
    role: "user" | "assistant";
    client_message_key?: string;
    content: string;
    attachments: Array<{
      id: string;
      name: string;
      content_type: string;
      kind: "file" | "image";
      size: number;
      data_url?: string;
    }>;
    created_at: number;
  }> = [
    {
      id: "message-0",
      session_id: "session-1",
      role: "assistant",
      content: "stored answer",
      attachments: [],
      created_at: 1,
    },
  ];

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
          api_type: "responses",
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
            api_type: "responses",
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
            api_type: "responses",
            reasoning_effort: null,
            allow_streaming: false,
            request_timeout_seconds: 60,
          },
        ],
        required_models_ready: true,
        missing_required_model_config_kinds: [],
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
        sessions: [
          {
            id: "session-1",
            channel_id: "channel-1",
            kind: "agent",
            title: "Run",
            created_at: 100,
            updated_at: 100,
          },
        ],
      },
    });
  });
  await page.route("**/api/channels/channel-1/sessions/session-1/messages", async (route) => {
    if (route.request().method() === "GET") {
      await route.fulfill({ json: { messages } });
      return;
    }

    const payload = route.request().postDataJSON() as {
      role: "user" | "assistant";
      content: string;
      client_message_key?: string;
    };
    const existing = payload.client_message_key
      ? messages.find((message) => message.client_message_key === payload.client_message_key)
      : undefined;
    if (existing) {
      await route.fulfill({ json: { message: existing } });
      return;
    }
    const nextMessage =
      payload.role === "assistant"
        ? {
            id: `message-${messages.length + 1}`,
            session_id: "session-1",
            role: "assistant" as const,
            client_message_key: payload.client_message_key,
            content: payload.content,
            attachments: [
              {
                id: "attachment-cat",
                name: "cat.png",
                content_type: "image/png",
                kind: "image" as const,
                size: 12,
                data_url: "data:image/png;base64,iVBORw0KGgo=",
              },
            ],
            created_at: 2,
          }
        : {
            id: `message-${messages.length + 1}`,
            session_id: "session-1",
            role: "user" as const,
            client_message_key: payload.client_message_key,
            content: payload.content,
            attachments: [],
            created_at: 2,
          };
    messages = [...messages, nextMessage];
    await route.fulfill({ json: { message: nextMessage } });
  });
  await page.route("**/api/hermes/v1/runs", async (route) => {
    await route.fulfill({
      status: 202,
      json: { run_id: "run-image", status: "started" },
    });
  });
  await page.route("**/api/hermes/v1/runs/run-image/events", async (route) => {
    // 人工延迟一下，让页面有机会展示“正在输入”的临时状态。
    await page.waitForTimeout(250);
    await route.fulfill({
      headers: {
        "Content-Type": "text/event-stream",
      },
      body:
        'data: {"event":"message.delta","delta":"生成好了：\\\\n"}\n' +
        'data: {"event":"run.completed","output":"生成好了：\\\\n"}\n\n',
    });
  });

  await page.goto("/");

  await expect(page.getByRole("button", { name: "New chat" })).toBeVisible();
  await page.getByLabel("Message").fill("给我生成一个小猫");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("Hermes is typing")).toBeVisible();
  await expect(page.getByRole("button", { name: "Preview image cat.png" })).toBeVisible({
    timeout: 15000,
  });
  await page.reload();
  await expect(page.getByRole("button", { name: "Preview image cat.png" })).toHaveCount(1);
  await page.getByRole("button", { name: "Preview image cat.png" }).click();
  await expect(page.getByRole("dialog", { name: "Image preview" })).toBeVisible();
  await page.getByRole("button", { name: "Close image preview", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Image preview" })).toHaveCount(0);
});

test("uses a left drawer for the chat sidebar on mobile", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
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
        sessions: [
          {
            id: "session-1",
            channel_id: "channel-1",
            kind: "agent",
            title: "Mobile Run",
            created_at: 100,
            updated_at: 100,
          },
          ...Array.from({ length: 18 }, (_, index) => ({
            id: `session-mobile-${index}`,
            channel_id: "channel-1",
            kind: "agent",
            title: `Mobile History ${index + 1}`,
            created_at: 90 - index,
            updated_at: 90 - index,
          })),
        ],
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

  await expect(page.getByRole("button", { name: "Open menu" })).toBeVisible();
  await expect(page.locator(".mobile-topbar").getByRole("button", { name: "Hermes Hub" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mobile Run" })).toBeVisible();
  await expect(page.getByText("stored answer")).toBeVisible();
  await expect(page.getByLabel("Message")).toHaveCSS("font-size", "16px");

  const closedBoxes = await page.evaluate(() => {
    const box = (selector: string) => {
      const node = document.querySelector(selector);
      if (!node) {
        return null;
      }
      const rect = node.getBoundingClientRect();
      return {
        top: rect.top,
        bottom: rect.bottom,
        height: rect.height,
      };
    };
    const list = document.querySelector(".session-list") as HTMLElement | null;
    return {
      viewportHeight: window.innerHeight,
      bodyScrollHeight: document.body.scrollHeight,
      sidebar: box(".sidebar"),
      header: box(".chat-header"),
      messageList: box(".message-list"),
      composer: box(".composer"),
      sessionListScrollable: list ? list.scrollHeight > list.clientHeight : false,
    };
  });

  expect(closedBoxes.bodyScrollHeight).toBeLessThanOrEqual(closedBoxes.viewportHeight);
  expect(closedBoxes.sidebar?.bottom).toBeLessThanOrEqual(closedBoxes.viewportHeight);
  expect(closedBoxes.sidebar?.top).toBeGreaterThanOrEqual(0);
  expect(closedBoxes.header?.top).toBeGreaterThanOrEqual(0);
  expect(closedBoxes.header?.bottom).toBeLessThanOrEqual(closedBoxes.viewportHeight);
  expect(closedBoxes.messageList?.height).toBeGreaterThan(240);
  expect(closedBoxes.composer?.bottom).toBeLessThanOrEqual(closedBoxes.viewportHeight);

  const closedLeft = await page.locator(".sidebar").evaluate((node) => node.getBoundingClientRect().right);
  expect(closedLeft).toBeLessThanOrEqual(0);

  await page.getByRole("button", { name: "Open menu" }).click();
  await expect(page.getByRole("button", { name: "Close menu" }).first()).toBeVisible();
  await expect(page.locator(".sidebar")).toHaveClass(/open/);
  await expect(page.getByRole("button", { name: "New chat" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Mobile Run" })).toBeVisible();
  await expect(page.getByRole("button", { name: "User management" })).toBeVisible();
  await expect
    .poll(() => page.locator(".sidebar").evaluate((node) => node.getBoundingClientRect().left))
    .toBeGreaterThanOrEqual(0);
  const openBox = await page.locator(".sidebar").evaluate((node) => {
    const rect = node.getBoundingClientRect();
    const list = document.querySelector(".session-list") as HTMLElement | null;
    return {
      left: rect.left,
      right: rect.right,
      bottom: rect.bottom,
      viewportHeight: window.innerHeight,
      sessionListScrollable: list ? list.scrollHeight > list.clientHeight : false,
    };
  });
  expect(openBox.right).toBeGreaterThan(240);
  expect(openBox.bottom).toBeLessThanOrEqual(openBox.viewportHeight);
  expect(openBox.sessionListScrollable).toBe(true);

  await page.getByRole("button", { name: "Mobile History 1", exact: true }).click();
  await expect(page.locator(".sidebar")).not.toHaveClass(/open/);
});
