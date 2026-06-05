import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const vditorMock = vi.hoisted(() => {
  const instances: Array<{
    destroy: ReturnType<typeof vi.fn>;
    element: HTMLElement;
    getValue: ReturnType<typeof vi.fn>;
    options: Record<string, unknown>;
    setValue: ReturnType<typeof vi.fn>;
    value: string;
  }> = [];

  const Vditor = vi.fn(function (
    this: {
      destroy: ReturnType<typeof vi.fn>;
      element: HTMLElement;
      getValue: ReturnType<typeof vi.fn>;
      options: Record<string, unknown>;
      setValue: ReturnType<typeof vi.fn>;
      value: string;
    },
    element: HTMLElement,
    options: Record<string, unknown> = {},
  ) {
    this.element = element;
    this.options = options;
    this.value = String(options.value ?? "");
    this.destroy = vi.fn();
    this.getValue = vi.fn(() => this.value);
    this.setValue = vi.fn((nextValue: string) => {
      this.value = nextValue;
      this.element.textContent = nextValue;
    });
    element.classList.add("vditor");
    element.textContent = this.value;
    instances.push(this);
    queueMicrotask(() => (options.after as (() => void) | undefined)?.());
  });

  return { instances, Vditor };
});

vi.mock("vditor", () => ({
  default: vditorMock.Vditor,
}));

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
  type HermesInstance,
  type HermesVerboseEvent,
  type PublicPlatformSessionSummary,
} from "./api/client";
import { createClientMessageId } from "./routes/channel-session";

