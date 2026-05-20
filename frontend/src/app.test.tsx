import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./app";
import { createApiClient, createMockApiClient } from "./api/client";
import { createClientMessageId } from "./routes/channel-session";

describe("App", () => {
  afterEach(() => {
    window.history.pushState({}, "", "/");
  });

  it("renders the authenticated admin workspace and can send a Hermes prompt", async () => {
    render(<App apiClient={createMockApiClient()} />);

    expect(await screen.findByRole("heading", { name: "hermes-hub" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "用户管理" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "模型配置管理" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hermes 管理" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(screen.getAllByText("hello")).toHaveLength(2);
    });

    fireEvent.click(screen.getByRole("button", { name: "用户管理" }));
    expect(await screen.findByRole("heading", { name: "用户管理" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "模型配置管理" }));
    expect(await screen.findByRole("heading", { name: "大模型" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "图片生成模型" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "标题生成模型" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Save" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Test" })).toHaveLength(3);
    const apiKeyInputs = screen.getAllByLabelText("API key");
    expect(apiKeyInputs[0]).toHaveAttribute("type", "password");
    expect(apiKeyInputs[0]).toHaveValue("ready-provider-key");
    fireEvent.click(screen.getAllByRole("button", { name: "Test" })[0]);
    expect(await screen.findByText("model test succeeded")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Hermes 管理" }));
    expect(await screen.findByRole("heading", { name: "Hermes 管理" })).toBeInTheDocument();
  });

  it("renders login and authenticates with email and password", async () => {
    const client = createMockApiClient();
    await client.logout();

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "admin@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "admin-password-123" },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Sign in" }).at(-1)!);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "hermes-hub" })).toBeInTheDocument();
    });
  });

  it("shows the first-user registration form without the app sidebar", async () => {
    const client = createMockApiClient({ initialUser: null, bootstrapOpen: true });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Create account" })).toBeInTheDocument();
    expect(screen.getByLabelText("Confirm password")).toBeInTheDocument();
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "admin@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "admin-password-123" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "admin-password-123" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create account" }));

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "hermes-hub" })).toBeInTheDocument();
    });
  });

  it("opens invite links as registration without exposing the token field", async () => {
    window.history.pushState({}, "", "/?invite=secret-invite-token");
    const client = createMockApiClient({ initialUser: null, bootstrapOpen: false });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Create account" })).toBeInTheDocument();
    expect(screen.getByLabelText("Confirm password")).toBeInTheDocument();
    expect(screen.queryByLabelText("Invite token")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue("secret-invite-token")).not.toBeInTheDocument();
    expect(screen.queryByText("secret-invite-token")).not.toBeInTheDocument();
  });

  it("blocks invites and Hermes start controls until required models are ready", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          requiredModelsReady: false,
          missingRequiredModelConfigKinds: ["llm", "title"],
        })}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "用户管理" }));
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(screen.getByText("请先在模型配置管理中保存可用的大模型、标题生成模型。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create invite" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Hermes 管理" }));
    expect(await screen.findByRole("heading", { name: "Hermes 管理" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Rebuild" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Stop" })).not.toBeDisabled();
  });

  it("can create a managed Hermes instance for a user without one", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialInstance: null,
        })}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Hermes 管理" }));
    expect(await screen.findAllByText("not_created")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Start" })).toBeInTheDocument();
    });
  });

  it("keeps returned model API keys in the real API client while password inputs hide them", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        model_config: {
          config_kind: "llm",
          provider_name: "openai-compatible",
          provider_base_url: "https://ready-provider.example/v1",
          provider_api_key: "stored-provider-key",
          default_model: "gpt-4.1-mini",
          allowed_models: ["gpt-4.1-mini"],
          allow_streaming: true,
          request_timeout_seconds: 60,
        },
        model_configs: [],
        required_models_ready: true,
        missing_required_model_config_kinds: [],
      }),
    } as Response);

    const status = await createApiClient().modelConfigStatus();

    expect(status.model_config.provider_api_key).toBe("stored-provider-key");
    fetchMock.mockRestore();
  });

  it("uses Hermes runs input and reads run events in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      if (path === "/api/hermes/v1/runs") {
        expect(JSON.parse(String(init?.body))).toMatchObject({
          input: "hello",
          stream: true,
          session_id: "session-1",
        });
        return {
          ok: true,
          status: 202,
          json: async () => ({ run_id: "run-1", status: "started" }),
        } as Response;
      }

      expect(path).toBe("/api/hermes/v1/runs/run-1/events");
      return {
        ok: true,
        status: 200,
        text: async () =>
          [
            'data: {"event":"message.delta","delta":"he"}',
            'data: {"event":"message.delta","delta":"llo"}',
            'data: {"event":"run.completed","output":"hello"}',
            "",
          ].join("\n"),
      } as Response;
    });

    await expect(createApiClient().sendHermesPrompt("hello", [], "session-1")).resolves.toBe(
      "hello",
    );
    fetchMock.mockRestore();
  });

  it("generates message ids when crypto.randomUUID is unavailable", () => {
    expect(createClientMessageId({})).toMatch(/^msg-/);
    expect(
      createClientMessageId({
        getRandomValues(array) {
          array.fill(7);
          return array;
        },
      }),
    ).toBe("07070707070707070707070707070707");
  });
});
