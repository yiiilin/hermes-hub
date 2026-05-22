import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { App } from "./app";
import {
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

  function expectPendingLoader() {
    expect(document.querySelector(".typing-indicator .typing-dots")).toBeInTheDocument();
    expect(screen.queryByText("Hermes is typing")).not.toBeInTheDocument();
    expect(screen.queryByText("Hermes is responding")).not.toBeInTheDocument();
  }

  function expectNoPendingLoader() {
    expect(document.querySelector(".typing-dots")).not.toBeInTheDocument();
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
    const clearHermesRun = vi.fn(async () => {
      activeRun = null;
    });
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
    client.clearHermesRun = clearHermesRun;

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
    expect(clearHermesRun).toHaveBeenCalledWith("channel-1", "session-1");
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
    const clearHermesRun = vi.fn(async () => undefined);
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
    client.clearHermesRun = clearHermesRun;

    render(<App apiClient={client} />);

    expect(await screen.findByText("最终答案")).toBeInTheDocument();
    await waitFor(() => {
      expect(clearHermesRun).toHaveBeenCalledWith("channel-1", "session-1");
    });
    expect(screen.getAllByText("最终答案")).toHaveLength(1);
    await expect(client.listSessionMessages("channel-1", "session-1")).resolves.toHaveLength(1);
  });

  it("renders the authenticated admin workspace and can send a Hermes prompt", async () => {
    render(<App apiClient={createMockApiClient()} />);

    expect(await screen.findByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "对话" })).not.toBeInTheDocument();
    expect(screen.queryByText("running")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Session" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "User management" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Model configuration" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hermes management" })).toBeInTheDocument();

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

    fireEvent.click(screen.getByRole("button", { name: "User management" }));
    expect(await screen.findByRole("heading", { name: "User management" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Session" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Model configuration" }));
    expect(await screen.findByRole("heading", { name: "Large language model" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Image model" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Title model" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Save" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Test" })).toHaveLength(3);
    const apiKeyInputs = screen.getAllByLabelText("API key");
    expect(apiKeyInputs[0]).toHaveAttribute("type", "password");
    expect(apiKeyInputs[0]).toHaveValue("ready-provider-key");
    fireEvent.click(screen.getAllByRole("button", { name: "Test" })[0]);
    expect(await screen.findByText("model test succeeded")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Hermes management" }));
    expect(await screen.findByRole("heading", { name: "Hermes management" })).toBeInTheDocument();
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

  it("keeps live tool calls complete but truncates long tool results", async () => {
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

    expect(await screen.findByText(`call terminal：${longToolCall}`)).toBeInTheDocument();
    const expectedResult = `completed terminal：${Array.from(longToolResult).slice(0, 50).join("")}…`;
    expect(screen.getByText(expectedResult)).toBeInTheDocument();
    expect(screen.queryByText(`completed terminal：${longToolResult}`)).not.toBeInTheDocument();
    const pendingBubble = document.querySelector(".message-bubble.assistant.pending");
    expect(pendingBubble?.lastElementChild).toHaveClass("typing-indicator");

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByText("done")).toBeInTheDocument();
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

  it("deduplicates replayed execution history and repeated final answers from persisted messages", async () => {
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

    expect(await screen.findByText("call terminal：echo 1")).toBeInTheDocument();
    expect(screen.getAllByText("call terminal：echo 1")).toHaveLength(1);
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
    const clearHermesRun = vi.fn(async () => undefined);
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
    client.clearHermesRun = clearHermesRun;

    render(<App apiClient={client} />);

    await waitFor(() => {
      expect(clearHermesRun).toHaveBeenCalledWith("channel-1", "session-1");
    });
    expectNoPendingLoader();
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

  it("blocks invites and Hermes start controls until required models are ready", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          requiredModelsReady: false,
          missingRequiredModelConfigKinds: ["llm", "title"],
        })}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "User management" }));
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(
      screen.getByText(
        "Save usable Large language model, Title model in model configuration first.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create invite" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Hermes management" }));
    expect(await screen.findByRole("heading", { name: "Hermes management" })).toBeInTheDocument();
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

    fireEvent.click(await screen.findByRole("button", { name: "Hermes management" }));
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
          api_type: "responses",
          reasoning_effort: "medium",
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
      const encoder = new TextEncoder();
      return {
        ok: true,
        status: 200,
        body: new ReadableStream({
          start(controller) {
            controller.enqueue(encoder.encode('data: {"event":"message.delta","delta":"he"}\n'));
            controller.enqueue(
              encoder.encode('data: {"event":"message.delta","delta":"llo"}\n'),
            );
            controller.enqueue(
              encoder.encode('data: {"event":"run.completed","output":"hello"}\n\n'),
            );
            controller.close();
          },
        }),
      } as Response;
    });

    const deltas: string[] = [];
    await expect(
      createApiClient().sendHermesPrompt("hello", [], "session-1", {
        onDelta(delta) {
          deltas.push(delta);
        },
      }),
    ).resolves.toBe("hello");
    expect(deltas).toEqual(["he", "llo"]);
    fetchMock.mockRestore();
  });

  it("reads Hermes verbose SSE events in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path) => {
      if (path === "/api/hermes/v1/runs") {
        return {
          ok: true,
          status: 202,
          json: async () => ({ run_id: "run-verbose", status: "started" }),
        } as Response;
      }

      expect(path).toBe("/api/hermes/v1/runs/run-verbose/events");
      const encoder = new TextEncoder();
      return {
        ok: true,
        status: 200,
        body: new ReadableStream({
          start(controller) {
            controller.enqueue(
              encoder.encode(
                `event: tool.started\ndata: ${JSON.stringify({
                  tool: "terminal",
                  preview:
                    "node -e \"try{require('pptxgenjs'); console.log('pptxgenjs ok')}\"",
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode(
                `event: approval.request\ndata: ${JSON.stringify({
                  command:
                    "node -e \"try{require('pptxgenjs'); console.log('pptxgenjs ok')}\"",
                  description: "script execution via -e/-c flag",
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode(
                `event: approval.responded\ndata: ${JSON.stringify({
                  choice: "session",
                  resolved: 1,
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode(
                `event: tool.completed\ndata: ${JSON.stringify({
                  tool: "terminal",
                  command:
                    "node -e \"try{require('pptxgenjs'); console.log('pptxgenjs ok')}\"",
                  output: "command finished",
                  duration: 1.234,
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode(
                `event: response.output_item.added\ndata: ${JSON.stringify({
                  item: {
                    type: "function_call",
                    name: "image_generate",
                    arguments: "{\"prompt\":\"小学生加减法配图\"}",
                  },
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode(
                `data: ${JSON.stringify({
                  event: "reasoning.available",
                  text: "准备生成 PPT 文件",
                })}\n\n`,
              ),
            );
            controller.enqueue(
              encoder.encode('data: {"event":"run.completed","output":"done"}\n\n'),
            );
            controller.close();
          },
        }),
      } as Response;
    });

    const verbose: unknown[] = [];
    await expect(
      createApiClient().sendHermesPrompt("hello", [], "session-1", {
        onVerbose(message) {
          verbose.push(message);
        },
      }),
    ).resolves.toBe("done");
    expect(verbose).toHaveLength(6);
    expect(verbose[0]).toMatchObject({
      kind: "tool.started",
      tool: "terminal",
      detail: "node -e \"try{require('pptxgenjs'); console.log('pptxgenjs ok')}\"",
    });
    expect(verbose[1]).toMatchObject({
      kind: "approval.request",
      detail: "node -e \"try{require('pptxgenjs'); console.log('pptxgenjs ok')}\"",
    });
    expect(verbose[2]).toMatchObject({ kind: "approval.responded", choice: "session" });
    expect(verbose[3]).toMatchObject({
      kind: "tool.completed",
      tool: "terminal",
      detail: "command finished",
    });
    expect(verbose[4]).toMatchObject({
      kind: "tool.call",
      tool: "image_generate",
      detail: "{\"prompt\":\"小学生加减法配图\"}",
    });
    expect(verbose[5]).toMatchObject({
      kind: "text",
      detail: "准备生成 PPT 文件",
    });
    fetchMock.mockRestore();
  });

  it("reconnects Hermes run events after a transport interruption", async () => {
    let eventRequests = 0;
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      if (path === "/api/hermes/v1/runs") {
        return {
          ok: true,
          status: 202,
          json: async () => ({ run_id: "run-image", status: "started" }),
        } as Response;
      }

      expect(path).toBe("/api/hermes/v1/runs/run-image/events");
      eventRequests += 1;
      const encoder = new TextEncoder();
      if (eventRequests === 2) {
        const headers = new Headers(init?.headers);
        expect(Number(headers.get("X-Hermes-Hub-Received-Bytes"))).toBeGreaterThan(0);
        return {
          ok: true,
          status: 200,
          body: new ReadableStream({
            start(controller) {
              controller.enqueue(
                encoder.encode('data: {"event":"message.delta","delta":"cat.png"}\n'),
              );
              controller.enqueue(
                encoder.encode(
                  'data: {"event":"run.completed","output":"生成好了：\\n/config/cache/images/cat.png"}\n\n',
                ),
              );
              controller.close();
            },
          }),
        } as Response;
      }

      let sentChunk = false;
      return {
        ok: true,
        status: 200,
        body: new ReadableStream({
          pull(controller) {
            if (sentChunk) {
              controller.error(new Error("Load failed"));
              return;
            }

            sentChunk = true;
            controller.enqueue(
              encoder.encode(
                'data: {"event":"message.delta","delta":"生成好了：\\n/config/cache/images/"}\n',
              ),
            );
          },
        }),
      } as Response;
    });

    const outputs: string[] = [];
    await expect(
      createApiClient().sendHermesPrompt("画猫", [], "session-1", {
        onOutput(output) {
          outputs.push(output);
        },
      }),
    ).resolves.toBe("生成好了：\n/config/cache/images/cat.png");
    expect(outputs).toEqual(["生成好了：\n/config/cache/images/cat.png"]);
    expect(eventRequests).toBe(2);
    fetchMock.mockRestore();
  });

  it("does not hide explicit Hermes run failures behind partial stream text", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path) => {
      if (path === "/api/hermes/v1/runs") {
        return {
          ok: true,
          status: 202,
          json: async () => ({ run_id: "run-failed", status: "started" }),
        } as Response;
      }

      expect(path).toBe("/api/hermes/v1/runs/run-failed/events");
      const encoder = new TextEncoder();
      return {
        ok: true,
        status: 200,
        body: new ReadableStream({
          start(controller) {
            controller.enqueue(encoder.encode('data: {"event":"message.delta","delta":"partial"}\n'));
            controller.enqueue(
              encoder.encode('data: {"event":"run.failed","error":"tool failed"}\n\n'),
            );
            controller.close();
          },
        }),
      } as Response;
    });

    await expect(createApiClient().sendHermesPrompt("hello", [], "session-1")).rejects.toThrow(
      "tool failed",
    );
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

  it("uses active run, stop, resume, clear, and session delete endpoints in the real API client", async () => {
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

      if (path === "/api/hermes/v1/runs/run-1/events") {
        const encoder = new TextEncoder();
        return {
          ok: true,
          status: 200,
          body: new ReadableStream({
            start(controller) {
              controller.enqueue(
                encoder.encode('data: {"event":"run.completed","output":"done"}\n\n'),
              );
              controller.close();
            },
          }),
        } as Response;
      }

      throw new Error(`unexpected fetch ${String(path)}`);
    });

    const client = createApiClient();
    await expect(client.activeHermesRun("channel-1", "session-1")).resolves.toMatchObject({
      run_id: "run-1",
    });
    await expect(client.resumeHermesRun("run-1")).resolves.toBe("done");
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
