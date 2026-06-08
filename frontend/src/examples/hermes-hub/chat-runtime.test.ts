import { waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createChatRuntime } from "./chat-runtime";
import type {
  ExampleHubClient,
  ExampleHubConfig,
  ExampleSessionEvent,
  ExampleSessionSummary,
} from "./hub-client";
import { createLocalToolRegistry } from "./tool-registry";

const TEST_CONFIG: ExampleHubConfig = {
  baseUrl: "https://hub.example",
  clientId: "client-demo",
  clientSecret: "secret-demo",
  redirectUri: "https://app.example/examples/hermes-hub/",
  scopes: "openid profile email",
};

const TEST_SESSION: ExampleSessionSummary = {
  id: "session-1",
  title: "演示会话",
  isHome: false,
  deletable: true,
  hiddenFromWeb: true,
  createdAt: 1,
  updatedAt: 1,
};

describe("chat runtime", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("收到 pending business tool request 时会执行本地工具并回写结果", async () => {
    let emitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const submitBusinessToolResult = vi.fn().mockResolvedValue({
      id: "message-2",
      sessionId: "session-1",
      role: "assistant",
      messageKind: "text",
      clientMessageKey: "business-tool-result:1",
      content: "已保存笔记 note-1",
      attachments: [],
      createdAt: 2,
      updatedAt: 2,
    });
    const client: ExampleHubClient = {
      replaceTools: vi.fn(),
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
      listSessions: vi.fn(),
      createSession: vi.fn(),
      deleteSession: vi.fn(),
      appendMessage: vi.fn(),
      submitBusinessToolResult,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        emitEvent = onEvent;
        return () => undefined;
      }),
    };
    const notesStore = {
      saveNote: vi.fn().mockResolvedValue({
        id: "note-1",
        title: "未命名笔记",
        content: "把这条消息存起来",
        tags: ["demo"],
        createdAt: 100,
        updatedAt: 100,
      }),
      searchNotes: vi.fn(),
      listNotes: vi.fn(),
    };
    const toolStates: Array<{ requestId: string; status: string; summary: string }> = [];

    const runtime = createChatRuntime({
      client,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry: createLocalToolRegistry(notesStore),
      onToolCallUpdate(toolCall) {
        toolStates.push({
          requestId: toolCall.requestId,
          status: toolCall.status,
          summary: toolCall.summary,
        });
      },
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
    });

    runtime.start();
    expect(emitEvent).not.toBeNull();
    emitEvent!({
      type: "messages_snapshot",
      messages: [],
      activeRun: null,
      session: TEST_SESSION,
      businessToolRequests: [
        {
          type: "business_tool_request",
          request: {
            requestId: "req-1",
            sessionId: "session-1",
            integrationId: "crm",
            toolName: "save_note",
            arguments: {
              content: "把这条消息存起来",
              tags: ["demo"],
            },
            timeoutSeconds: 60,
            expiresAt: 9_999_999_999,
            status: "pending",
            createdAt: 1,
            updatedAt: 1,
          },
        },
      ],
    });
    await waitFor(() => {
      expect(notesStore.saveNote).toHaveBeenCalledWith({
        content: "把这条消息存起来",
        tags: ["demo"],
        title: undefined,
      });
    });
    await waitFor(() => {
      expect(submitBusinessToolResult).toHaveBeenCalledWith(
        TEST_CONFIG,
        "oauth-token",
        "session-1",
        "req-1",
        expect.stringContaining("note-1"),
      );
    });
    expect(toolStates.at(-1)).toEqual({
      requestId: "req-1",
      status: "completed",
      summary: "已写入本地笔记库",
    });
  });

  it("工具执行失败时仍会回写错误文本，保证链路闭环", async () => {
    let emitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const submitBusinessToolResult = vi.fn().mockResolvedValue({
      id: "message-3",
      sessionId: "session-1",
      role: "assistant",
      messageKind: "text",
      clientMessageKey: "business-tool-result:2",
      content: "工具执行失败",
      attachments: [],
      createdAt: 3,
      updatedAt: 3,
    });
    const client: ExampleHubClient = {
      replaceTools: vi.fn(),
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
      listSessions: vi.fn(),
      createSession: vi.fn(),
      deleteSession: vi.fn(),
      appendMessage: vi.fn(),
      submitBusinessToolResult,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        emitEvent = onEvent;
        return () => undefined;
      }),
    };
    const toolRegistry = createLocalToolRegistry({
      saveNote: vi.fn(),
      searchNotes: vi.fn(),
      listNotes: vi.fn(),
    });
    const toolStates: Array<{ requestId: string; status: string; summary: string }> = [];

    const runtime = createChatRuntime({
      client,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry,
      onToolCallUpdate(toolCall) {
        toolStates.push({
          requestId: toolCall.requestId,
          status: toolCall.status,
          summary: toolCall.summary,
        });
      },
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
    });

    runtime.start();
    expect(emitEvent).not.toBeNull();
    emitEvent!({
      type: "business_tool_request",
      request: {
        requestId: "req-invalid-1",
        sessionId: "session-1",
        integrationId: "crm",
        toolName: "save_note",
        arguments: {},
        timeoutSeconds: 60,
        expiresAt: 9_999_999_999,
        status: "pending",
        createdAt: 1,
        updatedAt: 1,
      },
    });
    await waitFor(() => {
      expect(submitBusinessToolResult).toHaveBeenCalledWith(
        TEST_CONFIG,
        "oauth-token",
        "session-1",
        "req-invalid-1",
        expect.stringContaining("工具执行失败"),
      );
    });
    expect(toolStates.at(-1)).toEqual({
      requestId: "req-invalid-1",
      status: "failed",
      summary: "工具执行失败",
    });
  });

  it("本地工具成功但结果回写失败时，不应伪造成工具执行失败结果再次回写", async () => {
    let emitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const submitBusinessToolResult = vi
      .fn()
      .mockRejectedValue(new Error("business tool request expired"));
    const client: ExampleHubClient = {
      replaceTools: vi.fn(),
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
      listSessions: vi.fn(),
      createSession: vi.fn(),
      deleteSession: vi.fn(),
      appendMessage: vi.fn(),
      submitBusinessToolResult,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        emitEvent = onEvent;
        return () => undefined;
      }),
    };
    const notesStore = {
      saveNote: vi.fn().mockResolvedValue({
        id: "note-2",
        title: "未命名笔记",
        content: "这条笔记已经落本地",
        tags: [],
        createdAt: 100,
        updatedAt: 100,
      }),
      searchNotes: vi.fn(),
      listNotes: vi.fn(),
    };
    const onLocalToolSuccess = vi.fn();
    const toolStates: Array<{ requestId: string; status: string; summary: string }> = [];

    const runtime = createChatRuntime({
      client,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry: createLocalToolRegistry(notesStore),
      onToolCallUpdate(toolCall) {
        toolStates.push({
          requestId: toolCall.requestId,
          status: toolCall.status,
          summary: toolCall.summary,
        });
      },
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
      onLocalToolSuccess,
    });

    runtime.start();
    expect(emitEvent).not.toBeNull();
    emitEvent!({
      type: "business_tool_request",
      request: {
        requestId: "req-callback-failed-1",
        sessionId: "session-1",
        integrationId: "crm",
        toolName: "save_note",
        arguments: {
          content: "这条笔记已经落本地",
        },
        timeoutSeconds: 60,
        expiresAt: 9_999_999_999,
        status: "pending",
        createdAt: 1,
        updatedAt: 1,
      },
    });

    await waitFor(() => {
      expect(notesStore.saveNote).toHaveBeenCalledTimes(1);
      expect(submitBusinessToolResult).toHaveBeenCalledTimes(1);
    });
    expect(submitBusinessToolResult).toHaveBeenCalledWith(
      TEST_CONFIG,
      "oauth-token",
      "session-1",
      "req-callback-failed-1",
      expect.stringContaining("note-2"),
    );
    expect(onLocalToolSuccess).toHaveBeenCalledWith("save_note");
    expect(toolStates.at(-1)).toEqual({
      requestId: "req-callback-failed-1",
      status: "failed",
      summary: "结果回写失败，本地工具已执行",
    });
  });

  it("结果回写失败后，同页重放 pending request 时只重试回写，不重复执行本地副作用", async () => {
    let emitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const submitBusinessToolResult = vi
      .fn()
      .mockRejectedValueOnce(new Error("temporary offline"))
      .mockResolvedValueOnce({
        id: "message-4",
        sessionId: "session-1",
        role: "assistant",
        messageKind: "text",
        clientMessageKey: "business-tool-result:3",
        content: "已保存笔记 note-3",
        attachments: [],
        createdAt: 4,
        updatedAt: 4,
      });
    const client: ExampleHubClient = {
      replaceTools: vi.fn(),
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
      listSessions: vi.fn(),
      createSession: vi.fn(),
      deleteSession: vi.fn(),
      appendMessage: vi.fn(),
      submitBusinessToolResult,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        emitEvent = onEvent;
        return () => undefined;
      }),
    };
    const notesStore = {
      saveNote: vi.fn().mockResolvedValue({
        id: "note-3",
        title: "未命名笔记",
        content: "这条笔记只允许写一次",
        tags: [],
        createdAt: 100,
        updatedAt: 100,
      }),
      searchNotes: vi.fn(),
      listNotes: vi.fn(),
    };

    const runtime = createChatRuntime({
      client,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry: createLocalToolRegistry(notesStore),
      onToolCallUpdate: vi.fn(),
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
    });

    runtime.start();
    expect(emitEvent).not.toBeNull();

    const requestEvent: ExampleSessionEvent = {
      type: "business_tool_request",
      request: {
        requestId: "req-retry-1",
        sessionId: "session-1",
        integrationId: "crm",
        toolName: "save_note",
        arguments: {
          content: "这条笔记只允许写一次",
        },
        timeoutSeconds: 60,
        expiresAt: 9_999_999_999,
        status: "pending",
        createdAt: 1,
        updatedAt: 1,
      },
    };

    emitEvent!(requestEvent);
    await waitFor(() => {
      expect(notesStore.saveNote).toHaveBeenCalledTimes(1);
      expect(submitBusinessToolResult).toHaveBeenCalledTimes(1);
    });

    emitEvent!(requestEvent);
    await waitFor(() => {
      expect(submitBusinessToolResult).toHaveBeenCalledTimes(2);
    });
    expect(notesStore.saveNote).toHaveBeenCalledTimes(1);
  });

  it("页面刷新后仍会优先重试暂存结果，而不是重复执行 save_note", async () => {
    let firstEmitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const firstSubmit = vi.fn().mockRejectedValue(new Error("temporary offline"));
    const firstClient: ExampleHubClient = {
      replaceTools: vi.fn(),
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
      listSessions: vi.fn(),
      createSession: vi.fn(),
      deleteSession: vi.fn(),
      appendMessage: vi.fn(),
      submitBusinessToolResult: firstSubmit,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        firstEmitEvent = onEvent;
        return () => undefined;
      }),
    };
    const notesStore = {
      saveNote: vi.fn().mockResolvedValue({
        id: "note-refresh-1",
        title: "未命名笔记",
        content: "刷新后不应再写第二次",
        tags: [],
        createdAt: 100,
        updatedAt: 100,
      }),
      searchNotes: vi.fn(),
      listNotes: vi.fn(),
    };
    const requestEvent: ExampleSessionEvent = {
      type: "business_tool_request",
      request: {
        requestId: "req-refresh-1",
        sessionId: "session-1",
        integrationId: "crm",
        toolName: "save_note",
        arguments: {
          content: "刷新后不应再写第二次",
        },
        timeoutSeconds: 60,
        expiresAt: 9_999_999_999,
        status: "pending",
        createdAt: 1,
        updatedAt: 1,
      },
    };

    const firstRuntime = createChatRuntime({
      client: firstClient,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry: createLocalToolRegistry(notesStore),
      onToolCallUpdate: vi.fn(),
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
    });
    firstRuntime.start();
    firstEmitEvent!(requestEvent);

    await waitFor(() => {
      expect(notesStore.saveNote).toHaveBeenCalledTimes(1);
      expect(firstSubmit).toHaveBeenCalledTimes(1);
    });

    let secondEmitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const secondSubmit = vi.fn().mockResolvedValue({
      id: "message-5",
      sessionId: "session-1",
      role: "assistant",
      messageKind: "text",
      clientMessageKey: "business-tool-result:4",
      content: "已保存笔记 note-refresh-1",
      attachments: [],
      createdAt: 5,
      updatedAt: 5,
    });
    const secondClient: ExampleHubClient = {
      ...firstClient,
      submitBusinessToolResult: secondSubmit,
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        secondEmitEvent = onEvent;
        return () => undefined;
      }),
    };
    const secondRuntime = createChatRuntime({
      client: secondClient,
      config: TEST_CONFIG,
      accessToken: "oauth-token",
      sessionId: "session-1",
      toolRegistry: createLocalToolRegistry(notesStore),
      onToolCallUpdate: vi.fn(),
      onSessionEvent: vi.fn(),
      onEventLog: vi.fn(),
    });
    secondRuntime.start();
    secondEmitEvent!(requestEvent);

    await waitFor(() => {
      expect(secondSubmit).toHaveBeenCalledTimes(1);
    });
    expect(notesStore.saveNote).toHaveBeenCalledTimes(1);
  });
});
