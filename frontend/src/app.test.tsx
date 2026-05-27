import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./app";
import {
  ApiRequestError,
  createApiClient,
  createMockApiClient,
  type ApiClient,
  type ChannelMessage,
  type ChannelRun,
  type ChannelSessionEvent,
  type HermesActiveRun,
  type HermesVerboseEvent,
} from "./api/client";
import { createClientMessageId } from "./routes/channel-session";

describe("App", () => {
  afterEach(() => {
    window.history.pushState({}, "", "/");
    localStorage.clear();
  });

  function createDeferred<T>() {
    let resolve!: (value: T | PromiseLike<T>) => void;
    let reject!: (reason?: unknown) => void;
    const promise = new Promise<T>((nextResolve, nextReject) => {
      resolve = nextResolve;
      reject = nextReject;
    });
    return { promise, resolve, reject };
  }

  function executionMessage(content: HermesVerboseEvent[], id = "message-execution"): ChannelMessage {
    return {
      id,
      session_id: "session-1",
      role: "assistant",
      content: `<!-- hermes-hub:execution:v1 -->\n${JSON.stringify(content)}`,
      attachments: [],
      created_at: Date.now(),
    };
  }

  function legacyExecutionMessage(
    content: string,
    id = "message-legacy-execution",
    createdAt = Date.now(),
  ): ChannelMessage {
    return {
      id,
      session_id: "session-1",
      role: "assistant",
      content,
      attachments: [],
      created_at: createdAt,
    };
  }

  function expectPendingLoader() {
    const indicator = document.querySelector(".message-bubble.assistant.pending .typing-indicator");
    expect(indicator?.querySelector(".typing-dots")).toBeInTheDocument();
    expect(indicator).not.toHaveTextContent("Hermes is typing");
    expect(indicator).not.toHaveTextContent("Hermes is responding");
  }

  function expectNoPendingLoader() {
    expect(document.querySelector(".typing-dots")).not.toBeInTheDocument();
  }

  function expectedMessageTime(timestampSeconds: number) {
    return new Intl.DateTimeFormat("en", {
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(timestampSeconds * 1000));
  }

  async function openSettingsTab(tabName: string, settingsName = "System settings") {
    fireEvent.click(await screen.findByRole("button", { name: settingsName }));
    const settingsTabs = await screen.findByRole("tablist", { name: settingsName });
    fireEvent.click(within(settingsTabs).getByRole("tab", { name: tabName }));
    return settingsTabs;
  }

  function createHubRunMock(
    options: {
      answer?: string;
      answerDelay?: Promise<void>;
      error?: string;
      executionEvents?: HermesVerboseEvent[];
      oldAssistant?: ChannelMessage;
    } = {},
  ): {
    client: ApiClient;
    createCalled: () => boolean;
    setFailed: (message: string) => void;
    getRun: () => ChannelRun | null;
    getMessages: () => ChannelMessage[];
  } {
    let run: ChannelRun | null = null;
    let userMessage: ChannelMessage | null = null;
    const messages: ChannelMessage[] = options.oldAssistant ? [options.oldAssistant] : [];
    let createCalled = false;
    const runId = "hub-run-test";
    let executionMessagesPushed = false;
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();

    function emitSessionEvent(event: ChannelSessionEvent) {
      for (const listener of eventListeners) {
        listener(event);
      }
    }

    function pushExecutionMessages(sessionId: string) {
      if (!options.executionEvents?.length || executionMessagesPushed) {
        return;
      }
      executionMessagesPushed = true;
      const message = executionMessage(options.executionEvents, `message-execution-${sessionId}`);
      messages.push(message);
      emitSessionEvent({ type: "message_created", message });
    }

    const client = createMockApiClient({
      initialMessagesBySessionId: {
        "session-1": messages,
      },
      async createChannelRun(_channelId, sessionId, input) {
          createCalled = true;
          userMessage = {
            id: "message-user",
            session_id: sessionId,
            role: "user",
            client_message_key: input.clientMessageKey,
            content: input.content,
            attachments: input.attachments ?? [],
            created_at: Date.now(),
          };
          run = {
            id: "run-storage-id",
            run_id: runId,
            session_id: sessionId,
            user_message_id: userMessage.id,
            status: "queued",
            input: input.content,
            input_attachments: input.attachments ?? [],
            created_at: Date.now(),
            updated_at: Date.now(),
          };
          messages.push(userMessage);
          emitSessionEvent({ type: "message_created", message: userMessage });
          run = { ...run, status: "running", updated_at: Date.now() };
          emitSessionEvent({ type: "run_updated", run });
          // Hub adapter 会在最终回答前持续落库执行步骤，测试 mock 也要模拟这个实时行为。
          queueMicrotask(() => pushExecutionMessages(sessionId));
          if (options.error) {
            run = { ...run, status: "failed", error: options.error, updated_at: Date.now() };
            emitSessionEvent({ type: "run_updated", run });
            return { message: userMessage, run };
          }
          void (async () => {
            await (options.answerDelay ?? Promise.resolve());
            pushExecutionMessages(sessionId);
            const answer = options.answer ?? input.content;
            const assistantMessage: ChannelMessage = {
              id: "message-assistant",
              session_id: sessionId,
              role: "assistant",
              client_message_key: `hermes-run:${runId}`,
              content: answer,
              attachments: [],
              created_at: Date.now(),
            };
            messages.push(assistantMessage);
            emitSessionEvent({ type: "message_created", message: assistantMessage });
            if (run) {
              emitSessionEvent({
                type: "run_updated",
                run: { ...run, status: "completed", output_message_id: assistantMessage.id },
              });
            }
            run = null;
            emitSessionEvent({ type: "run_cleared", session_id: sessionId });
          })();
          return { message: userMessage, run };
        },
        activeRunsBySessionId: {},
      });
    client.activeHermesRun = async () => run;
    client.subscribeSessionEvents = (_channelId, sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages,
          active_run: run
            ? {
                run_id: run.run_id,
                status: run.status,
                error: run.error,
                output_message_id: run.output_message_id,
                created_at: run.created_at,
                updated_at: run.updated_at,
              }
            : null,
        });
      });
      return () => {
        eventListeners.delete(onEvent);
      };
    };
    client.clearHermesRun = async () => {
      run = null;
    };

    return {
      createCalled: () => createCalled,
      client,
      setFailed(message: string) {
        run = { ...run!, status: "failed", error: message, updated_at: Date.now() };
      },
      getRun() {
        return run;
      },
      getMessages() {
        return messages;
      },
    };
  }

  it("restores a failed Hub adapter run from active-run instead of dropping it", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    let activeRun: HermesActiveRun | null = {
      run_id: "hub-run-failed-restored",
      status: "running",
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    const client = createMockApiClient({
      activeRunsBySessionId: {
        "session-1": activeRun,
      },
    });
    client.activeHermesRun = async () => activeRun;
    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages: [],
          active_run: activeRun,
        });
      });
      return () => eventListeners.delete(onEvent);
    };

    render(<App apiClient={client} />);

    await waitFor(() => expectPendingLoader());
    activeRun = {
      ...activeRun,
      status: "failed",
      error: "tool failed",
      updated_at: Date.now(),
    } as HermesActiveRun;
    for (const listener of eventListeners) {
      listener({
        type: "run_updated",
        run: {
          id: "run-storage-id",
          run_id: activeRun.run_id,
          session_id: "session-1",
          status: "failed",
          input: "failed input",
          input_attachments: [],
          error: "tool failed",
          created_at: activeRun.created_at,
          updated_at: activeRun.updated_at,
        },
      });
    }

    await waitFor(() => {
      expect(screen.getByText("Hermes run failed: tool failed")).toBeInTheDocument();
    });
    expectNoPendingLoader();
  });

  it("uses a completed Hub adapter run output message without adding an empty reply", async () => {
    const finalMessage: ChannelMessage = {
      id: "message-output",
      session_id: "session-1",
      role: "assistant",
      content: "最终答案",
      attachments: [],
      created_at: Date.now(),
    };
    const client = createMockApiClient({
      initialMessagesBySessionId: {
        "session-1": [finalMessage],
      },
      activeRunsBySessionId: {
        "session-1": {
          run_id: "hub-run-completed-restored",
          status: "completed",
          output_message_id: finalMessage.id,
          created_at: Date.now(),
          updated_at: Date.now(),
        },
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByText("最终答案")).toBeInTheDocument();
    expect(screen.getAllByText("最终答案")).toHaveLength(1);
  });

  it("loads a completed run output message with attachments when the live message event was missed", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    let activeRun: HermesActiveRun | null = {
      run_id: "hub-run-output-attachment",
      status: "running",
      created_at: 1,
      updated_at: 1,
    };
    const finalMessage: ChannelMessage = {
      id: "message-output-attachment",
      session_id: "session-1",
      role: "assistant",
      client_message_key: "hermes-run:hub-run-output-attachment",
      content: "文件已生成",
      attachments: [
        {
          id: "attachment-output",
          name: "result.txt",
          content_type: "text/plain",
          kind: "file",
          size: 12,
          download_url: "/api/attachments/attachment-output/download",
        },
      ],
      created_at: 2,
    };
    const client = createMockApiClient({
      activeRunsBySessionId: {
        "session-1": activeRun,
      },
    });
    client.activeHermesRun = async () => activeRun;
    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages: [],
          active_run: activeRun,
        });
      });
      return () => eventListeners.delete(onEvent);
    };

    render(<App apiClient={client} />);

    await waitFor(() => expectPendingLoader());
    activeRun = {
      ...activeRun,
      status: "completed",
      output_message_id: finalMessage.id,
      updated_at: 2,
    };
    for (const listener of eventListeners) {
      listener({
        type: "run_updated",
        run: {
          id: "run-storage-id",
          run_id: "hub-run-output-attachment",
          session_id: "session-1",
          status: "completed",
          input: "make file",
          input_attachments: [],
          output_message_id: finalMessage.id,
          created_at: 1,
          updated_at: 2,
        },
      });
      listener({
        type: "messages_snapshot",
        messages: [finalMessage],
        active_run: activeRun,
      });
    }

    expect(await screen.findByText("文件已生成")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Download file result.txt" })).toHaveAttribute(
      "href",
      "/api/attachments/attachment-output/download",
    );
  });

  it("renders the authenticated admin workspace and can send a Hermes prompt", async () => {
    render(<App apiClient={createMockApiClient()} />);

    expect(await screen.findByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "对话" })).not.toBeInTheDocument();
    expect(screen.queryByText("running")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Session" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "User management" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Model configuration" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hermes management" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "System settings" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(screen.getAllByText("hello")).toHaveLength(2);
    });
    fireEvent.click(screen.getByRole("button", { name: "Session" }));
    expect(screen.getAllByText("hello")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
    expect(document.querySelector(".shell.sidebar-collapsed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete session" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Expand sidebar" }));
    expect(document.querySelector(".shell.sidebar-collapsed")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete session" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Personalization" }));
    fireEvent.click(screen.getByRole("button", { name: "中文" }));
    expect(screen.getByRole("button", { name: "新建对话" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "English" }));

    const settingsTabs = await openSettingsTab("User management");
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "User management" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Session" })).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Model configuration" }));
    expect(await screen.findByRole("heading", { name: "Large language model" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Image model" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Title model" })).toBeInTheDocument();
    expect(
      screen
        .getAllByRole("heading", { level: 2 })
        .map((heading) => heading.textContent),
    ).toEqual(["Large language model", "Title model", "Image model"]);
    expect(screen.getByLabelText("Context window tokens")).toHaveValue(128000);
    expect(screen.getByLabelText("Max output tokens")).toHaveValue(4096);
    expect(screen.getByLabelText("Temperature")).toHaveValue(0.7);
    expect(screen.getByLabelText("Parallel tool calls")).toBeChecked();
    expect(screen.getByLabelText("Enable image generation")).not.toBeChecked();
    expect(screen.getAllByRole("button", { name: "Save" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Test" })).toHaveLength(3);
    const apiKeyInputs = screen.getAllByLabelText("API key");
    expect(apiKeyInputs[0]).toHaveAttribute("type", "password");
    expect(apiKeyInputs[0]).toHaveValue("ready-provider-key");
    fireEvent.click(screen.getAllByRole("button", { name: "Test" })[0]);
    expect(await screen.findByText("model test succeeded")).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Session settings" }));
    const maxSessionsInput = await screen.findByLabelText("Max sessions per user");
    expect(screen.queryByRole("heading", { name: "Session settings" })).not.toBeInTheDocument();
    fireEvent.change(maxSessionsInput, { target: { value: "12" } });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Settings saved")).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Authentication settings" }));
    expect(await screen.findByLabelText("Enable OIDC")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Authentication settings" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Enable OIDC"));
    fireEvent.change(screen.getByLabelText("OIDC display name"), {
      target: { value: "Acme SSO" },
    });
    fireEvent.change(screen.getByLabelText("OIDC client ID"), {
      target: { value: "hermes-hub" },
    });
    fireEvent.change(screen.getByLabelText("OIDC client secret"), {
      target: { value: "oidc-secret" },
    });
    fireEvent.change(screen.getByLabelText("OIDC authorization URL"), {
      target: { value: "https://idp.example.com/oauth2/v1/authorize" },
    });
    fireEvent.change(screen.getByLabelText("OIDC token URL"), {
      target: { value: "https://idp.example.com/oauth2/v1/token" },
    });
    fireEvent.change(screen.getByLabelText("OIDC userinfo URL"), {
      target: { value: "https://idp.example.com/oauth2/v1/userinfo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Settings saved")).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Hermes management" }));
    expect(await screen.findByRole("columnheader", { name: "Owner" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Hermes management" })).not.toBeInTheDocument();
  });

  it("groups admin modules under system settings tabs", async () => {
    render(<App apiClient={createMockApiClient()} />);

    const systemSettingsNav = await screen.findByRole("button", { name: "System settings" });
    expect(screen.queryByRole("button", { name: "User management" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Model configuration" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hermes management" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Managed skills" })).not.toBeInTheDocument();

    fireEvent.click(systemSettingsNav);
    expect(await screen.findByRole("heading", { name: "System settings" })).toBeInTheDocument();

    const settingsTabs = screen.getByRole("tablist", { name: "System settings" });
    expect(within(settingsTabs).getByRole("tab", { name: "User management" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(within(settingsTabs).getByRole("tab", { name: "Model configuration" })).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Hermes management" })).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Managed skills" })).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Session settings" })).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Authentication settings" })).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Model configuration" }));
    expect(await screen.findByRole("heading", { name: "Large language model" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Model configuration" })).not.toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Session settings" }));
    expect(await screen.findByLabelText("Max sessions per user")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Session settings" })).not.toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Authentication settings" }));
    const enableOidcRow = await screen.findByLabelText("Enable OIDC");
    expect(enableOidcRow).toBeInTheDocument();
    expect(screen.getByLabelText("OIDC Redirect URI")).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Authentication settings" })).not.toBeInTheDocument();
  });

  it("saves the optional image generation toggle and keeps it at the bottom of the image card", async () => {
    render(<App apiClient={createMockApiClient()} />);

    await openSettingsTab("Model configuration");
    const imageHeading = await screen.findByRole("heading", { name: "Image model" });
    const imageCard = imageHeading.closest("section.panel");
    expect(imageCard).toBeInTheDocument();

    const imageLabels = Array.from(imageCard!.querySelectorAll("label")).map((label) =>
      label.textContent?.replace(/\s+/g, " ").trim(),
    );
    expect(imageLabels.at(-1)).toBe("Enable image generation");

    const imageToggle = within(imageCard as HTMLElement).getByLabelText("Enable image generation");
    expect(imageToggle).not.toBeChecked();
    fireEvent.click(imageToggle);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Model configuration saved")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText("Enable image generation")).toBeChecked();
    });
  });

  it("saves main model runtime limits and parallel tool support", async () => {
    const client = createMockApiClient();
    render(<App apiClient={client} />);

    await openSettingsTab("Model configuration");
    fireEvent.change(await screen.findByLabelText("Context window tokens"), {
      target: { value: "200000" },
    });
    fireEvent.change(screen.getByLabelText("Max output tokens"), {
      target: { value: "8192" },
    });
    fireEvent.change(screen.getByLabelText("Temperature"), {
      target: { value: "0.3" },
    });
    fireEvent.click(screen.getByLabelText("Parallel tool calls"));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Model configuration saved")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText("Context window tokens")).toHaveValue(200000);
      expect(screen.getByLabelText("Max output tokens")).toHaveValue(8192);
      expect(screen.getByLabelText("Temperature")).toHaveValue(0.3);
      expect(screen.getByLabelText("Parallel tool calls")).not.toBeChecked();
    });
  });

  it("opens image attachments in a preview dialog", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-image",
                session_id: "session-1",
                role: "assistant",
                content: "生成好了",
                attachments: [
                  {
                    id: "attachment-image",
                    name: "cat.png",
                    content_type: "image/png",
                    kind: "image",
                    size: 12,
                    data_url: "data:image/png;base64,iVBORw0KGgo=",
                  },
                ],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Preview image cat.png" }));

    expect(screen.getByRole("dialog", { name: "Image preview" })).toBeInTheDocument();
    expect(screen.getAllByRole("img", { name: "cat.png" })).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Close image preview" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Image preview" })).not.toBeInTheDocument();
    });
  });

  it("shows each message updated time on the correct bubble edge", async () => {
    localStorage.setItem("hermes-hub-language", "en");
    const userUpdatedAt = 1_717_231_500;
    const assistantUpdatedAt = 1_717_232_100;

    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-user-time",
                session_id: "session-1",
                role: "user",
                content: "show my time",
                attachments: [],
                created_at: 1,
                updated_at: userUpdatedAt,
              } as ChannelMessage,
              {
                id: "message-assistant-time",
                session_id: "session-1",
                role: "assistant",
                content: "reply with time",
                attachments: [],
                created_at: 2,
                updated_at: assistantUpdatedAt,
              } as ChannelMessage,
            ],
          },
        })}
      />,
    );

    const userBubble = (await screen.findByText("show my time")).closest(".message-bubble");
    const assistantBubble = screen.getByText("reply with time").closest(".message-bubble");
    const userTime = userBubble?.querySelector("time.message-time");
    const assistantTime = assistantBubble?.querySelector("time.message-time");

    expect(userBubble).toHaveClass("user");
    expect(assistantBubble).toHaveClass("assistant");
    expect(userTime).toHaveClass("message-time-end");
    expect(assistantTime).toHaveClass("message-time-start");
    expect(userTime).toHaveTextContent(expectedMessageTime(userUpdatedAt));
    expect(assistantTime).toHaveTextContent(expectedMessageTime(assistantUpdatedAt));
    expect(userTime).toHaveAttribute("dateTime", new Date(userUpdatedAt * 1000).toISOString());
    expect(assistantTime).toHaveAttribute(
      "dateTime",
      new Date(assistantUpdatedAt * 1000).toISOString(),
    );
  });

  it("renders markdown content and file chips inside message bubbles", async () => {
    const absolutePptUrl = `${window.location.origin}/api/attachments/attachment-ppt/download`;
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-markdown",
                session_id: "session-1",
                role: "assistant",
                content:
                  `## 结果\n\n**加粗文本** 和 \`code\`\n\n文件：[练习.pptx](${absolutePptUrl})\n\n![cat](/api/attachments/cat/download)\n\n[open](/download)`,
                attachments: [
                  {
                    id: "attachment-ppt",
                    name: "练习.pptx",
                    content_type:
                      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                    kind: "file",
                    size: 12,
                    download_url: "/api/attachments/attachment-ppt/download",
                  },
                ],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    expect(await screen.findByRole("heading", { name: "结果" })).toBeInTheDocument();
    expect(screen.getByText("加粗文本")).toBeInTheDocument();
    expect(screen.getByText("code")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "open" })).toHaveAttribute("href", "/download");
    expect(screen.getByRole("button", { name: "Preview image cat" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Download file 练习.pptx" })).toBeInTheDocument();
    const markdown = document.querySelector(".markdown-content");
    expect(markdown?.textContent).toContain("文件：练习.pptx");
    expect(document.querySelector(".message-attachments")).not.toBeInTheDocument();
  });

  it("renders bare links inside user message bubbles", async () => {
    const link = "https://example.com/docs?tab=chat";
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-user-link",
                session_id: "session-1",
                role: "user",
                content: `看这个 ${link}`,
                attachments: [],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    const userBubble = (await screen.findByText(/看这个/)).closest(".message-bubble");
    const renderedLink = screen.getByRole("link", { name: link });

    expect(userBubble).toHaveClass("user");
    expect(userBubble).toHaveTextContent(link);
    expect(renderedLink).toHaveAttribute("href", link);
  });

  it("shows Hermes input status while a response is pending", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({ answer: "pong", answerDelay: deferred.promise });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "ping" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expectPendingLoader());
    expect(hubRun.createCalled()).toBe(true);

    deferred.resolve();
    await waitFor(() => {
      expectNoPendingLoader();
      expect(screen.getByText("pong")).toBeInTheDocument();
    });
  });

  it("keeps the composer focused and editable while Hermes is responding", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({ answer: "done", answerDelay: deferred.promise });

    render(<App apiClient={hubRun.client} />);

    const composer = await screen.findByLabelText("Message");
    fireEvent.change(composer, {
      target: { value: "first prompt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expectPendingLoader());
    expect(composer).toHaveFocus();
    fireEvent.change(composer, {
      target: { value: "draft next prompt" },
    });
    expect(composer).toHaveValue("draft next prompt");

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByText("done")).toBeInTheDocument();
    });
    expect(composer).toHaveValue("draft next prompt");
  });

  it("allows sending a second prompt while Hermes is still responding", async () => {
    let createCount = 0;
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        createCount += 1;
        const userMessage: ChannelMessage = {
          id: `message-user-${createCount}`,
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        const run: ChannelRun = {
          id: `run-storage-id-${createCount}`,
          run_id: `hub-run-${createCount}`,
          session_id: sessionId,
          user_message_id: userMessage.id,
          status: "running",
          input: input.content,
          input_attachments: input.attachments ?? [],
          created_at: Date.now(),
          updated_at: Date.now(),
        };
        return { message: userMessage, run };
      },
    });

    render(<App apiClient={client} />);

    const composer = await screen.findByLabelText("Message");
    fireEvent.change(composer, {
      target: { value: "first prompt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expectPendingLoader());
    expect(composer).toHaveFocus();

    fireEvent.change(composer, {
      target: { value: "draft next prompt" },
    });
    const sendButton = screen.getByRole("button", { name: "Send" });
    expect(sendButton).toBeEnabled();
    fireEvent.click(sendButton);

    await waitFor(() => {
      expect(createCount).toBe(2);
    });
    expect(composer).toHaveValue("");
  });

  it("keeps the pending loader on streamed assistant content until the run clears", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    const run: ChannelRun = {
      id: "run-storage-id",
      run_id: "hub-run-streaming-answer",
      session_id: "session-1",
      user_message_id: "message-user",
      status: "running",
      input: "stream",
      input_attachments: [],
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        const userMessage: ChannelMessage = {
          id: "message-user",
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        queueMicrotask(() => {
          for (const listener of eventListeners) {
            listener({
              type: "run_updated",
              run: {
                ...run,
                status: "running",
                updated_at: Date.now(),
              },
            });
          }
        });
        return { message: userMessage, run };
      },
    });
    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      return () => eventListeners.delete(onEvent);
    };

    render(<App apiClient={client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "stream" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expectPendingLoader());
    const assistantMessage: ChannelMessage = {
      id: "message-assistant-streaming",
      session_id: "session-1",
      role: "assistant",
      client_message_key: "hermes-run:hub-run-streaming-answer",
      content: "partial answer",
      attachments: [],
      created_at: Date.now(),
    };
    for (const listener of eventListeners) {
      listener({ type: "message_created", message: assistantMessage });
    }

    expect(await screen.findByText("partial answer")).toBeInTheDocument();
    await waitFor(() => expectPendingLoader());

    for (const listener of eventListeners) {
      listener({ type: "run_cleared", session_id: "session-1" });
    }
    await waitFor(() => expectNoPendingLoader());
  });

  it("does not render an empty assistant run message before formal content arrives", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    const run: ChannelRun = {
      id: "run-storage-id",
      run_id: "hub-run-empty-assistant",
      session_id: "session-1",
      user_message_id: "message-user",
      status: "running",
      input: "empty first",
      input_attachments: [],
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        const userMessage: ChannelMessage = {
          id: "message-user",
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        return { message: userMessage, run };
      },
    });
    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      return () => eventListeners.delete(onEvent);
    };

    render(<App apiClient={client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "empty first" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expectPendingLoader());

    const emptyAssistantMessage: ChannelMessage = {
      id: "message-assistant-empty",
      session_id: "session-1",
      role: "assistant",
      client_message_key: "hermes-run:hub-run-empty-assistant",
      content: "",
      attachments: [],
      created_at: Date.now(),
    };
    for (const listener of eventListeners) {
      listener({ type: "message_created", message: emptyAssistantMessage });
    }

    await waitFor(() => {
      expect(document.querySelector(".message-bubble.assistant.empty-body")).not.toBeInTheDocument();
      expectPendingLoader();
    });
  });

  it("removes the local pending bubble when the final Hub message arrives", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({
      answer: "final answer",
      answerDelay: deferred.promise,
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "finish cleanly" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expectPendingLoader());

    deferred.resolve();
    expect(await screen.findByText("final answer")).toBeInTheDocument();
    await waitFor(() => {
      expect(document.querySelector(".message-bubble.empty-body")).not.toBeInTheDocument();
    });
  });

  it("shows Hermes verbose tool progress while a response is pending", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({
      answer: "done",
      answerDelay: deferred.promise,
      executionEvents: [
        {
          kind: "tool.call",
          tool: "image_generate",
          detail: "{\"prompt\":\"cat\"}",
        },
      ],
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "make ppt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("call image generation：{\"prompt\":\"cat\"}")).toBeInTheDocument();
    expect(screen.getByLabelText("Hermes run log")).toBeInTheDocument();

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByText("done")).toBeInTheDocument();
    });
  });

  it("renders legacy Hermes tool logs as execution history", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              legacyExecutionMessage(
                `📚 skill_view(['name'])\n{"name":"comfyui"}\n🎨 image_generate(['aspect_ratio', 'prompt'])\n{"aspect_ratio":"portrait","prompt":"cat"}`,
              ),
            ],
          },
        })}
      />,
    );

    expect(await screen.findByText("Execution steps")).toBeInTheDocument();
    expect(screen.getByText('call skill view：{"name":"comfyui"}')).toBeInTheDocument();
    expect(
      screen.getByText('call image generation：{"aspect_ratio":"portrait","prompt":"cat"}'),
    ).toBeInTheDocument();
    expect(screen.queryByText("📚 skill_view(['name'])")).not.toBeInTheDocument();
  });

  it("keeps earlier execution blocks visible while a later run is pending", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              legacyExecutionMessage(
                `💻 terminal(['command'])\n{"command":"first tool"}`,
                "message-execution-first",
                1,
              ),
              {
                id: "message-final-first",
                session_id: "session-1",
                role: "assistant",
                content: "第一次最终结果",
                attachments: [],
                created_at: 2,
              },
              legacyExecutionMessage(
                `💻 terminal(['command'])\n{"command":"second tool"}`,
                "message-execution-second",
                3,
              ),
            ],
          },
          activeRunsBySessionId: {
            "session-1": {
              run_id: "hub-run-second",
              status: "running",
              created_at: 3,
              updated_at: 3,
            },
          },
        })}
      />,
    );

    expect(await screen.findByText('call terminal：{"command":"first tool"}')).toBeInTheDocument();
    expect(screen.getByText("第一次最终结果")).toBeInTheDocument();
    expect(screen.getByText('call terminal：{"command":"second tool"}')).toBeInTheDocument();
    expect(screen.getAllByText("Execution steps")).toHaveLength(2);
  });

  it("truncates long execution parameters and shows the header typing state", async () => {
    const deferred = createDeferred<void>();
    const longToolCall = "write report " + "x".repeat(60);
    const longToolResult = "result " + "y".repeat(60);
    const hubRun = createHubRunMock({
      answer: "done",
      answerDelay: deferred.promise,
      executionEvents: [
        { kind: "tool.call", tool: "terminal", detail: longToolCall },
        { kind: "tool.completed", tool: "terminal", detail: longToolResult },
      ],
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "run long tool" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    const expectedCall = `call terminal：${Array.from(longToolCall).slice(0, 50).join("")}…`;
    expect(await screen.findByText(expectedCall)).toBeInTheDocument();
    expect(screen.queryByText(`call terminal：${longToolCall}`)).not.toBeInTheDocument();
    const expectedResult = `completed terminal：${Array.from(longToolResult).slice(0, 50).join("")}…`;
    expect(screen.getByText(expectedResult)).toBeInTheDocument();
    expect(screen.queryByText(`completed terminal：${longToolResult}`)).not.toBeInTheDocument();
    expect(screen.getByText("Hermes is typing")).toBeInTheDocument();
    const pendingBubble = document.querySelector(".message-bubble.assistant.pending");
    expect(pendingBubble?.querySelector(".typing-indicator")).toBeInTheDocument();

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByText("done")).toBeInTheDocument();
    });
  });

  it("shows the header typing state while restoring an active run", async () => {
    const client = createMockApiClient({
      activeRunsBySessionId: {
        "session-1": {
          run_id: "hub-run-restored",
          status: "running",
          created_at: Date.now(),
          updated_at: Date.now(),
        },
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByText("Hermes is typing")).toBeInTheDocument();
    await waitFor(() => {
      const pendingBubble = document.querySelector(".message-bubble.assistant.pending");
      expect(pendingBubble?.querySelector(".typing-indicator")).toBeInTheDocument();
    });
  });

  it("keeps the loader on execution history that arrives before run state settles", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    const run: ChannelRun = {
      id: "run-storage-id",
      run_id: "hub-run-early-execution",
      session_id: "session-1",
      user_message_id: "message-user",
      status: "running",
      input: "early execution",
      input_attachments: [],
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        const userMessage: ChannelMessage = {
          id: "message-user",
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        const execution = executionMessage(
          [{ kind: "tool.call", tool: "terminal", detail: "early tool" }],
          "message-early-execution",
        );
        for (const listener of eventListeners) {
          listener({ type: "message_created", message: execution });
        }
        return { message: userMessage, run };
      },
    });
    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      return () => eventListeners.delete(onEvent);
    };

    render(<App apiClient={client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "early execution" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("call terminal：early tool")).toBeInTheDocument();
    await waitFor(() => {
      const pendingBubble = document.querySelector(".message-bubble.assistant.pending");
      expect(pendingBubble?.textContent).toContain("call terminal：early tool");
      expect(pendingBubble?.querySelector(".typing-indicator")).toBeInTheDocument();
    });
  });

  it("keeps the first live Hermes execution entry when multiple entries stream in", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({
      answer: "done",
      answerDelay: deferred.promise,
      executionEvents: [
        { kind: "tool.call", tool: "terminal", detail: "第一步" },
        { kind: "tool.completed", tool: "terminal", detail: "第二步" },
      ],
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "run steps" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("call terminal：第一步")).toBeInTheDocument();
    expect(screen.getByText("completed terminal：第二步")).toBeInTheDocument();

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getAllByText("call terminal：第一步")).toHaveLength(1);
      expect(screen.getAllByText("completed terminal：第二步")).toHaveLength(1);
    });
  });

  it("keeps Hermes execution history and appends the final answer as a separate bubble", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({
      answer: "最终结果",
      answerDelay: deferred.promise,
      executionEvents: [
        { kind: "tool.started", tool: "terminal", detail: "写入PPT脚本" },
        { kind: "tool.completed", tool: "terminal", detail: "生成PPT文件" },
      ],
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "make ppt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("start terminal：写入PPT脚本")).toBeInTheDocument();

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByText("Execution steps")).toBeInTheDocument();
      expect(screen.getByText("completed terminal：生成PPT文件")).toBeInTheDocument();
      expect(screen.getByText("最终结果")).toBeInTheDocument();
    });
  });

  it("keeps every persisted execution entry and deduplicates repeated final answers", async () => {
    const executionEvents = [
      { kind: "tool.call", tool: "terminal", detail: "echo 1" },
      { kind: "tool.completed", tool: "terminal", detail: "done" },
      { kind: "tool.call", tool: "image_generate", detail: "cat" },
    ];
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-execution",
                session_id: "session-1",
                role: "assistant",
                content: `<!-- hermes-hub:execution:v1 -->\n${JSON.stringify([
                  ...executionEvents,
                  ...executionEvents,
                  { kind: "tool.completed", tool: "image_generate", detail: "image done" },
                ])}`,
                attachments: [],
                created_at: 1,
              },
              {
                id: "message-final-1",
                session_id: "session-1",
                role: "assistant",
                content: "最终结果",
                attachments: [],
                created_at: 2,
              },
              {
                id: "message-final-2",
                session_id: "session-1",
                role: "assistant",
                content: "最终结果",
                attachments: [],
                created_at: 3,
              },
            ],
          },
        })}
      />,
    );

    expect(await screen.findAllByText("call terminal：echo 1")).toHaveLength(2);
    expect(screen.getByText("completed image generation：image done")).toBeInTheDocument();
    expect(screen.getAllByText("最终结果")).toHaveLength(1);
  });

  it("keeps legitimate repeated execution steps in the same run", async () => {
    const repeatedEvents = [
      { kind: "tool.call", tool: "terminal", detail: "echo 1" },
      { kind: "tool.call", tool: "terminal", detail: "echo 1" },
    ];

    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-repeated-execution",
                session_id: "session-1",
                role: "assistant",
                content: `<!-- hermes-hub:execution:v1 -->\n${JSON.stringify(repeatedEvents)}`,
                attachments: [],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    expect(await screen.findAllByText("call terminal：echo 1")).toHaveLength(2);
  });

  it("does not treat old assistant messages as the current Hub run response", async () => {
    const deferred = createDeferred<void>();
    const hubRun = createHubRunMock({
      answer: "new answer",
      answerDelay: deferred.promise,
      oldAssistant: {
        id: "old-answer",
        session_id: "session-1",
        role: "assistant",
        content: "stored answer",
        attachments: [],
        created_at: 1,
      },
    });

    render(<App apiClient={hubRun.client} />);

    expect(await screen.findByText("stored answer")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "new question" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expectPendingLoader());
    expect(screen.queryByText("new answer")).not.toBeInTheDocument();

    deferred.resolve();
    expect(await screen.findByText("new answer")).toBeInTheDocument();
  });

  it("keeps execution history when switching sessions during a run", async () => {
    let releasePersist: (() => void) | undefined;
    let activeRun: {
      run_id: string;
      status: "running";
      created_at: number;
      updated_at: number;
    } | null = null;
    const persistGate = new Promise<void>((resolve) => {
      releasePersist = resolve;
    });
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        const message: ChannelMessage = {
          id: "message-user",
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        activeRun = {
          run_id: "hub-run-restored",
          status: "running",
          created_at: 1,
          updated_at: 1,
        };
        queueMicrotask(() => {
          const events: HermesVerboseEvent[] = [
            {
              kind: "tool.call",
              tool: "skill_view",
              detail: "inspect image skill",
            },
          ];
          for (const listener of eventListeners) {
            listener({ type: "message_created", message: executionMessage(events) });
          }
        });
        return {
          message,
          run: {
            id: "run-restored",
            run_id: "hub-run-restored",
            session_id: sessionId,
            user_message_id: message.id,
            status: "running",
            input: input.content,
            input_attachments: input.attachments ?? [],
            created_at: 1,
            updated_at: 1,
          },
        };
      },
    });
    client.activeHermesRun = async () => activeRun;
    client.subscribeSessionEvents = (_channelId, sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(async () => {
        onEvent({
          type: "messages_snapshot",
          messages: await client.listSessionMessages("channel-1", sessionId),
          active_run: activeRun,
        });
      });
      return () => eventListeners.delete(onEvent);
    };
    client.clearHermesRun = async () => {
      activeRun = null;
    };
    const appendSessionMessage = client.appendSessionMessage.bind(client);
    const updateSessionMessage = client.updateSessionMessage.bind(client);

    client.appendSessionMessage = async (...args) => {
      const input = args[2];
      if (input.content.startsWith("<!-- hermes-hub:execution:v1 -->")) {
        await persistGate;
      }
      return appendSessionMessage(...args);
    };
    client.updateSessionMessage = async (...args) => {
      const input = args[3];
      if (input.content.startsWith("<!-- hermes-hub:execution:v1 -->")) {
        await persistGate;
      }
      return updateSessionMessage(...args);
    };
    client.listSessionMessages = async (_channelId, sessionId) => {
      if (sessionId === "session-1") {
        const events: HermesVerboseEvent[] = [
          {
            kind: "tool.call",
            tool: "skill_view",
            detail: "inspect image skill",
          },
        ];
        const base = [
          {
            id: "message-user",
            session_id: "session-1",
            role: "user" as const,
            content: "check skill",
            attachments: [],
            created_at: 1,
          },
          executionMessage(events),
        ];
        if (!activeRun) {
          return [
            ...base,
            {
              id: "message-final",
              session_id: "session-1",
              role: "assistant" as const,
              client_message_key: "hermes-run:hub-run-restored",
              content: "done",
              attachments: [],
              created_at: 3,
            },
          ];
        }
        return base;
      }
      return [];
    };
    render(<App apiClient={client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "check skill" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(screen.getByText("call skill view：inspect image skill")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: "New chat" }));
    await waitFor(() => {
      expect(screen.queryByText("call skill view：inspect image skill")).not.toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Session" }));
    expect(await screen.findByText("call skill view：inspect image skill")).toBeInTheDocument();

    releasePersist?.();
    activeRun = null;
    const finalMessage: ChannelMessage = {
      id: "message-final",
      session_id: "session-1",
      role: "assistant",
      client_message_key: "hermes-run:hub-run-restored",
      content: "done",
      attachments: [],
      created_at: 3,
    };
    for (const listener of eventListeners) {
      listener({ type: "message_created", message: finalMessage });
      listener({ type: "run_cleared", session_id: "session-1" });
    }
    await waitFor(() => {
      expect(screen.getAllByText("call skill view：inspect image skill")).toHaveLength(1);
      expect(screen.getAllByText("done")).toHaveLength(1);
    });
  });

  it("restores an active Hermes run after refresh and can stop it", async () => {
    const stopHermesRun = vi.fn(async () => null);

    render(
      <App
        apiClient={createMockApiClient({
          activeRunsBySessionId: {
            "session-1": {
              run_id: "hub-run-restored",
              status: "running",
              created_at: 1,
              updated_at: 1,
            },
          },
          stopHermesRun,
        })}
      />,
    );

    await waitFor(() => expectPendingLoader());
    const stopButton = screen.getByRole("button", { name: "Stop" });
    expect(stopButton).not.toBeDisabled();

    fireEvent.click(stopButton);
    await waitFor(() => {
      expect(stopHermesRun).toHaveBeenCalledWith("channel-1", "session-1");
      expectNoPendingLoader();
    });
  });

  it("does not create an empty final reply for a completed run without output id", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          activeRunsBySessionId: {
            "session-1": {
              run_id: "hub-run-empty-output",
              status: "completed",
              created_at: 1,
              updated_at: 2,
            },
          },
        })}
      />,
    );

    await screen.findByRole("button", { name: "New chat" });
    await waitFor(() => {
      expect(screen.queryByText("Hermes returned an empty response.")).not.toBeInTheDocument();
      expect(document.querySelector(".message-bubble.empty-body")).not.toBeInTheDocument();
    });
  });

  it("ignores stale native Hermes runs when restoring chat state", async () => {
    const client = createMockApiClient({
      activeRunsBySessionId: {
        "session-1": {
          run_id: "run-restored",
          status: "running",
          created_at: 1,
          updated_at: 1,
        },
      },
    });

    render(<App apiClient={client} />);

    await screen.findByRole("button", { name: "New chat" });
    await waitFor(() => expectNoPendingLoader());
  });

  it("shows explicit Hermes run errors as assistant messages", async () => {
    const hubRun = createHubRunMock({ error: "tool failed" });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "run a tool" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("Hermes run failed: tool failed")).toBeInTheDocument();
    expectNoPendingLoader();
  });

  it("deletes a session from the sidebar", async () => {
    const deleteSession = vi.fn(async () => undefined);

    render(<App apiClient={createMockApiClient({ deleteSession })} />);

    expect(await screen.findByRole("button", { name: "Session" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Delete session" }));

    await waitFor(() => {
      expect(deleteSession).toHaveBeenCalledWith("channel-1", "session-1");
      expect(screen.queryByRole("button", { name: "Session" })).not.toBeInTheDocument();
    });
  });

  it("shows the configured session limit message when a new session is blocked", async () => {
    const client = createMockApiClient({
      createSession: async () => {
        throw new ApiRequestError("session limit exceeded", {
          error: "session_limit_exceeded",
          max_sessions_per_user: 2,
        });
      },
    });

    render(<App apiClient={client} />);

    await screen.findByRole("button", { name: "Session" });
    fireEvent.click(screen.getByRole("button", { name: "New chat" }));

    await waitFor(() => {
      expect(
        screen.getByText(
          "Each user can have at most 2 sessions. Delete a session before creating a new one.",
        ),
      ).toBeInTheDocument();
    });
  });

  it("shows the OIDC redirect URI directly below Enable OIDC", async () => {
    render(<App apiClient={createMockApiClient()} />);

    await openSettingsTab("Authentication settings");

    const enableOidcRow = screen.getByLabelText("Enable OIDC").closest("label");
    const redirectInput = await screen.findByLabelText("OIDC Redirect URI");

    expect(redirectInput).toHaveValue(`${window.location.origin}/api/auth/oidc/callback`);
    expect(enableOidcRow?.nextElementSibling).toBe(redirectInput.closest("label"));
  });

  it("localizes the configured session limit message in Chinese", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    const client = createMockApiClient({
      createSession: async () => {
        throw new ApiRequestError("session limit exceeded", {
          error: "session_limit_exceeded",
          max_sessions_per_user: 2,
        });
      },
    });

    render(<App apiClient={client} />);

    await screen.findByRole("button", { name: "Session" });
    fireEvent.click(screen.getByRole("button", { name: "新建对话" }));

    await waitFor(() => {
      expect(
        screen.getByText("每个用户最多2个会话，你得先删除一个会话才能创建新会话"),
      ).toBeInTheDocument();
    });
  });

  it("renders login and authenticates with email and password", async () => {
    const client = createMockApiClient();
    await client.logout();

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
    const signInButton = await screen.findByRole("button", { name: "Sign in" });
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "admin@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "admin-password-123" },
    });
    fireEvent.click(signInButton);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    });
  });

  it("shows OIDC sign-in when it is enabled", async () => {
    const client = createMockApiClient({
      initialUser: null,
      oidcPublicConfig: {
        enabled: true,
        display_name: "Acme SSO",
      },
    });

    render(<App apiClient={client} />);

    const oidcButton = await screen.findByRole("link", { name: "Sign in with Acme SSO" });
    expect(oidcButton).toHaveAttribute("href", "/api/auth/oidc/start");
  });

  it("hides first-admin registration entry when bootstrap is closed", async () => {
    const client = createMockApiClient({ initialUser: null, bootstrapOpen: false });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Need to create the first admin?" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Confirm password")).not.toBeInTheDocument();
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
      expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
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

  it("returns to sign in after invite registration creates the account", async () => {
    window.history.pushState({}, "", "/?invite=secret-invite-token");
    const client = createMockApiClient({ initialUser: null, bootstrapOpen: false });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Create account" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "invited@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "invited-password-123" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "invited-password-123" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create account" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Sign in" })).toBeInTheDocument();
    });
    expect(screen.queryByRole("button", { name: "New chat" })).not.toBeInTheDocument();
    expect(window.location.search).toBe("");
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

    const settingsTabs = await openSettingsTab("User management");
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(
      screen.getByText(
        "Save usable Large language model, Title model in model configuration first.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create invite" })).toBeDisabled();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Hermes management" }));
    expect(await screen.findByRole("columnheader", { name: "Owner" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Hermes management" })).not.toBeInTheDocument();
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

    await openSettingsTab("Hermes management");
    expect(await screen.findAllByText("not_created")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Start" })).toBeInTheDocument();
    });
  });

  it("shows Hermes runtime version in the management table", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialInstance: {
            id: "instance-1",
            user_id: "user-1",
            kind: "managed_docker",
            status: "running",
            runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:1.2.3",
            runtime_version: "1.2.3",
          },
        })}
      />,
    );

    await openSettingsTab("Hermes management");

    expect(await screen.findByRole("columnheader", { name: "Version" })).toBeInTheDocument();
    expect(screen.getByText("1.2.3")).toBeInTheDocument();
  });

  it("does not show latest as the Hermes runtime version", async () => {
    render(<App apiClient={createMockApiClient()} />);

    await openSettingsTab("Hermes management");

    expect(await screen.findByRole("columnheader", { name: "Version" })).toBeInTheDocument();
    expect(screen.queryByText("ghcr.io/yiiilin/hermes-hub-hermes:latest")).not.toBeInTheDocument();
  });

  it("shows one Hermes status and the error detail in the management table", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialInstance: {
            id: "instance-1",
            user_id: "user-1",
            kind: "managed_docker",
            status: "error",
            health_status: "unhealthy",
            status_message: "curl: connection refused",
          },
        })}
      />,
    );

    await openSettingsTab("Hermes management");

    expect(await screen.findByText("error")).toBeInTheDocument();
    expect(screen.getByText("curl: connection refused")).toBeInTheDocument();
    expect(screen.queryByText("error / unhealthy")).not.toBeInTheDocument();
  });

  it("loads and displays Hermes scheduled tasks in system settings", async () => {
    const listHermesSchedulerSnapshots = vi.fn(async () => [
      {
        user_id: "user-1",
        user_email: "admin@example.com",
        hermes_instance_id: "instance-1",
        instance_status: "running",
        scheduler_enabled: true,
        running_jobs_count: 1,
        reported_at: 1_735_689_600,
        tasks: [
          {
            id: "task-daily-summary",
            name: "Daily summary",
            enabled: true,
            schedule: "0 9 * * *",
            timezone: "Asia/Shanghai",
            next_run_at: 1_735_722_000,
            last_run_at: 1_735_635_600,
            status: "scheduled",
            source: "hermes-adapter",
          },
        ],
      },
    ]);
    const client = Object.assign(createMockApiClient(), {
      listHermesSchedulerSnapshots,
    });

    render(<App apiClient={client} />);

    await openSettingsTab("Scheduled tasks");

    expect(listHermesSchedulerSnapshots).toHaveBeenCalled();
    const schedulerTable = await screen.findByRole("table", { name: "Scheduled tasks" });
    expect(within(schedulerTable).getByText("admin@example.com")).toBeInTheDocument();
    expect(within(schedulerTable).getByText("Daily summary")).toBeInTheDocument();
    expect(within(schedulerTable).getByText("0 9 * * *")).toBeInTheDocument();
    expect(within(schedulerTable).getByText("scheduled")).toBeInTheDocument();
  });

  it("shows the current user's scheduled tasks above personalization", async () => {
    const workspaceHermesSchedulerSnapshot = vi.fn(async () => ({
      user_id: "user-2",
      user_email: "user@example.com",
      hermes_instance_id: "instance-user",
      instance_status: "running",
      scheduler_enabled: true,
      running_jobs_count: 1,
      reported_at: 1_735_689_600,
      tasks: [
        {
          id: "task-user-daily",
          name: "User daily task",
          enabled: true,
          schedule: "0 9 * * *",
          timezone: "Asia/Shanghai",
          next_run_at: 1_735_722_000,
          last_run_at: 1_735_635_600,
          status: "scheduled",
          source: "hermes-adapter",
        },
      ],
    }));
    const client = Object.assign(
      createMockApiClient({
        initialUser: {
          id: "user-2",
          email: "user@example.com",
          role: "user",
          status: "active",
        },
      }),
      { workspaceHermesSchedulerSnapshot },
    );

    render(<App apiClient={client} />);

    const scheduledTasksNav = await screen.findByRole("button", { name: "Scheduled tasks" });
    const personalizationNav = screen.getByRole("button", { name: "Personalization" });
    expect(
      scheduledTasksNav.compareDocumentPosition(personalizationNav) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(scheduledTasksNav);

    expect(workspaceHermesSchedulerSnapshot).toHaveBeenCalled();
    const schedulerTable = await screen.findByRole("table", { name: "Scheduled tasks" });
    expect(within(schedulerTable).getByText("User daily task")).toBeInTheDocument();
    expect(within(schedulerTable).getByText("0 9 * * *")).toBeInTheDocument();
    expect(within(schedulerTable).getByText("scheduled")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "System settings" })).not.toBeInTheDocument();
  });

  it("shows header typing status without the animated dots", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          activeRunsBySessionId: {
            "session-1": {
              run_id: "hub-run-header-typing",
              status: "running",
              created_at: Date.now(),
              updated_at: Date.now(),
            },
          },
        })}
      />,
    );

    const headerTyping = await screen.findByText("Hermes is typing");
    expect(headerTyping.closest(".header-typing")?.querySelector(".typing-dots")).toBeNull();
  });

  it("shows pending feedback while creating a managed Hermes instance", async () => {
    const deferred = createDeferred<void>();
    const client = createMockApiClient({ initialInstance: null });
    const originalCreateHermesInstance = client.createHermesInstance;
    const createHermesInstance = vi.fn(async (userId: string) => {
      await deferred.promise;
      return originalCreateHermesInstance(userId);
    });
    client.createHermesInstance = createHermesInstance;

    render(<App apiClient={client} />);

    await openSettingsTab("Hermes management");
    fireEvent.click(await screen.findByRole("button", { name: "Create" }));

    expect(await screen.findByRole("button", { name: "Creating..." })).toBeDisabled();
    expect(createHermesInstance).toHaveBeenCalledWith("user-1");

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Start" })).toBeInTheDocument();
    });
  });

  it.each([
    ["Start", "Starting...", "startHermesInstance", "running"],
    ["Stop", "Stopping...", "stopHermesInstance", "stopped"],
    ["Rebuild", "Rebuilding...", "rebuildHermesInstance", "running"],
  ] as const)(
    "shows pending feedback while %s runs",
    async (buttonName, pendingName, methodName, nextStatus) => {
      const deferred = createDeferred<void>();
      const client = createMockApiClient();
      const originalAction = client[methodName];
      const action = vi.fn(async (userId: string) => {
        await deferred.promise;
        return originalAction(userId);
      });
      client[methodName] = action;

      render(<App apiClient={client} />);

      await openSettingsTab("Hermes management");
      fireEvent.click(await screen.findByRole("button", { name: buttonName }));

      expect(await screen.findByRole("button", { name: pendingName })).toBeDisabled();
      expect(action).toHaveBeenCalledWith("user-1");

      expect(nextStatus).toMatch(/running|stopped/);
      deferred.resolve();
      await waitFor(() => {
        expect(screen.getByRole("button", { name: buttonName })).toBeInTheDocument();
      });
    },
  );

  it("lets admins manage shared skills from the Chinese navigation", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    // 用可观测的 mock API 验证页面不会只更新本地状态。
    const saveManagedSkill = vi.fn(async (path: string, content: string) => ({ path, content }));
    const deleteManagedSkill = vi.fn(async () => undefined);
    const createManagedSkillDirectory = vi.fn(async () => undefined);

    render(
      <App
        apiClient={createMockApiClient({
          initialManagedSkills: {
            "image/SKILL.md": "# Image\n\nUse sharp visual prompts.\n",
            "writing/references/style.md": "Use direct language.\n",
          },
          initialManagedSkillDirectories: ["research", "writing/drafts/empty-child"],
          saveManagedSkill,
          deleteManagedSkill,
          createManagedSkillDirectory,
        })}
      />,
    );

    await openSettingsTab("统一 Skill 管理", "系统设置");

    expect(await screen.findByRole("button", { name: "文件夹 writing" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "统一 Skill 管理" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "文件夹 research" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /writing\/references\/style\.md/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /image\/SKILL\.md/ }));
    expect(await screen.findByLabelText("Skill 路径")).toHaveValue("image/SKILL.md");
    expect(screen.getByLabelText("Skill 内容")).toHaveValue(
      "# Image\n\nUse sharp visual prompts.\n",
    );

    fireEvent.change(screen.getByLabelText("Skill 内容"), {
      target: { value: "# Image\n\nUse cinematic visual prompts.\n" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(saveManagedSkill).toHaveBeenCalledWith(
        "image/SKILL.md",
        "# Image\n\nUse cinematic visual prompts.\n",
      );
    });
    expect(await screen.findByText("Skill 已保存")).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "删除" })).not.toBeDisabled();
    });
    fireEvent.click(screen.getByRole("button", { name: "删除" }));

    await waitFor(() => {
      expect(deleteManagedSkill).toHaveBeenCalledWith("image/SKILL.md");
      expect(screen.queryByRole("button", { name: /image\/SKILL\.md/ })).not.toBeInTheDocument();
    });
    expect(screen.getByLabelText("Skill 路径")).toHaveValue("");
    expect(screen.getByLabelText("Skill 内容")).toHaveValue("");
    expect(screen.getByRole("button", { name: "删除" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "文件夹 writing" }));
    fireEvent.click(screen.getByRole("button", { name: "新建文件夹" }));
    expect(screen.getByLabelText("Skill 路径")).toHaveValue("writing/new-folder");
    fireEvent.change(screen.getByLabelText("Skill 路径"), {
      target: { value: "writing/drafts" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建文件夹" }));
    await waitFor(() => {
      expect(createManagedSkillDirectory).toHaveBeenCalledWith("writing/drafts");
      expect(screen.getByRole("button", { name: "文件夹 writing/drafts" })).toBeInTheDocument();
    });

    fireEvent.change(screen.getByLabelText("Skill 路径"), {
      target: { value: "writing/archive" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建文件夹" }));
    await waitFor(() => {
      expect(createManagedSkillDirectory).toHaveBeenCalledWith("writing/archive");
      expect(createManagedSkillDirectory).toHaveBeenCalledWith("writing/archive/empty-child");
      expect(deleteManagedSkill).toHaveBeenCalledWith("writing/drafts");
      expect(screen.getByRole("button", { name: "文件夹 writing/archive" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "文件夹 writing/archive/empty-child" })).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "文件夹 writing/archive" }));
    fireEvent.click(screen.getByRole("button", { name: "新建 Skill" }));
    expect(screen.getByLabelText("Skill 路径")).toHaveValue("writing/archive/SKILL.md");
    expect(screen.getByLabelText("Skill 内容")).toHaveValue("");
  });

  it("opens managed skill management when the tree endpoint is not available yet", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    const apiClient = createMockApiClient({
      initialManagedSkills: {
        "image/SKILL.md": "# Image\n",
      },
    });
    apiClient.listManagedSkillTree = vi.fn(async () => {
      throw new Error("managed skill not found");
    });

    render(<App apiClient={apiClient} />);

    await openSettingsTab("统一 Skill 管理", "系统设置");

    expect(await screen.findByRole("button", { name: /image\/SKILL\.md/ })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "统一 Skill 管理" })).not.toBeInTheDocument();
    expect(screen.queryByText("managed skill not found")).not.toBeInTheDocument();
  });

  it("uploads managed skill folders and zip archives from the admin tree", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    const uploadManagedSkills = vi.fn(async (files: File[], targetPath?: string) =>
      files.map((file) => ({
        path: `${targetPath ? `${targetPath}/` : ""}${(file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name}`,
        size: file.size,
      })),
    );

    render(
      <App
        apiClient={createMockApiClient({
          uploadManagedSkills,
        })}
      />,
    );

    await openSettingsTab("统一 Skill 管理", "系统设置");
    fireEvent.click(await screen.findByRole("button", { name: "文件夹 writing" }));

    const folderFile = new File(["# Research\n"], "SKILL.md", { type: "text/markdown" });
    Object.defineProperty(folderFile, "webkitRelativePath", {
      value: "research/SKILL.md",
    });
    fireEvent.change(screen.getByTestId("managed-skills-folder-input"), {
      target: { files: [folderFile] },
    });
    await waitFor(() => {
      expect(uploadManagedSkills).toHaveBeenCalledWith([folderFile], "writing");
      expect(screen.getByRole("button", { name: /writing\/research\/SKILL\.md/ })).toBeInTheDocument();
    });

    const zipFile = new File(["zip"], "skills.zip", { type: "application/zip" });
    fireEvent.change(screen.getByTestId("managed-skills-zip-input"), {
      target: { files: [zipFile] },
    });
    await waitFor(() => {
      expect(uploadManagedSkills).toHaveBeenLastCalledWith([zipFile], "writing");
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
          api_type: "responses",
          reasoning_effort: "medium",
          allow_streaming: true,
          request_timeout_seconds: 60,
          context_window_tokens: 200000,
          max_output_tokens: 8192,
          temperature: 0.3,
          supports_parallel_tools: true,
        },
        model_configs: [],
        required_models_ready: true,
        missing_required_model_config_kinds: [],
      }),
    } as Response);

    const status = await createApiClient().modelConfigStatus();

    expect(status.model_config.provider_api_key).toBe("stored-provider-key");
    expect(status.model_config.context_window_tokens).toBe(200000);
    expect(status.model_config.max_output_tokens).toBe(8192);
    expect(status.model_config.temperature).toBe(0.3);
    expect(status.model_config.supports_parallel_tools).toBe(true);
    fetchMock.mockRestore();
  });

  it("sends main model runtime limits through the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue({
      ok: true,
      status: 204,
    } as Response);

    await createApiClient().updateModelConfig({
      config_kind: "llm",
      enabled: true,
      provider_name: "openai-compatible",
      provider_base_url: "https://ready-provider.example/v1",
      provider_api_key: "stored-provider-key",
      default_model: "gpt-4.1-mini",
      allowed_models: ["gpt-4.1-mini"],
      api_type: "responses",
      reasoning_effort: "medium",
      allow_streaming: true,
      request_timeout_seconds: 60,
      context_window_tokens: 200000,
      max_output_tokens: 8192,
      temperature: 0.3,
      supports_parallel_tools: false,
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/admin/model-config",
      expect.objectContaining({
        method: "PUT",
        body: expect.stringContaining('"context_window_tokens":200000'),
      }),
    );
    const requestBody = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(requestBody).toMatchObject({
      context_window_tokens: 200000,
      max_output_tokens: 8192,
      temperature: 0.3,
      supports_parallel_tools: false,
    });
    fetchMock.mockRestore();
  });

  it("uploads attachments and reads persisted messages in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      if (path === "/api/channels/channel-1/sessions/session-1/attachments") {
        expect(init?.method).toBe("POST");
        expect(init?.body).toBeInstanceOf(FormData);
        return {
          ok: true,
          status: 201,
          json: async () => ({
            attachments: [
              {
                id: "attachment-1",
                name: "note.txt",
                content_type: "text/plain",
                kind: "file",
                size: 5,
                download_url: "/api/attachments/attachment-1/download",
              },
            ],
          }),
        } as Response;
      }

      if (path === "/api/channels/channel-1/sessions/session-1/messages") {
        if (init?.method === "POST") {
          const body = JSON.parse(init?.body as string);
          expect(body).toMatchObject({
            role: "user",
            content: "hello",
            client_message_key: "client-key-1",
          });
          return {
            ok: true,
            status: 201,
            json: async () => ({
              message: {
                id: "message-2",
                session_id: "session-1",
                role: "user",
                client_message_key: "client-key-1",
                content: "hello",
                attachments: [],
                created_at: 1,
              },
            }),
          } as Response;
        }
        return {
          ok: true,
          status: 200,
          json: async () => ({
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
          }),
        } as Response;
      }

      if (path === "/api/channels/channel-1/sessions/session-1/messages/message-1") {
        expect(init?.method).toBe("PUT");
        expect(JSON.parse(init?.body as string)).toMatchObject({ content: "updated answer" });
        return {
          ok: true,
          status: 200,
          json: async () => ({
            message: {
              id: "message-1",
              session_id: "session-1",
              role: "assistant",
              content: "updated answer",
              attachments: [],
              created_at: 1,
            },
          }),
        } as Response;
      }

      throw new Error(`unexpected fetch ${String(path)}`);
    });

    const client = createApiClient() as any;
    await expect(
      client.uploadSessionAttachments("channel-1", "session-1", [
        new File(["hello"], "note.txt", { type: "text/plain" }),
      ]),
    ).resolves.toEqual([
      expect.objectContaining({
        id: "attachment-1",
        download_url: "/api/attachments/attachment-1/download",
      }),
    ]);
    await expect(client.listSessionMessages("channel-1", "session-1")).resolves.toEqual([
      expect.objectContaining({ content: "stored answer" }),
    ]);
    await expect(
      client.appendSessionMessage("channel-1", "session-1", {
        role: "user",
        content: "hello",
        attachments: [],
        clientMessageKey: "client-key-1",
      }),
    ).resolves.toEqual(expect.objectContaining({ content: "hello" }));
    await expect(
      client.updateSessionMessage("channel-1", "session-1", "message-1", {
        content: "updated answer",
        attachments: [],
      }),
    ).resolves.toEqual(expect.objectContaining({ content: "updated answer" }));
    fetchMock.mockRestore();
  });

  it("uses FormData for managed skill uploads in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      expect(path).toBe("/api/admin/managed-skills/upload");
      expect(init?.method).toBe("POST");
      expect(init?.credentials).toBe("include");
      expect(init?.body).toBeInstanceOf(FormData);
      const form = init?.body as FormData;
      expect(form.get("target_path")).toBe("writing");
      const files = form.getAll("files");
      expect(files).toHaveLength(2);
      expect(files[0]).toBeInstanceOf(File);
      expect((files[0] as File).name).toBe("research/SKILL.md");
      expect((files[1] as File).name).toBe("skills.zip");
      return {
        ok: true,
        status: 201,
        json: async () => ({
          skills: [
            { path: "writing/research/SKILL.md", size: 5 },
            { path: "writing/assistant/SKILL.md", size: 5 },
          ],
        }),
      } as Response;
    });

    const folderFile = new File(["hello"], "SKILL.md", { type: "text/markdown" });
    Object.defineProperty(folderFile, "webkitRelativePath", {
      value: "research/SKILL.md",
    });
    const zipFile = new File(["hello"], "skills.zip", { type: "application/zip" });

    await expect(
      createApiClient().uploadManagedSkills([folderFile, zipFile], "writing"),
    ).resolves.toEqual([
      { path: "writing/research/SKILL.md", size: 5 },
      { path: "writing/assistant/SKILL.md", size: 5 },
    ]);
    fetchMock.mockRestore();
  });

  it("uses active run, stop, clear, and session delete endpoints in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      if (path === "/api/channels/channel-1/sessions/session-1/active-run") {
        if (init?.method === "DELETE") {
          return {
            ok: true,
            status: 204,
            json: async () => ({}),
          } as Response;
        }
        return {
          ok: true,
          status: 200,
          json: async () => ({
            active_run: {
              run_id: "run-1",
              status: "running",
              created_at: 1,
              updated_at: 1,
            },
          }),
        } as Response;
      }

      if (path === "/api/channels/channel-1/sessions/session-1/active-run/stop") {
        expect(init?.method).toBe("POST");
        return {
          ok: true,
          status: 200,
          json: async () => ({ active_run: null }),
        } as Response;
      }

      if (path === "/api/channels/channel-1/sessions/session-1") {
        expect(init?.method).toBe("DELETE");
        return {
          ok: true,
          status: 204,
          json: async () => ({}),
        } as Response;
      }

      throw new Error(`unexpected fetch ${String(path)}`);
    });

    const client = createApiClient();
    await expect(client.activeHermesRun("channel-1", "session-1")).resolves.toMatchObject({
      run_id: "run-1",
    });
    await expect(client.stopHermesRun("channel-1", "session-1")).resolves.toBeNull();
    await expect(client.clearHermesRun("channel-1", "session-1")).resolves.toBeUndefined();
    await expect(client.deleteSession("channel-1", "session-1")).resolves.toBeUndefined();
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