describe("App", () => {
  afterEach(() => {
    cleanup();
    vditorMock.instances.splice(0);
    vditorMock.Vditor.mockClear();
    vi.useRealTimers();
    vi.restoreAllMocks();
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

  function executionMessage(
    content: HermesVerboseEvent[],
    id = "message-execution",
  ): ChannelMessage {
    return {
      id,
      session_id: "session-1",
      role: "assistant",
      message_kind: "execution",
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
      message_kind: "execution",
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

  function managedSkillTreeButton(path: string): HTMLButtonElement {
    const button = Array.from(
      document.querySelectorAll<HTMLButtonElement>("[data-managed-skill-path]"),
    ).find((candidate) => candidate.getAttribute("data-managed-skill-path") === path);
    expect(button).toBeDefined();
    return button!;
  }

  function vditorInstanceForLabel(label: string) {
    const editor = screen.getByRole("textbox", { name: label });
    const instance = vditorMock.instances.find((candidate) => candidate.element === editor);
    expect(instance).toBeDefined();
    return instance!;
  }

  function expectedMessageTime(timestampSeconds: number) {
    return new Intl.DateTimeFormat("en", {
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(timestampSeconds * 1000));
  }

  function publicHermesInstance(overrides: Partial<HermesInstance> = {}): HermesInstance {
    return {
      id: "public-instance-1",
      user_id: "public-user-1",
      kind: "managed_docker",
      status: "running",
      health_status: "healthy",
      runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:v2026.5.29.2",
      runtime_version: "v2026.5.29.2",
      last_user_activity_at: null,
      last_started_at: 1_735_689_600,
      last_stopped_at: null,
      stopped_reason: null,
      ...overrides,
    };
  }

  function installStreamingSpeechMock(
    options: { deferGetUserMedia?: boolean; deferOpen?: boolean } = {},
  ) {
    const originalMediaDevices = Object.getOwnPropertyDescriptor(navigator, "mediaDevices");
    const originalAudioContext = Object.getOwnPropertyDescriptor(globalThis, "AudioContext");
    const tracksByCall = [[{ stop: vi.fn() }], [{ stop: vi.fn() }]];
    const mediaStreams = tracksByCall.map(
      (tracks) =>
        ({
          getTracks: () => tracks,
        }) as unknown as MediaStream,
    );
    const getUserMediaDeferred = options.deferGetUserMedia ? createDeferred<MediaStream>() : null;
    let getUserMediaCallCount = 0;
    const getUserMediaSpy = vi.fn(() => {
      const callIndex = getUserMediaCallCount;
      getUserMediaCallCount += 1;
      if (getUserMediaDeferred && callIndex === 0) {
        return getUserMediaDeferred.promise;
      }
      return Promise.resolve(mediaStreams[Math.min(callIndex, mediaStreams.length - 1)]);
    });
    const stopSpy = vi.fn();
    const recorderConstructedSpy = vi.fn();
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: getUserMediaSpy,
      },
    });

    const audioContextCloseSpy = vi.fn(async () => undefined);
    const disconnectSpy = vi.fn();
    const connectSpy = vi.fn();
    const createScriptProcessorSpy = vi.fn();
    const processor = {
      connect: connectSpy,
      disconnect: disconnectSpy,
      onaudioprocess: null as
        | ((event: { inputBuffer: { getChannelData: () => Float32Array } }) => void)
        | null,
    };

    class MockAudioContext {
      sampleRate = 16000;
      destination = {};

      createMediaStreamSource() {
        return {
          connect: connectSpy,
          disconnect: disconnectSpy,
        };
      }

      createScriptProcessor(
        bufferSize?: number,
        numberOfInputChannels?: number,
        numberOfOutputChannels?: number,
      ) {
        createScriptProcessorSpy(bufferSize, numberOfInputChannels, numberOfOutputChannels);
        return processor;
      }

      createGain() {
        return {
          connect: connectSpy,
          disconnect: disconnectSpy,
          gain: { value: 0 },
        };
      }

      close = audioContextCloseSpy;
    }

    Object.defineProperty(globalThis, "AudioContext", {
      configurable: true,
      value: MockAudioContext,
    });

    let streamHandlers: Parameters<ApiClient["openSpeechInputStream"]>[0] | null = null;
    const connection = {
      close: vi.fn(),
      sendAudio: vi.fn(),
      sendStart: vi.fn(),
      stop: vi.fn(),
    };

    const restore = () => {
      if (originalMediaDevices) {
        Object.defineProperty(navigator, "mediaDevices", originalMediaDevices);
      } else {
        Reflect.deleteProperty(navigator, "mediaDevices");
      }
      if (originalAudioContext) {
        Object.defineProperty(globalThis, "AudioContext", originalAudioContext);
      } else {
        Reflect.deleteProperty(globalThis, "AudioContext");
      }
    };
    return {
      audioContextCloseSpy,
      connection,
      connectSpy,
      createScriptProcessorSpy,
      disconnectSpy,
      getUserMediaSpy,
      installClient(client: ApiClient) {
        client.openSpeechInputStream = vi.fn((handlers) => {
          streamHandlers = handlers;
          if (!options.deferOpen) {
            queueMicrotask(() => handlers.onOpen());
          }
          return connection;
        });
      },
      openStream: () => streamHandlers?.onOpen(),
      processor,
      resolveGetUserMedia: () => getUserMediaDeferred?.resolve(mediaStreams[0]),
      restore,
      streamHandlers: () => {
        if (!streamHandlers) {
          throw new Error("stream handlers are not installed");
        }
        return streamHandlers;
      },
      tracks: tracksByCall[0],
      tracksByCall,
    };
  }

  async function openSettingsTab(tabName: string, settingsName = "System settings") {
    const settingsButton = (
      await screen.findAllByRole(
        "button",
        {
          name: settingsName,
        },
        { timeout: 5_000 },
      )
    ).find((button) => button.closest("nav.sidebar-bottom"));
    if (!settingsButton) {
      throw new Error(`settings button for ${settingsName} was not found in the sidebar nav`);
    }
    fireEvent.click(settingsButton);
    await screen.findByRole("heading", { name: settingsName }, { timeout: 5_000 });
    const settingsTabs = screen.getByRole("tablist", {
      name: settingsName,
    });
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
          run = {
            ...run,
            status: "failed",
            error: options.error,
            updated_at: Date.now(),
          };
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
          emitSessionEvent({
            type: "message_created",
            message: assistantMessage,
          });
          if (run) {
            emitSessionEvent({
              type: "run_updated",
              run: {
                ...run,
                status: "completed",
                output_message_id: assistantMessage.id,
              },
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
        run = {
          ...run!,
          status: "failed",
          error: message,
          updated_at: Date.now(),
        };
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

  it("prewarms Hermes when an authenticated user becomes active", async () => {
    const client = createMockApiClient();
    const ensureHermes = vi.fn(client.ensureHermes.bind(client));
    client.ensureHermes = ensureHermes;

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "New chat" })).toBeInTheDocument();
    await waitFor(() => expect(ensureHermes).toHaveBeenCalledTimes(1));

    fireEvent.keyDown(window, { key: "Shift" });
    expect(ensureHermes).toHaveBeenCalledTimes(1);
  });

  it("opens the public chat for unauthenticated visitors and keeps sign-in in the lower sidebar", async () => {
    const client = createMockApiClient({
      initialUser: null,
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.getByLabelText("Primary")).toBeInTheDocument();
    expect(screen.getByLabelText("Message")).toBeInTheDocument();
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));

    expect(await screen.findByLabelText("Email")).toBeInTheDocument();
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back to public platform" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Back to public platform" }));

    expect(await screen.findByLabelText("Message")).toBeInTheDocument();
    expect(screen.getByLabelText("Primary")).toBeInTheDocument();
  });

  it("hides the public main session from the sidebar", async () => {
    const client = createMockApiClient({
      initialUser: null,
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
    });
    client.listSessionsPublic = vi.fn(async () => [
      {
        id: "home-session",
        title: "Generated title should not be shown",
        is_home: true,
        deletable: false,
        created_at: 1,
        updated_at: 1,
      },
      {
        id: "regular-session",
        title: "Regular session",
        is_home: false,
        deletable: true,
        created_at: 2,
        updated_at: 2,
      },
    ]);

    render(<App apiClient={client} />);

    expect(screen.queryByRole("button", { name: "Main session" })).not.toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Regular session" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Regular session" })).toBeInTheDocument();
  });

  it("stores the public session token in localStorage and only reuses it for public requests", async () => {
    const fetchMock = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            session: {
              id: "public-session-1",
              title: "Persistent public session",
              created_at: 1_767_225_600,
              updated_at: 1_767_225_600,
            },
            public_token: "public-token-1",
          }),
          {
            status: 201,
            headers: { "Content-Type": "application/json" },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ sessions: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ sessions: [] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );
    const client = createApiClient();

    await client.createSessionPublic("agent", "Persistent public session", {
      includePublicToken: true,
    });

    expect(localStorage.getItem("hermes-hub-public-session-token")).toBe("public-token-1");

    await client.listSessionsPublic();

    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/sessions",
      expect.objectContaining({
        headers: undefined,
      }),
    );

    await client.listSessionsPublic({ includePublicToken: true });

    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/sessions",
      expect.objectContaining({
        headers: expect.objectContaining({
          "X-Hermes-Hub-Public-Session": "public-token-1",
        }),
      }),
    );
  });

  it("clears the stored public token when a public session refresh no longer returns one", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(JSON.stringify({ sessions: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    localStorage.setItem("hermes-hub-public-session-token", "stale-public-token");

    await createApiClient().listSessionsPublic({ includePublicToken: true });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions",
      expect.objectContaining({
        headers: expect.objectContaining({
          "X-Hermes-Hub-Public-Session": "stale-public-token",
        }),
      }),
    );
    expect(localStorage.getItem("hermes-hub-public-session-token")).toBeNull();
  });

  it("keeps the stored public token when creating a non-public session", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          session: {
            id: "private-session-1",
            title: "Private session",
            created_at: 1_767_225_600,
            updated_at: 1_767_225_600,
          },
        }),
        {
          status: 201,
          headers: { "Content-Type": "application/json" },
        },
      ),
    );
    localStorage.setItem("hermes-hub-public-session-token", "public-token-to-keep");

    await createApiClient().createSessionPublic("agent", "Private session");

    expect(localStorage.getItem("hermes-hub-public-session-token")).toBe("public-token-to-keep");
  });

  it("clears the stored public token when public attachment upload is unauthorized", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(JSON.stringify({ message: "unauthorized" }), {
        status: 401,
        headers: { "Content-Type": "application/json" },
      }),
    );
    localStorage.setItem("hermes-hub-public-session-token", "expired-public-token");

    await expect(
      createApiClient().uploadSessionAttachmentsPublic(
        "public-session-1",
        [new File(["content"], "public.txt", { type: "text/plain" })],
        { includePublicToken: true },
      ),
    ).rejects.toThrow("unauthorized");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions/public-session-1/attachments",
      expect.objectContaining({
        headers: expect.objectContaining({
          "X-Hermes-Hub-Public-Session": "expired-public-token",
        }),
      }),
    );
    expect(localStorage.getItem("hermes-hub-public-session-token")).toBeNull();
  });

  it("keeps the public token out of the EventSource URL", () => {
    const openedUrls: string[] = [];
    class MockEventSource {
      onerror: (() => void) | null = null;

      constructor(url: string) {
        openedUrls.push(url);
      }

      addEventListener() {}
      removeEventListener() {}
      close() {}
    }
    const originalEventSource = globalThis.EventSource;
    vi.stubGlobal("EventSource", MockEventSource);
    localStorage.setItem("hermes-hub-public-session-token", "public-token-1");

    try {
      const unsubscribe = createApiClient().subscribeSessionEventsPublic(
        "public-session-1",
        vi.fn(),
        vi.fn(),
        { includePublicToken: true },
      );
      unsubscribe();
    } finally {
      if (originalEventSource) {
        vi.stubGlobal("EventSource", originalEventSource);
      } else {
        vi.unstubAllGlobals();
      }
    }

    expect(openedUrls).toEqual(["/api/sessions/public-session-1/events"]);
  });

  it("opens the public session from the session id path and follows browser navigation", async () => {
    window.history.pushState({}, "", "/public/sessions/public-session-2?stale=1#old");
    const client = createMockApiClient({
      initialUser: null,
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
    });
    client.listSessionsPublic = vi.fn(async () => [
      {
        id: "public-session-1",
        title: "First public session",
        created_at: 1_767_225_600,
        updated_at: 1_767_225_600,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
      {
        id: "public-session-2",
        title: "Target public session",
        created_at: 1_767_225_700,
        updated_at: 1_767_225_700,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
    ]);

    render(<App apiClient={client} />);

    expect(
      await screen.findByRole("heading", { name: "Target public session" }),
    ).toBeInTheDocument();
    expect(client.listSessionsPublic).toHaveBeenCalledWith({
      includePublicToken: true,
      sessionId: "public-session-2",
    });
    expect(window.location.pathname).toBe("/public/sessions/public-session-2");
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");

    fireEvent.click(await screen.findByRole("button", { name: "First public session" }));

    expect(await screen.findByRole("heading", { name: "First public session" })).toBeInTheDocument();
    expect(window.location.pathname).toBe("/public/sessions/public-session-1");
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");

    window.history.pushState({}, "", "/public/sessions/public-session-2");
    window.dispatchEvent(new PopStateEvent("popstate"));

    expect(
      await screen.findByRole("heading", { name: "Target public session" }),
    ).toBeInTheDocument();
    expect(client.listSessionsPublic).toHaveBeenLastCalledWith({
      includePublicToken: true,
      sessionId: "public-session-2",
    });

    window.history.pushState({}, "", "/public");
    window.dispatchEvent(new PopStateEvent("popstate"));

    expect(await screen.findByRole("heading", { name: "First public session" })).toBeInTheDocument();
    expect(window.location.pathname).toBe("/public");
    expect(client.listSessionsPublic).toHaveBeenLastCalledWith({
      includePublicToken: true,
      sessionId: null,
    });
  });

  it("clears public session URLs when authenticated private chat is active", async () => {
    window.history.pushState({}, "", "/public/sessions/public-session-2?from=public#stale");

    render(<App apiClient={createMockApiClient()} />);

    expect(await screen.findByRole("button", { name: "Session" })).toBeInTheDocument();
    await waitFor(() => {
      expect(window.location.pathname).toBe("/chat");
    });
    expect(window.location.search).toBe("");
    expect(window.location.hash).toBe("");

    window.history.pushState({}, "", "/public/sessions/stale-public-session");
    fireEvent.click(screen.getByRole("button", { name: "Session" }));
    expect(window.location.pathname).toBe("/chat/sessions/session-1");
  });

  it("restores authenticated feature pages from URL paths", async () => {
    window.history.pushState({}, "", "/settings/auth");
    render(<App apiClient={createMockApiClient()} />);

    const settingsTabs = await screen.findByRole("tablist", {
      name: "System settings",
    });
    expect(within(settingsTabs).getByRole("tab", { name: "Authentication settings" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(await screen.findByLabelText("Enable OIDC")).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Public platform" }));
    expect(window.location.pathname).toBe("/settings/public-platform");

    window.history.pushState({}, "", "/personal/password");
    window.dispatchEvent(new PopStateEvent("popstate"));
    const personalTabs = await screen.findByRole("tablist", {
      name: "Personal settings",
    });
    expect(within(personalTabs).getByRole("tab", { name: "Change password" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByLabelText("New password")).toBeInTheDocument();

    window.history.pushState({}, "", "/scheduled-tasks");
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(await screen.findByRole("heading", { name: "Scheduled tasks" })).toBeInTheDocument();
  });

  it("restores authenticated chat sessions from URL paths", async () => {
    window.history.pushState({}, "", "/chat/sessions/session-1");
    const client = createMockApiClient({
      initialMessagesBySessionId: {
        "session-1": [
          {
            id: "message-1",
            session_id: "session-1",
            role: "assistant",
            content: "Restored from URL",
            attachments: [],
            created_at: 1_767_225_600,
          },
        ],
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Session" })).toBeInTheDocument();
    expect(await screen.findByText("Restored from URL")).toBeInTheDocument();
    expect(window.location.pathname).toBe("/chat/sessions/session-1");
  });

  it("shows a sidebar loading state while authenticated session history loads", async () => {
    const client = createMockApiClient();
    const sessionsDeferred =
      createDeferred<Awaited<ReturnType<ApiClient["listSessionsPublic"]>>>();
    client.listSessionsPublic = vi.fn(async () => sessionsDeferred.promise);

    render(<App apiClient={client} />);

    expect(await screen.findByText("Loading sessions")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delayed session" })).not.toBeInTheDocument();

    await act(async () => {
      sessionsDeferred.resolve([
        {
          id: "delayed-session",
          title: "Delayed session",
          created_at: 1_767_225_600,
          updated_at: 1_767_225_600,
        } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
      ]);
      await sessionsDeferred.promise;
    });

    expect(await screen.findByRole("button", { name: "Delayed session" })).toBeInTheDocument();
    expect(screen.queryByText("Loading sessions")).not.toBeInTheDocument();
  });

  it("waits for public platform status before showing the anonymous landing route", async () => {
    const client = createMockApiClient();
    const meDeferred = createDeferred<Awaited<ReturnType<ApiClient["me"]>>>();
    const bootstrapStatusDeferred =
      createDeferred<Awaited<ReturnType<ApiClient["bootstrapStatus"]>>>();
    client.me = vi.fn(async () => meDeferred.promise);
    client.bootstrapStatus = vi.fn(async () => bootstrapStatusDeferred.promise);
    client.listSessionsPublic = vi.fn(async () => [
      {
        id: "public-session-after-bootstrap",
        title: "Public after bootstrap",
        created_at: 1_767_225_600,
        updated_at: 1_767_225_600,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
    ]);

    render(<App apiClient={client} />);

    expect(screen.getByText("Loading")).toBeInTheDocument();
    await act(async () => {
      meDeferred.resolve(null);
      await meDeferred.promise;
    });
    expect(screen.getByText("Loading")).toBeInTheDocument();
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New chat" })).not.toBeInTheDocument();

    await act(async () => {
      bootstrapStatusDeferred.resolve({
        bootstrap_open: false,
        public_platform_enabled: true,
      });
      await bootstrapStatusDeferred.promise;
    });

    expect(
      await screen.findByRole("button", { name: "Public after bootstrap" }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();
  });

  it("clears authenticated chat sessions after signing out to the public platform", async () => {
    const client = createMockApiClient({
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
      initialMessagesBySessionId: {
        "private-session-1": [
          {
            id: "private-message-1",
            session_id: "private-session-1",
            role: "user",
            content: "private message should disappear",
            attachments: [],
            created_at: 1_767_225_600,
          },
        ],
      },
    });
    let authenticated = true;
    const originalLogout = client.logout.bind(client);
    const publicSessions = [
      {
        id: "public-session-1",
        title: "Public session",
        created_at: 1_767_225_600,
        updated_at: 1_767_225_600,
        recycle_at: 1_767_312_000,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
    ];
    const publicSessionsDeferred =
      createDeferred<Awaited<ReturnType<ApiClient["listSessionsPublic"]>>>();
    client.logout = vi.fn(async () => {
      authenticated = false;
      await originalLogout();
    });
    client.listSessionsPublic = vi.fn(async () => {
      if (!authenticated) {
        return publicSessionsDeferred.promise;
      }
      return [
        {
          id: "private-session-1",
          title: "Private session",
          created_at: 1_767_225_600,
          updated_at: 1_767_225_600,
        } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
      ];
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Private session" })).toBeInTheDocument();
    expect(await screen.findByText("private message should disappear")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Sign out" }));

    await waitFor(() => expect(client.logout).toHaveBeenCalled());
    expect(screen.getByRole("button", { name: "Sign in" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Private session" })).not.toBeInTheDocument();
    expect(screen.queryByText("private message should disappear")).not.toBeInTheDocument();

    await act(async () => {
      publicSessionsDeferred.resolve(publicSessions);
      await publicSessionsDeferred.promise;
    });

    expect(await screen.findByRole("button", { name: "Public session" })).toBeInTheDocument();
  });

  it("refreshes public platform availability when signing out", async () => {
    const client = createMockApiClient();
    let publicPlatformEnabled = false;
    let signedOut = false;
    const originalLogout = client.logout.bind(client);
    const logoutBootstrapStatus =
      createDeferred<Awaited<ReturnType<ApiClient["bootstrapStatus"]>>>();
    client.bootstrapStatus = vi.fn(async () => ({
      ...(publicPlatformEnabled
        ? await logoutBootstrapStatus.promise
        : { bootstrap_open: false, public_platform_enabled: false }),
    }));
    client.logout = vi.fn(async () => {
      signedOut = true;
      await originalLogout();
    });
    client.listSessionsPublic = vi.fn(async () => [
      signedOut
        ? ({
            id: "public-session-after-logout",
            title: "Public after logout",
            created_at: 1_767_225_600,
            updated_at: 1_767_225_600,
          } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number])
        : ({
            id: "private-session-before-logout",
            title: "Private before logout",
            created_at: 1_767_225_600,
            updated_at: 1_767_225_600,
          } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number]),
    ]);

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Private before logout" })).toBeInTheDocument();
    publicPlatformEnabled = true;
    fireEvent.click(screen.getByRole("button", { name: "Sign out" }));

    await waitFor(() => expect(client.logout).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: "Private before logout" })).not.toBeInTheDocument();

    await act(async () => {
      logoutBootstrapStatus.resolve({
        bootstrap_open: false,
        public_platform_enabled: true,
      });
      await logoutBootstrapStatus.promise;
    });

    expect(await screen.findByRole("button", { name: "Public after logout" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Email")).not.toBeInTheDocument();
  });

  it("shows public session recycle time in the chat header", async () => {
    const client = createMockApiClient({
      initialUser: null,
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
    });
    client.listSessionsPublic = vi.fn(async () => [
      {
        id: "public-session-1",
        title: "Public recycle session",
        created_at: 1_767_225_600,
        updated_at: 1_767_225_600,
        recycle_at: 1_767_312_000,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
    ]);

    render(<App apiClient={client} />);

    expect(await screen.findByText("Public recycle session")).toBeInTheDocument();
    expect(screen.getByText(/Recycles at/)).toBeInTheDocument();
  });

  it("localizes the public session recycle hint", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    const client = createMockApiClient({
      initialUser: null,
      publicPlatformSettings: { enabled: true },
      publicPlatformInstance: publicHermesInstance(),
    });
    client.listSessionsPublic = vi.fn(async () => [
      {
        id: "public-session-1",
        title: "公共回收会话",
        created_at: 1_767_225_600,
        updated_at: 1_767_225_600,
        recycle_at: 1_767_312_000,
      } as unknown as Awaited<ReturnType<ApiClient["listSessionsPublic"]>>[number],
    ]);

    render(<App apiClient={client} />);

    expect(await screen.findByText("公共回收会话")).toBeInTheDocument();
    expect(screen.getByText(/回收时间/)).toBeInTheDocument();
  });

  it("shows the standalone login page when public platform is disabled", async () => {
    const client = createMockApiClient({ initialUser: null });

    render(<App apiClient={client} />);

    expect(await screen.findByLabelText("Email")).toBeInTheDocument();
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New chat" })).not.toBeInTheDocument();
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

    fireEvent.click(screen.getByRole("button", { name: "Personal settings" }));
    expect(await screen.findByRole("heading", { name: "Personal settings" })).toBeInTheDocument();
    const personalTabs = screen.getByRole("tablist", {
      name: "Personal settings",
    });
    expect(within(personalTabs).getByRole("tab", { name: "Personalization" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "中文" }));
    expect(screen.getByRole("button", { name: "新建对话" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "English" }));

    const settingsTabs = await openSettingsTab("User management");
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "User management" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New chat" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Session" })).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Model configuration" }));
    expect(
      await screen.findByRole("heading", { name: "Large language model" }),
    ).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Image model" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Title model" })).toBeInTheDocument();
    expect(
      screen.getAllByRole("heading", { level: 2 }).map((heading) => heading.textContent),
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

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "System parameters" }));
    const maxSessionsInput = await screen.findByLabelText("Max sessions per user");
    expect(screen.queryByRole("heading", { name: "System parameters" })).not.toBeInTheDocument();
    fireEvent.change(maxSessionsInput, { target: { value: "12" } });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));
    expect(await screen.findByText("Settings saved")).toBeInTheDocument();

    fireEvent.click(
      within(settingsTabs).getByRole("tab", {
        name: "Authentication settings",
      }),
    );
    expect(await screen.findByLabelText("Enable OIDC")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Authentication settings" }),
    ).not.toBeInTheDocument();
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

  it("moves personalization into personal settings and updates local password", async () => {
    const client = createMockApiClient();
    client.updatePassword = vi.fn(async () => undefined);
    render(<App apiClient={client} />);

    fireEvent.click(await screen.findByRole("button", { name: "Personal settings" }));

    expect(await screen.findByRole("heading", { name: "Personal settings" })).toBeInTheDocument();
    const personalTabs = screen.getByRole("tablist", {
      name: "Personal settings",
    });
    expect(within(personalTabs).getByRole("tab", { name: "Personalization" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("group", { name: "Language" })).toBeInTheDocument();

    fireEvent.click(within(personalTabs).getByRole("tab", { name: "Change password" }));
    fireEvent.change(screen.getByLabelText("New password"), {
      target: { value: "new-password-456" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "different-password" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save password" }));

    expect(await screen.findByText("Passwords do not match")).toBeInTheDocument();
    expect(client.updatePassword).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "new-password-456" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save password" }));

    await waitFor(() => {
      expect(client.updatePassword).toHaveBeenCalledWith("new-password-456");
    });
    expect(await screen.findByText("Password saved")).toBeInTheDocument();
    expect(screen.getByLabelText("New password")).toHaveValue("");
    expect(screen.getByLabelText("Confirm password")).toHaveValue("");

    fireEvent.change(screen.getByLabelText("New password"), {
      target: { value: "draft-password" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "draft-password" },
    });
    fireEvent.click(screen.getByRole("button", { name: "New chat" }));
    fireEvent.click(screen.getByRole("button", { name: "Personal settings" }));
    fireEvent.click(
      within(screen.getByRole("tablist", { name: "Personal settings" })).getByRole("tab", {
        name: "Change password",
      }),
    );
    expect(screen.getByLabelText("New password")).toHaveValue("");
    expect(screen.getByLabelText("Confirm password")).toHaveValue("");
  });

  it("groups admin modules under system settings tabs", async () => {
    render(<App apiClient={createMockApiClient()} />);

    const systemSettingsNav = await screen.findByRole("button", {
      name: "System settings",
    });
    expect(screen.queryByRole("button", { name: "User management" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Model configuration" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hermes management" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Managed skills" })).not.toBeInTheDocument();

    fireEvent.click(systemSettingsNav);
    expect(await screen.findByRole("heading", { name: "System settings" })).toBeInTheDocument();

    const settingsTabs = screen.getByRole("tablist", {
      name: "System settings",
    });
    expect(within(settingsTabs).getByRole("tab", { name: "User management" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      within(settingsTabs).getByRole("tab", { name: "Model configuration" }),
    ).toBeInTheDocument();
    expect(
      within(settingsTabs).getByRole("tab", { name: "Hermes management" }),
    ).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Hermes Profile" })).toBeInTheDocument();
    expect(within(settingsTabs).getByRole("tab", { name: "Managed skills" })).toBeInTheDocument();
    expect(
      within(settingsTabs).getByRole("tab", { name: "System parameters" }),
    ).toBeInTheDocument();
    expect(
      within(settingsTabs).queryByRole("tab", { name: "Session settings" }),
    ).not.toBeInTheDocument();
    expect(
      within(settingsTabs).getByRole("tab", {
        name: "Authentication settings",
      }),
    ).toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Model configuration" }));
    expect(
      await screen.findByRole("heading", { name: "Large language model" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Model configuration" })).not.toBeInTheDocument();

    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "System parameters" }));
    expect(await screen.findByLabelText("Max sessions per user")).toBeInTheDocument();
    expect(await screen.findByLabelText("Max attachment upload size (MB)")).toBeInTheDocument();
    expect(screen.getByLabelText("Attachment retention (days)")).toBeInTheDocument();
    expect(screen.getByLabelText("Empty chat prompt")).toBeInTheDocument();

    fireEvent.click(
      within(settingsTabs).getByRole("tab", {
        name: "Authentication settings",
      }),
    );
    const enableOidcRow = await screen.findByLabelText("Enable OIDC");
    expect(enableOidcRow).toBeInTheDocument();
    expect(screen.getByLabelText("OIDC Redirect URI")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Authentication settings" }),
    ).not.toBeInTheDocument();
  });

  it("saves the empty chat prompt in system parameters and renders it in chat", async () => {
    const client = createMockApiClient();
    const updateSystemSettings = client.updateSystemSettings.bind(client);
    const systemSettings = client.systemSettings.bind(client);
    let savedSettings: unknown = null;
    let dropPromptOnReload = false;
    client.updateSystemSettings = vi.fn(async (settings) => {
      savedSettings = settings;
      await updateSystemSettings(settings);
      dropPromptOnReload = true;
    });
    client.systemSettings = vi.fn(async () => {
      const settings = await systemSettings();
      if (!dropPromptOnReload) {
        return settings;
      }
      dropPromptOnReload = false;
      return {
        ...settings,
        empty_chat_prompt: "",
      };
    });

    render(<App apiClient={client} />);

    const prompt = "Ask Hermes\nfrom the shared hub";
    await openSettingsTab("System parameters");
    fireEvent.change(await screen.findByLabelText("Empty chat prompt"), {
      target: { value: prompt },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByText("Settings saved")).toBeInTheDocument();
    expect(savedSettings).toMatchObject({
      empty_chat_prompt: prompt,
    });
    expect(screen.getByLabelText("Empty chat prompt")).toHaveValue(prompt);

    fireEvent.click(screen.getByRole("button", { name: "New chat" }));
    const emptyPrompt = await screen.findByText((content, element) => {
      return element?.tagName.toLowerCase() === "strong" && content.includes("Ask Hermes");
    });
    expect(emptyPrompt.textContent).toBe(prompt);
  });

  it("shows the empty state when a session only has hidden empty messages", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "hidden-empty-assistant",
                session_id: "session-1",
                role: "assistant",
                message_kind: "text",
                client_message_key: null,
                content: "",
                attachments: [],
                created_at: 1_767_225_600,
              },
            ],
          },
        })}
      />,
    );

    expect(await screen.findByText("Start a Hermes conversation")).toBeInTheDocument();
    expect(document.querySelector(".message-list.empty")).toBeInTheDocument();
  });

  it("shows a conversation loading state until the first session snapshot arrives", async () => {
    let snapshotListener: ((event: ChannelSessionEvent) => void) | null = null;
    const client = createMockApiClient({
      subscribeSessionEvents(_channelId, _sessionId, onEvent) {
        snapshotListener = onEvent;
        return () => {
          snapshotListener = null;
        };
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Session" })).toBeInTheDocument();
    expect(await screen.findByText("Loading conversation")).toBeInTheDocument();
    expect(screen.queryByText("Start a Hermes conversation")).not.toBeInTheDocument();

    await act(async () => {
      snapshotListener?.({
        type: "messages_snapshot",
        messages: [],
        active_run: null,
      });
    });

    expect(await screen.findByText("Start a Hermes conversation")).toBeInTheDocument();
    expect(screen.queryByText("Loading conversation")).not.toBeInTheDocument();
  });

  it("lets admins edit and save only SOUL.md with Vditor's Markdown WYSIWYG editor", async () => {
    const client = createMockApiClient({
      initialHermesProfile: {
        agents_md: "# Existing AGENTS\n\nUse the shared guardrails.\n",
        soul_md: "# Existing SOUL\n\nKeep a calm operator tone.\n",
      },
    });
    const updateHermesProfile = client.updateHermesProfile.bind(client);
    client.updateHermesProfile = vi.fn(async (profile) => {
      await updateHermesProfile(profile);
    });

    render(<App apiClient={client} />);

    await openSettingsTab("Hermes Profile");

    expect(screen.queryByLabelText("AGENTS.md")).not.toBeInTheDocument();
    expect(screen.queryByText("AGENTS.md")).not.toBeInTheDocument();

    const soulEditor = await screen.findByRole("textbox", { name: "SOUL.md" });
    expect(soulEditor).toHaveClass("soul-vditor-editor");
    expect(vditorMock.Vditor).toHaveBeenCalledTimes(1);
    expect(vditorMock.Vditor).toHaveBeenCalledWith(
      soulEditor,
      expect.objectContaining({
        cache: { enable: false },
        cdn: "/vditor",
        height: 520,
        mode: "wysiwyg",
        value: "",
      }),
    );
    await waitFor(() => {
      expect(vditorMock.instances[0].setValue).toHaveBeenCalledWith(
        "# Existing SOUL\n\nKeep a calm operator tone.\n",
        true,
      );
    });
    expect(vditorMock.instances[0].options).toEqual(
      expect.objectContaining({
        toolbar: expect.arrayContaining(["headings", "bold", "italic", "table", "code"]),
      }),
    );

    const nextSoul =
      "# SOUL\n\nPrefer **concise**, direct responses.\n\n| 场景 | 命令 |\n|------|------|\n| 启动 | `./start_ntp.sh` |\n\n```bash\n./start_ntp.sh\n```\n";
    await act(async () => {
      (vditorMock.instances[0].options.input as (value: string) => void)(nextSoul);
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(client.updateHermesProfile).toHaveBeenCalledWith({
        soul_md: nextSoul,
      });
    });
    expect(await screen.findByText("Hermes Profile saved")).toBeInTheDocument();
    expect(screen.queryByLabelText("AGENTS.md")).not.toBeInTheDocument();
  });

  it("saves the optional image generation toggle and keeps it at the bottom of the image card", async () => {
    render(<App apiClient={createMockApiClient()} />);

    await openSettingsTab("Model configuration");
    const imageHeading = await screen.findByRole("heading", {
      name: "Image model",
    });
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

  it("saves fallback model settings for main and title models", async () => {
    const client = createMockApiClient();
    render(<App apiClient={client} />);

    await openSettingsTab("Model configuration");
    const llmHeading = await screen.findByRole("heading", {
      name: "Large language model",
    });
    const llmCard = llmHeading.closest("section.panel") as HTMLElement;
    fireEvent.click(within(llmCard).getByLabelText("Enable fallback"));
    fireEvent.change(within(llmCard).getByLabelText("Fallback provider"), {
      target: { value: "fallback-provider" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback Base URL"), {
      target: { value: "https://fallback.example/v1" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback API key"), {
      target: { value: "fallback-key" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback model name"), {
      target: { value: "gpt-4.1-fallback" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback max output tokens"), {
      target: { value: "2048" },
    });
    fireEvent.click(within(llmCard).getByLabelText("Fallback parallel tool calls"));

    const titleHeading = screen.getByRole("heading", { name: "Title model" });
    const titleCard = titleHeading.closest("section.panel") as HTMLElement;
    fireEvent.click(within(titleCard).getByLabelText("Enable fallback"));
    fireEvent.change(within(titleCard).getByLabelText("Fallback provider"), {
      target: { value: "fallback-title-provider" },
    });
    fireEvent.change(within(titleCard).getByLabelText("Fallback Base URL"), {
      target: { value: "https://fallback-title.example/v1" },
    });
    fireEvent.change(within(titleCard).getByLabelText("Fallback model name"), {
      target: { value: "gpt-title-fallback" },
    });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("Model configuration saved")).toBeInTheDocument();
    await waitFor(() => {
      const savedLlmCard = screen
        .getByRole("heading", { name: "Large language model" })
        .closest("section.panel") as HTMLElement;
      const savedTitleCard = screen
        .getByRole("heading", { name: "Title model" })
        .closest("section.panel") as HTMLElement;
      expect(within(savedLlmCard).getByLabelText("Fallback model name")).toHaveValue(
        "gpt-4.1-fallback",
      );
      expect(within(savedLlmCard).getByLabelText("Fallback max output tokens")).toHaveValue(2048);
      expect(within(savedLlmCard).getByLabelText("Fallback parallel tool calls")).not.toBeChecked();
      expect(within(savedTitleCard).getByLabelText("Fallback model name")).toHaveValue(
        "gpt-title-fallback",
      );
    });
  });

  it("tests the fallback model separately from the primary model", async () => {
    const client = createMockApiClient();
    const testFallback = vi.fn(async () => ({
      ok: true,
      status_code: 200,
      message: "model test succeeded",
      duration_ms: 12,
    }));
    client.testModelFallbackConfig = testFallback;
    render(<App apiClient={client} />);

    await openSettingsTab("Model configuration");
    const llmHeading = await screen.findByRole("heading", {
      name: "Large language model",
    });
    const llmCard = llmHeading.closest("section.panel") as HTMLElement;
    fireEvent.click(within(llmCard).getByLabelText("Enable fallback"));
    fireEvent.change(within(llmCard).getByLabelText("Fallback provider"), {
      target: { value: "fallback-provider" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback Base URL"), {
      target: { value: "https://fallback.example/v1" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback API key"), {
      target: { value: "fallback-key" },
    });
    fireEvent.change(within(llmCard).getByLabelText("Fallback model name"), {
      target: { value: "gpt-4.1-fallback" },
    });

    fireEvent.click(within(llmCard).getByRole("button", { name: "Test fallback" }));

    await waitFor(() => {
      expect(testFallback).toHaveBeenCalledWith(
        expect.objectContaining({
          config_kind: "llm",
          fallback: expect.objectContaining({
            provider_name: "fallback-provider",
            provider_base_url: "https://fallback.example/v1",
            provider_api_key: "fallback-key",
            default_model: "gpt-4.1-fallback",
          }),
        }),
      );
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

  it("uploads pasted composer files as attachments", async () => {
    const hubRun = createHubRunMock({ answer: "received" });
    render(<App apiClient={hubRun.client} />);

    const composer = await screen.findByLabelText("Message");
    const image = new File(["image"], "pasted.png", { type: "image/png" });

    fireEvent.paste(composer, {
      clipboardData: {
        files: [image],
        items: [],
      },
    });

    expect(await screen.findByText("pasted.png")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Preview image pasted.png" }));
    expect(screen.getByRole("dialog", { name: "Image preview" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close image preview" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Image preview" })).not.toBeInTheDocument();
    });

    fireEvent.change(composer, {
      target: { value: "看一下这张图" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(hubRun.createCalled()).toBe(true));
    expect(hubRun.getMessages().find((message) => message.role === "user")?.attachments).toEqual([
      expect.objectContaining({
        name: "pasted.png",
        kind: "image",
      }),
    ]);
  });

  it("streams speech while the microphone is held and inserts the final transcript", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      expect(await screen.findByText("Recording and transcribing live")).toBeInTheDocument();
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));

      act(() => {
        speech.processor.onaudioprocess?.({
          inputBuffer: { getChannelData: () => new Float32Array([0, 1, -1]) },
        });
      });
      const audioChunk = speech.connection.sendAudio.mock.calls.at(-1)?.[0] as
        | ArrayBuffer
        | undefined;
      expect(audioChunk).toBeInstanceOf(ArrayBuffer);
      const audioView = new DataView(audioChunk as ArrayBuffer);
      expect(audioView.getInt16(0, true)).toBe(0);
      expect(audioView.getInt16(2, true)).toBe(32767);
      expect(audioView.getInt16(4, true)).toBe(-32768);

      act(() => {
        speech.streamHandlers().onPartial("语音");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("语音");
      const voiceStatus = screen.getByText("Recording and transcribing live");
      expect(voiceStatus).toBeInTheDocument();
      expect(voiceStatus.closest(".voice-status")).not.toHaveTextContent("语音");

      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      await waitFor(() => expect(speech.connection.stop).toHaveBeenCalledTimes(1));
      expect(await screen.findByText("Transcribing...")).toBeInTheDocument();
      expect(screen.getByLabelText("Message")).toHaveValue("语音");

      act(() => {
        speech.streamHandlers().onFinal("语音输入内容");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("语音输入内容");

      act(() => {
        speech.streamHandlers().onDone();
        speech.streamHandlers().onClose();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("语音输入内容");
      expect(screen.queryByText("Speech recognition failed")).not.toBeInTheDocument();
      expect(hubRun.createCalled()).toBe(false);
    } finally {
      speech.restore();
    }
  });

  it("replaces the live speech draft inside existing composer text", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const composer = await screen.findByLabelText("Message");
      fireEvent.change(composer, { target: { value: "请记录" } });
      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));

      act(() => {
        speech.streamHandlers().onPartial("第一段");
      });
      expect(composer).toHaveValue("请记录 第一段");

      act(() => {
        speech.streamHandlers().onPartial("第一段更新");
      });
      expect(composer).toHaveValue("请记录 第一段更新");

      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      await waitFor(() => expect(speech.connection.stop).toHaveBeenCalledTimes(1));
      expect(composer).toHaveValue("请记录 第一段更新");

      act(() => {
        speech.streamHandlers().onFinal("第一段完整");
        speech.streamHandlers().onDone();
      });
      expect(composer).toHaveValue("请记录 第一段完整");
    } finally {
      speech.restore();
    }
  });

  it("buffers speech captured before the stream opens and finalizes after quick release", async () => {
    const speech = installStreamingSpeechMock({ deferOpen: true });
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(hubRun.client.openSpeechInputStream).toHaveBeenCalledTimes(1));

      // 用户说完马上松开时，WebSocket 可能还没 open；此时也必须先保留麦克风音频。
      expect(speech.createScriptProcessorSpy).toHaveBeenCalledWith(1024, 1, 1);
      expect(speech.processor.onaudioprocess).toEqual(expect.any(Function));
      act(() => {
        speech.processor.onaudioprocess?.({
          inputBuffer: { getChannelData: () => new Float32Array([0.5, -0.5]) },
        });
      });
      expect(speech.connection.sendAudio).not.toHaveBeenCalled();

      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      expect(speech.connection.close).not.toHaveBeenCalled();
      expect(speech.connection.stop).not.toHaveBeenCalled();

      act(() => {
        speech.openStream();
      });
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));
      expect(speech.connection.sendAudio).toHaveBeenCalledTimes(1);
      expect(speech.connection.stop).toHaveBeenCalledTimes(1);
      expect(speech.connection.sendStart.mock.invocationCallOrder[0]).toBeLessThan(
        speech.connection.sendAudio.mock.invocationCallOrder[0],
      );
      expect(speech.connection.sendAudio.mock.invocationCallOrder[0]).toBeLessThan(
        speech.connection.stop.mock.invocationCallOrder[0],
      );

      act(() => {
        speech.streamHandlers().onFinal("快速松开也识别");
        speech.streamHandlers().onDone();
        speech.streamHandlers().onClose();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("快速松开也识别");
      expect(screen.queryByText("Speech recognition failed")).not.toBeInTheDocument();
    } finally {
      speech.restore();
    }
  });

  it("ignores late speech text after the stream fails", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));

      act(() => {
        speech.streamHandlers().onPartial("临时草稿");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("临时草稿");

      act(() => {
        speech.streamHandlers().onError("asr failed");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("");
      expect(screen.getByText("Speech recognition failed")).toBeInTheDocument();

      act(() => {
        speech.streamHandlers().onPartial("迟到 partial");
        speech.streamHandlers().onFinal("迟到 final");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("");
    } finally {
      speech.restore();
    }
  });

  it("keeps the final speech text when late stream text arrives after done", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));

      act(() => {
        speech.streamHandlers().onPartial("识别中");
        speech.streamHandlers().onFinal("最终文本");
        speech.streamHandlers().onDone();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("最终文本");

      act(() => {
        speech.streamHandlers().onPartial("迟到 partial");
        speech.streamHandlers().onFinal("迟到 final");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("最终文本");
    } finally {
      speech.restore();
    }
  });

  it("explains when the page origin cannot access the microphone", async () => {
    const originalSecureContext = Object.getOwnPropertyDescriptor(globalThis, "isSecureContext");
    Object.defineProperty(globalThis, "isSecureContext", {
      configurable: true,
      value: false,
    });
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);
      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);

      expect(
        await screen.findByText(
          "Speech input requires HTTPS or localhost. This address cannot access the microphone.",
        ),
      ).toBeInTheDocument();
      expect(speech.getUserMediaSpy).not.toHaveBeenCalled();
      expect(hubRun.client.openSpeechInputStream).not.toHaveBeenCalled();
    } finally {
      speech.restore();
      if (originalSecureContext) {
        Object.defineProperty(globalThis, "isSecureContext", originalSecureContext);
      } else {
        Reflect.deleteProperty(globalThis, "isSecureContext");
      }
    }
  });

  it("does not start speech streaming after release before microphone permission resolves", async () => {
    const speech = installStreamingSpeechMock({
      deferGetUserMedia: true,
    });
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      fireEvent.mouseUp(microphone);
      await act(async () => {
        speech.resolveGetUserMedia();
        await Promise.resolve();
      });

      await waitFor(() => expect(speech.tracks[0].stop).toHaveBeenCalledTimes(1));
      expect(hubRun.client.openSpeechInputStream).not.toHaveBeenCalled();
    } finally {
      speech.restore();
    }
  });

  it("ignores stale microphone permission results from an older streaming attempt", async () => {
    const speech = installStreamingSpeechMock({
      deferGetUserMedia: true,
    });
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      fireEvent.mouseUp(microphone);
      fireEvent.mouseDown(microphone);
      await act(async () => {
        await Promise.resolve();
      });
      speech.resolveGetUserMedia();
      await act(async () => {
        await Promise.resolve();
      });

      expect(speech.tracksByCall[0][0].stop).toHaveBeenCalledTimes(1);
      expect(hubRun.client.openSpeechInputStream).toHaveBeenCalledTimes(1);
      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      await waitFor(() => expect(speech.connection.stop).toHaveBeenCalledTimes(1));
      act(() => {
        speech.streamHandlers().onFinal("第二次录音");
        speech.streamHandlers().onDone();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("第二次录音");
    } finally {
      speech.restore();
    }
  });

  it("does not stream speech when unmounted while microphone permission is pending", async () => {
    const speech = installStreamingSpeechMock({
      deferGetUserMedia: true,
    });
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      const { unmount } = render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      unmount();
      await act(async () => {
        speech.resolveGetUserMedia();
        await Promise.resolve();
      });

      expect(speech.tracksByCall[0][0].stop).toHaveBeenCalledTimes(1);
      expect(hubRun.client.openSpeechInputStream).not.toHaveBeenCalled();
    } finally {
      speech.restore();
    }
  });

  it("closes speech streaming without inserting text when the composer unmounts", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      const { unmount } = render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      expect(await screen.findByText("Recording and transcribing live")).toBeInTheDocument();
      unmount();
      await act(async () => {
        await Promise.resolve();
      });

      expect(speech.connection.close).toHaveBeenCalledTimes(1);
      expect(speech.tracksByCall[0][0].stop).toHaveBeenCalledTimes(1);
    } finally {
      speech.restore();
    }
  });

  it("cancels speech streaming without inserting or sending", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(speech.connection.sendStart).toHaveBeenCalledWith(16000));
      act(() => {
        speech.streamHandlers().onPartial("临时语音");
      });
      expect(screen.getByLabelText("Message")).toHaveValue("临时语音");
      fireEvent.click(await screen.findByRole("button", { name: "Cancel speech input" }));

      await waitFor(() => expect(speech.connection.close).toHaveBeenCalledTimes(1));
      expect(speech.connection.stop).not.toHaveBeenCalled();
      expect(screen.getByLabelText("Message")).toHaveValue("");
      act(() => {
        speech.streamHandlers().onFinal("取消后的迟到文本");
        speech.streamHandlers().onDone();
        speech.streamHandlers().onClose();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("");
      expect(screen.queryByText("Speech recognition failed")).not.toBeInTheDocument();
      expect(hubRun.createCalled()).toBe(false);
    } finally {
      speech.restore();
    }
  });

  it("ignores late stream close after release before the stream opens", async () => {
    const speech = installStreamingSpeechMock({ deferOpen: true });
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      await waitFor(() => expect(hubRun.client.openSpeechInputStream).toHaveBeenCalledTimes(1));
      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      await waitFor(() => expect(speech.connection.close).toHaveBeenCalledTimes(1));
      expect(speech.connection.stop).not.toHaveBeenCalled();

      act(() => {
        speech.streamHandlers().onClose();
        speech.streamHandlers().onError("late stream error");
        speech.openStream();
      });

      expect(screen.queryByText("Speech recognition failed")).not.toBeInTheDocument();
      expect(screen.queryByText("Transcribing...")).not.toBeInTheDocument();
      expect(screen.getByLabelText("Message")).toHaveValue("");
      expect(hubRun.createCalled()).toBe(false);
    } finally {
      speech.restore();
    }
  });

  it("keeps transcribing visible when release events repeat", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      fireEvent.mouseDown(microphone);
      expect(await screen.findByText("Recording and transcribing live")).toBeInTheDocument();
      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));

      await waitFor(() => expect(speech.connection.stop).toHaveBeenCalledTimes(1));
      expect(await screen.findByText("Transcribing...")).toBeInTheDocument();
      window.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
      window.dispatchEvent(new Event("pointerup", { bubbles: true, cancelable: true }));
      expect(screen.getByText("Transcribing...")).toBeInTheDocument();
      expect(speech.connection.stop).toHaveBeenCalledTimes(1);

      act(() => {
        speech.streamHandlers().onFinal("稳定识别");
        speech.streamHandlers().onDone();
      });
      expect(screen.getByLabelText("Message")).toHaveValue("稳定识别");
    } finally {
      speech.restore();
    }
  });

  it("automatically stops speech streaming at the configured duration", async () => {
    const speech = installStreamingSpeechMock();
    try {
      const hubRun = createHubRunMock({ answer: "unused" });
      hubRun.client.speechInputConfig = vi.fn(async () => ({
        enabled: true,
        runtime_available: true,
        max_duration_seconds: 1,
        sample_rate: 16000,
        model: "streaming-zh",
      }));
      speech.installClient(hubRun.client);

      render(<App apiClient={hubRun.client} />);

      const microphone = await screen.findByRole("button", {
        name: "Hold to speak",
      });
      vi.useFakeTimers();
      fireEvent.mouseDown(microphone);
      await act(async () => {
        await Promise.resolve();
      });
      expect(screen.getByText("Recording and transcribing live")).toBeInTheDocument();

      await act(async () => {
        vi.advanceTimersByTime(1000);
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(speech.connection.stop).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
      speech.restore();
    }
  });

  it("hides the microphone when speech input is not enabled", async () => {
    const client = createMockApiClient();
    client.speechInputConfig = vi.fn(async () => ({
      enabled: false,
      runtime_available: false,
      max_duration_seconds: 60,
      sample_rate: 16000,
      model: "streaming-zh",
    }));

    render(<App apiClient={client} />);

    expect(await screen.findByLabelText("Message")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Hold to speak" })).not.toBeInTheDocument();
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
                content: `## 结果\n\n**加粗文本** 和 \`code\`\n\n\`\`\`bash\n./start_ntp.sh\n\`\`\`\n\n| 场景 | 命令 |\n|------|------|\n| 将本机作为时钟源 | ./start_ntp.sh |\n| 同步指定时钟源 | ./start_ntp.sh <时钟源IP> |\n\n文件：[练习.pptx](${absolutePptUrl})\n\n![cat](/api/attachments/cat/download)\n\n[open](/download)`,
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
                  {
                    id: "cat",
                    name: "cat",
                    content_type: "image/png",
                    kind: "image",
                    size: 99,
                    download_url: "/api/attachments/cat/download",
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
    expect(screen.getByText("bash")).toHaveClass("markdown-code-language");
    const codeBlock = document.querySelector(".markdown-code-block");
    expect(codeBlock).toHaveTextContent("./start_ntp.sh");
    const table = screen.getByRole("table");
    expect(within(table).getByRole("columnheader", { name: "场景" })).toBeInTheDocument();
    expect(within(table).getByRole("columnheader", { name: "命令" })).toBeInTheDocument();
    expect(within(table).getByText("将本机作为时钟源")).toBeInTheDocument();
    expect(within(table).getByText("./start_ntp.sh <时钟源IP>")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "open" })).toHaveAttribute("href", "/download");
    expect(screen.getByRole("button", { name: "Preview image cat" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Download file 练习.pptx" })).toBeInTheDocument();
    const markdown = document.querySelector(".markdown-content");
    expect(markdown?.textContent).toContain("文件：练习.pptx");
    expect(document.querySelector(".message-attachments")).not.toBeInTheDocument();
  });

  it("preserves single newlines in markdown responses like Hermes slash help output", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-help-output",
                session_id: "session-1",
                role: "assistant",
                content: "/help\n/start - start service\n/stop - stop service",
                attachments: [],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    await waitFor(() => {
      const markdown = document.querySelector(".markdown-content");
      expect(markdown?.textContent).toContain("/start - start service");
      expect(markdown?.querySelectorAll("br")).toHaveLength(2);
    });
  });

  it("renders attachment placeholders at their markdown positions", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-attachment-placeholders",
                session_id: "session-1",
                role: "assistant",
                content: "先看脚本：\n\n{{attachment:0}}\n\n再看图片：\n\n{{attachment:1}}\n\n结束",
                attachments: [
                  {
                    id: "script",
                    name: "start_ntp.sh",
                    content_type: "text/x-shellscript",
                    kind: "file",
                    size: 12,
                    download_url: "/api/attachments/script/download",
                  },
                  {
                    id: "chart",
                    name: "chart.png",
                    content_type: "image/png",
                    kind: "image",
                    size: 99,
                    download_url: "/api/attachments/chart/download",
                  },
                ],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    const firstText = await screen.findByText("先看脚本：");
    const scriptChip = screen.getByRole("link", {
      name: "Download file start_ntp.sh",
    });
    const secondText = screen.getByText("再看图片：");
    const imagePreview = screen.getByRole("button", {
      name: "Preview image chart.png",
    });
    const markdown = document.querySelector(".markdown-content");

    expect(markdown?.textContent).not.toContain("{{attachment:");
    expect(firstText.compareDocumentPosition(scriptChip) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(scriptChip.compareDocumentPosition(secondText) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(
      secondText.compareDocumentPosition(imagePreview) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  it("keeps repeated assistant content when attachment ids differ", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-first-image",
                session_id: "session-1",
                role: "assistant",
                content: "图片已生成\n\n{{attachment:0}}",
                attachments: [
                  {
                    id: "first-image",
                    name: "first.png",
                    content_type: "image/png",
                    kind: "image",
                    download_url: "/api/attachments/first-image/download",
                  },
                ],
                created_at: 1,
              },
              {
                id: "message-second-image",
                session_id: "session-1",
                role: "assistant",
                content: "图片已生成\n\n{{attachment:0}}",
                attachments: [
                  {
                    id: "second-image",
                    name: "second.png",
                    content_type: "image/png",
                    kind: "image",
                    download_url: "/api/attachments/second-image/download",
                  },
                ],
                created_at: 2,
              },
            ],
          },
        })}
      />,
    );

    expect(
      await screen.findByRole("button", { name: "Preview image first.png" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Preview image second.png" })).toBeInTheDocument();
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
    const hubRun = createHubRunMock({
      answer: "pong",
      answerDelay: deferred.promise,
    });

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
    const hubRun = createHubRunMock({
      answer: "done",
      answerDelay: deferred.promise,
    });

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

  it("unlocks sending once the echoed user message arrives before create-run resolves", async () => {
    const firstResponseDeferred = createDeferred<void>();
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    let createCount = 0;
    const client = createMockApiClient();

    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages: [],
          active_run: null,
        });
      });
      return () => eventListeners.delete(onEvent);
    };

    client.createChannelRun = async (_channelId, sessionId, input) => {
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

      if (createCount === 1) {
        queueMicrotask(() => {
          for (const listener of eventListeners) {
            listener({ type: "message_created", message: userMessage });
            listener({ type: "run_updated", run });
          }
        });
        await firstResponseDeferred.promise;
      }

      return { message: userMessage, run };
    };

    render(<App apiClient={client} />);

    const composer = await screen.findByLabelText("Message");
    const sendButton = screen.getByRole("button", { name: "Send" });

    fireEvent.change(composer, {
      target: { value: "first prompt" },
    });
    fireEvent.click(sendButton);

    fireEvent.change(composer, {
      target: { value: "second prompt" },
    });
    expect(sendButton).toBeDisabled();

    await waitFor(() => {
      expect(screen.getByText("first prompt")).toBeInTheDocument();
      expect(sendButton).toBeEnabled();
    });

    fireEvent.click(sendButton);
    await waitFor(() => {
      expect(createCount).toBe(2);
    });

    await act(async () => {
      firstResponseDeferred.resolve();
      await Promise.resolve();
    });
  });

  it("reconciles echoed user messages from snapshots before create-run resolves", async () => {
    const firstResponseDeferred = createDeferred<void>();
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    let createCount = 0;
    const client = createMockApiClient();

    client.subscribeSessionEvents = (_channelId, _sessionId, onEvent) => {
      eventListeners.add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages: [],
          active_run: null,
        });
      });
      return () => eventListeners.delete(onEvent);
    };

    client.createChannelRun = async (_channelId, sessionId, input) => {
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

      if (createCount === 1) {
        queueMicrotask(() => {
          for (const listener of eventListeners) {
            listener({
              type: "messages_snapshot",
              messages: [userMessage],
              active_run: {
                run_id: run.run_id,
                status: "running",
                created_at: run.created_at,
                updated_at: run.updated_at,
              },
            });
          }
        });
        await firstResponseDeferred.promise;
      }

      return { message: userMessage, run };
    };

    render(<App apiClient={client} />);

    const composer = await screen.findByLabelText("Message");
    const sendButton = screen.getByRole("button", { name: "Send" });

    fireEvent.change(composer, {
      target: { value: "first prompt" },
    });
    fireEvent.click(sendButton);

    fireEvent.change(composer, {
      target: { value: "second prompt" },
    });
    expect(sendButton).toBeDisabled();

    await waitFor(() => {
      expect(screen.getByText("first prompt")).toBeInTheDocument();
      expect(sendButton).toBeEnabled();
    });

    fireEvent.click(sendButton);
    await waitFor(() => {
      expect(createCount).toBe(2);
    });

    await act(async () => {
      firstResponseDeferred.resolve();
      await Promise.resolve();
    });
  });

  it("does not carry a pending send lock into a newly created session", async () => {
    const firstResponseDeferred = createDeferred<void>();
    let createCount = 0;
    const client = createMockApiClient();

    client.createChannelRun = async (_channelId, sessionId, input) => {
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
      if (createCount === 1) {
        await firstResponseDeferred.promise;
      }
      return { message: userMessage, run };
    };

    render(<App apiClient={client} />);

    const composer = await screen.findByLabelText("Message");
    const sendButton = screen.getByRole("button", { name: "Send" });

    fireEvent.change(composer, {
      target: { value: "first prompt" },
    });
    fireEvent.click(sendButton);

    await waitFor(() => {
      expect(sendButton).toBeDisabled();
    });

    fireEvent.click(screen.getByRole("button", { name: "New chat" }));

    await waitFor(() => {
      expect(screen.queryByText("first prompt")).not.toBeInTheDocument();
    });

    fireEvent.change(composer, {
      target: { value: "prompt in new session" },
    });
    await waitFor(() => {
      expect(sendButton).toBeEnabled();
    });

    fireEvent.click(sendButton);
    await waitFor(() => {
      expect(createCount).toBe(2);
    });

    await act(async () => {
      firstResponseDeferred.resolve();
      await Promise.resolve();
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

  it("updates the visible conversation title from live session events", async () => {
    let listener: ((event: ChannelSessionEvent) => void) | null = null;
    const client = createMockApiClient({
      subscribeSessionEvents(_channelId, _sessionId, onEvent) {
        listener = onEvent;
        queueMicrotask(() => {
          onEvent({
            type: "messages_snapshot",
            messages: [],
            active_run: null,
          });
        });
        return () => {
          listener = null;
        };
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Session" })).toBeInTheDocument();

    await act(async () => {
      listener?.({
        type: "session_updated",
        session: {
          id: "session-1",
          title: "自动标题",
          created_at: 1,
          updated_at: 2,
        },
      });
    });

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "自动标题" })).toBeInTheDocument();
    });
    expect(screen.getAllByText("自动标题").length).toBeGreaterThanOrEqual(2);
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
      expect(
        document.querySelector(".message-bubble.assistant.empty-body"),
      ).not.toBeInTheDocument();
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
          detail: '{"prompt":"cat"}',
        },
      ],
    });

    render(<App apiClient={hubRun.client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "make ppt" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText('call image generation：{"prompt":"cat"}')).toBeInTheDocument();
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

  it("uses backend message kind to render execution history without content guessing", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-kind-execution",
                session_id: "session-1",
                role: "assistant",
                message_kind: "execution",
                content: `<!-- hermes-hub:execution:v1 -->\n${JSON.stringify([
                  { kind: "tool.call", tool: "terminal", detail: "from kind" },
                ])}`,
                attachments: [],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    expect(await screen.findByText("Execution steps")).toBeInTheDocument();
    expect(screen.getByText("call terminal：from kind")).toBeInTheDocument();
  });

  it("does not parse execution-looking content when backend marks it as text", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-kind-text",
                session_id: "session-1",
                role: "assistant",
                message_kind: "text",
                content: `💻 terminal(['command'])\n{"command":"show raw text"}`,
                attachments: [],
                created_at: 1,
              },
            ],
          },
        })}
      />,
    );

    await waitFor(() => {
      expect(document.querySelector(".message-list")?.textContent).toContain("show raw text");
    });
    expect(screen.queryByText("Execution steps")).not.toBeInTheDocument();
    expect(document.querySelector(".message-list")?.textContent).toContain("💻 terminal");
  });

  it("virtualizes long chat histories while keeping recent messages rendered", async () => {
    const messages = Array.from(
      { length: 140 },
      (_, index): ChannelMessage => ({
        id: `message-long-${index}`,
        session_id: "session-1",
        role: index % 2 === 0 ? "user" : "assistant",
        message_kind: "text",
        content: `message ${index}`,
        attachments: [],
        created_at: index + 1,
      }),
    );

    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": messages,
          },
        })}
      />,
    );

    expect(await screen.findByText("message 139")).toBeInTheDocument();
    await waitFor(() => {
      const bubbles = document.querySelectorAll(".message-bubble");
      expect(bubbles.length).toBeGreaterThan(0);
      expect(bubbles.length).toBeLessThan(80);
    });
    expect(screen.queryByText("message 0")).not.toBeInTheDocument();
  });

  it("opens a session with only the two most recent conversation turns mounted", async () => {
    const messages = Array.from(
      { length: 12 },
      (_, index): ChannelMessage => ({
        id: `message-window-${index}`,
        session_id: "session-1",
        role: index % 2 === 0 ? "user" : "assistant",
        message_kind: "text",
        content: `window message ${index}`,
        attachments: [],
        created_at: index + 1,
      }),
    );

    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": messages,
          },
        })}
      />,
    );

    expect(await screen.findByText("window message 11")).toBeInTheDocument();
    expect(screen.getByText("window message 8")).toBeInTheDocument();
    expect(screen.queryByText("window message 7")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Load earlier messages" }));
    expect(await screen.findByText("window message 0")).toBeInTheDocument();
  });

  it("renders live session events immediately in long chat histories", async () => {
    const messages = Array.from(
      { length: 140 },
      (_, index): ChannelMessage => ({
        id: `message-live-long-${index}`,
        session_id: "session-1",
        role: index % 2 === 0 ? "user" : "assistant",
        message_kind: "text",
        content: `live history ${index}`,
        attachments: [],
        created_at: index + 1,
      }),
    );
    let listener: ((event: ChannelSessionEvent) => void) | null = null;
    const client = createMockApiClient({
      subscribeSessionEvents(_channelId, _sessionId, onEvent) {
        listener = onEvent;
        queueMicrotask(() => {
          onEvent({
            type: "messages_snapshot",
            messages,
            active_run: null,
          });
        });
        return () => {
          listener = null;
        };
      },
    });

    render(<App apiClient={client} />);

    expect(await screen.findByText("live history 139")).toBeInTheDocument();
    await act(async () => {
      listener?.({
        type: "message_created",
        message: {
          id: "message-live-now",
          session_id: "session-1",
          role: "assistant",
          message_kind: "text",
          content: "live event result",
          attachments: [],
          created_at: 141,
        },
      });
    });

    expect(screen.getByText("live event result")).toBeInTheDocument();
    await waitFor(() => {
      expect(document.querySelectorAll(".message-bubble").length).toBeLessThan(80);
    });
  });

  it("keeps execution bubbles in the session append order", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              {
                id: "message-user-order",
                session_id: "session-1",
                role: "user",
                content: "开始",
                attachments: [],
                created_at: 1,
              },
              {
                id: "message-final-order",
                session_id: "session-1",
                role: "assistant",
                content: "先到的正式回复",
                attachments: [],
                created_at: 1,
              },
              legacyExecutionMessage(
                `💻 terminal(['command'])\n{"command":"late tool"}`,
                "message-execution-order",
                1,
              ),
            ],
          },
        })}
      />,
    );

    await screen.findByText("先到的正式回复");
    const messageListText = document.querySelector(".message-list")?.textContent ?? "";
    expect(messageListText.indexOf("先到的正式回复")).toBeLessThan(
      messageListText.indexOf('call terminal：{"command":"late tool"}'),
    );
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
    await waitFor(() => expect(eventListeners.size).toBe(1));

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

  it("renders live hermes_hub_send execution steps that arrive before run state settles", async () => {
    const eventListeners = new Set<(event: ChannelSessionEvent) => void>();
    const run: ChannelRun = {
      id: "run-storage-id-send",
      run_id: "hub-run-early-send-execution",
      session_id: "session-1",
      user_message_id: "message-user-send",
      status: "running",
      input: "send file",
      input_attachments: [],
      created_at: Date.now(),
      updated_at: Date.now(),
    };
    const client = createMockApiClient({
      async createChannelRun(_channelId, sessionId, input) {
        const userMessage: ChannelMessage = {
          id: "message-user-send",
          session_id: sessionId,
          role: "user",
          client_message_key: input.clientMessageKey,
          content: input.content,
          attachments: input.attachments ?? [],
          created_at: Date.now(),
        };
        const execution = executionMessage(
          [{ kind: "tool.call", tool: "hermes_hub_send", detail: "MEDIA:/workspace/report.txt" }],
          "message-early-send-execution",
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
    await waitFor(() => expect(eventListeners.size).toBe(1));

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "send file" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(
      await screen.findByText("call hermes_hub_send：MEDIA:/workspace/report.txt"),
    ).toBeInTheDocument();
    await waitFor(() => {
      const pendingBubble = document.querySelector(".message-bubble.assistant.pending");
      expect(pendingBubble?.textContent).toContain(
        "call hermes_hub_send：MEDIA:/workspace/report.txt",
      );
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
    expect(await screen.findByText("completed terminal：生成PPT文件")).toBeInTheDocument();

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
                  {
                    kind: "tool.completed",
                    tool: "image_generate",
                    detail: "image done",
                  },
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

  it("renders hermes_hub_send in execution history alongside other tool steps", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              executionMessage(
                [
                  {
                    kind: "tool.call",
                    tool: "hermes_hub_send",
                    detail: "MEDIA:/workspace/report.txt",
                  },
                  {
                    kind: "tool.call",
                    tool: "terminal",
                    detail: "echo visible",
                  },
                ],
                "message-hidden-send-tool",
              ),
            ],
          },
        })}
      />,
    );

    expect(
      await screen.findByText("call hermes_hub_send：MEDIA:/workspace/report.txt"),
    ).toBeInTheDocument();
    expect(await screen.findByText("call terminal：echo visible")).toBeInTheDocument();
  });

  it("renders execution history messages that only contain hermes_hub_send", async () => {
    render(
      <App
        apiClient={createMockApiClient({
          initialMessagesBySessionId: {
            "session-1": [
              executionMessage(
                [
                  {
                    kind: "tool.call",
                    tool: "hermes_hub_send",
                    detail: "MEDIA:/workspace/archive.zip",
                  },
                ],
                "message-only-send-tool",
              ),
            ],
          },
        })}
      />,
    );

    expect(
      await screen.findByText("call hermes_hub_send：MEDIA:/workspace/archive.zip"),
    ).toBeInTheDocument();
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
            listener({
              type: "message_created",
              message: executionMessage(events),
            });
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
    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => undefined);
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
      const message =
        "You can create up to 2 sessions. Delete an old session before creating a new one.";
      expect(alertSpy).toHaveBeenCalledWith(message);
      expect(screen.queryByText(message)).not.toBeInTheDocument();
    });
  });

  it("keeps automatic session-create limit errors inline when sending a first message", async () => {
    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => undefined);
    const client = createMockApiClient({
      createSession: async () => {
        throw new ApiRequestError("session limit exceeded", {
          error: "session_limit_exceeded",
          max_sessions_per_user: 2,
        });
      },
    });
    client.listSessionsPublic = vi.fn(async () => []);

    render(<App apiClient={client} />);

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      const message =
        "You can create up to 2 sessions. Delete an old session before creating a new one.";
      expect(screen.getByText(message)).toBeInTheDocument();
      expect(alertSpy).not.toHaveBeenCalled();
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

  it("shows and saves LDAP authentication settings", async () => {
    const client = createMockApiClient();
    const updateSystemSettings = client.updateSystemSettings.bind(client);
    let savedSettings: unknown = null;
    client.updateSystemSettings = vi.fn(async (settings) => {
      savedSettings = settings;
      await updateSystemSettings(settings);
    });

    render(<App apiClient={client} />);

    await openSettingsTab("Authentication settings");

    expect(await screen.findByLabelText("Enable LDAP")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Enable LDAP"));
    fireEvent.change(screen.getByLabelText("LDAP display name"), {
      target: { value: "Corporate LDAP" },
    });
    fireEvent.change(screen.getByLabelText("LDAP URL"), {
      target: { value: "ldap://ldap.example.com:389" },
    });
    fireEvent.change(screen.getByLabelText("LDAP bind DN"), {
      target: { value: "cn=readonly,dc=example,dc=com" },
    });
    fireEvent.change(screen.getByLabelText("LDAP bind password"), {
      target: { value: "ldap-secret" },
    });
    fireEvent.change(screen.getByLabelText("LDAP base DN"), {
      target: { value: "dc=example,dc=com" },
    });
    fireEvent.change(screen.getByLabelText("LDAP user filter"), {
      target: { value: "(mail={email})" },
    });
    fireEvent.change(screen.getByLabelText("LDAP email attribute"), {
      target: { value: "mail" },
    });
    expect(screen.getByLabelText("Auto-create LDAP users")).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByText("Settings saved")).toBeInTheDocument();
    expect(client.updateSystemSettings).toHaveBeenCalledTimes(1);
    expect(savedSettings).toMatchObject({
      ldap: {
        enabled: true,
        display_name: "Corporate LDAP",
        url: "ldap://ldap.example.com:389",
        bind_dn: "cn=readonly,dc=example,dc=com",
        bind_password: "ldap-secret",
        base_dn: "dc=example,dc=com",
        user_filter: "(mail={email})",
        email_attribute: "mail",
        auto_create_users: true,
      },
    });
  });

  it("shows and saves system attachment parameters", async () => {
    const client = createMockApiClient();
    const updateSystemSettings = client.updateSystemSettings.bind(client);
    let savedSettings: unknown = null;
    client.updateSystemSettings = vi.fn(async (settings) => {
      savedSettings = settings;
      await updateSystemSettings(settings);
    });

    render(<App apiClient={client} />);

    await openSettingsTab("System parameters");
    expect(
      await screen.findByText(
        "ASR service is not configured or the deployment switch is disabled.",
      ),
    ).toBeInTheDocument();

    fireEvent.change(await screen.findByLabelText("Max attachment upload size (MB)"), {
      target: { value: "15000" },
    });
    fireEvent.change(screen.getByLabelText("Attachment retention (days)"), {
      target: { value: "14" },
    });
    fireEvent.click(screen.getByLabelText("Enable speech input"));
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByText("Settings saved")).toBeInTheDocument();
    expect(savedSettings).toMatchObject({
      max_attachment_upload_bytes: 15000 * 1024 * 1024,
      attachment_retention_days: 14,
      speech_input: {
        enabled: true,
      },
    });
  });

  it("shows and saves public platform settings", async () => {
    const client = createMockApiClient();
    const updateSystemSettings = client.updateSystemSettings.bind(client);
    let savedSettings: unknown = null;
    client.updateSystemSettings = vi.fn(async (settings) => {
      savedSettings = settings;
      await updateSystemSettings(settings);
    });

    render(<App apiClient={client} />);

    const settingsTabs = await openSettingsTab("Public platform");
    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Public platform" }));
    expect(screen.getByRole("button", { name: "Rebuild public Hermes" })).toBeDisabled();

    fireEvent.change(await screen.findByLabelText("Temporary session retention (hours)"), {
      target: { value: "48" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    expect(await screen.findByText("Settings saved")).toBeInTheDocument();
    expect(savedSettings).toMatchObject({
      public_platform: {
        temporary_session_retention_hours: 48,
      },
    });
  });

  it("shows public platform Hermes status and rebuild feedback", async () => {
    const deferred = createDeferred<void>();
    const client = createMockApiClient({
      publicPlatformSettings: {
        enabled: true,
      },
      publicPlatformInstance: {
        id: "public-instance-1",
        user_id: "public-user-1",
        kind: "managed_docker",
        status: "running",
        health_status: "healthy",
        runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:v2026.5.29.2",
        runtime_version: "v2026.5.29.2",
        last_user_activity_at: null,
        last_started_at: 1_735_689_600,
        last_stopped_at: null,
        stopped_reason: null,
      },
    });
    const rebuildPublicPlatformHermesInstance = vi.fn(async () => {
      await deferred.promise;
      return {
        id: "public-instance-1",
        user_id: "public-user-1",
        kind: "managed_docker",
        status: "running",
        health_status: "starting",
        runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:v2026.5.29.2",
        runtime_version: "v2026.5.29.2",
        last_user_activity_at: null,
        last_started_at: 1_735_689_700,
        last_stopped_at: null,
        stopped_reason: null,
      } satisfies HermesInstance;
    });
    client.rebuildPublicPlatformHermesInstance = rebuildPublicPlatformHermesInstance;

    render(<App apiClient={client} />);

    const settingsTabs = await openSettingsTab("Public platform");
    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Public platform" }));

    expect(await screen.findByText("Public Hermes")).toBeInTheDocument();
    expect(screen.getByText("Ready")).toBeInTheDocument();
    expect(screen.getByText("Yes")).toBeInTheDocument();
    expect(screen.getByText("healthy")).toBeInTheDocument();
    expect(screen.getByText("v2026.5.29.2")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Rebuild public Hermes" }));
    expect(await screen.findByRole("button", { name: "Rebuilding..." })).toBeDisabled();
    expect(rebuildPublicPlatformHermesInstance).toHaveBeenCalledTimes(1);

    deferred.resolve();
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Rebuild public Hermes" })).toBeInTheDocument();
    });
  });

  it("shows paginated public sessions and force clears a session", async () => {
    const publicSessions: PublicPlatformSessionSummary[] = Array.from({ length: 12 }, (_, index) => {
      const number = index + 1;
      return {
        id: `public-session-${number}`,
        title: `Public session ${number}`,
        created_at: 1_735_600_000 + number,
        updated_at: 1_735_700_000 - index,
        recycle_at: 1_735_800_000 + number,
        public_url: `/public/sessions/public-session-${number}`,
      };
    });
    const client = createMockApiClient({
      publicPlatformSettings: {
        enabled: true,
      },
      publicPlatformInstance: {
        id: "public-instance-1",
        user_id: "public-user-1",
        kind: "managed_docker",
        status: "running",
        health_status: "healthy",
        runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:v2026.5.29.2",
        runtime_version: "v2026.5.29.2",
        last_user_activity_at: null,
        last_started_at: 1_735_689_600,
        last_stopped_at: null,
        stopped_reason: null,
      },
      initialPublicPlatformSessions: publicSessions,
    });
    const clearDeferred = createDeferred<void>();
    const forceClearPublicPlatformSession = client.forceClearPublicPlatformSession.bind(client);
    client.forceClearPublicPlatformSession = vi.fn(async (sessionId) => {
      await clearDeferred.promise;
      await forceClearPublicPlatformSession(sessionId);
    });

    render(<App apiClient={client} />);

    const settingsTabs = await openSettingsTab("Public platform");
    fireEvent.click(within(settingsTabs).getByRole("tab", { name: "Public platform" }));

    const table = await screen.findByRole("table", { name: "Public sessions" });
    expect(within(table).getByText("Public session 1")).toBeInTheDocument();
    expect(within(table).getByText("Public session 10")).toBeInTheDocument();
    expect(within(table).queryByText("Public session 11")).not.toBeInTheDocument();
    expect(screen.getByText("Page 1 of 2, 12 total")).toBeInTheDocument();
    const firstLink = `${window.location.origin}/public/sessions/public-session-1`;
    expect(within(table).getByRole("link", { name: firstLink })).toHaveAttribute(
      "href",
      firstLink,
    );

    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(await screen.findByText("Public session 11")).toBeInTheDocument();
    expect(screen.getByText("Public session 12")).toBeInTheDocument();
    expect(screen.getByText("Page 2 of 2, 12 total")).toBeInTheDocument();

    const sessionRow = screen.getByText("Public session 11").closest("tr");
    expect(sessionRow).not.toBeNull();
    fireEvent.click(
      within(sessionRow as HTMLTableRowElement).getByRole("button", { name: "Force clear" }),
    );
    expect(await screen.findByRole("button", { name: "Clearing..." })).toBeDisabled();
    expect(client.forceClearPublicPlatformSession).toHaveBeenCalledWith("public-session-11");

    clearDeferred.resolve();
    await waitFor(() => {
      expect(screen.queryByText("Public session 11")).not.toBeInTheDocument();
    });
    expect(screen.getByText("Public session 12")).toBeInTheDocument();
    expect(screen.getByText("Page 2 of 2, 11 total")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Previous" }));
    expect(await screen.findByText("Public session 1")).toBeInTheDocument();
    expect(screen.getByText("Page 1 of 2, 11 total")).toBeInTheDocument();
  });

  it("localizes the configured session limit message in Chinese", async () => {
    localStorage.setItem("hermes-hub-language", "zh");
    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => undefined);
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
      const message = "最多创建2个会话，请删除旧会话后再创建新会话";
      expect(alertSpy).toHaveBeenCalledWith(message);
      expect(screen.queryByText(message)).not.toBeInTheDocument();
    });
  });

  it("renders login and authenticates with email and password", async () => {
    const client = createMockApiClient();
    await client.logout();

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "admin@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "admin-password-123" },
    });
    const signInButton = screen.getByRole("button", { name: "Sign in" });
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
        allow_password_login: true,
      },
    });

    render(<App apiClient={client} />);

    fireEvent.click(await screen.findByRole("button", { name: "Sign in" }));

    const oidcButton = await screen.findByRole("link", {
      name: "Sign in with Acme SSO",
    });
    expect(oidcButton).toHaveAttribute("href", "/api/auth/oidc/start");
  });

  it("shows LDAP sign-in when it is enabled and authenticates through LDAP", async () => {
    const ldapLogin = vi.fn(async (email: string, _password: string) => ({
      id: "ldap-user-1",
      email,
      role: "user" as const,
      status: "active" as const,
    }));
    const client = createMockApiClient({
      initialUser: null,
      ldapPublicConfig: {
        enabled: true,
        display_name: "Corporate LDAP",
      },
      ldapLogin,
    });

    render(<App apiClient={client} />);

    const ldapButton = await screen.findByRole("button", {
      name: "Sign in with Corporate LDAP",
    });
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "person@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "ldap-password" },
    });
    fireEvent.click(ldapButton);

    await waitFor(() => {
      expect(ldapLogin).toHaveBeenCalledWith("person@example.com", "ldap-password");
    });
    expect(await screen.findByRole("button", { name: "New chat" })).toBeInTheDocument();
  });

  it("hides first-admin registration entry when bootstrap is closed", async () => {
    const client = createMockApiClient({
      initialUser: null,
      bootstrapOpen: false,
    });

    render(<App apiClient={client} />);

    expect(await screen.findByLabelText("Email")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Need to create the first admin?" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Confirm password")).not.toBeInTheDocument();
  });

  it("shows the first-user registration form without the app sidebar", async () => {
    const client = createMockApiClient({
      initialUser: null,
      bootstrapOpen: true,
    });

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
    const client = createMockApiClient({
      initialUser: null,
      bootstrapOpen: false,
    });

    render(<App apiClient={client} />);

    expect(await screen.findByRole("button", { name: "Create account" })).toBeInTheDocument();
    expect(screen.getByLabelText("Confirm password")).toBeInTheDocument();
    expect(screen.queryByLabelText("Primary")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Invite token")).not.toBeInTheDocument();
    expect(screen.queryByDisplayValue("secret-invite-token")).not.toBeInTheDocument();
    expect(screen.queryByText("secret-invite-token")).not.toBeInTheDocument();
  });

  it("returns to sign in after invite registration creates the account", async () => {
    window.history.pushState({}, "", "/?invite=secret-invite-token");
    const client = createMockApiClient({
      initialUser: null,
      bootstrapOpen: false,
    });

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
  }, 10_000);

  it("can create a managed Hermes instance for a user without one", async () => {
    const client = createMockApiClient({
      initialInstance: null,
    });
    client.ensureHermes = vi.fn(async () => {
      throw new Error("prewarm disabled for this action-focused test");
    });
    client.listUsers = async () => [
      {
        id: "user-1",
        email: "admin@example.com",
        role: "admin",
        status: "active",
      },
      {
        id: "user-2",
        email: "user@example.com",
        role: "user",
        status: "active",
      },
    ];

    render(<App apiClient={client} />);

    await openSettingsTab("Hermes management");
    const createButtons = await screen.findAllByRole("button", {
      name: "Create",
    });
    expect(createButtons.length).toBeGreaterThan(0);
    fireEvent.click(createButtons[0]);

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

  it("shows Hermes start time and estimated stop time in the management table", async () => {
    const lastStartedAt = 1_735_689_600;
    const lastUserActivityAt = 1_735_690_200;
    const expectedStarted = new Intl.DateTimeFormat("en-US", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(lastStartedAt * 1000));
    const expectedStop = new Intl.DateTimeFormat("en-US", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date((lastUserActivityAt + 30 * 60) * 1000));

    render(
      <App
        apiClient={createMockApiClient({
          initialInstance: {
            id: "instance-1",
            user_id: "user-1",
            kind: "managed_docker",
            status: "running",
            last_started_at: lastStartedAt,
            last_user_activity_at: lastUserActivityAt,
            last_stopped_at: null,
          },
        })}
      />,
    );

    await openSettingsTab("Hermes management");

    expect(await screen.findByRole("columnheader", { name: "Started" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Stop time" })).toBeInTheDocument();
    expect(screen.getByText(expectedStarted)).toBeInTheDocument();
    expect(screen.getByText(`Estimated: ${expectedStop}`)).toBeInTheDocument();
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
    const schedulerTable = await screen.findByRole("table", {
      name: "Scheduled tasks",
    });
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

    const scheduledTasksNav = await screen.findByRole("button", {
      name: "Scheduled tasks",
    });
    const personalizationNav = screen.getByRole("button", {
      name: "Personal settings",
    });
    expect(
      scheduledTasksNav.compareDocumentPosition(personalizationNav) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(scheduledTasksNav);

    await waitFor(() => {
      expect(workspaceHermesSchedulerSnapshot).toHaveBeenCalled();
    });
    const schedulerTable = await screen.findByRole("table", {
      name: "Scheduled tasks",
    });
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
    client.ensureHermes = vi.fn(async () => {
      throw new Error("prewarm disabled for this action-focused test");
    });
    client.listUsers = async () => [
      {
        id: "user-1",
        email: "admin@example.com",
        role: "admin",
        status: "active",
      },
      {
        id: "user-2",
        email: "user@example.com",
        role: "user",
        status: "active",
      },
    ];
    const originalCreateHermesInstance = client.createHermesInstance;
    const createHermesInstance = vi.fn(async (userId: string) => {
      await deferred.promise;
      return originalCreateHermesInstance(userId);
    });
    client.createHermesInstance = createHermesInstance;

    render(<App apiClient={client} />);

    await openSettingsTab("Hermes management");
    fireEvent.click((await screen.findAllByRole("button", { name: "Create" }))[0]);

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
    const saveManagedSkill = vi.fn(async (path: string, content: string) => ({
      path,
      content,
    }));
    const deleteManagedSkill = vi.fn(async () => undefined);
    const createManagedSkillDirectory = vi.fn(async () => undefined);

    render(
      <App
        apiClient={createMockApiClient({
          initialManagedSkills: {
            ".DS_Store": "metadata",
            "image/SKILL.md": "# Image\n\nUse sharp visual prompts.\n",
            "writing/.hidden.md": "hidden notes",
            "writing/references/style.md": "Use direct language.\n",
          },
          initialManagedSkillDirectories: [
            "research",
            "writing/.cache",
            "writing/drafts/empty-child",
          ],
          saveManagedSkill,
          deleteManagedSkill,
          createManagedSkillDirectory,
        })}
      />,
    );

    await openSettingsTab("统一 Skill 管理", "系统设置");

    expect(await screen.findByRole("button", { name: "writing" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "统一 Skill 管理" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "research" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "references" })).toBeInTheDocument();
    const styleButton = managedSkillTreeButton("writing/references/style.md");
    expect(styleButton).toHaveTextContent("style.md");
    expect(styleButton).toHaveTextContent(/\d+ B/);
    expect(
      screen.queryByRole("button", { name: /writing\/references\/style\.md/ }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(".DS_Store")).not.toBeInTheDocument();
    expect(screen.queryByText(".hidden.md")).not.toBeInTheDocument();
    expect(screen.queryByText(".cache")).not.toBeInTheDocument();
    const skillList = document.querySelector(".skill-list");
    expect(skillList).toBeInTheDocument();
    expect(within(skillList as HTMLElement).queryByText("文件夹")).not.toBeInTheDocument();
    expect(within(skillList as HTMLElement).queryByText("文件")).not.toBeInTheDocument();

    const imageSkillButton = managedSkillTreeButton("image/SKILL.md");
    expect(imageSkillButton).toHaveTextContent("SKILL.md");
    expect(imageSkillButton).toHaveTextContent(/\d+ B/);
    fireEvent.click(imageSkillButton);
    expect(await screen.findByText("Skill 路径：image/SKILL.md")).toBeInTheDocument();
    expect(screen.queryByLabelText("Skill 路径")).not.toBeInTheDocument();
    const skillEditor = await screen.findByRole("textbox", {
      name: "Skill 内容",
    });
    expect(skillEditor).toHaveClass("skill-vditor-editor");
    const skillVditor = vditorInstanceForLabel("Skill 内容");
    await waitFor(() => {
      expect(skillVditor.setValue).toHaveBeenCalledWith(
        "# Image\n\nUse sharp visual prompts.\n",
        true,
      );
    });

    await act(async () => {
      (skillVditor.options.input as (value: string) => void)(
        "# Image\n\nUse cinematic visual prompts.\n",
      );
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
      expect(
        Array.from(
          document.querySelectorAll<HTMLButtonElement>(
            "[data-managed-skill-path='image/SKILL.md']",
          ),
        ).some((button) => button.getAttribute("data-managed-skill-path") === "image/SKILL.md"),
      ).toBe(false);
    });
    expect(screen.queryByLabelText("Skill 路径")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(vditorInstanceForLabel("Skill 内容").setValue).toHaveBeenLastCalledWith("", true);
    });
    expect(screen.getByRole("button", { name: "删除" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "writing" }));
    fireEvent.click(screen.getByRole("button", { name: "新建文件夹" }));
    expect(screen.queryByLabelText("Skill 路径")).not.toBeInTheDocument();
    expect(screen.getByText("Skill 路径：writing/new-folder")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "创建文件夹" }));
    await waitFor(() => {
      expect(createManagedSkillDirectory).toHaveBeenCalledWith("writing/new-folder");
      expect(screen.getByRole("button", { name: "new-folder" })).toBeInTheDocument();
    });

    expect(await screen.findByText("Skill 路径：writing/new-folder")).toBeInTheDocument();
    expect(screen.queryByLabelText("Skill 路径")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "new-folder" }));
    fireEvent.click(screen.getByRole("button", { name: "新建 Skill" }));
    expect(screen.queryByLabelText("Skill 路径")).not.toBeInTheDocument();
    expect(screen.getByText("Skill 路径：writing/new-folder/SKILL.md")).toBeInTheDocument();
    expect(vditorInstanceForLabel("Skill 内容").value).toBe("");
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

    await screen.findByRole("button", { name: "image" });
    expect(
      Boolean(
        Array.from(document.querySelectorAll<HTMLButtonElement>("[data-managed-skill-path]")).find(
          (button) => button.getAttribute("data-managed-skill-path") === "image/SKILL.md",
        ),
      ),
    ).toBe(true);
    expect(screen.queryByRole("heading", { name: "统一 Skill 管理" })).not.toBeInTheDocument();
    expect(screen.queryByText("managed skill not found")).not.toBeInTheDocument();
  });

  it("allows deleting a binary managed skill even when text loading fails", async () => {
    const deleteManagedSkill = vi.fn(async () => undefined);

    render(
      <App
        apiClient={createMockApiClient({
          initialManagedSkills: {
            "mindoc-search.tgz": "binary-placeholder",
          },
          readManagedSkill: async (path) => {
            if (path === "mindoc-search.tgz") {
              throw new Error("managed skill is not valid utf-8");
            }
            return { path, content: "" };
          },
          deleteManagedSkill,
        })}
      />,
    );

    await openSettingsTab("Managed skills");

    fireEvent.click(await screen.findByRole("button", { name: /mindoc-search\.tgz/ }));
    expect(await screen.findByText("managed skill is not valid utf-8")).toBeInTheDocument();
    expect(screen.getByText("Skill path: mindoc-search.tgz")).toBeInTheDocument();
    expect(screen.queryByLabelText("Skill path")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete" })).not.toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    await waitFor(() => {
      expect(deleteManagedSkill).toHaveBeenCalledWith("mindoc-search.tgz");
    });
  });

  it("uploads managed skill folders from the admin tree without a zip upload control", async () => {
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
    fireEvent.click(await screen.findByRole("button", { name: "writing" }));
    expect(screen.queryByRole("button", { name: "上传压缩包" })).not.toBeInTheDocument();
    expect(screen.queryByTestId("managed-skills-zip-input")).not.toBeInTheDocument();

    const folderFile = new File(["# Research\n"], "SKILL.md", {
      type: "text/markdown",
    });
    Object.defineProperty(folderFile, "webkitRelativePath", {
      value: "research/SKILL.md",
    });
    fireEvent.change(screen.getByTestId("managed-skills-folder-input"), {
      target: { files: [folderFile] },
    });
    await waitFor(() => {
      expect(uploadManagedSkills).toHaveBeenCalledWith([folderFile], "writing");
      expect(screen.getByRole("button", { name: "research" })).toBeInTheDocument();
      expect(managedSkillTreeButton("writing/research/SKILL.md")).toBeInTheDocument();
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
          fallback: {
            enabled: true,
            provider_name: "fallback-provider",
            provider_base_url: "https://fallback-provider.example/v1",
            provider_api_key: "stored-fallback-key",
            default_model: "gpt-4.1-fallback",
            allowed_models: ["gpt-4.1-fallback"],
            api_type: "responses",
            reasoning_effort: "low",
            allow_streaming: true,
            request_timeout_seconds: 45,
            context_window_tokens: 100000,
            max_output_tokens: 2048,
            temperature: 0.2,
            supports_parallel_tools: false,
          },
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
    expect(status.model_config.fallback?.provider_api_key).toBe("stored-fallback-key");
    expect(status.model_config.fallback?.max_output_tokens).toBe(2048);
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

  it("uses the real speech input API endpoints", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path) => {
      if (path === "/api/speech-input/config") {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            speech_input: {
              enabled: true,
              runtime_available: true,
              max_duration_seconds: 60,
              sample_rate: 16000,
              model: "streaming-zh",
            },
          }),
        } as Response;
      }
      throw new Error(`unexpected fetch ${String(path)}`);
    });

    const client = createApiClient();
    await expect(client.speechInputConfig()).resolves.toEqual({
      enabled: true,
      runtime_available: true,
      max_duration_seconds: 60,
      sample_rate: 16000,
      model: "streaming-zh",
    });
    fetchMock.mockRestore();
  });

  it("sends fallback model settings through the real API client", async () => {
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
      supports_parallel_tools: true,
      fallback: {
        enabled: true,
        provider_name: "fallback-provider",
        provider_base_url: "https://fallback-provider.example/v1",
        provider_api_key: "fallback-key",
        default_model: "gpt-4.1-fallback",
        allowed_models: [],
        api_type: "responses",
        reasoning_effort: "low",
        allow_streaming: true,
        request_timeout_seconds: 45,
        context_window_tokens: 100000,
        max_output_tokens: 2048,
        temperature: 0.2,
        supports_parallel_tools: false,
      },
    });

    const requestBody = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(requestBody.fallback).toMatchObject({
      enabled: true,
      provider_name: "fallback-provider",
      provider_base_url: "https://fallback-provider.example/v1",
      provider_api_key: "fallback-key",
      default_model: "gpt-4.1-fallback",
      allowed_models: ["gpt-4.1-fallback"],
      api_type: "responses",
      reasoning_effort: "low",
      request_timeout_seconds: 45,
      context_window_tokens: 100000,
      max_output_tokens: 2048,
      temperature: 0.2,
      supports_parallel_tools: false,
    });
    fetchMock.mockRestore();
  });

  it("sends fallback model test requests through the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        ok: true,
        status_code: 200,
        message: "model test succeeded",
        duration_ms: 10,
      }),
    } as Response);

    await createApiClient().testModelFallbackConfig({
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
      supports_parallel_tools: true,
      fallback: {
        enabled: true,
        provider_name: "fallback-provider",
        provider_base_url: "https://fallback-provider.example/v1",
        provider_api_key: "fallback-key",
        default_model: "gpt-4.1-fallback",
        allowed_models: [],
        api_type: "responses",
        reasoning_effort: "low",
        allow_streaming: true,
        request_timeout_seconds: 45,
        context_window_tokens: 100000,
        max_output_tokens: 2048,
        temperature: 0.2,
        supports_parallel_tools: false,
      },
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/admin/model-config/llm/fallback/test",
      expect.objectContaining({
        method: "POST",
      }),
    );
    const requestBody = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(requestBody.fallback).toMatchObject({
      enabled: true,
      provider_name: "fallback-provider",
      provider_base_url: "https://fallback-provider.example/v1",
      provider_api_key: "fallback-key",
      default_model: "gpt-4.1-fallback",
    });
    fetchMock.mockRestore();
  });

  it("uses Hermes profile endpoints in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (path, init) => {
      if (path === "/api/admin/hermes-profile" && init?.method === "PUT") {
        expect(JSON.parse(String(init.body))).toEqual({
          soul_md: "# SOUL\n",
        });
        return {
          ok: true,
          status: 204,
        } as Response;
      }

      expect(path).toBe("/api/admin/hermes-profile");
      expect(init?.method).toBe("GET");
      return {
        ok: true,
        status: 200,
        json: async () => ({
          profile: {
            agents_md: "# Stored AGENTS\n",
            soul_md: "# Stored SOUL\n",
          },
        }),
      } as Response;
    });

    await expect(createApiClient().hermesProfile()).resolves.toEqual({
      soul_md: "# Stored SOUL\n",
    });
    await createApiClient().updateHermesProfile({
      soul_md: "# SOUL\n",
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
        expect(JSON.parse(init?.body as string)).toMatchObject({
          content: "updated answer",
        });
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
      expect(files).toHaveLength(1);
      expect(files[0]).toBeInstanceOf(File);
      expect((files[0] as File).name).toBe("research/SKILL.md");
      return {
        ok: true,
        status: 201,
        json: async () => ({
          skills: [{ path: "writing/research/SKILL.md", size: 5 }],
        }),
      } as Response;
    });

    const folderFile = new File(["hello"], "SKILL.md", {
      type: "text/markdown",
    });
    Object.defineProperty(folderFile, "webkitRelativePath", {
      value: "research/SKILL.md",
    });

    await expect(createApiClient().uploadManagedSkills([folderFile], "writing")).resolves.toEqual([
      { path: "writing/research/SKILL.md", size: 5 },
    ]);
    fetchMock.mockRestore();
  });

  it("rejects managed skill zip uploads before posting in the real API client", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    const zipFile = new File(["hello"], "skills.zip", {
      type: "application/zip",
    });

    await expect(createApiClient().uploadManagedSkills([zipFile], "writing")).rejects.toThrow(
      "managed skill zip uploads are not supported",
    );
    expect(fetchMock).not.toHaveBeenCalled();
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
